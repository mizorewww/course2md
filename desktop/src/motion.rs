//! Small, shared motion vocabulary: entrances, retargetable values and live work.
use crate::activity::{TransferEta, TransferMetrics};
use crate::theme::{
    ACCENT, CONTROL, GRAY, INK, SECONDARY_CONTAINER, TEXT_AUX, TEXT_BODY, accessible_text, color,
};
use gpui::{prelude::*, *};
use gpui_kit::component::{Sizable, h_flex, v_flex};
use std::time::Duration;

const SPINNER_TURN_MS: u64 = 1000;
const INDETERMINATE_SWEEP_MS: u64 = 1400;

/// A Material 3 spring: the Standard motion scheme's tokens
/// (`motionSpring*` in Material Components for Android). Spatial springs
/// move or reshape something and may settle with a barely visible overshoot;
/// effects springs fade color and opacity and never overshoot.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spring {
    pub damping: f32,
    pub stiffness: f32,
}

/// Small components: a button's shape, a switch thumb, a selection indicator.
pub const FAST_SPATIAL: Spring = Spring {
    damping: 0.9,
    stiffness: 1400.,
};
/// Partly covering surfaces: a page region arriving, a disclosure opening.
pub const DEFAULT_SPATIAL: Spring = Spring {
    damping: 0.9,
    stiffness: 700.,
};
pub const DEFAULT_EFFECTS: Spring = Spring {
    damping: 1.,
    stiffness: 1600.,
};
/// Whole-window color changes, such as switching palettes.
pub const SLOW_EFFECTS: Spring = Spring {
    damping: 1.,
    stiffness: 800.,
};

impl Spring {
    fn omega(self) -> f32 {
        self.stiffness.sqrt()
    }

    /// Time until the remaining distance stays under 0.1% of the change.
    pub fn duration(self) -> Duration {
        let rate = self.damping.min(1.) * self.omega();
        // A critically damped spring decays as (1 + ωt)·e^(-ωt), slower than
        // its envelope alone; 9.23/ω is where that falls below 0.001.
        let seconds = if self.damping >= 1. {
            9.23 / self.omega()
        } else {
            1000_f32.ln() / rate
        };
        Duration::from_secs_f32(seconds)
    }

    /// Position from rest at 0 toward 1 after `seconds`.
    pub fn position(self, seconds: f32) -> f32 {
        let t = seconds.max(0.);
        let omega = self.omega();
        let zeta = self.damping;
        if zeta >= 1. {
            1. - (1. + omega * t) * (-omega * t).exp()
        } else {
            let damped = omega * (1. - zeta * zeta).sqrt();
            1. - (-zeta * omega * t).exp()
                * ((damped * t).cos() + zeta * omega / damped * (damped * t).sin())
        }
    }

    /// The spring as an easing over its own `duration`, ending exactly at 1.
    pub fn ease(self) -> impl Fn(f32) -> f32 + Copy + 'static {
        let seconds = self.duration().as_secs_f32();
        move |t| {
            if t >= 1. {
                1.
            } else {
                self.position(t * seconds)
            }
        }
    }
}

/// IDs belong to a logical state, not a frame or a progress value.
pub fn enter<E: IntoElement + Styled + 'static>(id: impl Into<ElementId>, view: E) -> AnyElement {
    // Keep the same ancestor ID chain when the preference changes. GPUI's
    // AnimationElement already paints the final state without scheduling frames
    // under reduce-motion; removing it here remounts every keyed child.
    view.with_animation(
        id,
        Animation::new(DEFAULT_EFFECTS.duration()).with_easing(DEFAULT_EFFECTS.ease()),
        |view, t| view.opacity(t),
    )
    .into_any_element()
}

/// A change of task state settles into place, so the new title and available
/// action read as a consequence of the user's previous action.
pub fn state_enter<E: IntoElement + Styled + 'static>(
    id: impl Into<ElementId>,
    view: E,
) -> AnyElement {
    rise_in(id, view, 8.)
}

/// A destination, settings category or secondary page arrives as one unit;
/// navigation and the app bar stay put.
pub fn page_enter<E: IntoElement + Styled + 'static>(
    id: impl Into<ElementId>,
    view: E,
) -> AnyElement {
    rise_in(id, view, 12.)
}

