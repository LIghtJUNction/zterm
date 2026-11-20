use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt::{self, Write};
use core::mem;
use unicode_width::UnicodeWidthChar;
use zvte::{Params, Parser, Perform};

use crate::screen::*;

mod checkpoint;
mod dispatch;
mod editing;
mod framing;
mod grapheme;
mod graphics;
mod image_cells;
mod modes;
mod reflow;
mod style;
mod view;

use editing::push_history;
pub use reflow::ResizeAnchor;

const MAX_HISTORY_BYTES: usize = 8 * 1024 * 1024;
const MAX_LINKS: usize = 1024;
const MAX_RESPONSES: usize = 65536;

struct HistoryRow {
    cells: Vec<Cell>,
    wrapped: usize,
}

pub const ANSI_PALETTE: [u32; 16] = [
    0x000000, 0xcd0000, 0x00cd00, 0xcdcd00, 0x0000ee, 0xcd00cd, 0x00cdcd, 0xe5e5e5, 0x7f7f7f,
    0xff0000, 0x00ff00, 0xffff00, 0x5c5cff, 0xff00ff, 0x00ffff, 0xffffff,
];

/// Configured foreground, background, and ANSI colors (24-bit RGB).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Theme {
    pub foreground: u32,
    pub background: u32,
    pub palette: [u32; 16],
}

/// Shape selected by DECSCUSR.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorShape {
    Block,
    Underline,
    Bar,
}

/// Input and rendering modes negotiated by the child process.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Modes {
    pub app_cursor: bool,
    pub app_keypad: bool,
    pub cursor_visible: bool,
    /// None follows the frontend preference.
    pub cursor_blink: Option<bool>,
    pub cursor_shape: Option<CursorShape>,
    pub bracketed_paste: bool,
    pub focus_events: bool,
    /// Zero, or DECSET 1000 (click), 1002 (drag), or 1003 (all motion).
    pub mouse_tracking: u16,
    pub sgr_mouse: bool,
    pub synchronized_output: bool,
    pub kitty_keyboard: u8,
    pub alternate_screen: bool,
}

/// Streaming terminal parser and screen state. No operating system calls.
pub struct Terminal {
    parser: Parser<4096>,
    state: State,
    // Large image bodies bypass fixed string buffers. zvte parses VT text and
    // DCS headers; framing distinguishes ST from cancellation for image data.
    framing: u8,
    apc: zgraphics::CommandStream,
    apc_route: u8,
    ui_frame: zgpui::Frame,
    ui_host_enabled: bool,
    text_revision: u64,
    image_cache: core::cell::RefCell<Option<Box<image_cells::Cache>>>,
    osc_probe: u8,
    clipboard_frame: Option<Box<crate::clipboard::Frame>>,
}

fn dimensions(cols: usize, rows: usize) -> (usize, usize) {
    (cols.clamp(1, 1024), rows.clamp(1, 1024))
}

impl Terminal {
    pub fn new(cols: usize, rows: usize, history_limit: usize) -> Self {
        Self::with_theme(cols, rows, history_limit, Theme::default())
    }

    pub fn with_theme(cols: usize, rows: usize, history_limit: usize, theme: Theme) -> Self {
        let (cols, rows) = dimensions(cols, rows);
        Self {
            parser: Parser::new_with_size(),
            state: State::new(cols, rows, history_limit, theme),
            framing: 0,
            apc: zgraphics::CommandStream::new(),
            apc_route: 0,
            ui_frame: zgpui::Frame::new(),
            ui_host_enabled: false,
            text_revision: 0,
            image_cache: core::cell::RefCell::new(None),
            osc_probe: 0,
            clipboard_frame: None,
        }
    }
}

struct Link {
    id: String,
    uri: String,
}

