//! Streaming VT and extension-string framing, cancellation and host UI access.
use super::*;

impl Terminal {
    /// Consume arbitrary PTY chunks, preserving partial UTF-8 and sequences.
    pub fn feed(&mut self, bytes: &[u8]) {
        if !bytes.is_empty() {
            self.text_revision = self.text_revision.wrapping_add(1);
        }
        let mut offset = 0;
        while offset < bytes.len() {
            match self.framing {
                0 => {
                    // Keep ordinary VT chunks intact, including every CSI/SGR
                    // in them. Only image-capable string introducers split it.
                    // `contains` uses core's byte-search specialization, so
                    // the common no-underscore path scans words, not callbacks.
                    let remaining = &bytes[offset..];
                    let string = if remaining.contains(&b'_')
                        || remaining.contains(&b'P')
                        || remaining.contains(&b']')
                    {
                        remaining.windows(2).position(|pair| {
                            pair[0] == 0x1b && matches!(pair[1], b'_' | b'P' | b']')
                        })
                    } else {
                        None
                    };
                    if let Some(start) = string {
                        // First deliver ESC: zvte must cancel/dispatch an older
                        // OSC or DCS before the APC dispatcher takes ownership.
                        if remaining[start + 1] == b'_' {
                            self.parser
                                .advance(&remaining[..start + 1], &mut self.state);
                            self.begin_apc();
                        } else {
                            self.parser
                                .advance(&remaining[..start + 2], &mut self.state);
                            if remaining[start + 1] == b']' {
                                self.osc_probe = 0;
                                self.framing = 7;
                            } else {
                                self.framing = 4;
                            }
                        }
                        offset += start + 2;
                    } else {
                        self.parser.advance(remaining, &mut self.state);
                        self.framing = u8::from(remaining.last() == Some(&0x1b));
                        offset = bytes.len();
                    }
                }
                1 => {
                    let byte = bytes[offset];
                    offset += 1;
                    if byte == b'_' {
                        self.begin_apc();
                    } else if byte == b']' {
                        self.parser.advance(&[byte], &mut self.state);
                        self.osc_probe = 0;
                        self.framing = 7;
                    } else if byte == b'P' {
                        self.parser.advance(&[byte], &mut self.state);
                        self.framing = 4;
                    } else {
                        self.parser.advance(&[byte], &mut self.state);
                        self.framing = u8::from(byte == 0x1b);
                    }
                }
                2 => {
                    let end = bytes[offset..]
                        .iter()
                        .position(|b| matches!(*b, 0x1b | 0x18 | 0x1a))
                        .map(|i| offset + i)
                        .unwrap_or(bytes.len());
                    if end != offset {
                        self.push_apc(&bytes[offset..end]);
                        offset = end;
                    }
                    if offset == bytes.len() {
                        continue;
                    }
                    let byte = bytes[offset];
                    offset += 1;
                    match byte {
                        0x1b => self.framing = 3,
                        0x18 | 0x1a => {
                            self.abort_apc();
                            self.framing = 0;
                        }
                        _ => unreachable!(),
                    }
                }
                3 => {
                    let byte = bytes[offset];
                    if byte == b'\\' {
                        offset += 1;
                        self.finish_apc();
                        self.framing = 0;
                    } else {
                        // A new ESC sequence cancels an unterminated APC. The
                        // current byte is processed again as an ESC continuation.
                        self.abort_apc();
                        self.parser.advance(b"\x1b", &mut self.state);
                        self.framing = 1;
                    }
                }
                4 => {
                    // DCS headers are short, bounded by zvte's parameter array.
                    let byte = bytes[offset];
                    offset += 1;
                    self.parser.advance(&[byte], &mut self.state);
                    self.framing = match byte {
                        0x1b => 1,
                        0x18 | 0x1a | 0x9c => 0,
                        0x40..=0x7e => {
                            if self.state.sixel_active {
                                5
                            } else {
                                0
                            }
                        }
                        _ => 4,
                    };
                }
                5 => {
                    let end = bytes[offset..]
                        .iter()
                        .position(|b| matches!(*b, 0x1b | 0x18 | 0x1a | 0x9c))
                        .map(|i| offset + i)
                        .unwrap_or(bytes.len());
                    for &byte in &bytes[offset..end] {
                        self.state.graphics.sixel_put(byte);
                    }
                    offset = end;
                    if offset == bytes.len() {
                        continue;
                    }
                    let byte = bytes[offset];
                    offset += 1;
                    if byte == 0x1b {
                        self.framing = 6;
                    } else {
                        self.state.sixel_complete = byte == 0x9c;
                        self.parser.advance(&[byte], &mut self.state);
                        self.state.sixel_complete = false;
                        self.framing = 0;
                    }
                }
                6 => {
                    if bytes[offset] == b'\\' {
                        offset += 1;
                        self.state.sixel_complete = true;
                        self.parser.advance(b"\x1b\\", &mut self.state);
                        self.state.sixel_complete = false;
                        self.framing = 0;
                    } else {
                        self.parser.advance(b"\x1b", &mut self.state);
                        self.framing = 1;
                    }
                }
                7 => {
                    // Probe only OSC 52; all other OSC commands keep zvte's
                    // existing bounded parser and termination semantics.
                    let byte = bytes[offset];
                    offset += 1;
                    let prefix = b"52;";
                    if byte == prefix[usize::from(self.osc_probe)] {
                        self.osc_probe += 1;
                        if self.osc_probe == 3 {
                            self.parser.reset();
                            self.state.last_grapheme = None;
                            self.state.osc_overflow = false;
                            self.clipboard_frame = Some(Box::new(crate::clipboard::Frame::new()));
                            self.framing = 8;
                        }
                    } else {
                        self.parser
                            .advance(&prefix[..usize::from(self.osc_probe)], &mut self.state);
                        self.parser.advance(&[byte], &mut self.state);
                        self.framing = u8::from(byte == 0x1b);
                    }
                }
                8 => {
                    let end = bytes[offset..]
                        .iter()
                        .position(|b| matches!(*b, 7 | 0x1b | 0x18 | 0x1a | 0x9c))
                        .map_or(bytes.len(), |i| offset + i);
                    self.clipboard_frame
                        .as_mut()
                        .unwrap()
                        .push(&bytes[offset..end]);
                    offset = end;
                    if offset == bytes.len() {
                        continue;
                    }
                    let byte = bytes[offset];
                    offset += 1;
                    match byte {
                        0x1b => self.framing = 9,
                        7 | 0x9c => {
                            self.clipboard_frame
                                .take()
                                .unwrap()
                                .finish(&mut self.state.clipboard, byte == 7);
                            self.framing = 0;
                        }
                        0x18 | 0x1a => {
                            self.clipboard_frame = None;
                            self.framing = 0;
                        }
                        _ => unreachable!(),
                    }
                }
                9 => {
                    if bytes[offset] == b'\\' {
                        offset += 1;
                        self.clipboard_frame
                            .take()
                            .unwrap()
                            .finish(&mut self.state.clipboard, false);
                        self.framing = 0;
                    } else {
                        self.clipboard_frame = None;
                        self.parser.advance(b"\x1b", &mut self.state);
                        self.framing = 1;
                    }
                }
                _ => unreachable!(),
            }
        }
    }

