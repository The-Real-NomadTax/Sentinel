use std::{path::PathBuf, sync::Arc, time::Duration};

use gpui::{
    AnyElement, Bounds, ClickEvent, Context, Corners, DevicePixels, ExternalPaths, IntoElement,
    ParentElement, PathPromptOptions, Pixels, Render, RenderImage, Rgba, SharedString, Size,
    Styled, Window, canvas, div, point, prelude::*, px, rgb, size,
};
use sentinel_core::{Device, DeviceState, Frame, Mirror, MirrorConfig, MirrorStatus, PixelFormat, adb};

const FRAME_POLL: Duration = Duration::from_millis(8);
const DEVICE_POLL: Duration = Duration::from_secs(2);

// Colors
const BG: u32 = 0x111318;
const PANEL: u32 = 0x1a1d24;
const PANEL_HOVER: u32 = 0x252a33;
const BORDER: u32 = 0x2b303b;
const TEXT: u32 = 0xe6e8ee;
const MUTED: u32 = 0x8b92a1;
const ACCENT: u32 = 0x3d8bfd;
const OK: u32 = 0x3fb950;
const WARN: u32 = 0xd29922;
const ERR: u32 = 0xf85149;

enum InstallState {
    Running,
    Done,
    Failed(String),
}

struct InstallJob {
    file_name: String,
    state: InstallState,
}

pub struct SentinelApp {
    devices: Vec<Device>,
    device_error: Option<String>,
    selected: Option<String>,
    mirror: Option<Mirror>,
    screen: Option<Arc<RenderImage>>,
    installs: Vec<InstallJob>,
    /// Start mirroring as soon as a device is available (`--mirror` flag).
    auto_mirror: bool,
    _quit_subscription: gpui::Subscription,
}

