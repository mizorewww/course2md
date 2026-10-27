//! Caption buttons for the title bar the app draws itself.
//!
//! The window has no system title bar, so every page draws its own (see
//! `views::shell_topbar`). macOS keeps its native traffic lights in the bar's
//! leading edge. Windows has nothing of its own left, and Linux under
//! client-side decorations neither, so the app draws minimize, maximize or
//! restore, and close at the trailing edge, once for the whole window.
//!
//! They paint and hit-test above everything else, dialogs included: dialogs
//! are deferred layers (priority 10 and up) whose backdrop is itself a drag
//! area, so the buttons are a deferred layer of a higher priority and
//! occlude what lies beneath them, the way the system's own buttons stay
//! usable over an app's modal content.
//!
//! gpui-component's `TitleBar` lays out its own window controls after its
//! children. Each page's bar content takes the full window width, so those
//! sit past the window's right edge, where the window's content mask leaves
//! their hitboxes empty: they are never drawn or hit, and these are the
//! only caption buttons.
//!
//! On Windows the buttons are window control areas: GPUI answers the
//! system's non-client hit test with `HTMINBUTTON` / `HTMAXBUTTON` /
//! `HTCLOSE` for them, so Windows itself minimizes, maximizes (with the snap
//! layouts flyout over maximize) and closes. Everything else in the bar is
//! a drag area answered with `HTCAPTION`; see [`title_bar_control`] for what
//! that means for the bar's own buttons.
use crate::Desktop;
use crate::theme::*;
use gpui::{
    AnyElement, Context, Decorations, InteractiveElement as _, IntoElement, MouseButton,
    ParentElement as _, Pixels, StatefulInteractiveElement as _, Styled, Window, WindowControlArea,
    div, px,
};

/// Which caption buttons the window needs from the app.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Caption {
    /// The system draws them: macOS traffic lights, or a Linux window
    /// manager's server-side title bar.
    System,
    /// Windows: 46×32px buttons along the top of the bar's trailing edge.
    Windows,
    /// Linux under client-side decorations: round buttons, as GNOME and KDE
    /// draw them.
    Linux,
}

/// The caption buttons this window needs.
pub(crate) fn caption(window: &Window) -> Caption {
    #[cfg(feature = "capture")]
    if let Some(caption) = crate::capture::caption_override() {
        return caption;
    }
    if cfg!(target_os = "windows") {
        Caption::Windows
    } else if cfg!(any(target_os = "linux", target_os = "freebsd"))
        && matches!(window.window_decorations(), Decorations::Client { .. })
    {
        Caption::Linux
    } else {
        Caption::System
    }
}

/// Whether macOS draws its traffic lights inside the title bar's leading
/// edge.
pub(crate) fn traffic_lights() -> bool {
    #[cfg(feature = "capture")]
    if crate::capture::caption_override().is_some() {
        return false;
    }
    cfg!(target_os = "macos")
}

/// Windows caption buttons are 46×32px at 100% display scaling, as Windows
/// 11 draws them on a standard caption, top-aligned whatever the bar's
/// height. Logical pixels follow the display scaling; the app's text scale
/// does not change them.
const WINDOWS_BUTTON_WIDTH: f32 = 46.;
const WINDOWS_BUTTON_HEIGHT: f32 = 32.;
/// Above every dialog layer (gpui-component paints those at 10 + layer).
const CAPTION_PRIORITY: usize = 1_000;
/// A Linux caption button's target, the round shape inside it, and the gap
/// between two of them and after the last.
const LINUX_BUTTON: f32 = 32.;
const LINUX_SHAPE: f32 = 24.;
const LINUX_GAP: f32 = 4.;
const LINUX_END: f32 = 12.;
/// The glyphs are drawn on a 10px grid.
const GLYPH: f32 = 10.;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Button {
    Minimize,
    Maximize,
    Restore,
    Close,
}

impl Button {
    fn id(self) -> &'static str {
        match self {
            Self::Minimize => "caption-minimize",
            Self::Maximize => "caption-maximize",
            Self::Restore => "caption-restore",
            Self::Close => "caption-close",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Minimize => course2md::tr!("最小化"),
            Self::Maximize => course2md::tr!("最大化"),
            Self::Restore => course2md::tr!("还原"),
            Self::Close => course2md::tr!("关闭"),
        }
    }

    fn glyph(self) -> gpui_component::Icon {
        match self {
            Self::Minimize => crate::icons::caption_minimize(),
            Self::Maximize => crate::icons::caption_maximize(),
            Self::Restore => crate::icons::caption_restore(),
            Self::Close => crate::icons::caption_close(),
        }
    }

    fn area(self) -> WindowControlArea {
        match self {
            Self::Minimize => WindowControlArea::Min,
            Self::Maximize | Self::Restore => WindowControlArea::Max,
            Self::Close => WindowControlArea::Close,
        }
    }
}

