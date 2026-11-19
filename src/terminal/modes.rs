//! Terminal mode negotiation, alternate screens and keyboard protocol state.
use super::*;

impl State {
    pub(super) fn alternate(&mut self, enabled: bool, save: bool, clear: bool) {
        if enabled == self.modes.alternate_screen {
            return;
        }
        self.ui.reset();
        if enabled {
            if save {
                self.save_cursor();
            }
            let mut alternate = self.inactive_alternate.take().unwrap_or_else(|| {
                Screen::new(self.screen.cols, self.screen.rows, self.theme.style())
            });
            if clear {
                alternate.x = 0;
                alternate.y = 0;
                alternate.wrap = false;
            }
            self.primary = Some(mem::replace(&mut self.screen, alternate));
        } else if let Some(primary) = self.primary.take() {
            self.inactive_alternate = Some(mem::replace(&mut self.screen, primary));
            if save {
                self.restore_cursor();
            }
        }
        self.modes.alternate_screen = enabled;
        self.graphics.abort_transfer();
        self.graphics.advance_animations(0, &[]);
        mem::swap(&mut self.graphics, &mut self.inactive_graphics);
        self.graphics.abort_transfer();
        self.graphics
            .set_viewport(self.cell_width, self.cell_height, self.screen.rows);
        self.graphics.invalidate();
        self.modes.kitty_keyboard = self.screen.keyboard;
        if enabled && clear {
            self.screen.clear(self.style.blank());
            self.graphics.clear();
        }
        self.screen.mark_all();
    }

    pub(super) fn set_mode(&mut self, private: bool, mode: u16, enabled: bool) {
        if !private {
            match mode {
                4 => self.insert = enabled,
                20 => self.newline = enabled,
                _ => {}
            }
            return;
        }
        match mode {
            1 => self.modes.app_cursor = enabled,
            6 => {
                self.screen.origin = enabled;
                self.cursor_to(0, if enabled { self.screen.top } else { 0 });
            }
            7 => {
                self.autowrap = enabled;
                self.screen.wrap = false;
            }
            12 => self.modes.cursor_blink = Some(enabled),
            25 => {
                self.modes.cursor_visible = enabled;
                self.screen.dirty[self.screen.y] = true;
            }
            47 => self.alternate(enabled, false, false),
            1047 => {
                // DEC 1047 clears the alternate screen on exit, before it is cached.
                if !enabled && self.modes.alternate_screen {
                    self.screen.clear(self.style.blank());
                    self.graphics.clear();
                }
                self.alternate(enabled, false, false);
            }
            80 => self.sixel_display = enabled,
            1070 => self.sixel_private = enabled,
            8452 => self.sixel_right = enabled,
            1048 => {
                if enabled {
                    self.save_cursor();
                } else {
                    self.restore_cursor();
                }
            }
            1049 => self.alternate(enabled, true, true),
            9 | 1000 | 1002 | 1003 => {
                if enabled {
                    self.modes.mouse_tracking = mode;
                } else if self.modes.mouse_tracking == mode {
                    self.modes.mouse_tracking = 0;
                }
            }
            1004 => self.modes.focus_events = enabled,
            1006 => self.modes.sgr_mouse = enabled,
            2004 => self.modes.bracketed_paste = enabled,
            2026 => {
                self.sync_commit_pending |= self.modes.synchronized_output && !enabled;
                self.modes.synchronized_output = enabled;
            }
            _ => {}
        }
    }

    pub(super) fn query_mode(&mut self, private: bool, mode: u16) {
        let state = if private {
            match mode {
                1 => Some(self.modes.app_cursor),
                6 => Some(self.screen.origin),
                7 => Some(self.autowrap),
                12 => self.modes.cursor_blink,
                25 => Some(self.modes.cursor_visible),
                47 | 1047 | 1049 => Some(self.modes.alternate_screen),
                80 => Some(self.sixel_display),
                1070 => Some(self.sixel_private),
                8452 => Some(self.sixel_right),
                9 | 1000 | 1002 | 1003 => Some(self.modes.mouse_tracking == mode),
                1004 => Some(self.modes.focus_events),
                1006 => Some(self.modes.sgr_mouse),
                2004 => Some(self.modes.bracketed_paste),
                2026 => Some(self.modes.synchronized_output),
                _ => None,
            }
        } else {
            match mode {
                4 => Some(self.insert),
                20 => Some(self.newline),
                _ => None,
            }
        };
        self.reply_args(format_args!(
            "\x1b[{}{};{}$y",
            if private { "?" } else { "" },
            mode,
            match state {
                Some(true) => 1,
                Some(false) => 2,
                None => 0,
            }
        ));
    }

    pub(super) fn kitty(&mut self, marker: u8, groups: &[&[u16]]) {
        let value = param(groups, 0, 0) as u8 & 0b11111;
        match marker {
            b'?' => {
                let flags = self.screen.keyboard;
                self.reply_args(format_args!("\x1b[?{flags}u"));
            }
            b'>' => {
                if self.screen.keyboard_depth == self.screen.keyboard_stack.len() {
                    self.screen.keyboard_stack.copy_within(1.., 0);
                    self.screen.keyboard_depth -= 1;
                }
                self.screen.keyboard_stack[self.screen.keyboard_depth] = self.screen.keyboard;
                self.screen.keyboard_depth += 1;
                self.screen.keyboard = value;
            }
            b'<' => {
                let count = param(groups, 0, 1) as usize;
                if count > self.screen.keyboard_depth {
                    self.screen.keyboard = 0;
                    self.screen.keyboard_depth = 0;
                } else {
                    self.screen.keyboard_depth -= count;
                    self.screen.keyboard = self.screen.keyboard_stack[self.screen.keyboard_depth];
                }
            }
            b'=' => match param(groups, 1, 1) {
                1 => self.screen.keyboard = value,
                2 => self.screen.keyboard |= value,
                3 => self.screen.keyboard &= !value,
                _ => {}
            },
            _ => {}
        }
        self.modes.kitty_keyboard = self.screen.keyboard;
    }
}

impl Default for Modes {
    fn default() -> Self {
        Self {
            app_cursor: false,
            app_keypad: false,
            cursor_visible: true,
            cursor_blink: None,
            cursor_shape: None,
            bracketed_paste: false,
            focus_events: false,
            mouse_tracking: 0,
            sgr_mouse: false,
            synchronized_output: false,
            kitty_keyboard: 0,
            alternate_screen: false,
        }
    }
}

impl Modes {
    /// Snapshot only the modes needed by a platform-independent input encoder.
    ///
    /// Rendering state remains owned by this terminal model. The returned value
    /// can also be used by clients which depend on `zinput` directly.
    pub const fn input(&self) -> zinput::Modes {
        zinput::Modes {
            app_cursor: self.app_cursor,
            app_keypad: self.app_keypad,
            bracketed_paste: self.bracketed_paste,
            focus_events: self.focus_events,
            sgr_mouse: self.sgr_mouse,
            mouse_tracking: self.mouse_tracking,
            kitty_keyboard: self.kitty_keyboard,
        }
    }
}
