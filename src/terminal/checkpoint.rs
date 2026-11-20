//! Explicit, versioned terminal state for an owned process replacement.
//!
//! This is an internal wire format, never a Rust memory image. Unknown versions
//! and unsupported live resources fail before the original terminal is changed.
use super::*;
use alloc::sync::Arc;

const MAGIC: &[u8; 8] = b"ZTERMCP\0";
const VERSION: u16 = 1;
const HEADER: usize = 24;
const LIMIT: usize = 128 * 1024 * 1024;
const CELL_BYTES: usize = 32;
const MAX_CELLS: usize = 1024 * 1024;
type Result<T> = core::result::Result<T, &'static str>;

fn digest(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
    })
}

struct Writer(Vec<u8>);

impl Writer {
    fn new() -> Self {
        let mut bytes = Vec::with_capacity(4096);
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&VERSION.to_le_bytes());
        bytes.extend_from_slice(&[0; HEADER - 10]);
        Self(bytes)
    }

    fn room(&self, count: usize) -> Result<()> {
        if self
            .0
            .len()
            .checked_add(count)
            .is_none_or(|len| len > LIMIT)
        {
            Err("terminal checkpoint exceeds 128 MiB")
        } else {
            Ok(())
        }
    }

    fn byte(&mut self, value: u8) {
        self.0.push(value);
    }

    fn boolean(&mut self, value: bool) {
        self.byte(u8::from(value));
    }

    fn word(&mut self, value: u16) {
        self.0.extend_from_slice(&value.to_le_bytes());
    }

    fn dword(&mut self, value: u32) {
        self.0.extend_from_slice(&value.to_le_bytes());
    }

    fn qword(&mut self, value: u64) {
        self.0.extend_from_slice(&value.to_le_bytes());
    }

    fn size(&mut self, value: usize) -> Result<()> {
        self.dword(u32::try_from(value).map_err(|_| "checkpoint length cannot be encoded")?);
        Ok(())
    }

    fn blob(&mut self, bytes: &[u8]) -> Result<()> {
        self.room(
            bytes
                .len()
                .checked_add(4)
                .ok_or("checkpoint length overflow")?,
        )?;
        self.size(bytes.len())?;
        self.0.extend_from_slice(bytes);
        Ok(())
    }

    fn text(&mut self, text: &str) -> Result<()> {
        self.blob(text.as_bytes())
    }

    fn finish(mut self) -> Result<Vec<u8>> {
        self.room(0)?;
        let length = (self.0.len() - HEADER) as u32;
        self.0[12..16].copy_from_slice(&length.to_le_bytes());
        let checksum = digest(&self.0[HEADER..]);
        self.0[16..24].copy_from_slice(&checksum.to_le_bytes());
        Ok(self.0)
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Result<Self> {
        if bytes.len() < HEADER || bytes.len() > LIMIT || bytes.get(..8) != Some(MAGIC.as_slice()) {
            return Err("invalid terminal checkpoint header or length");
        }
        let mut header = Self { bytes, offset: 8 };
        if header.word()? != VERSION || header.word()? != 0 {
            return Err("unsupported terminal checkpoint version");
        }
        if header.dword()? as usize != bytes.len() - HEADER {
            return Err("terminal checkpoint length does not match its header");
        }
        if header.qword()? != digest(&bytes[HEADER..]) {
            return Err("terminal checkpoint checksum mismatch");
        }
        Ok(header)
    }

    fn need(&self, count: usize) -> Result<()> {
        if self
            .offset
            .checked_add(count)
            .is_none_or(|end| end > self.bytes.len())
        {
            Err("truncated terminal checkpoint")
        } else {
            Ok(())
        }
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8]> {
        self.need(count)?;
        let value = &self.bytes[self.offset..self.offset + count];
        self.offset += count;
        Ok(value)
    }

    fn byte(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn boolean(&mut self) -> Result<bool> {
        match self.byte()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err("invalid checkpoint boolean"),
        }
    }

    fn word(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }

    fn dword(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn qword(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }

    fn count(&mut self, limit: usize) -> Result<usize> {
        let value = self.dword()? as usize;
        if value > limit {
            Err("checkpoint collection exceeds its limit")
        } else {
            Ok(value)
        }
    }

    fn counter(&mut self) -> Result<usize> {
        usize::try_from(self.qword()?).map_err(|_| "checkpoint counter exceeds target width")
    }

    fn scalar(&mut self) -> Result<char> {
        char::from_u32(self.dword()?).ok_or("invalid checkpoint Unicode scalar")
    }

    fn blob(&mut self, limit: usize) -> Result<&'a [u8]> {
        let length = self.count(limit)?;
        self.take(length)
    }

    fn text(&mut self, limit: usize) -> Result<&'a str> {
        core::str::from_utf8(self.blob(limit)?).map_err(|_| "invalid checkpoint UTF-8")
    }

    fn done(&self) -> Result<()> {
        if self.offset == self.bytes.len() {
            Ok(())
        } else {
            Err("trailing terminal checkpoint data")
        }
    }
}

