use std::{path::PathBuf, sync::Arc, time::Duration};

use gpui::{
    AnyElement, App, AppContext, Bounds, ClickEvent, Context, ExternalPaths, IntoElement,
    ParentElement, PathPromptOptions, Render, RenderImage, SharedString, Styled, TitlebarOptions,
    Window, WindowBounds, WindowHandle, WindowOptions, div, prelude::*, px, rgb, size,
};
use sentinel_core::{Device, DeviceState, Mirror, MirrorConfig, MirrorStatus, PixelFormat, adb};

use crate::{
    popout::PopoutWindow,
    screen::{self, ScreenState},
    theme::*,
};

const FRAME_POLL: Duration = Duration::from_millis(8);
const DEVICE_POLL: Duration = Duration::from_secs(2);

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
    /// The pop-out screen window, while the screen is popped out.
    popout: Option<WindowHandle<PopoutWindow>>,
    /// A pop-out window has been requested but not opened yet.
    popout_opening: bool,
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
            popout: None,
            popout_opening: false,
            installs: Vec::new(),
            auto_mirror: std::env::args().any(|a| a == "--mirror"),
            _quit_subscription: quit,
        }
    }

    /// Everything a window needs to draw the mirrored screen.
    pub fn screen_state(&self) -> ScreenState {
        ScreenState {
            image: self.screen.clone(),
            status: self.mirror.as_ref().map(|m| m.status()),
            has_device: self.selected.is_some(),
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
            let image = Arc::new(screen::to_render_image(frame));
            // Free the previous frame's GPU texture (in every window), otherwise each frame leaks one.
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
        self.pop_in(cx);
        if let Some(mirror) = self.mirror.take() {
            // Stopping joins the decode thread; do it off the UI thread.
            cx.background_executor().spawn(async move { drop(mirror) }).detach();
        }
        if let Some(old) = self.screen.take() {
            cx.drop_image(old, None);
        }
    }

    /// Move the screen into its own window. The main window keeps running.
    fn pop_out(&mut self, cx: &mut Context<Self>) {
        if let Some(handle) = self.popout {
            // Already out: just bring it to the front.
            handle.update(cx, |_, window, _| window.activate_window()).ok();
            return;
        }
        if self.popout_opening {
            return;
        }
        let state = self.screen_state();
        let window_size = if state.is_portrait() { size(px(480.), px(940.)) } else { size(px(1000.), px(560.)) };
        let title = match state.device_name() {
            Some(name) => format!("{name} - Sentinel"),
            None => "Screen - Sentinel".into(),
        };
        let bounds = Bounds::centered(None, window_size, cx);
        let app = cx.entity();
        let weak_app = app.downgrade();
        self.popout_opening = true;

        // `open_window` renders the new window immediately, and the pop-out reads this entity
        // while rendering. We're inside this entity's update right now, so open it afterwards.
        cx.defer(move |cx| {
            let result = cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    titlebar: Some(TitlebarOptions { title: Some(title.into()), ..Default::default() }),
                    app_id: Some("sentinel".into()),
                    ..Default::default()
                },
                {
                    let app = app.clone();
                    move |window, cx| {
                        // Closing the window with the window manager's close button counts as popping back in.
                        window.on_window_should_close(cx, move |_, cx| {
                            weak_app.update(cx, |app, cx| app.popout_closed(cx)).ok();
                            true
                        });
                        cx.new(|cx| PopoutWindow::new(app, cx))
                    }
                },
            );
            app.update(cx, |this, cx| {
                this.popout_opening = false;
                match result {
                    Ok(handle) => this.popout = Some(handle),
                    Err(err) => eprintln!("failed to open pop-out window: {err:#}"),
                }
                cx.notify();
            });
        });
    }

    /// Close the pop-out window (if any) and show the screen in the main window again.
    /// Must not be called from inside the pop-out window itself; that uses [`Self::popout_closed`].
    fn pop_in(&mut self, cx: &mut Context<Self>) {
        if let Some(handle) = self.popout.take() {
            handle.update(cx, |_, window, _| window.remove_window()).ok();
            cx.notify();
        }
    }

    /// The pop-out window closed itself (its button, or the window manager).
    pub fn popout_closed(&mut self, cx: &mut Context<Self>) {
        self.popout = None;
        cx.notify();
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
        let popped_out = self.popout.is_some();

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
            .bg(rgb(SIDEBAR))
            .border_r_1()
            .border_color(rgb(BORDER))
            .child(div().text_xl().font_weight(gpui::FontWeight::BOLD).child("Sentinel"))
            .child(section_label("Devices"))
            .child(devices)
            .child(section_label("Screen"))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(button(
                        "mirror",
                        if mirroring { "Stop mirroring" } else { "Start mirroring" },
                        has_device,
                        mirroring,
                        cx.listener(Self::toggle_mirror),
                    ))
                    .child(button(
                        "pop-toggle",
                        if popped_out { "Pop back in" } else { "Pop out screen" },
                        mirroring,
                        true,
                        cx.listener(|this, _, _, cx| {
                            if this.popout.is_some() { this.pop_in(cx) } else { this.pop_out(cx) }
                        }),
                    )),
            )
            .child(section_label("Apps"))
            .child(button("install", "Install APK…", has_device, false, cx.listener(Self::pick_apks)))
            .child(div().text_xs().text_color(rgb(MUTED)).child("…or drop .apk files anywhere on the window"))
            .child(div().flex().flex_col().gap_1().children(installs))
    }

    fn render_main_pane(&mut self, cx: &mut Context<Self>) -> AnyElement {
        if self.popout.is_none() {
            return screen::render(&self.screen_state());
        }
        div()
            .size_full()
            .flex()
            .flex_col()
            .gap_3()
            .items_center()
            .justify_center()
            .text_color(rgb(MUTED))
            .child("The screen is open in its own window")
            .child(
                div().flex().gap_2().child(div().w(px(150.)).child(button(
                    "show-popout",
                    "Show window",
                    true,
                    true,
                    cx.listener(|this, _, _, cx| this.pop_out(cx)),
                )))
                .child(div().w(px(150.)).child(button(
                    "pop-in-main",
                    "Pop back in",
                    true,
                    false,
                    cx.listener(|this, _, _, cx| this.pop_in(cx)),
                ))),
            )
            .into_any_element()
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
            .drag_over::<ExternalPaths>(|style, _, _, _| style.bg(rgb(DROP_HIGHLIGHT)))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                this.install(paths.paths().to_vec(), cx);
            }))
            .child(self.render_sidebar(cx))
            .child(div().flex_1().min_w_0().h_full().overflow_hidden().child(self.render_main_pane(cx)))
    }
}

/// Open the main Sentinel window.
pub fn open_main_window(cx: &mut App) -> WindowHandle<SentinelApp> {
    let bounds = Bounds::centered(None, size(px(1100.), px(760.)), cx);
    cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions { title: Some("Sentinel".into()), ..Default::default() }),
            app_id: Some("sentinel".into()),
            ..Default::default()
        },
        |_, cx| cx.new(SentinelApp::new),
    )
    .expect("failed to open window")
}
