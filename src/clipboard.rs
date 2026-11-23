//! Bounded OSC 52 requests. The host supplies clipboard ownership and I/O.
use alloc::{collections::VecDeque, string::String, vec::Vec};

pub const MAX_CLIPBOARD_BYTES: usize = 1024 * 1024;
pub const MAX_CLIPBOARD_REQUESTS: usize = 8;
pub(crate) const MAX_ENCODED: usize = MAX_CLIPBOARD_BYTES.div_ceil(3) * 4 + 17;

#[derive(Debug)]
pub struct ClipboardRequest {
    selectors: [u8; 16],
    count: u8,
    /// None requests text; Some("") clears the selected clipboard.
    pub text: Option<String>,
    pub bell_terminated: bool,
}
impl ClipboardRequest {
    pub fn selectors(&self) -> &[u8] {
        &self.selectors[..usize::from(self.count)]
    }
    /// A denied/unavailable query returns an empty payload, never partial text.
    /// Responses are sent through the host's bounded PTY response queue, not paste.
    pub fn reply(&self, selector: u8, text: Option<&[u8]>) -> Vec<u8> {
        let text = text
            .filter(|bytes| {
                bytes.len() <= MAX_CLIPBOARD_BYTES
                    && !bytes.contains(&0)
                    && core::str::from_utf8(bytes).is_ok()
            })
            .unwrap_or_default();
        let selector = if self.selectors().contains(&selector) {
            selector
        } else {
            self.selectors[0]
        };
        let encoded = z64::encoded_len(text.len(), z64::Mode::Standard)
            .expect("bounded clipboard text has a representable encoded length");
        let mut bytes = Vec::with_capacity(9 + encoded);
        bytes.extend_from_slice(b"\x1b]52;");
        bytes.push(selector);
        bytes.push(b';');
        let start = bytes.len();
        bytes.resize(start + encoded, 0);
        z64::encode(text, &mut bytes[start..], z64::Mode::Standard)
            .expect("exact clipboard encoding capacity");
        if self.bell_terminated {
            bytes.push(7);
        } else {
            bytes.extend_from_slice(b"\x1b\\");
        }
        bytes
    }
}
#[derive(Default)]
pub(crate) struct Queue {
    pending: VecDeque<ClipboardRequest>,
    bytes: usize,
    pub epoch: u64,
}
impl Queue {
    pub(crate) fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    pub fn cancel(&mut self) {
        self.pending.clear();
        self.bytes = 0;
        self.epoch = self.epoch.wrapping_add(1);
    }
    pub fn pop(&mut self) -> Option<ClipboardRequest> {
        let request = self.pending.pop_front()?;
        self.bytes -= request.text.as_ref().map_or(0, String::len);
        Some(request)
    }
    pub fn receive(&mut self, body: &[u8], bell_terminated: bool) {
        if body.len() > MAX_ENCODED || self.pending.len() >= MAX_CLIPBOARD_REQUESTS {
            return;
        }
        let Some(split) = body.iter().position(|&b| b == b';') else {
            return;
        };
        let selected = &body[..split];
        if selected.len() > 16 {
            return;
        }
        let selected = if selected.is_empty() {
            b"s0".as_slice()
        } else {
            selected
        };
        let mut selectors = [0; 16];
        let mut count = 0;
        for &selector in selected {
            if !matches!(selector, b'c' | b'p' | b'q' | b's' | b'0'..=b'7') {
                return;
            }
            if !selectors[..count].contains(&selector) {
                selectors[count] = selector;
                count += 1;
            }
        }
        let encoded = &body[split + 1..];
        let text = if encoded == b"?" {
            None
        } else {
            // xterm specifies that a non-base64 payload clears the selection.
            let decoded = match decode(encoded) {
                Ok(value) => value,
                Err(DecodeError::Invalid) => Vec::new(),
                Err(DecodeError::Limit) => return,
            };
            if decoded.contains(&0) {
                return;
            }
            let Ok(text) = String::from_utf8(decoded) else {
                return;
            };
            if self.bytes.saturating_add(text.len()) > MAX_CLIPBOARD_BYTES {
                return;
            }
            Some(text)
        };
        self.bytes += text.as_ref().map_or(0, String::len);
        self.pending.push_back(ClipboardRequest {
            selectors,
            count: count as u8,
            text,
            bell_terminated,
        });
    }
}
pub(crate) struct Frame {
    bytes: Vec<u8>,
    discarded: bool,
}
impl Frame {
    pub fn new() -> Self {
        Self {
            bytes: Vec::new(),
            discarded: false,
        }
    }
    pub fn push(&mut self, bytes: &[u8]) {
        if self.discarded {
            return;
        }
        if self.bytes.len().saturating_add(bytes.len()) > MAX_ENCODED {
            self.abort();
            return;
        }
        self.bytes.extend_from_slice(bytes);
    }
    pub fn abort(&mut self) {
        self.discarded = true;
        self.bytes = Vec::new();
    }
    pub fn finish(self, queue: &mut Queue, bell: bool) {
        if !self.discarded {
            queue.receive(&self.bytes, bell);
        }
    }
}
enum DecodeError {
    Invalid,
    Limit,
}
fn decode(bytes: &[u8]) -> Result<Vec<u8>, DecodeError> {
    if bytes.len() > MAX_CLIPBOARD_BYTES.div_ceil(3) * 4 {
        return Err(DecodeError::Limit);
    }
    if !bytes.len().is_multiple_of(4) {
        return Err(DecodeError::Invalid);
    }
    let padding = usize::from(bytes.last() == Some(&b'='))
        + usize::from(bytes.len() > 1 && bytes.get(bytes.len() - 2) == Some(&b'='));
    let size = z64::decoded_capacity(bytes.len())
        .expect("bounded clipboard encoding has a representable decoded length")
        .saturating_sub(padding);
    // An overlimit request must be ignored even if its content is also invalid.
    // Invalid Base64 within the limit instead requests a clipboard clear.
    if size > MAX_CLIPBOARD_BYTES {
        return Err(DecodeError::Limit);
    }
    let mut out = alloc::vec![0; size];
    let written =
        z64::decode(bytes, &mut out, z64::Mode::Standard).map_err(|_| DecodeError::Invalid)?;
    out.truncate(written);
    Ok(out)
}

#[cfg(test)]
mod tests;