fn write_cell(w: &mut Writer, cell: &Cell) {
    w.dword(cell.ch as u32);
    w.dword(cell.combining[0] as u32);
    w.dword(cell.combining[1] as u32);
    w.dword(cell.fg);
    w.dword(cell.bg);
    w.word(cell.flags);
    w.byte(cell.width);
    w.byte(cell.palette_indices);
    w.dword(cell.link_id);
    w.dword(cell.grapheme_id);
}

fn read_cell(r: &mut Reader<'_>) -> Result<Cell> {
    let cell = Cell {
        ch: r.scalar()?,
        combining: [r.scalar()?, r.scalar()?],
        fg: r.dword()?,
        bg: r.dword()?,
        flags: r.word()?,
        width: r.byte()?,
        palette_indices: r.byte()?,
        link_id: r.dword()?,
        grapheme_id: r.dword()?,
    };
    if cell.width > 2 || cell.flags & !0x1fff != 0 {
        return Err("invalid checkpoint cell flags or width");
    }
    Ok(cell)
}

fn write_style(w: &mut Writer, style: &Style) {
    w.dword(style.fg);
    w.dword(style.bg);
    w.word(style.flags);
    w.byte(style.palette_indices);
    w.dword(style.link_id);
    w.dword(style.graphics_fg);
    w.dword(style.underline_color);
}

fn read_style(r: &mut Reader<'_>) -> Result<Style> {
    let style = Style {
        fg: r.dword()?,
        bg: r.dword()?,
        flags: r.word()?,
        palette_indices: r.byte()?,
        link_id: r.dword()?,
        graphics_fg: r.dword()?,
        underline_color: r.dword()?,
    };
    if style.flags & !0x1fff != 0 {
        return Err("invalid checkpoint style flags");
    }
    Ok(style)
}

fn write_screen(w: &mut Writer, screen: &Screen) -> Result<()> {
    w.size(screen.cols)?;
    w.size(screen.rows)?;
    w.size(screen.row0)?;
    write_cell(w, &screen.blank);
    w.size(screen.x)?;
    w.size(screen.y)?;
    w.boolean(screen.wrap);
    w.size(screen.top)?;
    w.size(screen.bottom)?;
    w.boolean(screen.origin);
    let saved = &screen.saved;
    w.size(saved.x)?;
    w.size(saved.y)?;
    write_style(w, &saved.style);
    for value in [saved.origin, saved.g0, saved.g1, saved.use_g1, saved.wrap] {
        w.boolean(value);
    }
    w.byte(screen.keyboard);
    w.size(screen.keyboard_depth)?;
    w.0.extend_from_slice(&screen.keyboard_stack);
    for (&wrapped, &dirty) in screen.wrapped.iter().zip(&screen.dirty) {
        w.size(wrapped)?;
        w.boolean(dirty);
    }
    w.room(
        screen
            .cells
            .len()
            .checked_mul(CELL_BYTES)
            .ok_or("checkpoint screen overflow")?,
    )?;
    for cell in &screen.cells {
        write_cell(w, cell);
    }
    Ok(())
}