/// The buttons in order, as far as the window and its window manager allow.
fn buttons(caption: Caption, window: &Window) -> Vec<Button> {
    if caption == Caption::System || window.is_fullscreen() {
        return Vec::new();
    }
    let (minimize, maximize) = match caption {
        // The window can always be minimized and resized; Windows itself
        // acts on these buttons.
        Caption::Windows => (true, true),
        _ => {
            let supported = window.window_controls();
            (supported.minimize, supported.maximize)
        }
    };
    let zoom = if window.is_maximized() {
        Button::Restore
    } else {
        Button::Maximize
    };
    [
        minimize.then_some(Button::Minimize),
        maximize.then_some(zoom),
        Some(Button::Close),
    ]
    .into_iter()
    .flatten()
    .collect()
}

fn width_of(caption: Caption, count: usize) -> f32 {
    match (caption, count) {
        (_, 0) | (Caption::System, _) => 0.,
        (Caption::Windows, count) => WINDOWS_BUTTON_WIDTH * count as f32,
        (Caption::Linux, count) => {
            LINUX_BUTTON * count as f32 + LINUX_GAP * (count - 1) as f32 + LINUX_END
        }
    }
}

/// The width the caption buttons take from the title bar's trailing edge.
pub(crate) fn caption_width(window: &Window) -> f32 {
    let caption = caption(window);
    width_of(caption, buttons(caption, window).len())
}

/// Where the title bar's own content ends: before the caption buttons with a
/// little open space, or the bar's 16px inset where the system draws them.
pub(crate) fn title_bar_trailing(window: &Window) -> f32 {
    trailing_for(caption_width(window))
}

fn trailing_for(caption_width: f32) -> f32 {
    if caption_width > 0. {
        caption_width + 8.
    } else {
        16.
    }
}

/// Makes a control inside the title bar take the press itself. The whole bar
/// is a drag area: on Windows a press there arrives as a caption press, and
/// one that no element stops goes on to the system, which starts its
/// move-window loop (and maximizes on a double click). Stopping it keeps
/// the press, its release and so the click with the control, as Zed's title
/// bar does for its controls; empty bar space still moves the window. On
/// every platform it also keeps a drag that starts on a control from moving
/// the window, and a double click on it from maximizing.
pub(crate) fn title_bar_control<E: gpui::InteractiveElement>(element: E) -> E {
    element.on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
}

impl Desktop {
    /// The caption buttons over the trailing end of a `height` tall title
    /// bar, above any dialog, or nothing where the system draws them.
    pub(crate) fn caption_buttons(
        &self,
        height: Pixels,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let caption = caption(window);
        let buttons = buttons(caption, window);
        if buttons.is_empty() {
            return None;
        }
        let active = window.is_window_active();
        let glyph_color = color(if active { INK } else { GRAY });
        let row = gpui_component::h_flex()
            .id("window-caption")
            .text_color(glyph_color);
        let row = match caption {
            Caption::Windows => row
                .h(px(WINDOWS_BUTTON_HEIGHT).min(height))
                .children(buttons.into_iter().map(windows_button)),
            _ => row
                .h(height)
                .items_center()
                .pr(px(LINUX_END))
                .gap(px(LINUX_GAP))
                .children(buttons.into_iter().map(|button| linux_button(button, cx))),
        };
        Some(caption_layer(row, window))
    }
}

/// The buttons' row at the window's top-right corner, painted and hit-tested
/// after every dialog layer.
fn caption_layer(row: impl IntoElement, window: &Window) -> AnyElement {
    let trailing = gpui::point(window.viewport_size().width, px(0.));
    gpui::deferred(
        gpui::anchored()
            .anchor(gpui::Anchor::TopRight)
            .position(trailing)
            .child(row),
    )
    .with_priority(CAPTION_PRIORITY)
    .into_any_element()
}

/// A Windows caption button: a window control area, so the system acts on
/// it. It occludes the drag area behind it, or the hit test would find the
/// drag area too.
fn windows_button(button: Button) -> gpui::Stateful<gpui::Div> {
    let close = button == Button::Close;
    let (hover, pressed) = if close {
        (caption_close_hover(), caption_close_pressed())
    } else {
        (caption_hover(), caption_pressed())
    };
    div()
        .id(button.id())
        .role(gpui::accesskit::Role::Button)
        .aria_label(button.label())
        .occlude()
        .window_control_area(button.area())
        .flex()
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .w(px(WINDOWS_BUTTON_WIDTH))
        .h_full()
        .hover(move |style| {
            let style = style.bg(hover);
            if close {
                style.text_color(on_caption_close())
            } else {
                style
            }
        })
        .active(move |style| {
            let style = style.bg(pressed);
            if close {
                style.text_color(on_caption_close())
            } else {
                style
            }
        })
        .child(button.glyph().size(px(GLYPH)))
}

