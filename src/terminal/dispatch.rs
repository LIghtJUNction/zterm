//! VT parser callbacks and bounded protocol replies.
use super::*;

impl State {
    pub(super) fn reply(&mut self, value: &str) {
        if self.responses.len() + value.len() <= MAX_RESPONSES {
            self.responses.extend_from_slice(value.as_bytes());
        }
    }

    pub(super) fn reply_args(&mut self, arguments: fmt::Arguments<'_>) {
        let mut buffer = ReplyBuffer {
            bytes: [0; 64],
            len: 0,
        };
        if buffer.write_fmt(arguments).is_ok() && self.responses.len() + buffer.len <= MAX_RESPONSES
        {
            self.responses
                .extend_from_slice(&buffer.bytes[..buffer.len]);
        }
    }

    pub(super) fn reply_color(&mut self, command: u16, index: Option<u16>, color: u32, bell: bool) {
        let red = ((color >> 16) & 255) * 257;
        let green = ((color >> 8) & 255) * 257;
        let blue = (color & 255) * 257;
        let end = if bell { "\x07" } else { "\x1b\\" };
        if let Some(index) = index {
            self.reply(&format!(
                "\x1b]{command};{index};rgb:{red:04x}/{green:04x}/{blue:04x}{end}"
            ));
        } else {
            self.reply(&format!(
                "\x1b]{command};rgb:{red:04x}/{green:04x}/{blue:04x}{end}"
            ));
        }
    }
}

// Common numeric protocol replies fit in this stack buffer. Formatting fails
// atomically if the whole reply cannot fit; no temporary String is allocated.
struct ReplyBuffer {
    bytes: [u8; 64],
    len: usize,
}

impl fmt::Write for ReplyBuffer {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        if text.len() > self.bytes.len() - self.len {
            return Err(fmt::Error);
        }
        self.bytes[self.len..self.len + text.len()].copy_from_slice(text.as_bytes());
        self.len += text.len();
        Ok(())
    }
}

impl Perform for State {
    fn print(&mut self, ch: char) {
        self.put_char(ch);
    }
    fn text(&mut self, ascii: &[u8]) {
        self.ascii(ascii);
    }