fn read_screen(r: &mut Reader<'_>) -> Result<Screen> {
    let cols = r.count(1024)?;
    let rows = r.count(1024)?;
    if cols == 0 || rows == 0 {
        return Err("checkpoint screen dimensions must be positive");
    }
    let row0 = r.count(rows - 1)?;
    let blank = read_cell(r)?;
    if blank.width != 1 || blank.grapheme_id != 0 || blank.link_id != 0 {
        return Err("invalid checkpoint blank cell");
    }
    let x = r.count(cols - 1)?;
    let y = r.count(rows - 1)?;
    let wrap = r.boolean()?;
    let top = r.count(rows - 1)?;
    let bottom = r.count(rows - 1)?;
    let origin = r.boolean()?;
    let saved = SavedCursor {
        x: r.count(cols - 1)?,
        y: r.count(rows - 1)?,
        style: read_style(r)?,
        origin: r.boolean()?,
        g0: r.boolean()?,
        g1: r.boolean()?,
        use_g1: r.boolean()?,
        wrap: r.boolean()?,
    };
    // Grid resizing keeps the saved deferred-wrap flag independently of the
    // old column. Restoring that cursor can also make the active wrap flag
    // precede the new right margin. Preserve both flags exactly as the live
    // terminal does; only positions and scroll-margin bounds are invariants.
    if top > bottom {
        return Err("invalid checkpoint scroll margins");
    }
    let keyboard = r.byte()?;
    let keyboard_depth = r.count(32)?;
    let keyboard_stack: [u8; 32] = r.take(32)?.try_into().unwrap();
    if keyboard > 31 || keyboard_stack.iter().any(|value| *value > 31) {
        return Err("invalid checkpoint keyboard flags");
    }
    let mut wrapped = Vec::with_capacity(rows);
    let mut dirty = Vec::with_capacity(rows);
    r.need(rows * 5)?;
    for _ in 0..rows {
        wrapped.push(r.count(cols)?);
        dirty.push(r.boolean()?);
    }
    let count = cols
        .checked_mul(rows)
        .filter(|count| *count <= MAX_CELLS)
        .ok_or("checkpoint screen cell count overflow")?;
    r.need(count * CELL_BYTES)?;
    let mut cells = Vec::with_capacity(count);
    for _ in 0..count {
        cells.push(read_cell(r)?);
    }
    for row in cells.chunks_exact(cols) {
        validate_row(row)?;
    }
    Ok(Screen {
        cols,
        rows,
        cells,
        blank,
        row0,
        wrapped,
        x,
        y,
        wrap,
        dirty,
        top,
        bottom,
        origin,
        saved,
        keyboard,
        keyboard_stack,
        keyboard_depth,
    })
}

fn validate_row(row: &[Cell]) -> Result<()> {
    for (x, cell) in row.iter().enumerate() {
        if cell.width == 0 && (x == 0 || row[x - 1].width != 2)
            || cell.width == 2 && row.get(x + 1).is_none_or(|next| next.width != 0)
        {
            return Err("checkpoint contains an unpaired wide cell");
        }
    }
    Ok(())
}

fn write_optional_screen(w: &mut Writer, screen: Option<&Screen>) -> Result<()> {
    w.boolean(screen.is_some());
    if let Some(screen) = screen {
        write_screen(w, screen)?;
    }
    Ok(())
}

fn read_optional_screen(r: &mut Reader<'_>) -> Result<Option<Screen>> {
    if r.boolean()? {
        Ok(Some(read_screen(r)?))
    } else {
        Ok(None)
    }
}

