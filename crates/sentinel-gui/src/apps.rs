//! The Apps tab: installed apps, searchable, with an Export button that saves each app's APK
//! on this computer.

use std::{collections::HashMap, path::PathBuf, rc::Rc};

use gpui::{
    AnyElement, ClipboardItem, Context, FocusHandle, IntoElement, KeyDownEvent, ParentElement, Render, SharedString,
    Styled, UniformListScrollHandle, Window, div, prelude::*, px, rgb, uniform_list,
};
use sentinel_core::{AppInfo, apps};

use crate::theme::*;

const ROW_HEIGHT: f32 = 52.;

enum Load {
    Idle,
    Loading,
    Failed(String),
}

enum Export {
    Running,
    Saved(PathBuf),
    /// The message is in the footer notice.
    Failed,
}

/// Result of the most recent export, shown in the footer.
struct Notice {
    label: String,
    result: Result<PathBuf, String>,
}

pub struct AppsPanel {
    serial: Option<String>,
    /// The device `apps` was loaded from.
    loaded_for: Option<String>,
    apps: Rc<Vec<AppInfo>>,
    load: Load,
    query: String,
    pub search_focus: FocusHandle,
    show_system: bool,
    /// Indices into `apps` that match the search and filter.
    visible: Rc<Vec<usize>>,
    exports: HashMap<String, Export>,
    notice: Option<Notice>,
    scroll: UniformListScrollHandle,
}