    fn execute(&mut self, byte: u8) {
        self.last_grapheme = None;
        match byte {
            0x08 => self.cursor_to(self.screen.x.saturating_sub(1), self.screen.y),
            0x09 => {
                let x = (self.screen.x + 1..self.screen.cols)
                    .find(|x| self.tabs[*x])
                    .unwrap_or(self.screen.cols - 1);
                self.cursor_to(x, self.screen.y);
            }
            0x0a..=0x0c | 0x84 => self.linefeed(),
            0x0d => self.cursor_to(0, self.screen.y),
            0x0e => self.use_g1 = true,
            0x0f => self.use_g1 = false,
            0x85 => {
                self.linefeed();
                self.screen.x = 0;
            }
            0x88 => self.tabs[self.screen.x] = true,
            0x8d => self.reverse_index(),
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], ignore: bool, byte: u8) {
        self.last_grapheme = None;
        if ignore {
            return;
        }
        match (intermediates, byte) {
            ([], b'7') => self.save_cursor(),
            ([], b'8') => self.restore_cursor(),
            ([], b'D') => self.linefeed(),
            ([], b'E') => {
                self.linefeed();
                self.screen.x = 0;
            }
            ([], b'M') => self.reverse_index(),
            ([], b'H') => self.tabs[self.screen.x] = true,
            ([], b'=') => self.modes.app_keypad = true,
            ([], b'>') => self.modes.app_keypad = false,
            ([], b'c') => {
                let (cols, rows, history) =
                    (self.screen.cols, self.screen.rows, self.history_limit);
                let (width, height) = (self.cell_width, self.cell_height);
                let mut graphics = mem::take(&mut self.graphics);
                let mut inactive_graphics = mem::take(&mut self.inactive_graphics);
                graphics.clear();
                inactive_graphics.clear();
                let mut shell = mem::take(&mut self.shell);
                shell.clear();
                let mut ui = mem::take(&mut self.ui);
                ui.reset();
                let sync_commit_pending =
                    self.sync_commit_pending || self.modes.synchronized_output;
                let mut clipboard = mem::take(&mut self.clipboard);
                clipboard.cancel();
                *self = Self::new(cols, rows, history, self.theme);
                self.clipboard = clipboard;
                self.sync_commit_pending = sync_commit_pending;
                self.shell = shell;
                self.ui = ui;
                self.graphics = graphics;
                self.inactive_graphics = inactive_graphics;
                self.cell_width = width;
                self.cell_height = height;
                self.graphics.set_viewport(width, height, rows);
            }
            ([b'('], b'0') => self.g0 = true,
            ([b'('], b'B' | b'A') => self.g0 = false,
            ([b')'], b'0') => self.g1 = true,
            ([b')'], b'B' | b'A') => self.g1 = false,
            ([b'#'], b'8') => {
                self.break_before(0);
                self.screen.cells.fill(self.theme.style().cell('E', 1));
                self.screen.wrapped.fill(0);
                self.screen.mark_all();
            }
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        if action != 'm' {
            self.last_grapheme = None;
        }
        if ignore {
            return;
        }
        if action == 'm' && intermediates.is_empty() {
            self.sgr(params);
            return;
        }
        let mut storage: [&[u16]; 32] = [&[]; 32];
        let mut count = 0;
        for group in params.iter() {
            storage[count] = group;
            count += 1;
        }
        let groups = &storage[..count];
        let private = intermediates.first() == Some(&b'?');
        if action == 'u'
            && intermediates.len() == 1
            && matches!(intermediates[0], b'?' | b'>' | b'<' | b'=')
        {
            self.kitty(intermediates[0], groups);
            return;
        }
        if action == 'q' && intermediates == b" " {
            let style = param(groups, 0, 0);
            let shape = match style {
                0..=2 => CursorShape::Block,
                3 | 4 => CursorShape::Underline,
                5 | 6 => CursorShape::Bar,
                _ => return,
            };
            self.modes.cursor_shape = Some(shape);
            self.modes.cursor_blink = Some(style == 0 || style % 2 == 1);
            return;
        }
        if action == 'p' && (intermediates == b"?$" || intermediates == b"$") {
            for group in groups {
                self.query_mode(private, group.first().copied().unwrap_or(0));
            }
            return;
        }
        if (action == 'h' || action == 'l') && (intermediates.is_empty() || intermediates == b"?") {
            for group in groups {
                self.set_mode(private, group.first().copied().unwrap_or(0), action == 'h');
            }
            return;
        }
        if action == 'c' && param(groups, 0, 0) == 0 {
            match intermediates {
                // VT220-style DA1 advertises the implemented sixel capability.
                b"" => self.reply("\x1b[?62;4c"),
                b">" => self.reply("\x1b[>0;1;0c"),
                _ => {}
            }
            return;
        }
        if action == 'n' && (intermediates.is_empty() || private) {
            match param(groups, 0, 0) {
                5 if !private => self.reply("\x1b[0n"),
                6 => {
                    let row = if self.screen.origin {
                        self.screen.y.saturating_sub(self.screen.top)
                    } else {
                        self.screen.y
                    };
                    let column = self.screen.x + 1;
                    self.reply_args(format_args!(
                        "\x1b[{}{};{}R",
                        if private { "?" } else { "" },
                        row + 1,
                        column
                    ));
                }
                _ => {}
            }
            return;
        }
        if !intermediates.is_empty() {
            return;
        }
        let p = param(groups, 0, 1) as usize;
        let raw = param(groups, 0, 0);
        let x = self.screen.x;
        let y = self.screen.y;
        let upper = if self.screen.origin {
            self.screen.top
        } else {
            0
        };
        let lower = if self.screen.origin {
            self.screen.bottom
        } else {
            self.screen.rows - 1
        };
        let move_upper = if y >= self.screen.top {
            self.screen.top
        } else {
            0
        };
        let move_lower = if y <= self.screen.bottom {
            self.screen.bottom
        } else {
            self.screen.rows - 1
        };
        match action {
            'A' => self.cursor_to(x, y.saturating_sub(p).max(move_upper)),
            'B' | 'e' => self.cursor_to(x, y.saturating_add(p).min(move_lower)),
            'C' | 'a' => self.cursor_to(x.saturating_add(p), y),
            'D' => self.cursor_to(x.saturating_sub(p), y),
            'E' => self.cursor_to(0, y.saturating_add(p).min(move_lower)),
            'F' => self.cursor_to(0, y.saturating_sub(p).max(move_upper)),
            'G' | '`' => self.cursor_to(p - 1, y),
            'H' | 'f' => {
                self.cursor_to(param(groups, 1, 1) as usize - 1, (upper + p - 1).min(lower))
            }
            'd' => self.cursor_to(x, (upper + p - 1).min(lower)),
            'J' => self.erase_display(raw),
            'K' => {
                match raw {
                    0 => self.erase_range(y, x, self.screen.cols, self.style.blank()),
                    1 => self.erase_range(y, 0, x + 1, self.style.blank()),
                    2 => self.erase_range(y, 0, self.screen.cols, self.style.blank()),
                    _ => {}
                }
                self.screen.wrap = false;
            }
            'X' => {
                self.erase_range(y, x, x.saturating_add(p), self.style.blank());
                self.screen.wrap = false;
            }
            '@' => self.insert_chars(p),
            'P' => self.delete_chars(p),
            'L' if y >= self.screen.top && y <= self.screen.bottom => {
                self.scroll_down(y, self.screen.bottom, p)
            }
            'M' if y >= self.screen.top && y <= self.screen.bottom => {
                self.scroll_up(y, self.screen.bottom, p, false)
            }
            'S' => self.scroll_up(self.screen.top, self.screen.bottom, p, false),
            'T' => self.scroll_down(self.screen.top, self.screen.bottom, p),
            's' => self.save_cursor(),
            'u' => self.restore_cursor(),
            'r' => {
                let top = p.saturating_sub(1).min(self.screen.rows - 1);
                let bottom = (param(groups, 1, self.screen.rows as u16) as usize - 1)
                    .min(self.screen.rows - 1);
                if top < bottom {
                    self.screen.top = top;
                    self.screen.bottom = bottom;
                    self.cursor_to(0, if self.screen.origin { top } else { 0 });
                }
            }
            'g' => match raw {
                0 => self.tabs[x] = false,
                3 => self.tabs.fill(false),
                _ => {}
            },
            'I' => {
                let mut x = x;
                for _ in 0..p.min(self.screen.cols) {
                    x = (x + 1..self.screen.cols)
                        .find(|x| self.tabs[*x])
                        .unwrap_or(self.screen.cols - 1);
                }
                self.cursor_to(x, y);
            }
            'Z' => {
                let mut x = x;
                for _ in 0..p.min(self.screen.cols) {
                    x = (0..x).rev().find(|x| self.tabs[*x]).unwrap_or(0);
                }
                self.cursor_to(x, y);
            }
            'b' => {
                for _ in 0..p.min(65535) {
                    self.put_char(self.last_char);
                }
            }
            't' if raw == 18 => {
                let (rows, cols) = (self.screen.rows, self.screen.cols);
                self.reply_args(format_args!("\x1b[8;{rows};{cols}t"));
            }
            't' if raw == 14 => {
                let height = self.screen.rows as u32 * self.cell_height;
                let width = self.screen.cols as u32 * self.cell_width;
                self.reply_args(format_args!("\x1b[4;{height};{width}t"));
            }
            't' if raw == 16 => {
                let (height, width) = (self.cell_height, self.cell_width);
                self.reply_args(format_args!("\x1b[6;{height};{width}t"));
            }
            _ => {}
        }
    }

    fn osc_overflow(&mut self) {
        self.osc_overflow = true;
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], bell_terminated: bool) {
        self.last_grapheme = None;
        if mem::take(&mut self.osc_overflow) {
            return;
        }
        let Some(command) = params.first() else {
            return;
        };
        match *command {
            b"0" | b"2" if params.len() > 1 => self.title = join_text_fields(&params[1..], 256),
            b"7" if params.len() > 1 => self.cwd = join_text_fields(&params[1..], 2048),
            b"133" if !self.modes.alternate_screen => {
                self.shell.mark(&params[1..], self.primary_position());
            }
            b"8" => {
                if params.len() < 3 {
                    return;
                }
                let mut uri = String::new();
                for (i, part) in params[2..].iter().enumerate() {
                    if i > 0 {
                        uri.push(';');
                    }
                    if let Ok(text) = core::str::from_utf8(part) {
                        uri.push_str(text);
                    } else {
                        return;
                    }
                }
                if uri.is_empty() {
                    self.style.link_id = 0;
                    return;
                }
                if uri.len() > 2048 || uri.chars().any(char::is_control) {
                    self.style.link_id = 0;
                    return;
                }
                let id = clean_text(params[1], 256);
                if let Some(index) = self
                    .links
                    .iter()
                    .position(|link| link.id == id && link.uri == uri)
                {
                    self.style.link_id = index as u32 + 1;
                } else if self.links.len() < MAX_LINKS {
                    self.links.push(Link { id, uri });
                    self.style.link_id = self.links.len() as u32;
                } else {
                    self.style.link_id = 0;
                }
            }
            b"4" => {
                for pair in params[1..].as_chunks::<2>().0 {
                    if pair[1] != b"?" {
                        continue;
                    }
                    if let Some(index) = core::str::from_utf8(pair[0])
                        .ok()
                        .and_then(|value| value.parse::<u16>().ok())
                        .filter(|index| *index <= 255)
                    {
                        self.reply_color(4, Some(index), self.theme.color(index), bell_terminated);
                    }
                }
            }
            b"10" | b"11" => {
                let first = if *command == b"10" { 10 } else { 11 };
                for (offset, value) in params[1..].iter().enumerate() {
                    let command = first + offset;
                    if command > 11 {
                        break;
                    }
                    if *value == b"?" {
                        self.reply_color(
                            command as u16,
                            None,
                            if command == 10 {
                                self.theme.foreground
                            } else {
                                self.theme.background
                            },
                            bell_terminated,
                        );
                    }
                }
            }
            // OSC 52 is streamed into a separate bounded request queue above.
            // The native host enforces session ownership before clipboard I/O.
            _ => {}
        }
    }