fn write_modes(w: &mut Writer, modes: &Modes) {
    for value in [modes.app_cursor, modes.app_keypad, modes.cursor_visible] {
        w.boolean(value);
    }
    w.byte(match modes.cursor_blink {
        None => 0,
        Some(false) => 1,
        Some(true) => 2,
    });
    w.byte(match modes.cursor_shape {
        None => 0,
        Some(CursorShape::Block) => 1,
        Some(CursorShape::Underline) => 2,
        Some(CursorShape::Bar) => 3,
    });
    w.boolean(modes.bracketed_paste);
    w.boolean(modes.focus_events);
    w.word(modes.mouse_tracking);
    w.boolean(modes.sgr_mouse);
    w.boolean(modes.synchronized_output);
    w.byte(modes.kitty_keyboard);
    w.boolean(modes.alternate_screen);
}

fn read_modes(r: &mut Reader<'_>) -> Result<Modes> {
    let app_cursor = r.boolean()?;
    let app_keypad = r.boolean()?;
    let cursor_visible = r.boolean()?;
    let cursor_blink = match r.byte()? {
        0 => None,
        1 => Some(false),
        2 => Some(true),
        _ => return Err("invalid checkpoint cursor blink"),
    };
    let cursor_shape = match r.byte()? {
        0 => None,
        1 => Some(CursorShape::Block),
        2 => Some(CursorShape::Underline),
        3 => Some(CursorShape::Bar),
        _ => return Err("invalid checkpoint cursor shape"),
    };
    let modes = Modes {
        app_cursor,
        app_keypad,
        cursor_visible,
        cursor_blink,
        cursor_shape,
        bracketed_paste: r.boolean()?,
        focus_events: r.boolean()?,
        mouse_tracking: r.word()?,
        sgr_mouse: r.boolean()?,
        synchronized_output: r.boolean()?,
        kitty_keyboard: r.byte()?,
        alternate_screen: r.boolean()?,
    };
    if !matches!(modes.mouse_tracking, 0 | 9 | 1000 | 1002 | 1003) || modes.kitty_keyboard > 31 {
        return Err("invalid checkpoint terminal mode");
    }
    Ok(modes)
}

fn write_position(w: &mut Writer, position: crate::TextPosition) -> Result<()> {
    w.qword(position.line);
    w.size(position.column)
}

fn read_position(r: &mut Reader<'_>) -> Result<crate::TextPosition> {
    Ok(crate::TextPosition {
        line: r.qword()?,
        column: r.count(1024)?,
    })
}

fn write_optional_position(w: &mut Writer, position: Option<crate::TextPosition>) -> Result<()> {
    w.boolean(position.is_some());
    if let Some(position) = position {
        write_position(w, position)?;
    }
    Ok(())
}

fn read_optional_position(r: &mut Reader<'_>) -> Result<Option<crate::TextPosition>> {
    if r.boolean()? {
        Ok(Some(read_position(r)?))
    } else {
        Ok(None)
    }
}

fn write_shell(w: &mut Writer, shell: &crate::shell::ShellIntegration) -> Result<()> {
    w.qword(shell.next_id);
    w.byte(match shell.phase {
        crate::ShellPhase::Unknown => 0,
        crate::ShellPhase::Prompt => 1,
        crate::ShellPhase::Input => 2,
        crate::ShellPhase::Output => 3,
    });
    w.size(shell.records.len())?;
    for record in &shell.records {
        w.qword(record.id);
        write_position(w, record.prompt)?;
        write_optional_position(w, record.input)?;
        write_optional_position(w, record.output)?;
        write_optional_position(w, record.end)?;
        w.boolean(record.exit_status.is_some());
        if let Some(status) = record.exit_status {
            w.dword(status);
        }
        w.boolean(record.available);
    }
    Ok(())
}