    pub fn next_clipboard_request(&mut self) -> Option<crate::ClipboardRequest> {
        self.state.clipboard.pop()
    }
    /// Changes on reset/host cancellation, so an asynchronous reply cannot
    /// complete a request made before a control or terminal generation change.
    pub fn clipboard_epoch(&self) -> u64 {
        self.state.clipboard.epoch
    }
    pub fn cancel_clipboard_requests(&mut self) {
        self.state.clipboard.cancel();
        if let Some(frame) = &mut self.clipboard_frame {
            frame.abort();
        }
    }

    pub(super) fn begin_apc(&mut self) {
        self.parser.reset();
        self.apc.begin();
        self.apc_route = 0;
        self.ui_frame.begin();
        self.framing = 2;
    }

    pub(super) fn push_apc(&mut self, bytes: &[u8]) {
        if self.apc_route == 0 {
            self.apc_route = match bytes.first() {
                Some(b'G') => 1,
                Some(b'Z') => 2,
                _ => 3,
            };
        }
        if matches!(self.apc_route, 1 | 2) {
            self.state.checkpoint_extensions_seen = true;
        }
        match self.apc_route {
            1 => {
                let context = self.state.graphics_context();
                self.apc.push(
                    &mut self.state.graphics,
                    bytes,
                    context,
                    &mut self.state.responses,
                );
            }
            2 => self.ui_frame.push(bytes),
            _ => {}
        }
    }

    pub(super) fn abort_apc(&mut self) {
        match self.apc_route {
            1 => self.apc.abort(&mut self.state.graphics),
            2 => self.ui_frame.abort(),
            _ => {}
        }
    }

    pub(super) fn finish_apc(&mut self) {
        match self.apc_route {
            1 => {
                let previous = self.state.graphics.revision();
                let context = self.state.graphics_context();
                let outcome =
                    self.apc
                        .finish(&mut self.state.graphics, context, &mut self.state.responses);
                self.state.graphics_result(previous, outcome);
            }
            2 => {
                let Some(payload) = self.ui_frame.payload() else {
                    return;
                };
                if !self.ui_host_enabled {
                    if zgpui::is_capabilities_request(payload) {
                        self.state.reply("\x1b_ZUI;1;{\"v\":1,\"type\":\"capabilities\",\"supported\":false,\"events\":false,\"components\":[]}\x1b\\");
                    }
                    return;
                }
                match self.state.ui.apply(payload) {
                    Ok(zgpui::Change::Updated) => self.state.screen.mark_all(),
                    Ok(zgpui::Change::Capabilities) => {
                        let mut reply = String::from("\x1b_ZUI;1;");
                        if self.state.ui.write_capabilities(&mut reply).is_ok() {
                            reply.push_str("\x1b\\");
                            self.state.reply(&reply);
                        }
                    }
                    Err(_) => {}
                }
            }
            _ => {}
        }
    }

    /// A frontend opts in only after attaching a native renderer and event router.
    /// Generic/headless terminals default to unsupported capability replies.
    pub fn set_ui_host_enabled(&mut self, enabled: bool) {
        self.ui_host_enabled = enabled;
        if !enabled {
            self.reset_ui();
            self.ui_frame.abort();
        }
    }

    /// Native host panel scene, separate from terminal cells and scrollback.
    pub fn ui_scene(&self) -> Option<&zgpui::Scene> {
        self.state.ui.scene()
    }

    /// Invalidation token for painted native UI geometry and captured gestures.
    pub fn ui_revision(&self) -> u64 {
        self.state.ui.revision()
    }

    /// Produces an opted-in intent only for a button in the expected scene.
    /// The host must check its current input owner, live session and painted hit.
    pub fn ui_action(
        &mut self,
        node_id: u32,
        expected_surface: u64,
        expected_revision: u64,
    ) -> Option<zgpui::Action> {
        if !self.ui_host_enabled {
            return None;
        }

        self.state
            .ui
            .action(node_id, expected_surface, expected_revision)
    }

    /// Host lifecycle hook for PTY exit/close. Never resets revision identity.
    pub fn reset_ui(&mut self) {
        if self.state.ui.scene().is_some() {
            self.state.ui.reset();
            self.state.screen.mark_all();
        }
    }
}
