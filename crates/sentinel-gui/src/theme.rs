//! Shared colors and small widgets used by every window.

use gpui::{ClickEvent, IntoElement, ParentElement, SharedString, Styled, Window, div, prelude::*, rgb};

pub const BG: u32 = 0x111318;
pub const SIDEBAR: u32 = 0x15171d;
pub const PANEL: u32 = 0x1a1d24;
pub const PANEL_HOVER: u32 = 0x252a33;
pub const BORDER: u32 = 0x2b303b;
pub const TEXT: u32 = 0xe6e8ee;
pub const MUTED: u32 = 0x8b92a1;
pub const ACCENT: u32 = 0x3d8bfd;
pub const OK: u32 = 0x3fb950;
pub const WARN: u32 = 0xd29922;
pub const ERR: u32 = 0xf85149;
pub const DROP_HIGHLIGHT: u32 = 0x18243a;
pub const RECORD: u32 = 0xc93a3a;

pub fn section_label(text: &'static str) -> impl IntoElement {
    div().text_xs().text_color(rgb(MUTED)).child(text.to_uppercase())
}

/// A full-width button. `secondary` draws it in the muted style.
pub fn button(
    id: &'static str,
    label: impl Into<SharedString>,
    enabled: bool,
    secondary: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    colored_button(id, label, enabled, if secondary { PANEL_HOVER } else { ACCENT }, on_click)
}

/// A full-width button with a custom background color.
pub fn colored_button(
    id: &'static str,
    label: impl Into<SharedString>,
    enabled: bool,
    color: u32,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    let label: SharedString = label.into();
    let bg = rgb(color);
    div()
        .id(id)
        .px_3()
        .py_2()
        .rounded_md()
        .text_sm()
        .text_color(rgb(TEXT))
        .flex()
        .justify_center()
        .bg(bg)
        .when(!enabled, |el| el.opacity(0.4))
        .when(enabled, |el| el.cursor_pointer().hover(|s| s.opacity(0.85)).on_click(on_click))
        .child(label)
}