fn read_shell(r: &mut Reader<'_>) -> Result<crate::shell::ShellIntegration> {
    let next_id = r.qword()?;
    let phase = match r.byte()? {
        0 => crate::ShellPhase::Unknown,
        1 => crate::ShellPhase::Prompt,
        2 => crate::ShellPhase::Input,
        3 => crate::ShellPhase::Output,
        _ => return Err("invalid checkpoint shell phase"),
    };
    let count = r.count(crate::MAX_COMMANDS)?;
    let mut records = VecDeque::with_capacity(count);
    let mut previous = 0;
    for _ in 0..count {
        let id = r.qword()?;
        if id == 0 || id < previous || id > next_id {
            return Err("invalid checkpoint command identity");
        }
        previous = id;
        records.push_back(crate::CommandRecord {
            id,
            prompt: read_position(r)?,
            input: read_optional_position(r)?,
            output: read_optional_position(r)?,
            end: read_optional_position(r)?,
            exit_status: if r.boolean()? { Some(r.dword()?) } else { None },
            available: r.boolean()?,
        });
    }
    if records.is_empty() && phase != crate::ShellPhase::Unknown {
        return Err("checkpoint shell phase has no command");
    }
    Ok(crate::shell::ShellIntegration {
        records,
        phase,
        next_id,
    })
}

fn write_graphemes(w: &mut Writer, pool: &crate::grapheme::Graphemes) -> Result<()> {
    let (slots, free) = pool.checkpoint_parts();
    w.size(slots.len())?;
    for slot in slots {
        w.boolean(slot.is_some());
        if let Some(text) = slot {
            w.text(text)?;
        }
    }
    w.size(free.len())?;
    for &index in free {
        w.dword(index);
    }
    Ok(())
}

fn read_graphemes(r: &mut Reader<'_>) -> Result<crate::grapheme::Graphemes> {
    use unicode_segmentation::UnicodeSegmentation;
    let count = r.count(16_384)?;
    r.need(count)?;
    let mut slots = Vec::with_capacity(count);
    let mut text_bytes = 0usize;
    for _ in 0..count {
        slots.push(if r.boolean()? {
            let text = r.text(crate::MAX_GRAPHEME_BYTES)?;
            text_bytes += text.len();
            if text_bytes > 1024 * 1024 || text.graphemes(true).count() != 1 {
                return Err("invalid checkpoint grapheme pool");
            }
            Some(Arc::<str>::from(text))
        } else {
            None
        });
    }
    let count = r.count(16_384)?;
    r.need(count * 4)?;
    let mut free = Vec::with_capacity(count);
    for _ in 0..count {
        free.push(r.dword()?);
    }
    crate::grapheme::Graphemes::restore_checkpoint_parts(slots, free)
        .ok_or("invalid checkpoint grapheme identities or budget")
}

fn check_cell_references(
    cell: &Cell,
    pool: &crate::grapheme::Graphemes,
    links: usize,
) -> Result<()> {
    if cell.grapheme_id != 0 && pool.get(cell.grapheme_id).is_none()
        || cell.ch != '\u{10eeee}' && cell.link_id as usize > links
    {
        return Err("checkpoint cell refers to an absent grapheme or hyperlink");
    }
    if cell.width == 0 && (cell.ch != '\0' || cell.combining != ['\0'; 2] || cell.grapheme_id != 0)
    {
        return Err("checkpoint wide trailer contains text");
    }
    if let Some(text) = pool.get(cell.grapheme_id) {
        let mut chars = text.chars();
        if chars.next() != Some(cell.ch)
            || cell.combining != [chars.next().unwrap_or('\0'), chars.next().unwrap_or('\0')]
        {
            return Err("checkpoint grapheme does not match its cell");
        }
    }
    Ok(())
}