impl SentinelApp {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self::spawn_device_poller(cx);
        Self::spawn_frame_poller(cx);
        // Stop the stream on quit so the phone-side server and the adb port forward are cleaned up.
        let quit = cx.on_app_quit(|this, _| {
            let mirror = this.mirror.take();
            async move { drop(mirror) }
        });
        Self {
            devices: Vec::new(),
            device_error: None,
            selected: None,
            mirror: None,
            screen: None,
            installs: Vec::new(),
            auto_mirror: std::env::args().any(|a| a == "--mirror"),
            _quit_subscription: quit,
        }
    }

    // ---- background loops -------------------------------------------------------------

    fn spawn_device_poller(cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                let result = cx.background_executor().spawn(async { adb::list_devices() }).await;
                let alive = this.update(cx, |this, cx| {
                    this.set_devices(result, cx);
                });
                if alive.is_err() {
                    break;
                }
                cx.background_executor().timer(DEVICE_POLL).await;
            }
        })
        .detach();
    }

    fn spawn_frame_poller(cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(FRAME_POLL).await;
                let alive = this.update(cx, |this, cx| this.pull_frame(cx));
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    fn set_devices(&mut self, result: anyhow::Result<Vec<Device>>, cx: &mut Context<Self>) {
        match result {
            Ok(devices) => {
                self.device_error = None;
                // Keep the selection if the device is still there; otherwise pick the first ready one.
                let still_there =
                    self.selected.as_ref().is_some_and(|s| devices.iter().any(|d| &d.serial == s));
                if !still_there {
                    self.selected = devices.iter().find(|d| d.is_ready()).map(|d| d.serial.clone());
                }
                if self.auto_mirror && self.mirror.is_none() && let Some(serial) = self.selected.clone() {
                    self.auto_mirror = false;
                    self.start_mirror(serial);
                }
                if self.devices != devices {
                    self.devices = devices;
                    cx.notify();
                }
            }
            Err(err) => {
                self.device_error = Some(format!("{err:#}"));
                cx.notify();
            }
        }
    }

    fn pull_frame(&mut self, cx: &mut Context<Self>) {
        let Some(mirror) = &self.mirror else { return };
        if let Some(frame) = mirror.take_frame() {
            let image = Arc::new(to_render_image(frame));
            // Free the previous frame's GPU texture, otherwise every frame leaks one.
            if let Some(old) = self.screen.replace(image) {
                cx.drop_image(old, None);
            }
            cx.notify();
        } else if !matches!(mirror.status(), MirrorStatus::Streaming { .. } | MirrorStatus::Starting) {
            // Repaint once so a failure/stop message shows up.
            cx.notify();
        }
    }

    // ---- actions ----------------------------------------------------------------------

    fn select_device(&mut self, serial: String, cx: &mut Context<Self>) {
        if self.selected.as_deref() != Some(serial.as_str()) {
            self.stop_mirror(cx);
            self.selected = Some(serial);
            cx.notify();
        }
    }

    fn toggle_mirror(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.mirror.is_some() {
            self.stop_mirror(cx);
        } else if let Some(serial) = self.selected.clone() {
            self.start_mirror(serial);
        }
        cx.notify();
    }

    fn start_mirror(&mut self, serial: String) {
        // GPUI textures are BGRA, so ask core to convert while decoding.
        let config = MirrorConfig { pixel_format: PixelFormat::Bgra8, ..Default::default() };
        self.mirror = Some(Mirror::start(serial, config));
    }

    fn stop_mirror(&mut self, cx: &mut Context<Self>) {
        if let Some(mirror) = self.mirror.take() {
            // Stopping joins the decode thread; do it off the UI thread.
            cx.background_executor().spawn(async move { drop(mirror) }).detach();
        }
        if let Some(old) = self.screen.take() {
            cx.drop_image(old, None);
        }
    }

    fn pick_apks(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Install".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = paths.await {
                this.update(cx, |this, cx| this.install(paths, cx)).ok();
            }
        })
        .detach();
    }

    fn install(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        let Some(serial) = self.selected.clone() else { return };
        for path in paths {
            let file_name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            if !file_name.to_lowercase().ends_with(".apk") {
                self.installs.push(InstallJob {
                    file_name,
                    state: InstallState::Failed("not an .apk file".into()),
                });
                continue;
            }
            let ix = self.installs.len();
            self.installs.push(InstallJob { file_name, state: InstallState::Running });

            let serial = serial.clone();
            cx.spawn(async move |this, cx| {
                let result = cx
                    .background_executor()
                    .spawn(async move { adb::install_apk(&serial, &path) })
                    .await;
                this.update(cx, |this, cx| {
                    this.installs[ix].state = match result {
                        Ok(_) => InstallState::Done,
                        Err(err) => InstallState::Failed(format!("{err:#}")),
                    };
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
        cx.notify();
    }

    // ---- rendering --------------------------------------------------------------------

    fn render_sidebar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let has_device = self.selected.is_some();
        let mirroring = self.mirror.is_some();

        let mut devices = div().flex().flex_col().gap_1();
        if let Some(err) = &self.device_error {
            devices = devices.child(div().text_xs().text_color(rgb(ERR)).child(err.clone()));
        } else if self.devices.is_empty() {
            devices = devices.child(
                div().text_sm().text_color(rgb(MUTED)).child("No devices. Plug in a phone with USB debugging on."),
            );
        }
        for (ix, device) in self.devices.iter().enumerate() {
            let selected = self.selected.as_deref() == Some(device.serial.as_str());
            let (dot, note) = match &device.state {
                DeviceState::Device => (OK, None),
                DeviceState::Unauthorized => (WARN, Some("Accept the debugging prompt on the phone")),
                DeviceState::Offline => (ERR, Some("Offline")),
                DeviceState::Other(_) => (WARN, None),
            };
            let serial = device.serial.clone();
            let ready = device.is_ready();
            devices = devices.child(
                div()
                    .id(("device", ix))
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .border_1()
                    .border_color(rgb(if selected { ACCENT } else { BORDER }))
                    .bg(rgb(if selected { PANEL_HOVER } else { PANEL }))
                    .when(ready, |el| {
                        el.cursor_pointer()
                            .hover(|s| s.bg(rgb(PANEL_HOVER)))
                            .on_click(cx.listener(move |this, _, _, cx| this.select_device(serial.clone(), cx)))
                    })
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(div().size_2().rounded_full().bg(rgb(dot)))
                            .child(div().text_sm().child(device.model.clone().unwrap_or_else(|| device.serial.clone()))),
                    )
                    .child(div().text_xs().text_color(rgb(MUTED)).child(device.serial.clone()))
                    .children(note.map(|n| div().text_xs().text_color(rgb(WARN)).child(n))),
            );
        }

        let installs = self.installs.iter().rev().take(8).map(|job| {
            let (color, label): (u32, SharedString) = match &job.state {
                InstallState::Running => (MUTED, "Installing…".into()),
                InstallState::Done => (OK, "Installed".into()),
                InstallState::Failed(e) => (ERR, e.clone().into()),
            };
            div()
                .flex()
                .flex_col()
                .px_2()
                .py_1()
                .rounded_sm()
                .bg(rgb(PANEL))
                .child(div().text_xs().truncate().child(job.file_name.clone()))
                .child(div().text_xs().text_color(rgb(color)).child(label))
        });

        div()
            .flex()
            .flex_col()
            .w(px(280.))
            .h_full()
            .flex_none()
            .gap_4()
            .p_4()
            .bg(rgb(0x15171d))
            .border_r_1()
            .border_color(rgb(BORDER))
            .child(div().text_xl().font_weight(gpui::FontWeight::BOLD).child("Sentinel"))
            .child(section_label("Devices"))
            .child(devices)
            .child(section_label("Screen"))
            .child(button(
                "mirror",
                if mirroring { "Stop mirroring" } else { "Start mirroring" },
                has_device,
                mirroring,
                cx.listener(Self::toggle_mirror),
            ))
            .child(section_label("Apps"))
            .child(button("install", "Install APK…", has_device, false, cx.listener(Self::pick_apks)))
            .child(div().text_xs().text_color(rgb(MUTED)).child("…or drop .apk files anywhere on the window"))
            .child(div().flex().flex_col().gap_1().children(installs))
    }

    fn render_screen(&self) -> AnyElement {
        let center = || div().size_full().flex().items_center().justify_center().text_color(rgb(MUTED));

        let status = self.mirror.as_ref().map(|m| m.status());
        match (&self.screen, status) {
            (_, Some(MirrorStatus::Failed(err))) => center()
                .flex_col()
                .gap_2()
                .p_8()
                .child(div().text_color(rgb(ERR)).child("Mirroring failed"))
                .child(div().text_xs().child(err))
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
                .child(if self.selected.is_some() {
                    "Press “Start mirroring” to see your phone’s screen"
                } else {
                    "Connect a device to get started"
                })
                .into_any_element(),
        }
    }
}

impl Render for SentinelApp {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("root")
            .flex()
            .size_full()
            .bg(rgb(BG))
            .text_color(rgb(TEXT))
            .drag_over::<ExternalPaths>(|style, _, _, _| style.bg(rgb(0x18243a)))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                this.install(paths.paths().to_vec(), cx);
            }))
            .child(self.render_sidebar(cx))
            .child(div().flex_1().min_w_0().h_full().overflow_hidden().child(self.render_screen()))
    }
}

fn section_label(text: &'static str) -> impl IntoElement {
    div().text_xs().text_color(rgb(MUTED)).child(text.to_uppercase())
}

fn button(
    id: &'static str,
    label: &'static str,
    enabled: bool,
    active: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    let bg: Rgba = if active { rgb(PANEL_HOVER) } else { rgb(ACCENT) };
    div()
        .id(id)
        .px_3()
        .py_2()
        .rounded_md()
        .text_sm()
        .flex()
        .justify_center()
        .bg(bg)
        .when(!enabled, |el| el.opacity(0.4))
        .when(enabled, |el| el.cursor_pointer().hover(|s| s.opacity(0.85)).on_click(on_click))
        .child(label)
}

/// Turn a core frame (already BGRA, which is what GPUI expects) into a GPUI image.
fn to_render_image(frame: Frame) -> RenderImage {
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
