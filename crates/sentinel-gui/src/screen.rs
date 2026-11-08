//! The mirrored phone screen, drawn the same way in the main window and the pop-out window.
//!
//! When the mirror accepts input, the mouse acts as a finger on the screen (left button to
//! tap/drag, wheel to scroll, right-click = Back, middle-click = Home), and typing goes to the
//! phone once the screen has been clicked.

use std::{cell::Cell, rc::Rc, sync::Arc};

use gpui::{
    AnyElement, BorderStyle, Bounds, Corners, CursorStyle, DevicePixels, DispatchPhase, FocusHandle,
    HitboxBehavior, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, ParentElement, Pixels, Point, RenderImage, ScrollDelta, ScrollWheelEvent, Size,
    Styled, canvas, div, outline, point, prelude::*, px, rgb, size,
};
use sentinel_core::{Controller, Frame, MirrorStatus, TouchAction, control::keycode};

use crate::{
    input,
    theme::{ACCENT, BORDER, ERR, MUTED, PANEL, PANEL_HOVER, TEXT},
};

/// What the screen area needs to draw itself. Cheap to clone.
#[derive(Clone)]
pub struct ScreenState {
    pub image: Option<Arc<RenderImage>>,
    /// `None` when no mirroring session exists.
    pub status: Option<MirrorStatus>,
    pub has_device: bool,
    /// Present while the phone accepts input.
    pub controller: Option<Controller>,
    /// Receives typing for the phone once the screen is clicked.
    pub focus: FocusHandle,
    /// Whether the left button is currently held on the screen (a finger is down).
    /// Shared across frames and windows.
    pub finger_down: Rc<Cell<bool>>,
}

impl ScreenState {
    pub fn device_name(&self) -> Option<&str> {
        match &self.status {
            Some(MirrorStatus::Streaming { device_name, .. }) => Some(device_name),
            _ => None,
        }
    }

    /// True if the current frame is taller than it is wide (or unknown, since phones usually are).
    pub fn is_portrait(&self) -> bool {
        self.image.as_ref().is_none_or(|img| {
            let s = img.size(0);
            s.height.0 >= s.width.0
        })
    }
}

pub fn render(state: &ScreenState) -> AnyElement {
    let center = || div().size_full().flex().items_center().justify_center().text_color(rgb(MUTED));

    match (&state.image, &state.status) {
        (_, Some(MirrorStatus::Failed(err))) => center()
            .flex_col()
            .gap_2()
            .p_8()
            .child(div().text_color(rgb(ERR)).child("Mirroring failed"))
            .child(div().text_xs().child(err.clone()))
            .into_any_element(),
        (Some(image), _) => {
            let ctl = state.controller.clone();
            div()
                .id("screen")
                .size_full()
                .flex()
                .flex_col()
                .track_focus(&state.focus)
                .when_some(ctl.clone(), |el, ctl| {
                    el.on_key_down(move |event: &KeyDownEvent, _, cx| {
                        if input::forward_key(&ctl, &event.keystroke) {
                            cx.stop_propagation();
                        }
                    })
                })
                .child(div().flex_1().min_h_0().p_4().child(screen_canvas(image.clone(), state)))
                .child(nav_bar(ctl))
                .into_any_element()
        }
        (None, Some(MirrorStatus::Starting | MirrorStatus::Streaming { .. })) => {
            center().child("Connecting to device…").into_any_element()
        }
        _ => center()
            .child(if state.has_device {
                "Press “Start mirroring” to see your phone’s screen"
            } else {
                "Connect a device to get started"
            })
            .into_any_element(),
    }
}

