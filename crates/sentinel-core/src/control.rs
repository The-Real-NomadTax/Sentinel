//! Sending input to the phone: touches, scrolling, keys and text.
//!
//! Messages go over the agent's control socket. The layouts below must match
//! `agent/src/dev/sentinel/agent/Input.kt`:
//!
//! ```text
//! 1 TOUCH   action u8 (0 down, 1 up, 2 move) | x i32 | y i32 | frame width u16 | frame height u16
//! 2 SCROLL  x i32 | y i32 | frame width u16 | frame height u16 | horizontal f32 | vertical f32
//! 3 KEY     action u8 (0 down, 1 up) | keycode i32 | meta state i32
//! 4 TEXT    length i32 | UTF-8 bytes
//! 5 KEY_FRAME_REQUEST  (no payload): encode a key frame now, e.g. to start a recording
//! 6 CLIPBOARD  paste u8 (1 = also paste it into the focused field) | length i32 | UTF-8 bytes
//! ```
//!
//! TEXT the phone's virtual keyboard can't type (accented letters, CJK, emoji…) is pasted
//! through the phone's clipboard instead, which replaces what was on it.

use std::{
    io::{Read, Write},
    net::TcpStream,
    sync::mpsc,
    thread,
};

/// Android key codes (`android.view.KeyEvent`) that the frontend is likely to need.
pub mod keycode {
    pub const HOME: u32 = 3;
    pub const BACK: u32 = 4;
    pub const DPAD_UP: u32 = 19;
    pub const DPAD_DOWN: u32 = 20;
    pub const DPAD_LEFT: u32 = 21;
    pub const DPAD_RIGHT: u32 = 22;
    pub const VOLUME_UP: u32 = 24;
    pub const VOLUME_DOWN: u32 = 25;
    pub const POWER: u32 = 26;
    /// `A` is 29, `B` is 30, … `Z` is 54.
    pub const A: u32 = 29;
    pub const TAB: u32 = 61;
    pub const SPACE: u32 = 62;
    pub const ENTER: u32 = 66;
    /// Backspace.
    pub const DEL: u32 = 67;
    pub const PAGE_UP: u32 = 92;
    pub const PAGE_DOWN: u32 = 93;
    pub const ESCAPE: u32 = 111;
    /// Delete (forward).
    pub const FORWARD_DEL: u32 = 112;
    pub const MOVE_HOME: u32 = 122;
    pub const MOVE_END: u32 = 123;
    /// Recent apps.
    pub const APP_SWITCH: u32 = 187;
}