/// Fades in while rising `distance` px into place: the offset follows the
/// default spatial spring and the opacity its effects spring, on one clock.
fn rise_in<E: IntoElement + Styled + 'static>(
    id: impl Into<ElementId>,
    view: E,
    distance: f32,
) -> AnyElement {
    let id = id.into();
    #[cfg(feature = "performance")]
    let trace_id = id.clone();
    let total = DEFAULT_SPATIAL.duration().max(DEFAULT_EFFECTS.duration());
    let seconds = total.as_secs_f32();
    view.with_animation(id, Animation::new(total), move |view, t| {
        #[cfg(feature = "performance")]
        crate::performance::record_motion(trace_id.clone(), 1., t);
        let (offset, opacity) = if t >= 1. {
            (1., 1.)
        } else {
            (
                DEFAULT_SPATIAL.position(t * seconds),
                DEFAULT_EFFECTS.position(t * seconds),
            )
        };
        view.relative()
            .top(px(distance * (1. - offset)))
            .opacity(opacity.clamp(0., 1.))
    })
    .into_any_element()
}

pub fn spinner(id: impl Into<ElementId>, cx: &App) -> AnyElement {
    spin(crate::icons::loader_circle().small(), id, cx)
}

/// A spinner standing in an icon column, at that column's icon size.
pub fn icon_spinner(id: impl Into<ElementId>, size: Rems, cx: &App) -> AnyElement {
    spin(crate::icons::loader_circle().size(size), id, cx)
}

fn spin(icon: gpui_kit::component::Icon, id: impl Into<ElementId>, cx: &App) -> AnyElement {
    let icon = icon.text_color(color(ACCENT));
    if cx.reduce_motion() {
        return icon.into_any_element();
    }
    icon.with_animation(
        id,
        Animation::new(Duration::from_millis(SPINNER_TURN_MS)).repeat(),
        |icon, t| icon.rotate(radians(std::f32::consts::TAU * t)),
    )
    .into_any_element()
}

/// Unmeasured work uses motion, never a fabricated completion percentage.
pub fn indeterminate_progress(id: impl Into<ElementId>, cx: &App) -> AnyElement {
    indeterminate_bar(id, px(4.), false, cx)
}

/// One active segment sweeping across the track, kept 4px apart from the
/// track on both sides as in Material 3 linear progress.
fn indeterminate_bar(
    id: impl Into<ElementId>,
    thickness: Pixels,
    muted: bool,
    cx: &App,
) -> AnyElement {
    let ink = color(if muted { CONTROL } else { ACCENT });
    let pieces = move |bar: gpui::Div, start: f32| {
        let end = (start + 0.3).min(1.);
        let start = start.max(0.);
        let piece = |width: f32, fill| {
            div()
                .flex_shrink_0()
                .h_full()
                .w(relative(width))
                .rounded_full()
                .bg(fill)
        };
        let track = color(SECONDARY_CONTAINER);
        bar.when(start > 0.005, |bar| bar.child(piece(start, track)))
            .when(end - start > 0.005, |bar| {
                bar.child(piece(end - start, ink))
            })
            .when(end < 0.995, |bar| {
                bar.child(div().flex_1().min_w_0().h_full().rounded_full().bg(track))
            })
    };
    let bar = h_flex().w_full().h(thickness).gap(px(4.)).items_center();
    if cx.reduce_motion() {
        return pieces(bar, 0.35).into_any_element();
    }
    bar.with_animation(
        id,
        Animation::new(Duration::from_millis(INDETERMINATE_SWEEP_MS)).repeat(),
        move |bar, t| pieces(bar, t * 1.3 - 0.3),
    )
    .into_any_element()
}

pub fn value(id: impl Into<ElementId>, target: f32, window: &mut Window, cx: &mut App) -> f32 {
    let id = id.into();
    #[cfg(feature = "performance")]
    let trace_id = id.clone();
    let value = gpui_base::transition(
        id,
        target,
        gpui_base::Transition::new(DEFAULT_SPATIAL.duration()).ease(DEFAULT_SPATIAL.ease()),
        window,
        cx,
    );
    #[cfg(feature = "performance")]
    crate::performance::record_motion(trace_id, target, value);
    value
}