    fn hook(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        self.last_grapheme = None;
        self.dcs.clear();
        self.dcs_overflow = false;
        self.dcs_query = !ignore && intermediates == b"+" && action == 'q';
        self.sixel_active = !ignore
            && intermediates.is_empty()
            && action == 'q'
            && params.iter().count() <= 3
            && params.iter().all(|group| group.len() == 1);
        if self.sixel_active {
            let aspect = params
                .iter()
                .next()
                .and_then(|p| p.first())
                .copied()
                .unwrap_or(0);
            let background = params
                .iter()
                .nth(1)
                .and_then(|p| p.first())
                .copied()
                .unwrap_or(0);
            let rgb = self.style.bg;
            self.graphics.begin_sixel(
                aspect,
                background == 1,
                [(rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8],
                if self.sixel_private {
                    None
                } else {
                    self.sixel_palette.as_deref()
                },
            );
        }
    }

    fn put(&mut self, byte: u8) {
        if self.sixel_active {
            self.graphics.sixel_put(byte);
        }
        if self.dcs_query {
            if self.dcs.len() < 512 {
                self.dcs.push(byte);
            } else {
                self.dcs_overflow = true;
            }
        }
    }

    fn unhook(&mut self) {
        if self.sixel_active {
            self.sixel_active = false;
            if self.sixel_complete {
                self.finish_sixel();
            } else {
                self.graphics.abort_sixel();
            }
        }
        if !self.dcs_query || self.dcs_overflow {
            self.dcs_query = false;
            return;
        }
        let mut start = 0;
        while start < self.dcs.len() {
            let end = self.dcs[start..]
                .iter()
                .position(|b| *b == b';')
                .map(|v| start + v)
                .unwrap_or(self.dcs.len());
            let name = &self.dcs[start..end];
            let result = match name {
                b"544e" | b"544E" => Some("787465726d2d323536636f6c6f72"), // TN=xterm-256color
                b"436f" | b"436F" => Some("323536"),                       // Co=256
                b"5463" => Some(""),                                       // Tc boolean
                _ => None,
            };
            if name.iter().all(u8::is_ascii_hexdigit) && name.len().is_multiple_of(2) {
                let name = core::str::from_utf8(name).unwrap_or("").to_string();
                if let Some(value) = result {
                    self.reply(&format!("\x1bP1+r{name}={value}\x1b\\"));
                } else {
                    self.reply(&format!("\x1bP0+r{name}\x1b\\"));
                }
            }
            start = end + 1;
        }
        self.dcs_query = false;
    }
}

fn join_text_fields(fields: &[&[u8]], max: usize) -> String {
    let mut text = String::new();
    for (index, field) in fields.iter().enumerate() {
        if index > 0 {
            if text.len() == max {
                break;
            }
            text.push(';');
        }
        let Ok(value) = core::str::from_utf8(field) else {
            return String::new();
        };
        for ch in value.chars().filter(|ch| !ch.is_control()) {
            if text.len() + ch.len_utf8() > max {
                return text;
            }
            text.push(ch);
        }
    }
    text
}

fn clean_text(bytes: &[u8], max: usize) -> String {
    let mut text = String::new();
    if let Ok(value) = core::str::from_utf8(bytes) {
        for ch in value.chars().filter(|ch| !ch.is_control()) {
            if text.len() + ch.len_utf8() > max {
                break;
            }
            text.push(ch);
        }
    }
    text
}
