//! Theme application and SGR color/rendition state.
use super::*;

impl Terminal {
    /// Return the configured default colors and ANSI palette.
    pub fn theme(&self) -> Theme {
        self.state.theme
    }

    /// Apply a theme to retained text without changing terminal content or modes.
    ///
    /// Default colors and ANSI palette references follow the new theme on the
    /// live screen, in scrollback, and in both active and cached alternate-screen
    /// state. Explicit RGB colors and indexed colors 16..=255 keep their values.
    /// Current and saved SGR styles are updated by the same source information;
    /// matching RGB values alone never identify a default or a palette reference.
    ///
    /// An actual change marks every screen row dirty. This scans retained cells
    /// in place and does not allocate. Reapplying the same theme does nothing.
    pub fn set_theme(&mut self, theme: Theme) {
        if self.state.theme == theme {
            return;
        }
        self.state.theme = theme;
        theme.recolor_style(&mut self.state.style);
        theme.recolor_screen(&mut self.state.screen);
        if let Some(primary) = self.state.primary.as_mut() {
            theme.recolor_screen(primary);
        }
        if let Some(alternate) = self.state.inactive_alternate.as_mut() {
            theme.recolor_screen(alternate);
        }
        for row in &mut self.state.history {
            for cell in &mut row.cells {
                theme.recolor_cell(cell);
            }
        }
    }
}

