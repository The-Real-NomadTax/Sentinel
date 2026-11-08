use std::{
    cell::Cell,
    path::PathBuf,
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};

use gpui::{
    AnyElement, App, AppContext, Bounds, ClickEvent, Context, ExternalPaths, FocusHandle, IntoElement,
    ParentElement, PathPromptOptions, Render, RenderImage, SharedString, Styled, TitlebarOptions,
    Window, WindowBounds, WindowHandle, WindowOptions, div, prelude::*, px, rgb, size,
};
use sentinel_core::{
    Device, DeviceInfo, DeviceState, Mirror, MirrorConfig, MirrorStatus, PixelFormat, adb, device_info, media,
};

use crate::{
    dashboard,
    popout::PopoutWindow,
    screen::{self, ScreenState},
    theme::*,
};

const FRAME_POLL: Duration = Duration::from_millis(8);
const DEVICE_POLL: Duration = Duration::from_secs(2);
const INFO_POLL: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Dashboard,
    Screen,
}

enum InstallState {
    Running,
    Done,
    Failed(String),
}

/// Result of the last screenshot/recording, shown under the buttons.
enum MediaNotice {
    Busy(&'static str),
    Saved { what: &'static str, path: PathBuf },
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
    /// Keyboard focus for sending typing to the phone.
    screen_focus: FocusHandle,
    finger_down: Rc<Cell<bool>>,
    /// The pop-out screen window, while the screen is popped out.
    popout: Option<WindowHandle<PopoutWindow>>,
    /// A pop-out window has been requested but not opened yet.
    popout_opening: bool,
    installs: Vec<InstallJob>,
    tab: Tab,
    /// Dashboard data for the selected device.
    info: Option<DeviceInfo>,
    info_error: Option<String>,
    info_in_flight: bool,
    /// Start mirroring as soon as a device is available (`--mirror` flag).
    auto_mirror: bool,
    media_notice: Option<MediaNotice>,
    screenshot_in_flight: bool,
    /// Record was pressed before the mirror was streaming; start as soon as it is.
    record_when_ready: bool,
    /// Last repaint for the recording timer.
    last_tick: Instant,
    _quit_subscription: gpui::Subscription,
}

impl SentinelApp {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self::spawn_device_poller(cx);
        Self::spawn_frame_poller(cx);
        Self::spawn_info_poller(cx);
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
            screen_focus: cx.focus_handle(),
            finger_down: Rc::new(Cell::new(false)),
            popout: None,
            popout_opening: false,
            installs: Vec::new(),
            tab: Tab::Dashboard,
            info: None,
            info_error: None,
            info_in_flight: false,
            auto_mirror: std::env::args().any(|a| a == "--mirror"),
            media_notice: None,
            screenshot_in_flight: false,
            record_when_ready: false,
            last_tick: Instant::now(),
            _quit_subscription: quit,
        }
    }