/// A selection moves and reshapes on the fast spatial spring, retargeting
/// from wherever it is when the choice changes again.
pub fn selection_value(
    id: impl Into<ElementId>,
    target: f32,
    window: &mut Window,
    cx: &mut App,
) -> f32 {
    let id = id.into();
    #[cfg(feature = "performance")]
    let trace_id = id.clone();
    let amount = gpui_base::transition(
        id,
        target,
        gpui_base::Transition::new(FAST_SPATIAL.duration()).ease(FAST_SPATIAL.ease()),
        window,
        cx,
    );
    #[cfg(feature = "performance")]
    crate::performance::record_motion(trace_id, target, amount);
    amount
}

/// A measured fraction as Material 3 linear progress, 4px thick.
pub fn progress(
    id: impl Into<ElementId>,
    progress: f32,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    linear_progress(id, Some(progress), px(4.), false, window, cx)
}

/// Material 3 linear progress: the active indicator, a 4px gap, the track
/// and a stop dot at the track's end. Only a measured fraction draws it;
/// `None` shows the indeterminate indicator at the same thickness. A paused
/// or stopping process (`muted`) keeps its fraction in the outline color.
pub fn linear_progress(
    id: impl Into<ElementId>,
    fraction: Option<f32>,
    thickness: Pixels,
    muted: bool,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let Some(fraction) = fraction else {
        return indeterminate_bar(id, thickness, muted, cx);
    };
    let target = if fraction.is_finite() {
        fraction.clamp(0., 1.)
    } else {
        0.
    };
    let amount = value(id, target, window, cx);
    let ink = color(if muted { CONTROL } else { ACCENT });
    let dot = (thickness / 2.).max(px(4.)).min(thickness);
    h_flex()
        .w_full()
        .h(thickness)
        .gap(px(4.))
        .items_center()
        .when(amount > 0.005, |bar| {
            bar.child(
                div()
                    .flex_shrink_0()
                    .h_full()
                    .w(relative(amount))
                    .rounded_full()
                    .bg(ink),
            )
        })
        .when(amount < 0.995, |bar| {
            bar.child(
                div()
                    .relative()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .rounded_full()
                    .bg(color(SECONDARY_CONTAINER))
                    .child(
                        div()
                            .absolute()
                            .top((thickness - dot) / 2.)
                            .right((thickness - dot) / 2.)
                            .size(dot)
                            .rounded_full()
                            .bg(ink),
                    ),
            )
        })
        .into_any_element()
}

/// Model downloads keep quantity, labeled speed and labeled remaining time
/// on one stable block so the numbers do not hide behind a single sentence.
pub fn transfer_status(
    id: impl Into<ElementId>,
    title: impl Into<SharedString>,
    metrics: &TransferMetrics,
    fraction: Option<f32>,
    window: &mut Window,
    cx: &mut App,
) -> Div {
    let id = id.into();
    let title = title.into();
    let mut view = v_flex().w_full().min_w_0().gap_2().child(
        accessible_text(SharedString::from(format!("{id:?}-title")), title)
            .text_size(TEXT_BODY)
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(color(INK)),
    );
    if !metrics.quantity.is_empty() {
        view = view.child(
            accessible_text(
                SharedString::from(format!("{id:?}-quantity")),
                metrics.quantity.clone(),
            )
            .text_size(TEXT_BODY)
            .text_color(color(INK)),
        );
    }
    if metrics.speed.is_some() || matches!(&metrics.eta, Some(TransferEta::Remaining(_))) {
        let mut meters = h_flex().w_full().min_w_0().gap_6().flex_wrap();
        if let Some(speed) = &metrics.speed {
            meters = meters.child(transfer_meter(
                SharedString::from(format!("{id:?}-speed")),
                course2md::tr!("速度"),
                speed.clone(),
            ));
        }
        if let Some(TransferEta::Remaining(value)) = &metrics.eta {
            meters = meters.child(transfer_meter(
                SharedString::from(format!("{id:?}-eta")),
                course2md::tr!("预计剩余"),
                value.clone(),
            ));
        }
        view = view.child(meters);
    }
    if let Some(TransferEta::Note(value)) = &metrics.eta {
        view = view.child(
            accessible_text(
                SharedString::from(format!("{id:?}-eta-note")),
                value.clone(),
            )
            .text_size(TEXT_AUX)
            .text_color(color(GRAY)),
        );
    }
    for (key, text) in [
        ("note", &metrics.note),
        ("aside", &metrics.aside),
        ("source", &metrics.source),
    ] {
        if let Some(text) = text {
            view = view.child(
                accessible_text(SharedString::from(format!("{id:?}-{key}")), text.clone())
                    .text_size(TEXT_AUX)
                    .text_color(color(GRAY)),
            );
        }
    }
    view.when_some(fraction, |view, fraction| {
        view.child(progress(
            SharedString::from(format!("{id:?}-bar")),
            fraction,
            window,
            cx,
        ))
    })
}

