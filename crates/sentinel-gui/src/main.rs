//! GPUI frontend for Sentinel. All device logic lives in `sentinel-core`; this crate only
//! renders state and forwards user actions, so it can be replaced without touching the backend.

mod app;
mod dashboard;
mod input;
mod popout;
mod screen;
mod theme;

use gpui::{AnyWindowHandle, App, Application, KeyBinding, actions};

actions!(sentinel, [Quit]);

fn main() {
    Application::new().run(|cx: &mut App| {
        // Ctrl+Q (Cmd+Q on macOS) quits from any window.
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.bind_keys([KeyBinding::new("secondary-q", Quit, None)]);
        let main_window: AnyWindowHandle = app::open_main_window(cx).into();
        // Closing the main window quits, even if the pop-out screen window is still open.
        cx.on_window_closed(move |cx| {
            if !cx.windows().contains(&main_window) {
                // Quit from a separate task: quitting inside the platform's window-close
                // handler panics on X11 (gpui re-borrows its client state).
                cx.spawn(async move |cx| cx.update(|cx| cx.quit())).detach();
            }
        })
        .detach();
        cx.activate(true);
    });
}
