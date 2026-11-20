//! Cell editing, cursor movement, scrolling and bounded history retention.
use super::*;

impl State {
    pub(super) fn cursor_to(&mut self, x: usize, y: usize) {
        self.screen.dirty[self.screen.y] = true;
        self.screen.x = x.min(self.screen.cols - 1);
        self.screen.y = y.min(self.screen.rows - 1);
        self.screen.dirty[self.screen.y] = true;
        self.screen.wrap = false;
    }

    pub(super) fn save_cursor(&mut self) {
        self.screen.saved = SavedCursor {
            x: self.screen.x,
            y: self.screen.y,
            style: self.style,
            origin: self.screen.origin,
            g0: self.g0,
            g1: self.g1,
            use_g1: self.use_g1,
            wrap: self.screen.wrap,
        };
    }

    pub(super) fn restore_cursor(&mut self) {
        let saved = self.screen.saved;
        self.cursor_to(saved.x, saved.y);
        self.style = saved.style;
        self.screen.origin = saved.origin;
        self.screen.wrap = saved.wrap;
        self.g0 = saved.g0;
        self.g1 = saved.g1;
        self.use_g1 = saved.use_g1;
    }

    pub(super) fn record_history_row(&mut self, row: usize) {
        if self.modes.alternate_screen {
            return;
        }
        self.line_base = self.line_base.saturating_add(1);
        push_history(
            &mut self.history,
            self.history_limit,
            &mut self.history_bytes,
            self.screen.row(row),
            self.screen.row_wrap(row),
        );
    }

    pub(super) fn break_before(&mut self, row: usize) {
        if row != 0 {
            self.screen.set_row_wrap(row - 1, 0);
        } else if !self.modes.alternate_screen
            && let Some(row) = self.history.back_mut()
        {
            row.wrapped = 0;
        }
    }

    #[inline]
    pub(super) fn erase_sixel_span(&mut self, row: usize, start: usize, end: usize) {
        // Text-only output avoids scanning the image store. ASCII calls this
        // once per contiguous row span, never once per character.
        if self.graphics.is_empty() {
            return;
        }
        let mut start = start.min(self.screen.cols);
        let mut end = end.min(self.screen.cols);
        if start >= end {
            return;
        }
        let cells = self.screen.row(row);
        if start > 0 && cells[start].width == 0 {
            start -= 1;
        }
        if end < cells.len() && cells[end].width == 0 {
            end += 1;
        }
        self.graphics.erase_sixel_cells(start, row, end - start, 1);
    }

    pub(super) fn erase_range(&mut self, row: usize, start: usize, end: usize, blank: Cell) {
        if start == 0 && end >= self.screen.cols {
            self.break_before(row);
        }
        self.erase_sixel_span(row, start, end);
        self.screen.clear_range(row, start, end, blank);
    }

    pub(super) fn scroll_up(
        &mut self,
        top: usize,
        bottom: usize,
        count: usize,
        save_history: bool,
    ) {
        let count = count.min(bottom - top + 1);
        if top != 0 || !save_history {
            self.break_before(top);
        }
        if !self.modes.alternate_screen
            && (top != 0 || bottom + 1 != self.screen.rows || !save_history)
        {
            self.shell.invalidate_screen(self.line_base);
        }
        let blank = self.style.blank();
        if top == 0 && bottom + 1 == self.screen.rows {
            for _ in 0..count {
                if save_history {
                    self.record_history_row(0);
                }
                self.screen.row0 += 1;
                if self.screen.row0 == self.screen.rows {
                    self.screen.row0 = 0;
                }
                self.screen.clear_row(bottom, blank);
            }
        } else {
            self.screen
                .copy_rows(top + count, top, bottom - top + 1 - count);
            for y in bottom + 1 - count..=bottom {
                self.screen.clear_row(y, blank);
            }
        }
        if !save_history && count <= bottom - top {
            self.screen.set_row_wrap(bottom - count, 0);
        }
        self.graphics.scroll(
            top,
            bottom,
            -(count as i32),
            self.history.len(),
            save_history && !self.modes.alternate_screen,
        );
        self.screen.dirty[top..=bottom].fill(true);
    }

    pub(super) fn scroll_down(&mut self, top: usize, bottom: usize, count: usize) {
        self.break_before(top);
        if !self.modes.alternate_screen {
            self.shell.invalidate_screen(self.line_base);
        }
        let blank = self.style.blank();
        let count = count.min(bottom - top + 1);
        if top == 0 && bottom + 1 == self.screen.rows {
            for _ in 0..count {
                self.screen.row0 = if self.screen.row0 == 0 {
                    self.screen.rows - 1
                } else {
                    self.screen.row0 - 1
                };
                self.screen.clear_row(top, blank);
            }
        } else {
            self.screen
                .copy_rows(top, top + count, bottom - top + 1 - count);
            for y in top..top + count {
                self.screen.clear_row(y, blank);
            }
        }
        self.screen.set_row_wrap(bottom, 0);
        self.graphics
            .scroll(top, bottom, count as i32, self.history.len(), false);
        self.screen.dirty[top..=bottom].fill(true);
    }