fn transfer_meter(
    id: impl Into<SharedString>,
    label: &'static str,
    value: impl Into<SharedString>,
) -> Div {
    let id = id.into();
    v_flex()
        .min_w(rems(7.))
        .gap_1()
        .child(
            accessible_text(SharedString::from(format!("{id}-label")), label)
                .text_size(TEXT_AUX)
                .text_color(color(GRAY)),
        )
        .child(
            accessible_text(SharedString::from(format!("{id}-value")), value)
                .text_size(TEXT_BODY)
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(color(INK)),
        )
}

pub fn disclosure(id: impl Into<ElementId>, open: bool, content: Div) -> AnyElement {
    if open {
        // Keep intrinsic measurement, padding and child layout in the normal
        // tree. Measuring in prepaint and clipping to a previous frame's height
        // cuts off controls when the content or available width changes.
        enter(id, content)
    } else {
        div().hidden().into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::{enter, value};
    use gpui::{
        App, Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _, Pixels,
        Render, RenderOnce, Styled as _, TestAppContext, VisualTestContext, Window, div, px, size,
    };
    use std::{cell::Cell, rc::Rc, time::Duration};

    #[derive(gpui::IntoElement)]
    struct StatefulChild {
        target: f32,
        mounts: Rc<Cell<usize>>,
        sample: Rc<Cell<f32>>,
    }

    impl RenderOnce for StatefulChild {
        fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
            window.use_keyed_state("motion-child-state", cx, |_, _| {
                self.mounts.set(self.mounts.get() + 1);
            });
            self.sample
                .set(value("motion-child-value", self.target, window, cx));
            div().size(px(40.))
        }
    }

    struct PreferenceHarness {
        target: f32,
        mounts: Rc<Cell<usize>>,
        sample: Rc<Cell<f32>>,
    }

    impl Render for PreferenceHarness {
        fn render(&mut self, _: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            enter(
                "motion-preference-parent",
                div().child(StatefulChild {
                    target: self.target,
                    mounts: self.mounts.clone(),
                    sample: self.sample.clone(),
                }),
            )
        }
    }

    #[gpui::test]
    fn reduce_motion_preserves_child_state_and_later_value_transitions(cx: &mut TestAppContext) {
        let mounts = Rc::new(Cell::new(0));
        let sample = Rc::new(Cell::new(-1.));
        let (view, cx) = cx.add_window_view({
            let mounts = mounts.clone();
            let sample = sample.clone();
            move |_, _| PreferenceHarness {
                target: 0.,
                mounts,
                sample,
            }
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert_eq!(mounts.get(), 1);
        assert_eq!(sample.get(), 0.);

        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.target = 1.;
                cx.set_reduce_motion(true);
                cx.notify();
            });
            window.draw(cx).clear(cx);
        });
        assert_eq!(mounts.get(), 1, "preference must not remount the child");
        assert_eq!(
            sample.get(),
            1.,
            "reduced motion paints the target immediately"
        );

        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.target = 0.;
                cx.set_reduce_motion(false);
                cx.notify();
            });
            window.draw(cx).clear(cx);
        });
        assert_eq!(mounts.get(), 1);
        assert_eq!(
            sample.get(),
            1.,
            "restored motion retains the previous target"
        );
        cx.executor().advance_clock(Duration::from_millis(50));
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        });
        assert!(
            sample.get() > 0. && sample.get() < 1.,
            "the next change must paint an intermediate value"
        );
        assert_eq!(mounts.get(), 1);
    }

    struct RevealHarness {
        open: bool,
        width: Pixels,
        content_height: Pixels,
    }

    impl Render for RevealHarness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let content = div()
                .debug_selector(|| "motion-reveal-surface".into())
                .flex()
                .flex_col()
                .p(px(16.))
                .child(
                    div()
                        .debug_selector(|| "motion-reveal-content".into())
                        .h(self.content_height),
                );
            let reveal = crate::motion::disclosure("geometry-reveal", self.open, content);
            div()
                .debug_selector(|| "motion-layout-root".into())
                .flex()
                .flex_col()
                .w(self.width)
                .gap(px(8.))
                .child(div().h(px(20.)))
                .child(reveal)
                .child(
                    div()
                        .debug_selector(|| "motion-layout-footer".into())
                        .h(px(20.)),
                )
        }
    }

    fn draw_open_reveal(
        view: &Entity<RevealHarness>,
        cx: &mut VisualTestContext,
        width: Pixels,
        content_height: Pixels,
    ) {
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.open = true;
                view.width = width;
                view.content_height = content_height;
                cx.notify();
            });
            window.draw(cx).clear(cx);
        });
    }

    fn assert_reveal_geometry(cx: &mut VisualTestContext, width: Pixels, height: Pixels) {
        let root = cx.debug_bounds("motion-layout-root").unwrap();
        let surface = cx.debug_bounds("motion-reveal-surface").unwrap();
        let content = cx.debug_bounds("motion-reveal-content").unwrap();
        let footer = cx.debug_bounds("motion-layout-footer").unwrap();
        assert_eq!(surface.left(), root.left());
        assert_eq!(surface.top(), root.top() + px(28.));
        assert_eq!(surface.size, size(width, height + px(32.)));
        assert_eq!(content.top() - surface.top(), px(16.));
        assert_eq!(surface.bottom() - content.bottom(), px(16.));
        assert_eq!(footer.top() - surface.bottom(), px(8.));
        assert!(footer.bottom() <= root.bottom());
    }

    #[gpui::test]
    fn disclosure_first_frame_preserves_natural_size_and_padding(cx: &mut TestAppContext) {
        let (view, cx) = cx.add_window_view(|_, _| RevealHarness {
            open: false,
            width: px(240.),
            content_height: px(40.),
        });
        draw_open_reveal(&view, cx, px(240.), px(40.));
        assert_reveal_geometry(cx, px(240.), px(40.));
    }

    #[gpui::test]
    fn disclosure_content_resize_updates_following_rows_in_the_same_frame(cx: &mut TestAppContext) {
        let (view, cx) = cx.add_window_view(|_, _| RevealHarness {
            open: false,
            width: px(240.),
            content_height: px(40.),
        });
        draw_open_reveal(&view, cx, px(240.), px(40.));
        draw_open_reveal(&view, cx, px(180.), px(96.));
        assert_reveal_geometry(cx, px(180.), px(96.));
    }

    #[gpui::test]
    fn disclosure_closes_without_leaving_height_or_children(cx: &mut TestAppContext) {
        let (view, cx) = cx.add_window_view(|_, _| RevealHarness {
            open: false,
            width: px(240.),
            content_height: px(40.),
        });
        draw_open_reveal(&view, cx, px(240.), px(40.));
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.open = false;
                cx.notify();
            });
            window.draw(cx).clear(cx);
        });
        let root = cx.debug_bounds("motion-layout-root").unwrap();
        let footer = cx.debug_bounds("motion-layout-footer").unwrap();
        assert_eq!(footer.top(), root.top() + px(28.));
        assert!(cx.debug_bounds("motion-reveal-surface").is_none());
        assert!(cx.debug_bounds("motion-reveal-content").is_none());
    }

    #[test]
    fn springs_settle_on_their_target() {
        use super::{DEFAULT_EFFECTS, DEFAULT_SPATIAL, FAST_SPATIAL, SLOW_EFFECTS};
        for spring in [FAST_SPATIAL, DEFAULT_SPATIAL, DEFAULT_EFFECTS, SLOW_EFFECTS] {
            let ease = spring.ease();
            assert_eq!(ease(0.), 0.);
            assert_eq!(ease(1.), 1.);
            assert!((ease(0.999) - 1.).abs() < 0.002, "{spring:?}");
            let duration = spring.duration().as_secs_f32();
            assert!((0.1..0.5).contains(&duration), "{spring:?}: {duration}");
        }
        // Effects springs never overshoot; spatial ones barely do.
        let effects = (0..=200).map(|i| DEFAULT_EFFECTS.ease()(i as f32 / 200.));
        assert!(effects.clone().all(|v| v <= 1.));
        assert!(effects.collect::<Vec<_>>().windows(2).all(|p| p[0] <= p[1]));
        assert!((0..=200).all(|i| FAST_SPATIAL.ease()(i as f32 / 200.) < 1.003));
    }
}
