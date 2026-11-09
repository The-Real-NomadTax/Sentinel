//! Translating desktop keyboard input into phone input.

use gpui::{App, Keystroke};
use sentinel_core::{
    Controller,
    control::{keycode, meta},
};

/// Forward a key press to the phone. Returns `false` for keys left to Sentinel itself
/// (Ctrl+Q) or that have no phone equivalent.
pub fn forward_key(ctl: &Controller, ks: &Keystroke, cx: &App) -> bool {
    let m = &ks.modifiers;
    if (m.control || m.platform) && ks.key == "q" {
        return false;
    }

    // Ctrl+V (Cmd+V on macOS) pastes this computer's clipboard on the phone. With nothing
    // usable on it, it falls through to the phone's own Ctrl+V below.
    if (m.control || m.platform) && !m.alt && !m.shift && ks.key == "v" {
        let text = cx.read_from_clipboard().and_then(|c| c.text()).filter(|t| !t.is_empty());
        if let Some(text) = text
            && ctl.set_clipboard(&text, true)
        {
            return true;
        }
    }

    let mut metastate = 0;
    if m.shift {
        metastate |= meta::SHIFT_ON;
    }
    if m.alt {
        metastate |= meta::ALT_ON;
    }
    if m.control {
        metastate |= meta::CTRL_ON;
    }
    if m.platform {
        metastate |= meta::META_ON;
    }

    let special = match ks.key.as_str() {
        "enter" => Some(keycode::ENTER),
        "backspace" => Some(keycode::DEL),
        "delete" => Some(keycode::FORWARD_DEL),
        "tab" => Some(keycode::TAB),
        "escape" => Some(keycode::ESCAPE),
        "up" => Some(keycode::DPAD_UP),
        "down" => Some(keycode::DPAD_DOWN),
        "left" => Some(keycode::DPAD_LEFT),
        "right" => Some(keycode::DPAD_RIGHT),
        "home" => Some(keycode::MOVE_HOME),
        "end" => Some(keycode::MOVE_END),
        "pageup" => Some(keycode::PAGE_UP),
        "pagedown" => Some(keycode::PAGE_DOWN),
        _ => None,
    };
    if let Some(code) = special {
        ctl.press_key(code, metastate);
        return true;
    }

    if m.control || m.alt || m.platform {
        // Shortcuts like Ctrl+A / Ctrl+C / Ctrl+V go as key codes with modifiers.
        if let [c] = ks.key.as_bytes()
            && c.is_ascii_lowercase()
        {
            ctl.press_key(keycode::A + u32::from(c - b'a'), metastate);
            return true;
        }
        return false;
    }

    // Plain typing goes as text, which handles shifted characters and symbols.
    match (&ks.key_char, ks.key.as_str()) {
        (Some(text), _) if !text.is_empty() => {
            ctl.text(text);
            true
        }
        (_, "space") => {
            ctl.press_key(keycode::SPACE, metastate);
            true
        }
        _ => false,
    }
}
