//! Bounded batching of printable callbacks; protocol and ASCII callbacks keep order.
use super::*;
use unicode_segmentation::GraphemeCursor;

const PREFIX: usize = crate::MAX_GRAPHEME_BYTES;
const RUN: usize = 512;

impl Terminal {
    pub(super) fn advance<const BATCH: bool>(&mut self, bytes: &[u8]) {
        // Keep small writes on the scalar path to avoid batching setup.
        // Preserve the existing direct ASCII path as well.
        if !BATCH || bytes.len() <= 4 || bytes.is_ascii() {
            self.parser.advance(bytes, &mut self.state);
            return;
        }
        let mut batch = PrintBatch {
            state: &mut self.state,
            buffer: [0; PREFIX + RUN],
            len: 0,
        };
        self.parser.advance(bytes, &mut batch);
        batch.flush();
    }
}

struct PrintBatch<'a> {
    state: &'a mut State,
    buffer: [u8; PREFIX + RUN],
    len: usize,
}

impl PrintBatch<'_> {
    fn flush(&mut self) {
        if self.len != 0 {
            self.state.print_run(&mut self.buffer, self.len);
            self.len = 0;
        }
    }
}

impl Perform for PrintBatch<'_> {
    fn print(&mut self, ch: char) {
        let width = ch.len_utf8();
        if self.len + width > RUN {
            self.flush();
        }
        ch.encode_utf8(&mut self.buffer[PREFIX + self.len..PREFIX + self.len + width]);
        self.len += width;
    }

    fn text(&mut self, text: &[u8]) {
        self.flush();
        self.state.text(text);
    }

    fn execute(&mut self, byte: u8) {
        self.flush();
        self.state.execute(byte);
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], ignore: bool, byte: u8) {
        self.flush();
        self.state.esc_dispatch(intermediates, ignore, byte);
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        self.flush();
        self.state
            .csi_dispatch(params, intermediates, ignore, action);
    }

    fn hook(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        self.flush();
        self.state.hook(params, intermediates, ignore, action);
    }

    fn put(&mut self, byte: u8) {
        self.flush();
        self.state.put(byte);
    }

    fn unhook(&mut self) {
        self.flush();
        self.state.unhook();
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], bell_terminated: bool) {
        self.flush();
        self.state.osc_dispatch(params, bell_terminated);
    }

    fn osc_overflow(&mut self) {
        self.flush();
        self.state.osc_overflow();
    }
}

impl State {
    fn print_scalars(&mut self, text: &str) {
        for ch in text.chars() {
            self.put_char(ch);
        }
    }

    // Snapshot only the cluster which the existing scalar path can continue.
    // The copy is independent of screen scrolling and grapheme-pool mutations.
    fn copy_print_prefix(&self, buffer: &mut [u8; PREFIX + RUN]) -> Option<usize> {
        let Some((x, y)) = self.last_grapheme else {
            return Some(0);
        };
        if x >= self.screen.cols || y >= self.screen.rows {
            return None;
        }
        let cell = self.screen.row(y)[x];
        let width = usize::from(cell.width.max(1));
        if self.screen.y != y
            || self.screen.x != (x + width).min(self.screen.cols - 1)
            || self.screen.wrap != (self.autowrap && x + width >= self.screen.cols)
        {
            return None;
        }
        let len: usize = self.graphemes.chars(&cell).map(char::len_utf8).sum();
        if len > PREFIX {
            return None;
        }
        let mut offset = PREFIX - len;
        for ch in self.graphemes.chars(&cell) {
            let end = offset + ch.len_utf8();
            ch.encode_utf8(&mut buffer[offset..end]);
            offset = end;
        }
        Some(len)
    }

    fn print_run(&mut self, buffer: &mut [u8; PREFIX + RUN], len: usize) {
        let end = PREFIX + len;
        if !self.autowrap
            || self.insert
            || if self.use_g1 { self.g1 } else { self.g0 }
            || self.grapheme_overflow
        {
            self.print_scalars(core::str::from_utf8(&buffer[PREFIX..end]).unwrap());
            return;
        }
        let Some(prefix) = self.copy_print_prefix(buffer) else {
            self.print_scalars(core::str::from_utf8(&buffer[PREFIX..end]).unwrap());
            return;
        };
        let text = core::str::from_utf8(&buffer[PREFIX - prefix..end]).unwrap();
        let mut cursor = GraphemeCursor::new(prefix, text.len(), true);
        if cursor.is_boundary(text, 0) != Ok(true) {
            self.print_scalars(&text[prefix..]);
            return;
        }
        let mut offset = prefix;
        while offset < text.len() {
            let next = match cursor.next_boundary(text, 0) {
                Ok(Some(next)) if next > offset => next,
                _ => {
                    self.print_scalars(&text[offset..]);
                    return;
                }
            };
            let mut chars = text[offset..next].chars();
            let ch = chars.next().unwrap();
            let width = UnicodeWidthChar::width(ch).unwrap_or(0).min(2);
            if chars.next().is_some() || width == 0 || self.grapheme_overflow {
                self.print_scalars(&text[offset..]);
                return;
            }
            self.write_char(ch, width);
            offset = next;
        }
    }
}

#[cfg(test)]
mod tests;
