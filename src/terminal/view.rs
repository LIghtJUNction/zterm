//! Visible and retained text access, shell ranges and frontend acknowledgements.
use super::*;

impl Terminal {
    #[inline]
    pub fn cols(&self) -> usize {
        self.state.screen.cols
    }
    #[inline]
    pub fn rows(&self) -> usize {
        self.state.screen.rows
    }

    /// Invalidation token for borrowed text coordinates in this terminal.
    /// Nonempty input and actual resizes may change text, history, or its layout.
    /// Viewport scrolling and theme changes leave this token unchanged.
    pub fn text_revision(&self) -> u64 {
        self.text_revision
    }
    #[inline]
    pub fn cell(&self, col: usize, row: usize) -> &Cell {
        if col >= self.cols() || row >= self.rows() {
            return &self.state.screen.blank;
        }
        &self.state.screen.cells[self.state.screen.index(col, row)]
    }

    /// Complete original extended cluster for this Terminal-owned cell.
    /// None denotes ordinary inline scalar content. IDs are scoped to this
    /// Terminal and may be reclaimed after no retained cell references them.
    pub fn grapheme(&self, cell: &Cell) -> Option<&str> {
        self.state.graphemes.get(cell.grapheme_id)
    }

    /// Iterate the original cluster without normalization or allocation.
    pub fn cell_chars(&self, cell: &Cell) -> crate::CellChars<'_> {
        self.state.graphemes.chars(cell)
    }

    /// Convert viewport coordinates to a retained location. The right-hand
    /// boundary (col == cols) and retained rows below the viewport are allowed.
    pub fn view_position(
        &self,
        col: usize,
        row: usize,
        offset: usize,
    ) -> Option<crate::TextPosition> {
        if col > self.cols() {
            return None;
        }
        let base = if self.state.modes.alternate_screen {
            0
        } else {
            self.state.line_base
        };
        let top = base.saturating_sub(offset.min(self.history_len()) as u64);
        let line = top.checked_add(u64::try_from(row).ok()?)?;
        self.retained_row(line).filter(|cells| col <= cells.len())?;
        Some(crate::TextPosition { line, column: col })
    }

    /// Borrow a cell at a current retained location, including scrollback.
    pub fn position_cell(&self, position: crate::TextPosition) -> Option<&Cell> {
        self.retained_row(position.line)?.get(position.column)
    }

    /// Map a current retained location back to viewport coordinates. Rows below
    /// the viewport are retained so a selection can survive later scrolling.
    pub fn position_in_view(
        &self,
        position: crate::TextPosition,
        offset: usize,
    ) -> Option<(usize, usize)> {
        self.retained_row(position.line)
            .filter(|cells| position.column <= cells.len())?;
        let base = if self.state.modes.alternate_screen {
            0
        } else {
            self.state.line_base
        };
        let top = base.saturating_sub(offset.min(self.history_len()) as u64);
        Some((
            position.column,
            usize::try_from(position.line.checked_sub(top)?).ok()?,
        ))
    }

    pub(super) fn retained_row(&self, line: u64) -> Option<&[Cell]> {
        if self.state.modes.alternate_screen {
            let row = usize::try_from(line).ok()?;
            return (row < self.rows()).then(|| self.state.screen.row(row));
        }
        if line >= self.state.line_base {
            let row = usize::try_from(line - self.state.line_base).ok()?;
            return (row < self.rows()).then(|| self.state.screen.row(row));
        }
        let oldest = self
            .state
            .line_base
            .saturating_sub(self.state.history.len() as u64);
        let row = usize::try_from(line.checked_sub(oldest)?).ok()?;
        self.state.history.get(row).map(|row| row.cells.as_slice())
    }

    /// View the visible screen, moved `offset` lines into primary-screen history.
    pub fn view_cell(&self, col: usize, row: usize, offset: usize) -> &Cell {
        if col >= self.cols() || row >= self.rows() {
            return &self.state.screen.blank;
        }
        if self.state.modes.alternate_screen {
            return self.cell(col, row);
        }
        let history = &self.state.history;
        let absolute = history.len().saturating_sub(offset.min(history.len())) + row;
        if absolute >= history.len() {
            return self.cell(col, absolute - history.len());
        }
        let line = &history[absolute].cells;
        match line.get(col) {
            Some(cell) if !(cell.width == 2 && col + 1 >= self.cols()) => cell,
            _ => &self.state.screen.blank,
        }
    }

    /// A soft-wrap boundary in the same viewport coordinates as `view_cell`.
    ///
    /// `Some(columns)` joins this row to the next without a newline. Only
    /// `[0, columns)` belongs to its text: a wide glyph can wrap before the
    /// final padding cell. Real spaces in that range must be retained.
    /// `None` denotes a hard boundary, an out-of-range row, or a soft wrap
    /// whose text has been clipped by a narrower viewport.
    pub fn view_row_wrap(&self, row: usize, offset: usize) -> Option<usize> {
        if row >= self.rows() {
            return None;
        }
        let history = self.history_len();
        let absolute = history.saturating_sub(offset.min(history)) + row;
        let wrapped = if absolute < history {
            self.state.history[absolute].wrapped
        } else {
            self.state.screen.row_wrap(absolute - history)
        };
        (wrapped != 0 && wrapped <= self.cols()).then_some(wrapped)
    }

    #[inline]
    pub fn cursor(&self) -> (usize, usize) {
        (self.state.screen.x, self.state.screen.y)
    }
    #[inline]
    pub fn modes(&self) -> &Modes {
        &self.state.modes
    }
    pub fn title(&self) -> &str {
        &self.state.title
    }
    pub fn working_directory(&self) -> &str {
        &self.state.cwd
    }
    /// OSC 133 records, oldest first. No storage is allocated before the first mark.
    pub fn commands(
        &self,
    ) -> impl DoubleEndedIterator<Item = &crate::CommandRecord> + ExactSizeIterator {
        self.state.shell.records.iter()
    }

    pub fn shell_phase(&self) -> crate::ShellPhase {
        self.state.shell.phase
    }

    /// Scrollback offset that makes a retained command's prompt visible.
    pub fn command_scroll_offset(&self, id: u64) -> Option<usize> {
        if self.state.modes.alternate_screen {
            return None;
        }
        let command = self
            .state
            .shell
            .records
            .iter()
            .find(|record| record.id == id)?;
        let oldest = self
            .state
            .line_base
            .saturating_sub(self.state.history.len() as u64);
        if !command.available || command.prompt.line < oldest {
            return None;
        }
        Some(self.state.line_base.saturating_sub(command.prompt.line) as usize)
    }

    /// Copy rendered command output, excluding its input and following prompt.
    ///
    /// Soft-wrapped rows join without a newline; hard line boundaries remain.
    /// Trailing blank cells on hard lines are omitted.
    /// This reads existing primary-screen/history cells; it does not save a second
    /// output transcript. Evicted or destructively edited ranges set `truncated`.
    /// `max_bytes` is capped at `MAX_COMMAND_TEXT` and never splits UTF-8.
    pub fn command_output(&self, id: u64, max_bytes: usize) -> Option<crate::CommandText> {
        let record = self
            .state
            .shell
            .records
            .iter()
            .find(|record| record.id == id)?;
        let start = record.output?;
        let end = record.end.unwrap_or_else(|| self.state.primary_position());
        Some(self.state.command_text(
            start,
            end,
            max_bytes,
            record.available,
            record.end.is_some(),
        ))
    }

    /// Copy the rendered command line when the shell supplied an OSC 133 B mark.
    pub fn command_input(&self, id: u64, max_bytes: usize) -> Option<crate::CommandText> {
        let record = self
            .state
            .shell
            .records
            .iter()
            .find(|record| record.id == id)?;
        let start = record.input?;
        let end = record
            .output
            .unwrap_or_else(|| self.state.primary_position());
        Some(self.state.command_text(
            start,
            end,
            max_bytes,
            record.available,
            record.output.is_some(),
        ))
    }

    pub fn responses(&self) -> &[u8] {
        &self.state.responses
    }
    pub fn clear_responses(&mut self) {
        self.state.responses.clear();
    }
    pub fn dirty(&self) -> bool {
        self.state.screen.dirty.iter().any(|v| *v)
    }
    pub fn row_dirty(&self, row: usize) -> bool {
        self.state.screen.dirty.get(row).copied().unwrap_or(false)
    }
    pub fn clear_dirty(&mut self) {
        self.state.screen.dirty.fill(false);
        self.state.sync_commit_pending = false;
    }
    pub fn mark_all_dirty(&mut self) {
        self.state.screen.mark_all();
    }
    pub fn history_len(&self) -> usize {
        if self.state.modes.alternate_screen {
            0
        } else {
            self.state.history.len()
        }
    }
    /// Absolute row at the top of the primary screen. Unlike history_len(),
    /// this advances when a full history ring evicts its oldest rows. A full
    /// terminal reset restarts the position at zero. Width reflow can decrease
    /// this coordinate; retain locations through resize_anchored across resizes.
    pub fn history_position(&self) -> u64 {
        self.state.line_base
    }
    pub fn hyperlink(&self, id: u32) -> Option<&str> {
        if id == 0 {
            return None;
        }
        self.state
            .links
            .get(id as usize - 1)
            .map(|link| link.uri.as_str())
    }
    /// Placeholder link_id is a graphics placement ID, never an OSC 8 link.
    pub fn cell_hyperlink(&self, cell: &Cell) -> Option<&str> {
        if cell.ch == '\u{10eeee}' {
            None
        } else {
            self.hyperlink(cell.link_id)
        }
    }

    /// Whether a synchronized frame ended since the frontend last cleared damage.
    ///
    /// This remains set when begin/end arrive in the same PTY chunk. Frontends
    /// still honor an active synchronized-output mode before presenting.
    pub fn sync_commit_pending(&self) -> bool {
        self.state.sync_commit_pending
    }

    /// A frontend may end a stalled synchronized frame after a documented timeout.
    pub fn force_end_sync(&mut self) {
        self.state.set_mode(true, 2026, false);
    }
}