    pub(super) fn linefeed(&mut self) {
        self.advance_line(0);
    }

    pub(super) fn soft_wrap(&mut self, columns: usize) {
        self.advance_line(columns);
        self.screen.x = 0;
    }

    pub(super) fn advance_line(&mut self, wrapped: usize) {
        let advances = self.screen.y == self.screen.bottom || self.screen.y + 1 < self.screen.rows;
        self.screen
            .set_row_wrap(self.screen.y, if advances { wrapped } else { 0 });
        self.screen.dirty[self.screen.y] = true;
        if self.screen.y == self.screen.bottom {
            self.scroll_up(self.screen.top, self.screen.bottom, 1, true);
        } else if self.screen.y + 1 < self.screen.rows {
            self.screen.y += 1;
        }
        self.screen.dirty[self.screen.y] = true;
        self.screen.wrap = false;
        if self.newline {
            self.screen.x = 0;
        }
    }

    pub(super) fn reverse_index(&mut self) {
        if self.screen.y == self.screen.top {
            self.scroll_down(self.screen.top, self.screen.bottom, 1);
        } else {
            self.cursor_to(self.screen.x, self.screen.y.saturating_sub(1));
        }
        self.screen.wrap = false;
    }

    pub(super) fn put_char(&mut self, mut ch: char) {
        if if self.use_g1 { self.g1 } else { self.g0 } {
            ch = dec_graphics(ch);
        }
        if self.extend_grapheme(ch) {
            return;
        }
        let width = UnicodeWidthChar::width(ch).unwrap_or(0).min(2);
        if width == 0 {
            return;
        }
        if self.screen.wrap && self.autowrap {
            self.soft_wrap(self.screen.cols);
        }
        let mut width = width;
        if width == 2 && self.screen.x + 1 >= self.screen.cols {
            if self.autowrap && self.screen.cols > 1 {
                self.erase_range(
                    self.screen.y,
                    self.screen.x,
                    self.screen.cols,
                    self.style.blank(),
                );
                self.soft_wrap(self.screen.cols - 1);
            } else {
                // A one-column viewport clips presentation, not stored text.
                // With autowrap disabled retain the existing margin fallback.
                if !self.autowrap {
                    ch = '\u{fffd}';
                }
                width = 1;
            }
        }
        let x = self.screen.x;
        let y = self.screen.y;
        if self.insert {
            self.insert_chars(width);
        }
        let style = self.style;
        self.erase_sixel_span(y, x, x + width);
        let cells = self.screen.overwrite_span(y, x, x + width, style.blank());
        cells[0] = style.cell(ch, width as u8);
        if width == 2 {
            cells[1] = style.cell('\0', 0);
        }
        self.screen.dirty[y] = true;
        self.last_char = ch;
        if x + width >= self.screen.cols {
            self.screen.x = self.screen.cols - 1;
            self.screen.wrap = self.autowrap;
        } else {
            self.screen.x += width;
            self.screen.wrap = false;
        }
        self.remember_grapheme(x, y, 1);
    }

    pub(super) fn ascii(&mut self, mut bytes: &[u8]) {
        let graphics_charset = if self.use_g1 { self.g1 } else { self.g0 };
        if self.insert || graphics_charset || !self.autowrap {
            for &byte in bytes {
                self.put_char(byte as char);
            }
            return;
        }
        if let Some(&first) = bytes.first()
            && self.extend_grapheme(first as char)
        {
            bytes = &bytes[1..];
        }
        while !bytes.is_empty() {
            if self.screen.wrap {
                self.soft_wrap(self.screen.cols);
            }
            let count = bytes.len().min(self.screen.cols - self.screen.x);
            let x = self.screen.x;
            let y = self.screen.y;
            let style = self.style;
            self.erase_sixel_span(y, x, x + count);
            for (cell, &byte) in self
                .screen
                .overwrite_span(y, x, x + count, style.blank())
                .iter_mut()
                .zip(bytes)
            {
                *cell = style.cell(byte as char, 1);
            }
            self.screen.dirty[y] = true;
            self.last_char = bytes[count - 1] as char;
            if x + count == self.screen.cols {
                self.screen.x = self.screen.cols - 1;
                self.screen.wrap = true;
            } else {
                self.screen.x += count;
            }
            self.remember_grapheme(x + count - 1, y, count);
            bytes = &bytes[count..];
        }
    }