impl AppsPanel {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            serial: None,
            loaded_for: None,
            apps: Rc::new(Vec::new()),
            load: Load::Idle,
            query: String::new(),
            search_focus: cx.focus_handle(),
            show_system: false,
            visible: Rc::new(Vec::new()),
            exports: HashMap::new(),
            notice: None,
            scroll: UniformListScrollHandle::new(),
        }
    }

    /// The selected device changed (or went away).
    pub fn set_device(&mut self, serial: Option<String>, cx: &mut Context<Self>) {
        if self.serial == serial {
            return;
        }
        self.serial = serial;
        self.loaded_for = None;
        self.apps = Rc::new(Vec::new());
        self.visible = Rc::new(Vec::new());
        self.exports.clear();
        self.notice = None;
        self.load = Load::Idle;
        cx.notify();
    }

    /// Load the list if it hasn't been loaded for the current device yet.
    pub fn ensure_loaded(&mut self, cx: &mut Context<Self>) {
        if self.loaded_for != self.serial && matches!(self.load, Load::Idle) {
            self.reload(cx);
        }
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        let Some(serial) = self.serial.clone() else {
            return;
        };
        if matches!(self.load, Load::Loading) {
            return;
        }
        self.load = Load::Loading;
        cx.spawn(async move |this, cx| {
            let fetch = serial.clone();
            let result = cx.background_executor().spawn(async move { apps::list_apps(&fetch) }).await;
            this.update(cx, |this, cx| {
                if this.serial.as_deref() != Some(serial.as_str()) {
                    return; // device changed meanwhile
                }
                this.loaded_for = Some(serial);
                match result {
                    Ok(list) => {
                        this.apps = Rc::new(list);
                        this.load = Load::Idle;
                    }
                    Err(err) => this.load = Load::Failed(format!("{err:#}")),
                }
                this.refilter();
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn refilter(&mut self) {
        let query = self.query.trim().to_lowercase();
        let visible = self
            .apps
            .iter()
            .enumerate()
            .filter(|(_, a)| self.show_system || !a.system)
            .filter(|(_, a)| {
                query.is_empty() || a.label.to_lowercase().contains(&query) || a.package.to_lowercase().contains(&query)
            })
            .map(|(ix, _)| ix)
            .collect();
        self.visible = Rc::new(visible);
    }

    fn export(&mut self, ix: usize, cx: &mut Context<Self>) {
        let (Some(serial), Some(app)) = (self.serial.clone(), self.apps.get(ix).cloned()) else {
            return;
        };
        if matches!(self.exports.get(&app.package), Some(Export::Running)) {
            return;
        }
        self.exports.insert(app.package.clone(), Export::Running);
        cx.spawn(async move |this, cx| {
            let job = app.clone();
            let result = cx
                .background_executor()
                .spawn(async move { apps::export_app(&serial, &job, &apps::exports_dir()) })
                .await;
            this.update(cx, |this, cx| {
                let result = result.map_err(|e| format!("{e:#}"));
                let state = match &result {
                    Ok(path) => Export::Saved(path.clone()),
                    Err(_) => Export::Failed,
                };
                this.exports.insert(app.package.clone(), state);
                this.notice = Some(Notice { label: app.label.clone(), result });
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn on_search_key(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let ks = &event.keystroke;
        let m = &ks.modifiers;
        let changed = match ks.key.as_str() {
            "backspace" if m.control || m.alt => {
                // Delete the last word.
                let trimmed = self.query.trim_end().len();
                let cut = self.query[..trimmed].rfind(char::is_whitespace).map_or(0, |i| i + 1);
                self.query.truncate(cut);
                true
            }
            "backspace" => self.query.pop().is_some(),
            "escape" => !std::mem::take(&mut self.query).is_empty(),
            "v" if m.control || m.platform => {
                let text = cx.read_from_clipboard().and_then(|c| c.text()).unwrap_or_default();
                let text: String = text.chars().filter(|c| !c.is_control()).collect();
                self.query.push_str(&text);
                !text.is_empty()
            }
            "c" if m.control || m.platform => {
                cx.write_to_clipboard(ClipboardItem::new_string(self.query.clone()));
                false
            }
            _ if m.control || m.platform || m.alt => return, // leave shortcuts (e.g. Ctrl+Q) alone
            _ => match &ks.key_char {
                Some(text) if !text.is_empty() && !text.chars().any(char::is_control) => {
                    self.query.push_str(text);
                    true
                }
                _ if ks.key == "space" => {
                    self.query.push(' ');
                    true
                }
                _ => return,
            },
        };
        cx.stop_propagation();
        if changed {
            self.refilter();
            self.scroll.scroll_to_item(0, gpui::ScrollStrategy::Top);
            cx.notify();
        }
    }

    // ---- rendering --------------------------------------------------------------------

    fn render_toolbar(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.search_focus.is_focused(window);
        let search_text: AnyElement = if self.query.is_empty() {
            div()
                .flex()
                .items_center()
                .children(focused.then(caret))
                .child(div().text_color(rgb(MUTED)).child("Search apps"))
                .into_any_element()
        } else {
            div().flex().items_center().child(self.query.clone()).children(focused.then(caret)).into_any_element()
        };
        let search = div()
            .id("app-search")
            .track_focus(&self.search_focus)
            .on_key_down(cx.listener(Self::on_search_key))
            .on_click(cx.listener(|this, _, window, _| window.focus(&this.search_focus)))
            .flex_1()
            .min_w(px(160.))
            .h(px(34.))
            .px_3()
            .flex()
            .items_center()
            .rounded_md()
            .border_1()
            .border_color(rgb(if focused { ACCENT } else { BORDER }))
            .bg(rgb(PANEL))
            .text_sm()
            .cursor_text()
            .overflow_hidden()
            .child(search_text);

        let system_toggle = div()
            .id("show-system")
            .flex()
            .items_center()
            .gap_2()
            .text_sm()
            .text_color(rgb(MUTED))
            .cursor_pointer()
            .hover(|s| s.text_color(rgb(TEXT)))
            .on_click(cx.listener(|this, _, _, cx| {
                this.show_system = !this.show_system;
                this.refilter();
                cx.notify();
            }))
            .child(
                div()
                    .size(px(16.))
                    .rounded_sm()
                    .border_1()
                    .border_color(rgb(if self.show_system { ACCENT } else { BORDER }))
                    .bg(rgb(if self.show_system { ACCENT } else { PANEL }))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_xs()
                    .text_color(rgb(TEXT))
                    .when(self.show_system, |el| el.child("✓")),
            )
            .child("System apps");

        let loading = matches!(self.load, Load::Loading);
        div().flex().flex_none().items_center().gap_4().px_6().pt_5().pb_3().child(search).child(system_toggle).child(
            div().w(px(110.)).child(button(
                "reload-apps",
                if loading { "Loading…" } else { "Refresh" },
                self.serial.is_some() && !loading,
                true,
                cx.listener(|this, _, _, cx| this.reload(cx)),
            )),
        )
    }

    fn render_row(&self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let app = &self.apps[ix];
        let mut subtitle = app.package.clone();
        if let Some(version) = &app.version_name {
            subtitle.push_str(&format!("  ·  {version}"));
        }
        let badges = [
            app.system.then_some(("System", MUTED)),
            (!app.enabled).then_some(("Disabled", WARN)),
            app.is_split().then_some(("Split APK", ACCENT)),
        ]
        .into_iter()
        .flatten()
        .map(|(text, color)| {
            div().px_1p5().rounded_sm().border_1().border_color(rgb(color)).text_xs().text_color(rgb(color)).child(text)
        });

        let action: AnyElement = match self.exports.get(&app.package) {
            Some(Export::Running) => button(("export", ix), "Exporting…", false, true, |_, _, _| {}).into_any_element(),
            Some(Export::Saved(path)) => {
                let reveal = path.clone();
                div()
                    .id(("exported", ix))
                    .flex()
                    .justify_center()
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .text_sm()
                    .text_color(rgb(OK))
                    .cursor_pointer()
                    .hover(|s| s.bg(rgb(PANEL_HOVER)))
                    .on_click(move |_, _, cx| cx.reveal_path(&reveal))
                    .child(if app.is_split() { "✓ Show folder" } else { "✓ Show file" })
                    .into_any_element()
            }
            Some(Export::Failed) | None => {
                let failed = matches!(self.exports.get(&app.package), Some(Export::Failed));
                colored_button(
                    ("export", ix),
                    if failed { "Retry export" } else { "Export APK" },
                    true,
                    if failed { RECORD } else { PANEL_HOVER },
                    cx.listener(move |this, _, _, cx| this.export(ix, cx)),
                )
                .into_any_element()
            }
        };

        div()
            .id(("app", ix))
            .w_full()
            .h(px(ROW_HEIGHT))
            .flex()
            .items_center()
            .gap_4()
            .px_6()
            .border_b_1()
            .border_color(rgb(BORDER))
            .hover(|s| s.bg(rgb(PANEL)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(div().text_sm().truncate().child(app.label.clone()))
                            .children(badges),
                    )
                    .child(div().text_xs().text_color(rgb(MUTED)).truncate().child(subtitle)),
            )
            .child(
                div()
                    .w(px(80.))
                    .flex_none()
                    .flex()
                    .justify_end()
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child(if app.size > 0 { human_size(app.size) } else { String::new() }),
            )
            .child(div().w(px(130.)).flex_none().child(action))
            .into_any_element()
    }

    fn render_footer(&self) -> impl IntoElement {
        let dir = apps::exports_dir();
        let open_dir = dir.clone();
        let mut footer = div()
            .flex()
            .flex_none()
            .items_center()
            .gap_3()
            .px_6()
            .py_3()
            .border_t_1()
            .border_color(rgb(BORDER))
            .text_xs()
            .text_color(rgb(MUTED));
        footer = match &self.notice {
            Some(Notice { label, result: Ok(path) }) => {
                let file = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                footer
                    .child(div().text_color(rgb(OK)).flex_none().child(format!("Exported {label}")))
                    .child(div().flex_1().min_w_0().truncate().child(file))
            }
            Some(Notice { label, result: Err(err) }) => footer.child(
                div()
                    .text_color(rgb(ERR))
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(format!("Couldn't export {label}: {err}")),
            ),
            None => {
                footer.child(div().flex_1().min_w_0().truncate().child(format!("APKs are saved to {}", dir.display())))
            }
        };
        footer.child(
            div()
                .id("open-exports")
                .flex_none()
                .text_color(rgb(ACCENT))
                .cursor_pointer()
                .hover(|s| s.underline())
                .on_click(move |_, _, cx| {
                    // Show the folder even before the first export.
                    let _ = std::fs::create_dir_all(&open_dir);
                    cx.open_with_system(&open_dir);
                })
                .child("Open folder"),
        )
    }
}

impl Render for AppsPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.ensure_loaded(cx);
        let center = || div().flex_1().flex().flex_col().gap_2().items_center().justify_center().text_color(rgb(MUTED));

        let body: AnyElement = if self.serial.is_none() {
            center().child("Connect a device to see its apps").into_any_element()
        } else if let Load::Failed(err) = &self.load {
            center()
                .p_8()
                .child(div().text_color(rgb(ERR)).child("Couldn't list apps"))
                .child(div().text_xs().child(err.clone()))
                .into_any_element()
        } else if self.loaded_for.is_none() {
            center().child("Loading apps…").into_any_element()
        } else if self.visible.is_empty() {
            let msg = if self.query.is_empty() { "No apps to show" } else { "No apps match your search" };
            center().child(msg).into_any_element()
        } else {
            let visible = self.visible.clone();
            uniform_list(
                "apps",
                visible.len(),
                cx.processor(move |this, range: std::ops::Range<usize>, _window, cx| {
                    range.map(|i| this.render_row(visible[i], cx)).collect::<Vec<_>>()
                }),
            )
            .track_scroll(self.scroll.clone())
            .w_full()
            .flex_1()
            .into_any_element()
        };

        let user = self.apps.iter().filter(|a| !a.system).count();
        let count: SharedString = if self.loaded_for.is_some() && self.serial.is_some() {
            let shown = self.visible.len();
            if self.show_system {
                format!("{shown} of {} apps ({user} installed by you)", self.apps.len()).into()
            } else {
                format!("{shown} of {user} apps installed by you").into()
            }
        } else {
            "".into()
        };

        div()
            .size_full()
            .flex()
            .flex_col()
            .child(self.render_toolbar(window, cx))
            .child(div().flex_none().px_6().pb_2().text_xs().text_color(rgb(MUTED)).child(count))
            .child(div().flex_none().border_b_1().border_color(rgb(BORDER)))
            .child(body)
            .child(self.render_footer())
    }
}

fn caret() -> impl IntoElement {
    div().w(px(1.)).h(px(16.)).bg(rgb(ACCENT))
}

/// Sizes the way phones show them (decimal units).
fn human_size(bytes: u64) -> String {
    match bytes {
        b if b >= 1_000_000_000 => format!("{:.1} GB", b as f64 / 1e9),
        b if b >= 1_000_000 => format!("{:.1} MB", b as f64 / 1e6),
        b => format!("{} KB", b.div_ceil(1000)),
    }
}
