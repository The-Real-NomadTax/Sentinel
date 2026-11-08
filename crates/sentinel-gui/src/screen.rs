//! The mirrored phone screen, drawn the same way in the main window and the pop-out window.

use std::sync::Arc;

use gpui::{
    AnyElement, Bounds, Corners, DevicePixels, IntoElement, ParentElement, Pixels, RenderImage,
    Size, Styled, canvas, div, point, px, rgb, size,
};
use sentinel_core::{Frame, MirrorStatus};

use crate::theme::{ERR, MUTED};

/// What the screen area needs to draw itself. Cheap to clone (the image is an `Arc`).
#[derive(Clone)]
pub struct ScreenState {
    pub image: Option<Arc<RenderImage>>,
    /// `None` when no mirroring session exists.
    pub status: Option<MirrorStatus>,
    pub has_device: bool,
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
            let image = image.clone();
            // Paint directly so the frame always fits the pane (letterboxed), whatever the
            // phone's orientation. `img()` lets the aspect ratio override the height.
            div()
                .size_full()
                .p_4()
                .child(
                    canvas(
                        |_, _, _| (),
                        move |bounds, _, window, _| {
                            let target = fit_contain(bounds, image.size(0));
                            window.paint_image(target, Corners::all(px(6.)), image, 0, false).ok();
                        },
                    )
                    .size_full(),
                )
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
