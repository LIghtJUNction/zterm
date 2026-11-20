//! Image geometry, Sixel completion and terminal cursor integration.
use super::*;

impl Terminal {
    /// Set physical text-cell dimensions for graphics and CSI pixel-size replies.
    pub fn set_cell_size(&mut self, width: u32, height: u32) {
        let width = width.clamp(1, 4096);
        let height = height.clamp(1, 4096);
        if (self.state.cell_width, self.state.cell_height) != (width, height) {
            self.state.cell_width = width;
            self.state.cell_height = height;
            self.state.graphics.set_viewport(width, height, self.rows());
            self.state
                .inactive_graphics
                .set_viewport(width, height, self.rows());
            self.state.screen.mark_all();
        }
    }

    /// The current screen's bounded image store and placement scene.
    pub fn graphics(&self) -> &zgraphics::Graphics {
        &self.state.graphics
    }

    /// Visible image draws, with bounded cached text-backed geometry.
    pub fn graphics_draws(
        &self,
        history_offset: usize,
    ) -> impl Iterator<Item = zgraphics::Draw<'_>> {
        self.image_draws(history_offset)
    }
}

impl State {
    pub(super) fn erase_graphics(&mut self) {
        let previous = self.graphics.revision();
        let context = self.graphics_context();
        let outcome = self.graphics.erase_visible(context, &mut self.responses);
        self.graphics_result(previous, outcome);
    }

    pub(super) fn graphics_context(&self) -> zgraphics::Context {
        zgraphics::Context {
            col: self.screen.x,
            row: self.screen.y,
            cols: self.screen.cols,
            rows: self.screen.rows,
            cell_width: self.cell_width,
            cell_height: self.cell_height,
        }
    }

    pub(super) fn graphics_result(&mut self, previous: u64, outcome: zgraphics::Outcome) {
        if let Some((cols, rows)) = outcome.cursor_move {
            let col = self.screen.x.saturating_add(cols).min(self.screen.cols - 1);
            for _ in 0..rows.min(self.screen.rows) {
                self.linefeed();
            }
            self.screen.x = col;
            self.screen.wrap = false;
        }
        if self.graphics.revision() != previous {
            self.screen.mark_all();
        }
    }

    pub(super) fn finish_sixel(&mut self) {
        let mut context = self.graphics_context();
        if self.sixel_display {
            context.col = 0;
            context.row = 0;
        }
        let Some(result) = self.graphics.finish_sixel(context) else {
            return;
        };
        if !self.sixel_private {
            self.sixel_palette = Some(Box::new(result.palette));
        }
        if result.height == 0 {
            return;
        }
        if !self.sixel_display {
            let right = context
                .col
                .saturating_add(result.width.div_ceil(self.cell_width) as usize);
            let rows = result.height.div_ceil(self.cell_height) as usize;
            // With 8452 the cursor remains beside the last image row. The
            // default xterm behavior leaves it at the beginning of the next.
            let advance = rows.saturating_sub(usize::from(self.sixel_right));
            if self.screen.y <= self.screen.bottom {
                let end = self.screen.y.saturating_add(advance);
                let mut overflow = end.saturating_sub(self.screen.bottom);
                let region = self.screen.bottom - self.screen.top + 1;
                if self.screen.top != 0 || self.screen.bottom + 1 != self.screen.rows {
                    overflow = overflow.min(region);
                }
                while overflow != 0 {
                    let count = overflow.min(region);
                    self.scroll_up(self.screen.top, self.screen.bottom, count, true);
                    overflow -= count;
                }
                self.screen.y = end.min(self.screen.bottom);
            } else {
                self.screen.y = self
                    .screen
                    .y
                    .saturating_add(advance)
                    .min(self.screen.rows - 1);
            }
            self.screen.x = if self.sixel_right {
                right.min(self.screen.cols - 1)
            } else {
                0
            };
            self.screen.wrap = false;
        }
        self.screen.mark_all();
    }
}