    pub(super) fn insert_chars(&mut self, count: usize) {
        let x = self.screen.x;
        let y = self.screen.y;
        let cols = self.screen.cols;
        let count = count.min(cols - x);
        let blank = self.style.blank();
        if x == 0 && count == cols {
            self.break_before(y);
        }
        self.erase_sixel_span(y, x, cols);
        let row = self.screen.row_mut(y);
        if x > 0 && row[x].width == 0 {
            row[x - 1] = blank;
            row[x] = blank;
        }
        row.copy_within(x..cols - count, x + count);
        row[x..x + count].fill(blank);
        self.screen.repair_row(y, blank);
        self.screen.set_row_wrap(y, 0);
        self.screen.wrap = false;
    }

    pub(super) fn delete_chars(&mut self, count: usize) {
        let x = self.screen.x;
        let y = self.screen.y;
        let cols = self.screen.cols;
        let count = count.min(cols - x);
        let blank = self.style.blank();
        self.erase_sixel_span(y, x, cols);
        self.erase_range(y, x, x + count, blank);
        let row = self.screen.row_mut(y);
        row.copy_within(x + count..cols, x);
        row[cols - count..].fill(blank);
        self.screen.repair_row(y, blank);
        self.screen.set_row_wrap(y, 0);
        self.screen.wrap = false;
    }

    pub(super) fn erase_display(&mut self, mode: u16) {
        let blank = self.style.blank();
        let x = self.screen.x;
        let y = self.screen.y;
        match mode {
            0 => {
                self.erase_range(y, x, self.screen.cols, blank);
                self.graphics.erase_sixel_cells(
                    0,
                    y + 1,
                    self.screen.cols,
                    self.screen.rows - y - 1,
                );
                for row in y + 1..self.screen.rows {
                    self.screen.clear_row(row, blank);
                    self.screen.dirty[row] = true;
                }
            }
            1 => {
                if y != 0 {
                    self.break_before(0);
                }
                self.graphics.erase_sixel_cells(0, 0, self.screen.cols, y);
                for row in 0..y {
                    self.screen.clear_row(row, blank);
                    self.screen.dirty[row] = true;
                }
                self.erase_range(y, 0, x + 1, blank);
            }
            2 => {
                self.break_before(0);
                if !self.modes.alternate_screen {
                    self.shell.invalidate_screen(self.line_base);
                }
                self.screen.clear(blank);
                self.erase_graphics();
            }
            3 => {
                self.history.clear();
                self.history_bytes = 0;
                if self.modes.alternate_screen {
                    self.inactive_graphics.trim_history(0);
                } else {
                    self.graphics.trim_history(0);
                }
                self.screen.mark_all();
            }
            _ => {}
        }
        self.screen.wrap = false;
    }
}

pub(super) fn push_history(
    history: &mut VecDeque<HistoryRow>,
    limit: usize,
    used: &mut usize,
    row: &[Cell],
    wrapped: usize,
) {
    if limit == 0 {
        return;
    }
    let bytes = mem::size_of_val(row);
    if bytes + mem::size_of::<HistoryRow>() > MAX_HISTORY_BYTES {
        return;
    }
    let mut reusable = None;
    while !history.is_empty()
        && (history.len() >= limit
            || *used + bytes + history.capacity() * mem::size_of::<HistoryRow>()
                > MAX_HISTORY_BYTES)
    {
        let line = history.pop_front().unwrap();
        *used -= line.cells.capacity() * mem::size_of::<Cell>();
        reusable = Some(line.cells);
    }
    // Account for the allocated row container, including spare slots and wrap
    // metadata, as well as cell capacities. A row's text is still stored once.
    history.reserve(1);
    let mut line = reusable.unwrap_or_default();
    if line.capacity() > row.len() * 2 {
        line = Vec::new();
    }
    line.resize(row.len(), Cell::BLANK);
    line.copy_from_slice(row);
    while !history.is_empty()
        && *used
            + line.capacity() * mem::size_of::<Cell>()
            + history.capacity() * mem::size_of::<HistoryRow>()
            > MAX_HISTORY_BYTES
    {
        let old = history.pop_front().unwrap();
        *used -= old.cells.capacity() * mem::size_of::<Cell>();
    }
    *used += line.capacity() * mem::size_of::<Cell>();
    history.push_back(HistoryRow {
        cells: line,
        wrapped,
    });
}

fn dec_graphics(ch: char) -> char {
    match ch {
        '`' => '◆',
        'a' => '▒',
        'b' => '␉',
        'c' => '␌',
        'd' => '␍',
        'e' => '␊',
        'f' => '°',
        'g' => '±',
        'h' => '␤',
        'i' => '␋',
        'j' => '┘',
        'k' => '┐',
        'l' => '┌',
        'm' => '└',
        'n' => '┼',
        'o' => '⎺',
        'p' => '⎻',
        'q' => '─',
        'r' => '⎼',
        's' => '⎽',
        't' => '├',
        'u' => '┤',
        'v' => '┴',
        'w' => '┬',
        'x' => '│',
        'y' => '≤',
        'z' => '≥',
        '{' => 'π',
        '|' => '≠',
        '}' => '£',
        '~' => '·',
        _ => ch,
    }
}
