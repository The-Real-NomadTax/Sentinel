//! GPUI frontend for Sentinel. All device logic lives in `sentinel-core`; this crate only
//! renders state and forwards user actions, so it can be replaced without touching the backend.

mod app;

use gpui::{App, AppContext, Application, Bounds, TitlebarOptions, WindowBounds, WindowOptions, px, size};

fn main() {
    Application::new().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(1100.), px(760.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions { title: Some("Sentinel".into()), ..Default::default() }),
                app_id: Some("sentinel".into()),
                ..Default::default()
            },
            |_, cx| cx.new(app::SentinelApp::new),
        )
        .expect("failed to open window");
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        cx.activate(true);
    });
}