impl Terminal {
    /// Encode retained terminal state for an owned process replacement.
    ///
    /// The versioned, bounded format contains explicit fields and a corruption
    /// checksum, not native memory. This does not mutate the terminal. A partial
    /// parser sequence must finish before retrying; image, native UI or pending
    /// clipboard resources currently return an error instead of losing state.
    /// Completed stateless capability probes are supported; queued responses
    /// are retained. Stateful extension history remains unsupported after clear.
    pub fn checkpoint(&self) -> Result<Vec<u8>> {
        let s = &self.state;
        if !self.parser.is_ground() || self.framing != 0 {
            return Err("terminal parser has pending input; retry after a complete sequence");
        }
        if s.graphics.image_count() != 0
            || s.graphics.placement_count() != 0
            || s.graphics.pending_bytes() != 0
            || s.inactive_graphics.image_count() != 0
            || s.inactive_graphics.placement_count() != 0
            || s.inactive_graphics.pending_bytes() != 0
            || s.sixel_palette.is_some()
            || s.sixel_active
            || s.sixel_complete
        {
            return Err("terminal graphics resources do not support checkpoint restoration yet");
        }
        if s.ui.scene().is_some() {
            return Err("terminal native UI resources do not support checkpoint restoration yet");
        }
        if !s.graphics.is_query_only() || !s.inactive_graphics.is_query_only() {
            return Err(
                "terminal has pending graphics work or non-query graphics protocol history; checkpoint restoration is unsupported",
            );
        }
        // Successful scene operations retain revision/event identities even
        // after clear, RIS or a screen switch. Capability probes and rejected
        // frames do not change these counters; their replies are stored below.
        if s.ui.last_revision() != 0 || s.ui.revision() != 0 {
            return Err(
                "terminal native UI protocol identity cannot be restored yet, even after its scene is cleared",
            );
        }
        if !s.clipboard.is_empty() || self.clipboard_frame.is_some() {
            return Err("terminal has pending OSC 52 clipboard work; retry after it completes");
        }
        if s.dcs_query {
            return Err("terminal DCS query is incomplete; retry after a complete sequence");
        }
        let mut w = Writer::new();
        w.boolean(self.ui_host_enabled);
        w.qword(self.text_revision);
        w.dword(s.theme.foreground);
        w.dword(s.theme.background);
        for color in s.theme.palette {
            w.dword(color);
        }
        write_screen(&mut w, &s.screen)?;
        write_optional_screen(&mut w, s.primary.as_ref())?;
        write_optional_screen(&mut w, s.inactive_alternate.as_ref())?;
        w.size(s.history_limit)?;
        w.size(s.history.len())?;
        for row in &s.history {
            w.size(row.wrapped)?;
            w.size(row.cells.len())?;
            w.room(row.cells.len() * CELL_BYTES)?;
            for cell in &row.cells {
                write_cell(&mut w, cell);
            }
        }
        w.qword(s.line_base);
        write_shell(&mut w, &s.shell)?;
        write_style(&mut w, &s.style);
        write_modes(&mut w, &s.modes);
        for value in [
            s.sync_commit_pending,
            s.autowrap,
            s.insert,
            s.newline,
            s.g0,
            s.g1,
            s.use_g1,
        ] {
            w.boolean(value);
        }
        w.size(s.tabs.len())?;
        for &tab in &s.tabs {
            w.boolean(tab);
        }
        w.text(&s.title)?;
        w.text(&s.cwd)?;
        w.size(s.links.len())?;
        for link in &s.links {
            w.text(&link.id)?;
            w.text(&link.uri)?;
        }
        w.blob(&s.responses)?;
        w.qword(s.clipboard.epoch);
        w.boolean(s.osc_overflow);
        w.blob(&s.dcs)?;
        for value in [
            s.dcs_overflow,
            s.sixel_display,
            s.sixel_right,
            s.sixel_private,
        ] {
            w.boolean(value);
        }
        w.dword(s.last_char as u32);
        write_graphemes(&mut w, &s.graphemes)?;
        w.boolean(s.last_grapheme.is_some());
        if let Some((x, y)) = s.last_grapheme {
            w.size(x)?;
            w.size(y)?;
        }
        w.boolean(s.grapheme_overflow);
        w.qword(s.grapheme_writes as u64);
        w.qword(s.grapheme_collect_after as u64);
        w.dword(s.cell_width);
        w.dword(s.cell_height);
        w.finish()
    }