/// A Linux caption button: GPUI does not hit-test window controls there, so
/// the button acts itself. Close goes through the same exit as the window
/// manager's close, which saves first.
fn linux_button(button: Button, cx: &mut Context<Desktop>) -> gpui::Stateful<gpui::Div> {
    // As libadwaita draws them: every button, close included, takes the
    // same neutral hover and press.
    let group = button.id();
    let (hover, pressed) = (
        blend(color(SURFACE_HIGH), color(INK), 0.08),
        blend(color(SURFACE_HIGH), color(INK), 0.10),
    );
    div()
        .id(button.id())
        .group(group)
        .role(gpui::accesskit::Role::Button)
        .aria_label(button.label())
        .occlude()
        .flex()
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .size(px(LINUX_BUTTON))
        .child(
            div()
                .id("shape")
                .flex()
                .items_center()
                .justify_center()
                .size(px(LINUX_SHAPE))
                .rounded_full()
                .bg(color(SURFACE_HIGH))
                .group_hover(group, move |style| style.bg(hover))
                .group_active(group, move |style| style.bg(pressed))
                .child(button.glyph().size(px(GLYPH))),
        )
        .on_mouse_down(MouseButton::Left, |_, window, cx| {
            window.prevent_default();
            cx.stop_propagation();
        })
        .on_click(cx.listener(move |this, _, window, cx| {
            cx.stop_propagation();
            match button {
                Button::Minimize => window.minimize_window(),
                Button::Maximize | Button::Restore => window.zoom_window(),
                Button::Close => {
                    if this.request_close(cx) {
                        window.remove_window();
                    }
                }
            }
        }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Modifiers, Render, TestAppContext};

    /// A modal dialog as gpui-component lays it out: a deferred layer at
    /// priority 10 whose full-window backdrop takes every press.
    struct DialogHarness {
        backdrop_presses: usize,
    }

    impl Render for DialogHarness {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let backdrop = div()
                .id("backdrop")
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .occlude()
                .window_control_area(WindowControlArea::Drag)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, _, _| this.backdrop_presses += 1),
                );
            let row = gpui_component::h_flex()
                .h(px(WINDOWS_BUTTON_HEIGHT))
                .children([Button::Minimize, Button::Maximize, Button::Close].map(windows_button));
            div()
                .size_full()
                .child(gpui::deferred(backdrop).with_priority(10))
                .child(caption_layer(row, window))
        }
    }

    #[gpui::test]
    fn caption_buttons_take_presses_over_a_dialog_backdrop(cx: &mut TestAppContext) {
        let (view, cx) = cx.add_window_view(|_, _| DialogHarness {
            backdrop_presses: 0,
        });
        cx.run_until_parked();
        let width = cx.update(|window, _| f32::from(window.viewport_size().width));
        // Close, maximize and minimize, from the trailing edge.
        for offset in [23., 69., 115.] {
            cx.simulate_click(
                gpui::point(px(width - offset), px(16.)),
                Modifiers::default(),
            );
        }
        cx.update(|_, cx| assert_eq!(view.read(cx).backdrop_presses, 0));
        // Beside them the dialog's backdrop still takes the press.
        cx.simulate_click(gpui::point(px(width - 160.), px(16.)), Modifiers::default());
        // Below the 32px buttons, too.
        cx.simulate_click(gpui::point(px(width - 23.), px(40.)), Modifiers::default());
        cx.update(|_, cx| assert_eq!(view.read(cx).backdrop_presses, 2));
    }

    #[test]
    fn windows_buttons_take_the_system_width_and_leave_open_space_before_them() {
        assert_eq!(width_of(Caption::Windows, 3), 138.);
        assert_eq!(trailing_for(width_of(Caption::Windows, 3)), 146.);
    }

    #[test]
    fn a_system_title_bar_keeps_the_ordinary_inset() {
        assert_eq!(width_of(Caption::System, 3), 0.);
        assert_eq!(trailing_for(0.), 16.);
    }

    #[test]
    fn linux_buttons_follow_what_the_window_manager_offers() {
        // Close alone on a tiling compositor, all three otherwise.
        assert_eq!(width_of(Caption::Linux, 1), 44.);
        assert_eq!(width_of(Caption::Linux, 3), 32. * 3. + 4. * 2. + 12.);
    }
}