/// Paints the frame letterboxed into the available space and, when input is enabled, turns
/// mouse events over it into touches. Painting directly (instead of `img()`) keeps the frame
/// fitted in both orientations and gives us the exact on-screen rectangle for mapping clicks.
fn screen_canvas(image: Arc<RenderImage>, state: &ScreenState) -> impl IntoElement {
    let ctl = state.controller.clone();
    let focus = state.focus.clone();
    let finger_down = state.finger_down.clone();
    let frame = image.size(0);

    canvas(
        move |bounds, window, _| {
            let target = fit_contain(bounds, frame);
            (target, window.insert_hitbox(target, HitboxBehavior::Normal))
        },
        move |_, (target, hitbox), window, _| {
            window.paint_image(target, Corners::all(px(6.)), image, 0, false).ok();
            if focus.is_focused(window) {
                window.paint_quad(outline(target, rgb(ACCENT), BorderStyle::Solid));
            }
            let Some(ctl) = ctl else { return };
            window.set_cursor_style(CursorStyle::PointingHand, &hitbox);

            let (fw, fh) = (frame.width.0 as u32, frame.height.0 as u32);
            let to_device = move |pos: Point<Pixels>| map_point(pos, target, fw, fh);

            {
                let (ctl, finger_down, hitbox, focus) = (ctl.clone(), finger_down.clone(), hitbox.clone(), focus.clone());
                window.on_mouse_event(move |e: &MouseDownEvent, phase, window, _| {
                    if phase != DispatchPhase::Bubble || !hitbox.is_hovered(window) {
                        return;
                    }
                    window.focus(&focus);
                    match e.button {
                        MouseButton::Left => {
                            let (x, y) = to_device(e.position);
                            ctl.touch(TouchAction::Down, x, y, fw, fh);
                            finger_down.set(true);
                        }
                        MouseButton::Right => ctl.back(),
                        MouseButton::Middle => ctl.press_key(keycode::HOME, 0),
                        MouseButton::Navigate(_) => {}
                    }
                });
            }
            {
                let (ctl, finger_down) = (ctl.clone(), finger_down.clone());
                window.on_mouse_event(move |e: &MouseMoveEvent, phase, _, _| {
                    if phase != DispatchPhase::Bubble || !finger_down.get() {
                        return;
                    }
                    let (x, y) = to_device(e.position);
                    if e.pressed_button == Some(MouseButton::Left) {
                        ctl.touch(TouchAction::Move, x, y, fw, fh);
                    } else {
                        // The button was released somewhere we didn't see (e.g. outside the window).
                        ctl.touch(TouchAction::Up, x, y, fw, fh);
                        finger_down.set(false);
                    }
                });
            }
            {
                let (ctl, finger_down) = (ctl.clone(), finger_down.clone());
                window.on_mouse_event(move |e: &MouseUpEvent, phase, _, _| {
                    if phase == DispatchPhase::Bubble && e.button == MouseButton::Left && finger_down.get() {
                        let (x, y) = to_device(e.position);
                        ctl.touch(TouchAction::Up, x, y, fw, fh);
                        finger_down.set(false);
                    }
                });
            }
            window.on_mouse_event(move |e: &ScrollWheelEvent, phase, window, _| {
                if phase != DispatchPhase::Bubble || !hitbox.is_hovered(window) {
                    return;
                }
                // Android expects wheel notches; treat ~50px of smooth scrolling as one notch.
                let (h, v) = match e.delta {
                    ScrollDelta::Lines(d) => (d.x, d.y),
                    ScrollDelta::Pixels(d) => (f32::from(d.x) / 50.0, f32::from(d.y) / 50.0),
                };
                let (x, y) = to_device(e.position);
                ctl.scroll(x, y, fw, fh, h, v);
            });
        },
    )
    .size_full()
}

/// Back / Home / Recents and hardware buttons under the screen.
fn nav_bar(ctl: Option<Controller>) -> impl IntoElement {
    type Action = fn(&Controller);
    let buttons: [(&'static str, &'static str, Action); 6] = [
        ("nav-back", "Back", |c| c.back()),
        ("nav-home", "Home", |c| c.press_key(keycode::HOME, 0)),
        ("nav-recents", "Recents", |c| c.press_key(keycode::APP_SWITCH, 0)),
        ("nav-vol-down", "Vol −", |c| c.press_key(keycode::VOLUME_DOWN, 0)),
        ("nav-vol-up", "Vol +", |c| c.press_key(keycode::VOLUME_UP, 0)),
        ("nav-power", "Power", |c| c.press_key(keycode::POWER, 0)),
    ];
    let enabled = ctl.is_some();

    div()
        .flex()
        .flex_col()
        .flex_none()
        .items_center()
        .gap_1()
        .pb_3()
        .child(div().flex().gap_2().children(buttons.into_iter().map(|(id, label, action)| {
            let ctl = ctl.clone();
            div()
                .id(id)
                .px_3()
                .py_1()
                .rounded_md()
                .text_sm()
                .text_color(rgb(TEXT))
                .bg(rgb(PANEL))
                .border_1()
                .border_color(rgb(BORDER))
                .when(!enabled, |el| el.opacity(0.4))
                .when_some(ctl, |el, ctl| {
                    el.cursor_pointer().hover(|s| s.bg(rgb(PANEL_HOVER))).on_click(move |_, _, _| action(&ctl))
                })
                .child(label)
        })))
        .child(div().text_xs().text_color(rgb(MUTED)).child(if enabled {
            "Click the screen to use it and type · right-click = Back · middle-click = Home"
        } else {
            "View only"
        }))
}

/// Map a window position to device pixels, clamping to the frame.
fn map_point(pos: Point<Pixels>, target: Bounds<Pixels>, fw: u32, fh: u32) -> (u32, u32) {
    let rel_x = f32::from(pos.x - target.origin.x) / f32::from(target.size.width);
    let rel_y = f32::from(pos.y - target.origin.y) / f32::from(target.size.height);
    let x = (rel_x.clamp(0.0, 1.0) * fw as f32) as u32;
    let y = (rel_y.clamp(0.0, 1.0) * fh as f32) as u32;
    (x.min(fw.saturating_sub(1)), y.min(fh.saturating_sub(1)))
}

/// Turn a core frame (already BGRA, which is what GPUI expects) into a GPUI image.
pub fn to_render_image(frame: Frame) -> RenderImage {
    let buffer = image::RgbaImage::from_raw(frame.width, frame.height, frame.data)
        .expect("frame buffer size matches dimensions");
    RenderImage::new(vec![image::Frame::new(buffer)])
}

/// Largest rectangle with the image's aspect ratio that fits in `bounds`, centered.
fn fit_contain(bounds: Bounds<Pixels>, image: Size<DevicePixels>) -> Bounds<Pixels> {
    let (iw, ih) = (image.width.0 as f32, image.height.0 as f32);
    let (bw, bh) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
    if iw <= 0. || ih <= 0. {
        return bounds;
    }
    let scale = (bw / iw).min(bh / ih);
    let (w, h) = (iw * scale, ih * scale);
    let origin = bounds.origin + point(px((bw - w) / 2.), px((bh - h) / 2.));
    Bounds::new(origin, size(px(w), px(h)))
}
