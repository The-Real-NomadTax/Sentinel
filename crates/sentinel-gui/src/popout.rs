//! A separate window that shows only the mirrored screen.
//!
//! It owns no mirroring state: it reads frames from the main [`SentinelApp`] and re-renders
//! whenever the app does. The app tracks the window and closes it when popping back in.

use gpui::{
    Context, Entity, IntoElement, ParentElement, Render, Styled, Subscription, Window, div, px,
    rgb,
};

use crate::{
    app::SentinelApp,
    screen,
    theme::{BG, BORDER, MUTED, SIDEBAR, TEXT, button},
};

pub struct PopoutWindow {
    app: Entity<SentinelApp>,
    _observe_app: Subscription,
}

impl PopoutWindow {
    pub fn new(app: Entity<SentinelApp>, cx: &mut Context<Self>) -> Self {
        // Every new frame notifies the app entity; redraw this window when that happens.
        let observe = cx.observe(&app, |_, _, cx| cx.notify());
        Self { app, _observe_app: observe }
    }
}

impl Render for PopoutWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.app.read(cx).screen_state();
        let title = state.device_name().unwrap_or("Screen").to_string();

        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(BG))
            .text_color(rgb(TEXT))
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .bg(rgb(SIDEBAR))
                    .border_b_1()
                    .border_color(rgb(BORDER))
                    .child(div().text_sm().text_color(rgb(MUTED)).truncate().child(title))
                    .child(div().w(px(130.)).child(button(
                        "pop-in",
                        "Pop back in",
                        true,
                        true,
                        cx.listener(|this, _, window, cx| {
                            window.remove_window();
                            this.app.update(cx, |app, cx| app.popout_closed(cx));
                        }),
                    ))),
            )
            .child(div().flex_1().min_h_0().overflow_hidden().child(screen::render(&state)))
    }
}