struct State {
    theme: Theme,
    screen: Screen,
    primary: Option<Screen>,
    inactive_alternate: Option<Screen>,
    history: VecDeque<HistoryRow>,
    history_limit: usize,
    history_bytes: usize,
    line_base: u64,
    shell: crate::shell::ShellIntegration,
    style: Style,
    modes: Modes,
    sync_commit_pending: bool,
    autowrap: bool,
    insert: bool,
    newline: bool,
    tabs: Vec<bool>,
    g0: bool,
    g1: bool,
    use_g1: bool,
    title: String,
    cwd: String,
    links: Vec<Link>,
    responses: Vec<u8>,
    clipboard: crate::clipboard::Queue,
    osc_overflow: bool,
    dcs_query: bool,
    dcs: Vec<u8>,
    dcs_overflow: bool,
    sixel_active: bool,
    sixel_complete: bool,
    sixel_display: bool,
    sixel_right: bool,
    sixel_private: bool,
    sixel_palette: Option<Box<zgraphics::SixelPalette>>,
    last_char: char,
    graphemes: crate::grapheme::Graphemes,
    last_grapheme: Option<(usize, usize)>,
    grapheme_overflow: bool,
    grapheme_writes: usize,
    grapheme_collect_after: usize,
    graphics: zgraphics::Graphics,
    inactive_graphics: zgraphics::Graphics,
    ui: zgpui::State,
    cell_width: u32,
    cell_height: u32,
    checkpoint_extensions_seen: bool,
}

impl State {
    fn new(cols: usize, rows: usize, history_limit: usize, theme: Theme) -> Self {
        let tabs = (0..cols).map(|x| x % 8 == 0).collect();
        Self {
            theme,
            screen: Screen::new(cols, rows, theme.style()),
            primary: None,
            inactive_alternate: None,
            history: VecDeque::new(),
            history_limit: history_limit.min(65536),
            history_bytes: 0,
            line_base: 0,
            shell: crate::shell::ShellIntegration::default(),
            style: theme.style(),
            modes: Modes::default(),
            sync_commit_pending: false,
            autowrap: true,
            insert: false,
            newline: false,
            tabs,
            g0: false,
            g1: false,
            use_g1: false,
            title: "ztty".to_string(),
            cwd: String::new(),
            links: Vec::new(),
            responses: Vec::new(),
            clipboard: crate::clipboard::Queue::default(),
            osc_overflow: false,
            dcs_query: false,
            dcs: Vec::new(),
            dcs_overflow: false,
            sixel_active: false,
            sixel_complete: false,
            sixel_display: false,
            sixel_right: false,
            sixel_private: true,
            sixel_palette: None,
            last_char: ' ',
            graphemes: crate::grapheme::Graphemes::default(),
            last_grapheme: None,
            grapheme_overflow: false,
            grapheme_writes: 0,
            grapheme_collect_after: 0,
            graphics: zgraphics::Graphics::default(),
            inactive_graphics: zgraphics::Graphics::default(),
            ui: zgpui::State::new(),
            cell_width: 8,
            cell_height: 16,
            checkpoint_extensions_seen: false,
        }
    }
}

fn raw_param(groups: &[&[u16]], index: usize) -> Option<u16> {
    groups.get(index).and_then(|group| group.first()).copied()
}

fn param(groups: &[&[u16]], index: usize, default: u16) -> u16 {
    match raw_param(groups, index) {
        Some(0) | None => default,
        Some(value) => value,
    }
}

#[cfg(test)]
mod history_budget_tests {
    use super::*;

    #[test]
    fn retained_history_counts_wrap_metadata_spare_slots_and_reused_cell_capacity() {
        let mut history = VecDeque::new();
        let mut used = 0;
        for columns in [1, 1024, 80, 1] {
            let row = alloc::vec![Cell::BLANK; columns];
            let iterations =
                (MAX_HISTORY_BYTES / mem::size_of_val(row.as_slice()) + 100).min(70_000);
            for index in 0..iterations {
                let wrapped = if index % 7 == 0 { 0 } else { columns };
                push_history(&mut history, 65_536, &mut used, &row, wrapped);
                assert!(
                    used + history.capacity() * mem::size_of::<HistoryRow>() <= MAX_HISTORY_BYTES
                );
                assert!(history.len() <= 65_536);
                assert_eq!(history.back().unwrap().wrapped, wrapped);
                assert_eq!(history.back().unwrap().cells.len(), columns);
            }
            assert_eq!(
                used,
                history
                    .iter()
                    .map(|row| row.cells.capacity() * mem::size_of::<Cell>())
                    .sum()
            );
        }
    }
}