impl State {
    pub(super) fn sgr(&mut self, params: &Params) {
        // SGR is frequent in colored output. Consume parameter groups directly
        // instead of materializing a 32-entry slice table for every sequence.
        let mut groups = params.iter().peekable();
        while let Some(group) = groups.next() {
            let value = group.first().copied().unwrap_or(0);
            match value {
                0 => {
                    let link_id = self.style.link_id;
                    self.style = Style {
                        link_id,
                        ..self.theme.style()
                    };
                }
                1 => self.style.flags |= BOLD,
                2 => self.style.flags |= DIM,
                3 => self.style.flags |= ITALIC,
                4 => {
                    if group.get(1) == Some(&0) {
                        self.style.flags &= !UNDERLINE;
                    } else {
                        self.style.flags |= UNDERLINE;
                    }
                }
                5 => self.style.flags = (self.style.flags & !RAPID_BLINK) | BLINK,
                6 => self.style.flags = (self.style.flags & !BLINK) | RAPID_BLINK,
                21 => self.style.flags |= UNDERLINE,
                7 => self.style.flags |= INVERSE,
                8 => self.style.flags |= HIDDEN,
                9 => self.style.flags |= STRIKE,
                22 => self.style.flags &= !(BOLD | DIM),
                23 => self.style.flags &= !ITALIC,
                24 => self.style.flags &= !UNDERLINE,
                25 => self.style.flags &= !(BLINK | RAPID_BLINK),
                27 => self.style.flags &= !INVERSE,
                28 => self.style.flags &= !HIDDEN,
                29 => self.style.flags &= !STRIKE,
                30..=37 => {
                    self.style
                        .set_color(true, self.theme.color(value - 30), Some(value - 30))
                }
                40..=47 => {
                    self.style
                        .set_color(false, self.theme.color(value - 40), Some(value - 40));
                }
                90..=97 => self.style.set_color(
                    true,
                    self.theme.color(value - 90 + 8),
                    Some(value - 90 + 8),
                ),
                100..=107 => {
                    self.style.set_color(
                        false,
                        self.theme.color(value - 100 + 8),
                        Some(value - 100 + 8),
                    );
                }
                39 => {
                    self.style.fg = self.theme.foreground;
                    self.style.graphics_fg = 0;
                    self.style.flags &= !(EXPLICIT_FG | PALETTE_FG);
                    self.style.palette_indices &= 0xf0;
                }
                49 => {
                    self.style.bg = self.theme.background;
                    self.style.flags &= !(EXPLICIT_BG | PALETTE_BG);
                    self.style.palette_indices &= 0x0f;
                }
                59 => self.style.underline_color = 0,
                38 | 48 | 58 => {
                    let mut palette = None;
                    let color = if group.len() > 1 {
                        match group.get(1) {
                            Some(5) => group.get(2).filter(|v| **v <= 255).map(|v| {
                                palette = Some(*v);
                                self.theme.color(*v)
                            }),
                            Some(2) => {
                                let base = if group.len() >= 6 { 3 } else { 2 };
                                rgb(
                                    group.get(base).copied(),
                                    group.get(base + 1).copied(),
                                    group.get(base + 2).copied(),
                                )
                            }
                            _ => None,
                        }
                    } else {
                        match groups.peek().and_then(|group| group.first()).copied() {
                            Some(5) => {
                                groups.next();
                                groups
                                    .next()
                                    .and_then(|group| group.first())
                                    .copied()
                                    .filter(|v| *v <= 255)
                                    .map(|v| {
                                        palette = Some(v);
                                        self.theme.color(v)
                                    })
                            }
                            Some(2) => {
                                groups.next();
                                rgb(
                                    groups.next().and_then(|group| group.first()).copied(),
                                    groups.next().and_then(|group| group.first()).copied(),
                                    groups.next().and_then(|group| group.first()).copied(),
                                )
                            }
                            _ => None,
                        }
                    };
                    if let Some(color) = color {
                        if value == 58 {
                            self.style.underline_color = palette.map_or(color, u32::from);
                        } else {
                            self.style.set_color(value == 38, color, palette);
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            foreground: DEFAULT_FG,
            background: DEFAULT_BG,
            palette: ANSI_PALETTE,
        }
    }
}

impl Theme {
    pub(super) fn style(self) -> Style {
        Style {
            fg: self.foreground,
            bg: self.background,
            ..Style::default()
        }
    }

    pub(super) fn color(self, index: u16) -> u32 {
        self.palette
            .get(index as usize)
            .copied()
            .unwrap_or_else(|| extended_color(index))
    }

    pub(super) fn recolor(&self, fg: &mut u32, bg: &mut u32, flags: u16, indices: u8) {
        if flags & EXPLICIT_FG == 0 {
            *fg = self.foreground;
        } else if flags & PALETTE_FG != 0 {
            *fg = self.palette[usize::from(indices & 15)];
        }
        if flags & EXPLICIT_BG == 0 {
            *bg = self.background;
        } else if flags & PALETTE_BG != 0 {
            *bg = self.palette[usize::from(indices >> 4)];
        }
    }

    pub(super) fn recolor_cell(&self, cell: &mut Cell) {
        self.recolor(&mut cell.fg, &mut cell.bg, cell.flags, cell.palette_indices);
    }

    pub(super) fn recolor_style(&self, style: &mut Style) {
        self.recolor(
            &mut style.fg,
            &mut style.bg,
            style.flags,
            style.palette_indices,
        );
    }

    pub(super) fn recolor_screen(&self, screen: &mut Screen) {
        for cell in &mut screen.cells {
            self.recolor_cell(cell);
        }
        self.recolor_cell(&mut screen.blank);
        self.recolor_style(&mut screen.saved.style);
        screen.mark_all();
    }
}

fn rgb(r: Option<u16>, g: Option<u16>, b: Option<u16>) -> Option<u32> {
    match (r, g, b) {
        (Some(r), Some(g), Some(b)) if r <= 255 && g <= 255 && b <= 255 => {
            Some((r as u32) << 16 | (g as u32) << 8 | b as u32)
        }
        _ => None,
    }
}

fn extended_color(index: u16) -> u32 {
    if index >= 232 {
        let level = 8 + (index.min(255) - 232) as u32 * 10;
        return level * 0x010101;
    }
    let n = index - 16;
    let component = |v: u16| if v == 0 { 0 } else { 55 + v as u32 * 40 };
    component(n / 36) << 16 | component(n / 6 % 6) << 8 | component(n % 6)
}