    /// Everything a window needs to draw the mirrored screen.
    pub fn screen_state(&self) -> ScreenState {
        ScreenState {
            image: self.screen.clone(),
            status: self.mirror.as_ref().map(|m| m.status()),
            has_device: self.selected.is_some(),
            controller: self.mirror.as_ref().and_then(|m| m.controller()),
            focus: self.screen_focus.clone(),
            finger_down: self.finger_down.clone(),
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

    fn spawn_info_poller(cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(INFO_POLL).await;
                if this.update(cx, |this, cx| this.refresh_info(cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    /// Fetch dashboard data for the selected device in the background.
    fn refresh_info(&mut self, cx: &mut Context<Self>) {
        let ready = self.selected.as_ref().filter(|s| self.devices.iter().any(|d| &&d.serial == s && d.is_ready()));
        let Some(serial) = ready.cloned() else { return };
        if self.info_in_flight {
            return;
        }
        self.info_in_flight = true;
        cx.spawn(async move |this, cx| {
            let fetch_serial = serial.clone();
            let result = cx.background_executor().spawn(async move { device_info(&fetch_serial) }).await;
            this.update(cx, |this, cx| {
                this.info_in_flight = false;
                // Ignore results for a device that's no longer selected.
                if this.selected.as_deref() != Some(serial.as_str()) {
                    return;
                }
                match result {
                    Ok(info) => {
                        this.info = Some(info);
                        this.info_error = None;
                    }
                    Err(err) => this.info_error = Some(format!("{err:#}")),
                }
                cx.notify();
            })
            .ok();
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
                    self.info = None;
                    self.info_error = None;
                }
                let fetch_now = self.info.is_none() && self.info_error.is_none();
                if self.auto_mirror && self.mirror.is_none() && let Some(serial) = self.selected.clone() {
                    self.auto_mirror = false;
                    self.start_mirror(serial);
                    self.tab = Tab::Screen;
                }
                if self.devices != devices {
                    self.devices = devices;
                    cx.notify();
                }
                if fetch_now {
                    self.refresh_info(cx);
                }
            }
            Err(err) => {
                self.device_error = Some(format!("{err:#}"));
                cx.notify();
            }
        }
    }

    fn pull_frame(&mut self, cx: &mut Context<Self>) {
        self.try_start_recording(cx);
        if self.is_recording() && self.last_tick.elapsed() >= Duration::from_millis(500) {
            // Keep the recording timer moving even when the screen is static.
            self.last_tick = Instant::now();
            cx.notify();
        }
        let Some(mirror) = &self.mirror else { return };
        if let Some(frame) = mirror.take_frame() {
            let image = Arc::new(screen::to_render_image(frame));
            // Free the previous frame's GPU texture (in every window), otherwise each frame leaks one.
            if let Some(old) = self.screen.replace(image) {
                cx.drop_image(old, None);
            }
            cx.notify();
        } else if !matches!(mirror.status(), MirrorStatus::Streaming { .. } | MirrorStatus::Starting) {
            // The session ended (e.g. phone unplugged): report any recording it finished.
            self.stop_recording(cx);
            // Repaint once so a failure/stop message shows up.
            cx.notify();
        }
    }

    // ---- actions ----------------------------------------------------------------------

    fn select_device(&mut self, serial: String, cx: &mut Context<Self>) {
        if self.selected.as_deref() != Some(serial.as_str()) {
            self.stop_mirror(cx);
            self.selected = Some(serial);
            self.info = None;
            self.info_error = None;
            self.refresh_info(cx);
            cx.notify();
        }
    }

    fn toggle_mirror(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.mirror.is_some() {
            self.stop_mirror(cx);
        } else if let Some(serial) = self.selected.clone() {
            self.start_mirror(serial);
            self.tab = Tab::Screen;
        }
        cx.notify();
    }

    fn start_mirror(&mut self, serial: String) {
        // GPUI textures are BGRA, so ask core to convert while decoding.
        let config = MirrorConfig { pixel_format: PixelFormat::Bgra8, ..Default::default() };
        self.mirror = Some(Mirror::start(serial, config));
    }

    fn stop_mirror(&mut self, cx: &mut Context<Self>) {
        self.record_when_ready = false;
        self.stop_recording(cx);
        self.pop_in(cx);
        if let Some(mirror) = self.mirror.take() {
            // Stopping joins the decode thread; do it off the UI thread.
            cx.background_executor().spawn(async move { drop(mirror) }).detach();
        }
        if let Some(old) = self.screen.take() {
            cx.drop_image(old, None);
        }
    }

    /// Name used in screenshot/recording file names.
    fn device_label(&self) -> String {
        let serial = self.selected.clone().unwrap_or_default();
        self.devices
            .iter()
            .find(|d| d.serial == serial)
            .and_then(|d| d.model.clone())
            .unwrap_or(serial)
    }

    /// Save a full-resolution screenshot on this computer (works without mirroring).
    fn take_screenshot(&mut self, cx: &mut Context<Self>) {
        let Some(serial) = self.selected.clone() else { return };
        if self.screenshot_in_flight {
            return;
        }
        self.screenshot_in_flight = true;
        self.media_notice = Some(MediaNotice::Busy("Taking screenshot…"));
        let name = self.device_label();
        cx.spawn(async move |this, cx| {
            let result = cx.background_executor().spawn(async move { media::save_screenshot(&serial, &name) }).await;
            this.update(cx, |this, cx| {
                this.screenshot_in_flight = false;
                this.media_notice = Some(match result {
                    Ok(path) => MediaNotice::Saved { what: "Screenshot", path },
                    Err(err) => MediaNotice::Failed(format!("Screenshot failed: {err:#}")),
                });
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn is_recording(&self) -> bool {
        self.mirror.as_ref().is_some_and(|m| m.recording_status().is_some())
    }

    fn toggle_recording(&mut self, cx: &mut Context<Self>) {
        if self.is_recording() {
            self.stop_recording(cx);
            return;
        }
        if self.record_when_ready {
            // Pressed again while waiting for the mirror: cancel.
            self.record_when_ready = false;
            cx.notify();
            return;
        }
        let Some(serial) = self.selected.clone() else { return };
        if self.mirror.is_none() {
            self.start_mirror(serial);
            self.tab = Tab::Screen;
        }
        self.record_when_ready = true;
        self.media_notice = None;
        self.try_start_recording(cx);
        cx.notify();
    }

    fn try_start_recording(&mut self, cx: &mut Context<Self>) {
        if !self.record_when_ready {
            return;
        }
        let Some(mirror) = &self.mirror else {
            self.record_when_ready = false;
            return;
        };
        match mirror.status() {
            MirrorStatus::Starting => {}
            MirrorStatus::Streaming { .. } => {
                self.record_when_ready = false;
                let path = media::default_recording_path(&self.device_label());
                if let Err(err) = mirror.start_recording(path) {
                    self.media_notice = Some(MediaNotice::Failed(format!("Couldn't start recording: {err:#}")));
                }
                self.last_tick = Instant::now();
                cx.notify();
            }
            MirrorStatus::Stopped | MirrorStatus::Failed(_) => self.record_when_ready = false,
        }
    }

    /// Stop recording (if any) and finish the file.
    fn stop_recording(&mut self, cx: &mut Context<Self>) {
        let Some(result) = self.mirror.as_ref().and_then(|m| m.stop_recording()) else { return };
        self.media_notice = Some(match result {
            Ok(files) if files.is_empty() => MediaNotice::Failed("Nothing was recorded: no video arrived".into()),
            Ok(files) => MediaNotice::Saved {
                what: if files.len() > 1 { "Recordings" } else { "Recording" },
                path: files[0].clone(),
            },
            Err(err) => MediaNotice::Failed(format!("{err:#}")),
        });
        cx.notify();
    }

    fn render_media(&self, cx: &mut Context<Self>) -> AnyElement {
        let has_device = self.selected.as_ref().is_some_and(|s| self.devices.iter().any(|d| &d.serial == s && d.is_ready()));
        let recording = self.mirror.as_ref().and_then(|m| m.recording_status());
        let record_label: SharedString = match (&recording, self.record_when_ready) {
            (Some(status), _) => {
                let secs = status.duration.as_secs();
                format!("Stop  {}:{:02}", secs / 60, secs % 60).into()
            }
            (None, true) => "Starting…".into(),
            (None, false) => "Record".into(),
        };
        let record_color = if recording.is_some() || self.record_when_ready { RECORD } else { PANEL_HOVER };

        let status_line: Option<AnyElement> = if let Some(status) = &recording {
            let file = status.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let text = match (&status.error, status.waiting_for_key_frame) {
                (Some(err), _) => format!("Recording error: {err}"),
                (None, true) => "Waiting for the first frame…".into(),
                (None, false) => format!("● REC  {file}"),
            };
            let color = if status.error.is_some() { ERR } else { RECORD };
            Some(div().text_xs().text_color(rgb(color)).truncate().child(text).into_any_element())
        } else {
            match &self.media_notice {
                None => None,
                Some(MediaNotice::Busy(msg)) => Some(div().text_xs().text_color(rgb(MUTED)).child(*msg).into_any_element()),
                Some(MediaNotice::Failed(msg)) => {
                    Some(div().text_xs().text_color(rgb(ERR)).child(msg.clone()).into_any_element())
                }
                Some(MediaNotice::Saved { what, path }) => {
                    let file = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    let reveal = path.clone();
                    Some(
                        div()
                            .flex()
                            .flex_col()
                            .gap_0p5()
                            .text_xs()
                            .child(div().text_color(rgb(OK)).child(format!("{what} saved")))
                            .child(div().text_color(rgb(MUTED)).truncate().child(file))
                            .child(
                                div()
                                    .id("reveal-media")
                                    .text_color(rgb(ACCENT))
                                    .cursor_pointer()
                                    .hover(|s| s.underline())
                                    .on_click(move |_, _, cx| cx.reveal_path(&reveal))
                                    .child("Show in folder"),
                            )
                            .into_any_element(),
                    )
                }
            }
        };

        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(div().flex_1().child(button(
                        "screenshot",
                        if self.screenshot_in_flight { "Capturing…" } else { "Screenshot" },
                        has_device && !self.screenshot_in_flight,
                        true,
                        cx.listener(|this, _, _, cx| this.take_screenshot(cx)),
                    )))
                    .child(div().flex_1().child(colored_button(
                        "record",
                        record_label,
                        has_device,
                        record_color,
                        cx.listener(|this, _, _, cx| this.toggle_recording(cx)),
                    ))),
            )
            .children(status_line)
            .into_any_element()
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
        let media = self.render_media(cx);

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
                    ))
                    .child(media),
            )
            .child(section_label("Apps"))
            .child(button("install", "Install APK…", has_device, false, cx.listener(Self::pick_apks)))
            .child(div().text_xs().text_color(rgb(MUTED)).child("…or drop .apk files anywhere on the window"))
            .child(div().flex().flex_col().gap_1().children(installs))
    }

    fn render_main_pane(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let tab = |id: &'static str, label: &'static str, which: Tab, cx: &mut Context<Self>| {
            let active = self.tab == which;
            div()
                .id(id)
                .px_4()
                .py_2()
                .text_sm()
                .border_b_2()
                .border_color(rgb(if active { ACCENT } else { BG }))
                .text_color(rgb(if active { TEXT } else { MUTED }))
                .cursor_pointer()
                .hover(|s| s.text_color(rgb(TEXT)))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.tab = which;
                    cx.notify();
                }))
                .child(label)
        };
        let tabs = div()
            .flex()
            .flex_none()
            .px_4()
            .border_b_1()
            .border_color(rgb(BORDER))
            .child(tab("tab-dashboard", "Dashboard", Tab::Dashboard, cx))
            .child(tab("tab-screen", "Screen", Tab::Screen, cx));

        let content = match self.tab {
            Tab::Dashboard => dashboard::render(self.info.as_ref(), self.info_error.as_deref(), self.selected.is_some()),
            Tab::Screen => self.render_screen_tab(cx),
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(tabs)
            .child(div().flex_1().min_h_0().overflow_hidden().child(content))
            .into_any_element()
    }

    fn render_screen_tab(&mut self, cx: &mut Context<Self>) -> AnyElement {
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