    /// Decode a complete checkpoint without touching a live terminal or PTY.
    ///
    /// Unknown versions, malformed fields, missing references, invalid UTF-8,
    /// excessive lengths and unsupported states return an error. The host must
    /// validate all session checkpoints before replacing its current process.
    /// Process-local callbacks such as `set_graphics_reader` must be installed
    /// again by the new host; no function pointers are stored in this format.
    pub fn restore_checkpoint(bytes: &[u8]) -> Result<Self> {
        let mut r = Reader::new(bytes)?;
        let ui_host_enabled = r.boolean()?;
        let text_revision = r.qword()?;
        let mut theme = Theme {
            foreground: r.dword()?,
            background: r.dword()?,
            palette: [0; 16],
        };
        for color in &mut theme.palette {
            *color = r.dword()?;
        }
        if theme.foreground > 0xffffff
            || theme.background > 0xffffff
            || theme.palette.iter().any(|color| *color > 0xffffff)
        {
            return Err("invalid checkpoint theme color");
        }
        let screen = read_screen(&mut r)?;
        let primary = read_optional_screen(&mut r)?;
        let inactive_alternate = read_optional_screen(&mut r)?;
        for other in primary.iter().chain(&inactive_alternate) {
            if (other.cols, other.rows) != (screen.cols, screen.rows) {
                return Err("checkpoint screens have inconsistent dimensions");
            }
        }
        let history_limit = r.count(65_536)?;
        let count = r.count(history_limit)?;
        // Match the geometric growth used by push_history, so the first append
        // after restore does not over-allocate metadata and evict extra rows.
        let capacity = if count == 0 {
            0
        } else {
            count.next_power_of_two()
        };
        let mut history = VecDeque::with_capacity(capacity);
        let mut history_bytes = 0usize;
        for _ in 0..count {
            let wrapped = r.count(1024)?;
            let columns = r.count(1024)?;
            if columns == 0 || wrapped > columns {
                return Err("invalid checkpoint history row");
            }
            r.need(columns * CELL_BYTES)?;
            let mut cells = Vec::with_capacity(columns);
            history_bytes = history_bytes
                .checked_add(cells.capacity() * mem::size_of::<Cell>())
                .ok_or("checkpoint history size overflow")?;
            if history_bytes + history.capacity() * mem::size_of::<HistoryRow>() > MAX_HISTORY_BYTES
            {
                return Err("checkpoint history exceeds its memory budget");
            }
            for _ in 0..columns {
                cells.push(read_cell(&mut r)?);
            }
            validate_row(&cells)?;
            history.push_back(HistoryRow { cells, wrapped });
        }
        let line_base = r.qword()?;
        if line_base < history.len() as u64 {
            return Err("checkpoint history precedes its line origin");
        }
        let shell = read_shell(&mut r)?;
        let style = read_style(&mut r)?;
        let modes = read_modes(&mut r)?;
        if modes.alternate_screen != primary.is_some()
            || primary.is_some() && inactive_alternate.is_some()
            || modes.kitty_keyboard != screen.keyboard
        {
            return Err("inconsistent checkpoint alternate screen or keyboard state");
        }
        let sync_commit_pending = r.boolean()?;
        let autowrap = r.boolean()?;
        let insert = r.boolean()?;
        let newline = r.boolean()?;
        let g0 = r.boolean()?;
        let g1 = r.boolean()?;
        let use_g1 = r.boolean()?;
        let count = r.count(1024)?;
        if count != screen.cols {
            return Err("checkpoint tab stops do not match the screen");
        }
        let mut tabs = Vec::with_capacity(count);
        for _ in 0..count {
            tabs.push(r.boolean()?);
        }
        let title = r.text(256)?.to_string();
        let cwd = r.text(2048)?.to_string();
        let count = r.count(MAX_LINKS)?;
        let mut links = Vec::with_capacity(count);
        for _ in 0..count {
            let id = r.text(256)?.to_string();
            let uri = r.text(2048)?.to_string();
            if uri.is_empty() || uri.chars().any(char::is_control) {
                return Err("invalid checkpoint hyperlink");
            }
            links.push(Link { id, uri });
        }
        if style.link_id as usize > links.len() {
            return Err("checkpoint style has no hyperlink");
        }
        let responses = r.blob(MAX_RESPONSES)?.to_vec();
        let mut clipboard = crate::clipboard::Queue::default();
        clipboard.epoch = r.qword()?;
        let osc_overflow = r.boolean()?;
        let dcs = r.blob(512)?.to_vec();
        let dcs_overflow = r.boolean()?;
        let sixel_display = r.boolean()?;
        let sixel_right = r.boolean()?;
        let sixel_private = r.boolean()?;
        let last_char = r.scalar()?;
        let graphemes = read_graphemes(&mut r)?;
        let last_grapheme = if r.boolean()? {
            Some((r.count(screen.cols - 1)?, r.count(screen.rows - 1)?))
        } else {
            None
        };
        let grapheme_overflow = r.boolean()?;
        let grapheme_writes = r.counter()?;
        let grapheme_collect_after = r.counter()?;
        let cell_width = r.dword()?;
        let cell_height = r.dword()?;
        if !(1..=4096).contains(&cell_width) || !(1..=4096).contains(&cell_height) {
            return Err("invalid checkpoint cell dimensions");
        }
        r.done()?;
        for grid in core::iter::once(&screen)
            .chain(primary.iter())
            .chain(inactive_alternate.iter())
        {
            if grid.saved.style.link_id as usize > links.len() {
                return Err("checkpoint saved cursor has no hyperlink");
            }
            for cell in &grid.cells {
                check_cell_references(cell, &graphemes, links.len())?;
            }
        }
        for row in &history {
            for cell in &row.cells {
                check_cell_references(cell, &graphemes, links.len())?;
            }
        }
        let mut graphics = zgraphics::Graphics::default();
        let mut inactive_graphics = zgraphics::Graphics::default();
        graphics.set_viewport(cell_width, cell_height, screen.rows);
        inactive_graphics.set_viewport(cell_width, cell_height, screen.rows);
        Ok(Self {
            parser: Parser::new_with_size(),
            state: State {
                theme,
                screen,
                primary,
                inactive_alternate,
                history,
                history_limit,
                history_bytes,
                line_base,
                shell,
                style,
                modes,
                sync_commit_pending,
                autowrap,
                insert,
                newline,
                tabs,
                g0,
                g1,
                use_g1,
                title,
                cwd,
                links,
                responses,
                clipboard,
                osc_overflow,
                dcs_query: false,
                dcs,
                dcs_overflow,
                sixel_active: false,
                sixel_complete: false,
                sixel_display,
                sixel_right,
                sixel_private,
                sixel_palette: None,
                last_char,
                graphemes,
                last_grapheme,
                grapheme_overflow,
                grapheme_writes,
                grapheme_collect_after,
                graphics,
                inactive_graphics,
                ui: zgpui::State::new(),
                cell_width,
                cell_height,
            },
            framing: 0,
            apc: zgraphics::CommandStream::new(),
            apc_route: 0,
            ui_frame: zgpui::Frame::new(),
            ui_host_enabled,
            text_revision,
            image_cache: core::cell::RefCell::new(None),
            osc_probe: 0,
            clipboard_frame: None,
        })
    }
}

#[cfg(test)]
mod host_erase_tests;
#[cfg(test)]
mod tests;