/// Android meta state flags (`android.view.KeyEvent.META_*`).
pub mod meta {
    pub const SHIFT_ON: u32 = 0x1;
    pub const ALT_ON: u32 = 0x2;
    pub const CTRL_ON: u32 = 0x1000;
    pub const META_ON: u32 = 0x10000;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TouchAction {
    Down = 0,
    Up = 1,
    Move = 2,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyAction {
    Down = 0,
    Up = 1,
}

const MSG_TOUCH: u8 = 1;
const MSG_SCROLL: u8 = 2;
const MSG_KEY: u8 = 3;
const MSG_TEXT: u8 = 4;
const MSG_KEY_FRAME_REQUEST: u8 = 5;
const MSG_CLIPBOARD: u8 = 6;

/// Keep text messages small so one long paste doesn't stall touch events behind it.
const TEXT_CHUNK_BYTES: usize = 300;

/// Largest clipboard text sent to the phone. Android passes the clip through a binder call,
/// which fails for transactions approaching 1 MB.
pub const MAX_CLIPBOARD_BYTES: usize = 256 * 1024;

/// A handle for sending input to a mirrored device. Cheap to clone; sending never blocks.
///
/// Coordinates are in the pixel space of the current video frame (`frame_w` × `frame_h`).
/// The agent drops events whose frame size doesn't match the stream (e.g. mid-rotation).
#[derive(Clone)]
pub struct Controller {
    tx: mpsc::Sender<Vec<u8>>,
}

impl Controller {
    /// Start writing messages to `socket` on a background thread.
    pub(crate) fn spawn(socket: TcpStream) -> std::io::Result<Self> {
        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        let mut writer = socket.try_clone()?;
        thread::Builder::new().name("sentinel-control-tx".into()).spawn(move || {
            for msg in rx {
                if writer.write_all(&msg).is_err() {
                    break;
                }
            }
        })?;
        // The agent never writes to this socket, but reading notices when it closes.
        let mut reader = socket;
        thread::Builder::new().name("sentinel-control-rx".into()).spawn(move || {
            let mut buf = [0u8; 256];
            while matches!(reader.read(&mut buf), Ok(n) if n > 0) {}
        })?;
        Ok(Self { tx })
    }

    fn send(&self, msg: Vec<u8>) {
        // Fails only once the session has ended, which is fine to ignore.
        let _ = self.tx.send(msg);
    }

    /// A single-finger touch at (`x`, `y`).
    pub fn touch(&self, action: TouchAction, x: u32, y: u32, frame_w: u32, frame_h: u32) {
        self.send(encode_touch(action, x, y, frame_w, frame_h));
    }

    /// Scroll at (`x`, `y`). Amounts are in wheel notches; positive `vertical` scrolls up.
    pub fn scroll(&self, x: u32, y: u32, frame_w: u32, frame_h: u32, horizontal: f32, vertical: f32) {
        self.send(encode_scroll(x, y, frame_w, frame_h, horizontal, vertical));
    }

    pub fn key(&self, action: KeyAction, keycode: u32, metastate: u32) {
        self.send(encode_key(action, keycode, metastate));
    }

    /// Press and release a key.
    pub fn press_key(&self, keycode: u32, metastate: u32) {
        self.key(KeyAction::Down, keycode, metastate);
        self.key(KeyAction::Up, keycode, metastate);
    }

    /// Type text into the focused field on the phone.
    pub fn text(&self, text: &str) {
        let mut rest = text;
        while !rest.is_empty() {
            let mut end = rest.len().min(TEXT_CHUNK_BYTES);
            while !rest.is_char_boundary(end) {
                end -= 1;
            }
            self.send(encode_text(&rest[..end]));
            rest = &rest[end..];
        }
    }

    /// Put `text` on the phone's clipboard and, if `paste`, paste it into the focused field.
    /// Returns `false` (sending nothing) if the text is longer than [`MAX_CLIPBOARD_BYTES`].
    pub fn set_clipboard(&self, text: &str, paste: bool) -> bool {
        if text.len() > MAX_CLIPBOARD_BYTES {
            return false;
        }
        self.send(encode_clipboard(text, paste));
        true
    }

    /// Ask the agent to encode a key frame now.
    pub fn request_key_frame(&self) {
        self.send(vec![MSG_KEY_FRAME_REQUEST]);
    }

    pub fn back(&self) {
        self.press_key(keycode::BACK, 0);
    }
}

fn push_position(b: &mut Vec<u8>, x: u32, y: u32, frame_w: u32, frame_h: u32) {
    b.extend_from_slice(&x.to_be_bytes());
    b.extend_from_slice(&y.to_be_bytes());
    b.extend_from_slice(&(frame_w.min(u16::MAX as u32) as u16).to_be_bytes());
    b.extend_from_slice(&(frame_h.min(u16::MAX as u32) as u16).to_be_bytes());
}

fn encode_touch(action: TouchAction, x: u32, y: u32, frame_w: u32, frame_h: u32) -> Vec<u8> {
    let mut b = Vec::with_capacity(14);
    b.push(MSG_TOUCH);
    b.push(action as u8);
    push_position(&mut b, x, y, frame_w, frame_h);
    b
}

fn encode_scroll(x: u32, y: u32, frame_w: u32, frame_h: u32, h: f32, v: f32) -> Vec<u8> {
    let mut b = Vec::with_capacity(21);
    b.push(MSG_SCROLL);
    push_position(&mut b, x, y, frame_w, frame_h);
    b.extend_from_slice(&h.to_be_bytes());
    b.extend_from_slice(&v.to_be_bytes());
    b
}

fn encode_key(action: KeyAction, keycode: u32, metastate: u32) -> Vec<u8> {
    let mut b = Vec::with_capacity(10);
    b.push(MSG_KEY);
    b.push(action as u8);
    b.extend_from_slice(&keycode.to_be_bytes());
    b.extend_from_slice(&metastate.to_be_bytes());
    b
}

fn encode_text(text: &str) -> Vec<u8> {
    let mut b = Vec::with_capacity(5 + text.len());
    b.push(MSG_TEXT);
    b.extend_from_slice(&(text.len() as u32).to_be_bytes());
    b.extend_from_slice(text.as_bytes());
    b
}

fn encode_clipboard(text: &str, paste: bool) -> Vec<u8> {
    let mut b = Vec::with_capacity(6 + text.len());
    b.push(MSG_CLIPBOARD);
    b.push(u8::from(paste));
    b.extend_from_slice(&(text.len() as u32).to_be_bytes());
    b.extend_from_slice(text.as_bytes());
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn touch_layout() {
        assert_eq!(
            encode_touch(TouchAction::Move, 100, 200, 1080, 1920),
            [1, 2, 0, 0, 0, 100, 0, 0, 0, 200, 0x04, 0x38, 0x07, 0x80]
        );
    }

    #[test]
    fn scroll_layout() {
        let b = encode_scroll(260, 1026, 1080, 1920, 0.0, -1.5);
        assert_eq!(b[0], 2);
        assert_eq!(&b[1..13], [0, 0, 0x01, 0x04, 0, 0, 0x04, 0x02, 0x04, 0x38, 0x07, 0x80]);
        assert_eq!(f32::from_be_bytes(b[13..17].try_into().unwrap()), 0.0);
        assert_eq!(f32::from_be_bytes(b[17..21].try_into().unwrap()), -1.5);
    }

    #[test]
    fn key_layout() {
        assert_eq!(
            encode_key(KeyAction::Up, keycode::ENTER, meta::SHIFT_ON),
            [3, 1, 0, 0, 0, 66, 0, 0, 0, 1]
        );
    }

    #[test]
    fn text_layout() {
        let mut expected = vec![4, 0, 0, 0, 13];
        expected.extend_from_slice(b"hello, world!");
        assert_eq!(encode_text("hello, world!"), expected);
    }

    #[test]
    fn clipboard_layout() {
        let mut expected = vec![6, 1, 0, 0, 0, 5];
        expected.extend_from_slice("café".as_bytes());
        assert_eq!(encode_clipboard("café", true), expected);
        assert_eq!(encode_clipboard("", false), [6, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn oversized_clipboard_is_not_sent() {
        let (tx, rx) = mpsc::channel();
        let controller = Controller { tx };
        assert!(!controller.set_clipboard(&"x".repeat(MAX_CLIPBOARD_BYTES + 1), true));
        assert!(controller.set_clipboard("hi", true));
        assert_eq!(rx.try_iter().count(), 1);
    }

    #[test]
    fn long_text_is_split_on_char_boundaries() {
        let (tx, rx) = mpsc::channel();
        let controller = Controller { tx };
        let text = "é".repeat(200); // 400 bytes
        controller.text(&text);
        let msgs: Vec<Vec<u8>> = rx.try_iter().collect();
        assert_eq!(msgs.len(), 2);
        let decoded: String = msgs.iter().map(|m| std::str::from_utf8(&m[5..]).unwrap()).collect();
        assert_eq!(decoded, text);
    }
}