impl State {
    pub(super) fn primary_screen(&self) -> &Screen {
        self.primary.as_ref().unwrap_or(&self.screen)
    }

    pub(super) fn primary_position(&self) -> crate::TextPosition {
        let screen = self.primary_screen();
        crate::TextPosition {
            line: self.line_base.saturating_add(screen.y as u64),
            column: screen.x + usize::from(screen.wrap),
        }
    }

    pub(super) fn command_text(
        &self,
        start: crate::TextPosition,
        end: crate::TextPosition,
        max_bytes: usize,
        available: bool,
        complete: bool,
    ) -> crate::CommandText {
        let mut result = crate::CommandText {
            text: String::new(),
            truncated: !available,
            complete,
        };
        if !available || end < start {
            result.truncated = true;
            return result;
        }
        let limit = max_bytes.min(crate::MAX_COMMAND_TEXT);
        let screen = self.primary_screen();
        let oldest = self.line_base.saturating_sub(self.history.len() as u64);
        let last = self.line_base.saturating_add(screen.rows as u64 - 1);
        let first = start.line.max(oldest);
        result.truncated |= start.line < oldest || end.line > last;
        if first > end.line || first > last {
            result.truncated |= start != end;
            return result;
        }
        let mut pending_spaces = 0usize;
        for line in first..=end.line.min(last) {
            let (cells, wrapped) = if line < self.line_base {
                let row = &self.history[(line - oldest) as usize];
                (row.cells.as_slice(), row.wrapped)
            } else {
                let row = (line - self.line_base) as usize;
                (screen.row(row), screen.row_wrap(row))
            };
            let joined = wrapped != 0 && line < end.line;
            let from = if line == start.line { start.column } else { 0 };
            let until = if line == end.line {
                end.column
            } else if joined {
                wrapped.min(cells.len())
            } else {
                cells.len()
            };
            result.truncated |= from > cells.len() || until > cells.len();
            let from = from.min(cells.len());
            let until = until.min(cells.len()).max(from);
            for cell in &cells[from..until] {
                if cell.width == 0 {
                    continue;
                }
                // Reflow can retain blank columns for saved cursor positions.
                // Delay plain spaces until another glyph makes them interior
                // to the logical line, including across soft row boundaries.
                if cell.ch == ' ' && cell.combining == ['\0'; 2] && cell.grapheme_id == 0 {
                    pending_spaces = pending_spaces.saturating_add(1);
                    continue;
                }
                let chars = self.graphemes.chars(cell);
                let bytes = chars.clone().map(char::len_utf8).sum::<usize>();
                let spaces = pending_spaces.min(limit - result.text.len());
                result.text.extend(core::iter::repeat_n(' ', spaces));
                if spaces != pending_spaces || bytes > limit - result.text.len() {
                    result.truncated = true;
                    return result;
                }
                pending_spaces = 0;
                result.text.extend(chars);
            }
            if !joined {
                pending_spaces = 0;
            }
            if line < end.line && !joined {
                if result.text.len() == limit {
                    result.truncated = true;
                    return result;
                }
                result.text.push('\n');
            }
        }
        result
    }
}
