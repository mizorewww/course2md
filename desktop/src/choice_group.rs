//! A single controlled selection with native radio semantics and roving keyboard focus.
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::Icon;
use std::{collections::BTreeMap, rc::Rc};

actions!(
    course2md_choices,
    [NextChoice, PreviousChoice, FirstChoice, LastChoice]
);

pub(super) fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("right", NextChoice, Some("SingleChoiceGroup")),
        KeyBinding::new("down", NextChoice, Some("SingleChoiceGroup")),
        KeyBinding::new("left", PreviousChoice, Some("SingleChoiceGroup")),
        KeyBinding::new("up", PreviousChoice, Some("SingleChoiceGroup")),
        KeyBinding::new("home", FirstChoice, Some("SingleChoiceGroup")),
        KeyBinding::new("end", LastChoice, Some("SingleChoiceGroup")),
    ]);
}

type Change = Rc<dyn Fn(&SharedString, &mut Window, &mut App)>;
#[derive(Clone)]
struct OptionItem {
    value: SharedString,
    label: SharedString,
}

#[derive(IntoElement)]
pub struct SingleChoiceGroup {
    id: ElementId,
    label: SharedString,
    value: Option<SharedString>,
    options: Vec<OptionItem>,
    icons: BTreeMap<SharedString, Icon>,
    full_width: bool,
    segmented: bool,
    label_text_size: Option<Rems>,
    height: Option<Rems>,
    tabs: bool,
    provided_focus: Vec<FocusHandle>,
    disabled: bool,
    on_change: Option<Change>,
    reveal_in: Option<ScrollHandle>,
}
impl SingleChoiceGroup {
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            value: None,
            options: Vec::new(),
            icons: BTreeMap::new(),
            full_width: false,
            segmented: false,
            label_text_size: None,
            height: None,
            tabs: false,
            provided_focus: Vec::new(),
            disabled: false,
            on_change: None,
            reveal_in: None,
        }
    }
    pub fn options<K: Into<SharedString>, V: Into<SharedString>>(
        mut self,
        options: impl IntoIterator<Item = (K, V)>,
    ) -> Self {
        self.options = options
            .into_iter()
            .map(|(value, label)| OptionItem {
                value: value.into(),
                label: label.into(),
            })
            .collect();
        self
    }
    pub fn selected(mut self, value: impl Into<SharedString>) -> Self {
        self.value = Some(value.into());
        self
    }
    /// Fill the parent's width with equal segments and one moving selection surface.
    pub fn full_width(mut self) -> Self {
        self.full_width = true;
        self
    }
    /// Connected equal-width segments with a shared outline and persistent option icons.
    /// Radio/tab semantics, focus navigation and pointer feedback are unchanged.
    pub fn segmented(mut self) -> Self {
        self.segmented = true;
        self.full_width = true;
        self
    }
    /// Override option text for a page-specific typography role.
    /// Override the option height (48px segmented, 40px otherwise), e.g. for
    /// a page's view tabs.
    pub fn height(mut self, height: Rems) -> Self {
        self.height = Some(height);
        self
    }
    pub fn label_text_size(mut self, size: Rems) -> Self {
        self.label_text_size = Some(size);
        self
    }
    /// Navigation uses the same surfaces, with tab semantics and optional leading layout.
    pub fn tabs(mut self) -> Self {
        self.tabs = true;
        self
    }
    pub fn focus_handles(mut self, handles: impl IntoIterator<Item = FocusHandle>) -> Self {
        self.provided_focus = handles.into_iter().collect();
        self
    }
    /// Attach an icon by option value; this may be called before or after `options`.
    pub fn icon(mut self, value: impl Into<SharedString>, icon: impl Into<Icon>) -> Self {
        self.icons.insert(value.into(), icon.into());
        self
    }
    pub fn reveal_in(mut self, scroll: ScrollHandle) -> Self {
        self.reveal_in = Some(scroll);
        self
    }
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
    pub fn on_change(
        mut self,
        callback: impl Fn(&SharedString, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_change = Some(Rc::new(callback));
        self
    }
}

#[derive(Clone)]
struct Navigation {
    values: Vec<SharedString>,
    focus: Vec<FocusHandle>,
    selected: Option<usize>,
    on_change: Option<Change>,
}
#[derive(Clone, Copy)]
enum Direction {
    Next,
    Previous,
    First,
    Last,
}
/// Arrow keys wrap; with no current option, Next starts at the first and Previous at the last.
fn destination(count: usize, current: Option<usize>, direction: Direction) -> Option<usize> {
    if count == 0 {
        return None;
    }
    Some(match direction {
        Direction::First => 0,
        Direction::Last => count - 1,
        Direction::Next => current.map_or(0, |index| (index + 1) % count),
        Direction::Previous => current.map_or(count - 1, |index| (index + count - 1) % count),
    })
}
impl Navigation {
    fn navigate(&self, direction: Direction, window: &mut Window, cx: &mut App) {
        let current = self
            .focus
            .iter()
            .position(|focus| focus.is_focused(window))
            .or(self.selected);
        let Some(index) = destination(self.values.len(), current, direction) else {
            return;
        };
        self.focus[index].focus(window, cx);
        if self.selected != Some(index)
            && let Some(on_change) = &self.on_change
        {
            on_change(&self.values[index], window, cx);
        }
    }
}
/// The resolved paint of one option for the current motion frame.
#[derive(Clone, Copy, Debug, PartialEq)]
struct OptionPaint {
    background: Rgba,
    foreground: Rgba,
    hover: Rgba,
    press: Rgba,
    focus: Rgba,
    left_radius: Pixels,
    right_radius: Pixels,
}

/// Inner corners between neighbours; outer group ends are always fully round.
const INNER_RADIUS: f32 = 8.;
const GAP: f32 = 2.;

/// M3 connected button group: each option is its own button. `selection` is the
/// option's animated selection amount (0 unselected, 1 selected), which morphs
/// both colour and the inner corners toward a full pill.
fn option_paint(
    selection: f32,
    index: usize,
    count: usize,
    height: Pixels,
    disabled: bool,
) -> OptionPaint {
    let t = selection.clamp(0., 1.);
    let full = height / 2.;
    let inner = px(INNER_RADIUS) + (full - px(INNER_RADIUS)) * t;
    let left_radius = if index == 0 { full } else { inner };
    let right_radius = if index + 1 == count { full } else { inner };
    if disabled {
        // M3 disabled: on-surface at 12% container and 38% content. The pill
        // shape still tells which option is current.
        let ink = super::color(super::INK);
        return OptionPaint {
            background: ink.opacity(0.12),
            foreground: ink.opacity(0.38),
            hover: ink.opacity(0.12),
            press: ink.opacity(0.12),
            focus: gpui::transparent_black().into(),
            left_radius,
            right_radius,
        };
    }
    let background = super::blend(
        super::color(super::SURFACE_HIGH),
        super::color(super::ACCENT),
        t,
    );
    let foreground = super::blend(super::color(super::INK), super::color(super::ON_PRIMARY), t);
    OptionPaint {
        background,
        foreground,
        // One state layer in the content colour, resolved over the current
        // container instead of stacking translucent fills, so the selected
        // accent stays under hover and press.
        hover: super::blend(background, foreground, 0.08),
        press: super::blend(background, foreground, 0.10),
        // The 2px focus ring is painted inside the option so it is never
        // clipped. On an unselected option the accent ring reads against the
        // neutral container; on the accent pill an accent ring would vanish,
        // so it turns into the on-primary colour, a contrasting inner ring
        // that stays legible in light and dark palettes.
        focus: super::blend(
            super::color(super::ACCENT),
            super::color(super::ON_PRIMARY),
            t,
        ),
        left_radius,
        right_radius,
    }
}

#[cfg(test)]
thread_local! {
    static PAINTED: std::cell::RefCell<BTreeMap<String, OptionPaint>> =
        const { std::cell::RefCell::new(BTreeMap::new()) };
}

/// Containers stay behind the interactive item. Hover/press cannot erase selection.
fn item_style<T: Styled + StatefulInteractiveElement + gpui::prelude::FluentBuilder>(
    item: T,
    paint: OptionPaint,
    height: Pixels,
    fill: bool,
    disabled: bool,
) -> T {
    item.relative()
        .flex()
        .items_center()
        .justify_center()
        .min_w_0()
        .h(height)
        .min_h(height)
        // With the 2px focus border: 16px to the content of a content-sized
        // option; 8px in equal shares so a 312px panel still fits "语音识别".
        .px(if fill { rems(6. / 14.) } else { rems(1.) })
        .py_0()
        .rounded_l(paint.left_radius)
        .rounded_r(paint.right_radius)
        .border_2()
        .border_color(gpui::transparent_black())
        .bg(paint.background)
        .text_color(paint.foreground)
        .text_size(super::TEXT_BODY)
        .font_weight(FontWeight::MEDIUM)
        // Content-width options keep the default shrink so a narrow parent
        // ellipsizes labels instead of overflowing.
        .when(fill, |item| item.flex_1())
        .when(!disabled, |item| {
            item.hover(move |style| style.bg(paint.hover))
                .active(move |style| style.bg(paint.press))
                .focus_visible(move |style| style.border_color(paint.focus))
        })
}
impl RenderOnce for SingleChoiceGroup {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let disabled = self.disabled;
        let state = window.use_keyed_state(self.id.clone(), cx, |_, _| {
            BTreeMap::<SharedString, FocusHandle>::new()
        });
        let handles = if self.provided_focus.len() == self.options.len() {
            self.provided_focus.clone()
        } else {
            state.update(cx, |handles, cx| {
                self.options
                    .iter()
                    .map(|option| {
                        handles
                            .entry(option.value.clone())
                            .or_insert_with(|| cx.focus_handle())
                            .clone()
                    })
                    .collect::<Vec<_>>()
            })
        };
        let selected = self
            .options
            .iter()
            .position(|option| Some(&option.value) == self.value.as_ref());
        let entry = (!disabled).then(|| selected.unwrap_or(0));
        let navigation = Navigation {
            values: if disabled {
                Vec::new()
            } else {
                self.options
                    .iter()
                    .map(|option| option.value.clone())
                    .collect()
            },
            focus: handles.clone(),
            selected,
            on_change: self.on_change.clone(),
        };
        let next = navigation.clone();
        let previous = navigation.clone();
        let first = navigation.clone();
        let last = navigation;
        let count = self.options.len();
        let scale = f32::from(window.rem_size()) / 14.;
        let item_height = match self.height {
            Some(height) => height.to_pixels(window.rem_size()),
            None => px(if self.segmented { 48. } else { 40. } * scale),
        };
        let fill = self.full_width;
        let icon_size = rems(18. / 14.);
        let debug_kind = if fill { "full" } else { "content" };
        let mut lane = gpui_base::h_flex()
            .w_full()
            .min_w_0()
            .gap(px(GAP))
            .items_center()
            .when(cfg!(test), |lane| {
                lane.debug_selector(|| "full-choice-lane".into())
            });
        for (index, option) in self.options.into_iter().enumerate() {
            let checked = selected == Some(index);
            let focus = handles[index].clone().tab_stop(entry == Some(index));
            let callback = self.on_change.clone();
            let value = option.value.clone();
            let label = option.label.clone();
            // Each option animates its own selection amount; a rapid click
            // retargets from the current presentation value.
            let selection = crate::motion::selection_value(
                ElementId::NamedChild(self.id.clone().into(), format!("selection-{value}").into()),
                if checked { 1. } else { 0. },
                window,
                cx,
            );
            let paint = option_paint(selection, index, count, item_height, disabled);
            #[cfg(test)]
            PAINTED.with(|painted| {
                painted
                    .borrow_mut()
                    .insert(format!("{debug_kind}-{index}"), paint)
            });
            let content = gpui_base::h_flex()
                .max_w_full()
                .min_w_0()
                .gap(rems(8. / 14.))
                .items_center()
                // Text-only options center their label; no empty icon slot.
                .when_some(self.icons.get(&value).cloned(), |content, icon| {
                    content.child(
                        div()
                            .w(icon_size)
                            .h(icon_size)
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .when(cfg!(test), |slot| {
                                slot.debug_selector(move || {
                                    format!("{debug_kind}-choice-icon-{index}")
                                })
                            })
                            .child(icon.size(icon_size)),
                    )
                })
                .child(
                    div()
                        .min_w_0()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .when(cfg!(test), |label| {
                            label.debug_selector(move || {
                                format!("{debug_kind}-choice-label-{index}")
                            })
                        })
                        .child(label.clone()),
                );
            if self.tabs {
                let click_focus = focus.clone();
                let tab = item_style(
                    gpui_base::Tab::new(value.clone()),
                    paint,
                    item_height,
                    fill,
                    disabled,
                )
                .when_some(self.label_text_size, |item, size| item.text_size(size))
                .selected(checked)
                .disabled(disabled)
                .accessibility_label(label)
                .set_position(index + 1, count)
                .track_focus(&focus)
                .child(content)
                .when(cfg!(test), |tab| {
                    tab.debug_selector(move || format!("{debug_kind}-choice-option-{index}"))
                })
                .on_click(move |_, window, cx| {
                    click_focus.focus(window, cx);
                    if !checked && let Some(callback) = &callback {
                        callback(&value, window, cx);
                    }
                });
                lane = lane.child(tab);
            } else {
                let radio = item_style(
                    gpui_base::Radio::new(value.clone()),
                    paint,
                    item_height,
                    fill,
                    disabled,
                )
                .when_some(self.label_text_size, |item, size| item.text_size(size))
                .checked(checked)
                .disabled(disabled)
                .accessibility_label(label)
                .set_position(index + 1, count)
                .track_focus(&focus)
                .child(content)
                .when(cfg!(test), |radio| {
                    radio.debug_selector(move || format!("{debug_kind}-choice-option-{index}"))
                })
                .on_change(move |_, _, window, cx| {
                    focus.focus(window, cx);
                    if !checked && let Some(callback) = &callback {
                        callback(&value, window, cx);
                    }
                });
                lane = lane.child(radio);
            }
        }
        let group = div()
            .id(self.id.clone())
            .role(if self.tabs {
                Role::TabList
            } else {
                Role::RadioGroup
            })
            .aria_label(self.label)
            .key_context("SingleChoiceGroup")
            .flex()
            .min_w_0()
            .max_w_full()
            .when(fill, |group| group.w_full())
            .when(cfg!(test), |group| {
                group.debug_selector(|| "full-choice-track".into())
            })
            .on_action(move |_: &NextChoice, window, cx| next.navigate(Direction::Next, window, cx))
            .on_action(move |_: &PreviousChoice, window, cx| {
                previous.navigate(Direction::Previous, window, cx)
            })
            .on_action(move |_: &FirstChoice, window, cx| {
                first.navigate(Direction::First, window, cx)
            })
            .on_action(move |_: &LastChoice, window, cx| last.navigate(Direction::Last, window, cx))
            .child(lane);
        let reveal_id = SharedString::from(format!("choice-reveal-{:?}", self.id));
        if let Some(scroll) = self.reveal_in {
            let reveal = crate::focus_scroll::RevealFocus::new(reveal_id, group, scroll);
            if fill { reveal } else { reveal.inline() }.into_any_element()
        } else {
            group.into_any_element()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Direction, OptionPaint, PAINTED, SingleChoiceGroup, destination};
    use gpui::{
        Bounds, Context, FontWeight, IntoElement, Modifiers, ParentElement as _, Pixels, Render,
        SharedString, Styled as _, TestAppContext, TextRun, VisualTestContext, Window, div, font,
        px,
    };
    use std::time::Duration;

    struct FullWidthHarness {
        count: usize,
        selected: usize,
        width: Pixels,
        changes: usize,
        disabled: bool,
    }

    struct ContentWidthHarness {
        selected: SharedString,
        changes: usize,
    }

    const MODEL_LABELS: [&str; 3] = ["Qwen3-ASR 1.7B", "Qwen3-ASR 0.6B", "Whisper"];

    struct ModelWidthHarness {
        selected: SharedString,
    }

    impl Render for ModelWidthHarness {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            window.set_rem_size(px(14.));
            div().w(px(600.)).font_family(".SystemUIFont").child(
                SingleChoiceGroup::new("model-width-choice", "识别模型")
                    .options(MODEL_LABELS.map(|label| (label, label)))
                    .selected(self.selected.clone())
                    .on_change(cx.listener(|this, selected: &SharedString, _, cx| {
                        this.selected = selected.clone();
                        cx.notify();
                    })),
            )
        }
    }

    impl Render for ContentWidthHarness {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            window.set_rem_size(px(14.));
            div().w(px(600.)).child(
                SingleChoiceGroup::new("content-width-choice", "语音服务")
                    .options([
                        ("auto", "Auto"),
                        ("local", "Whisper"),
                        ("remote", "Cloud transcription service"),
                    ])
                    .selected(self.selected.clone())
                    .on_change(cx.listener(|this, next: &SharedString, _, cx| {
                        this.selected = next.clone();
                        this.changes += 1;
                        cx.notify();
                    })),
            )
        }
    }

    impl Render for FullWidthHarness {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            window.set_rem_size(px(14.));
            div().w(self.width).child(
                SingleChoiceGroup::new("full-width-choice", "文字大小")
                    .options(
                        (0..self.count)
                            .map(|index| (index.to_string(), format!("{}%", 100 + 25 * index))),
                    )
                    .selected(self.selected.to_string())
                    .full_width()
                    .disabled(self.disabled)
                    .on_change(cx.listener(|this, next: &SharedString, _, cx| {
                        this.selected = next.parse().unwrap();
                        this.changes += 1;
                        cx.notify();
                    })),
            )
        }
    }

    struct SegmentedHarness {
        selected: usize,
        changes: usize,
    }

    impl Render for SegmentedHarness {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            window.set_rem_size(px(14.));
            div().w(px(312.)).child(
                SingleChoiceGroup::new("connected-choice", "文字来源")
                    .options([("0", "自动"), ("1", "仅字幕"), ("2", "语音识别")])
                    .icon("0", crate::icons::settings())
                    .icon("1", crate::icons::article())
                    .icon("2", crate::icons::mic())
                    .segmented()
                    .selected(self.selected.to_string())
                    .on_change(cx.listener(|this, next: &SharedString, _, cx| {
                        this.selected = next.parse().unwrap();
                        this.changes += 1;
                        cx.notify();
                    })),
            )
        }
    }

    fn painted(name: &str) -> OptionPaint {
        PAINTED.with(|painted| {
            *painted
                .borrow()
                .get(name)
                .unwrap_or_else(|| panic!("no paint for {name}"))
        })
    }

    fn token(token: crate::theme::ColorToken) -> gpui::Rgba {
        crate::theme::color(token)
    }

    /// Blended endpoints may differ from a token in the last float bit.
    fn same(a: gpui::Rgba, b: gpui::Rgba) -> bool {
        [a.r - b.r, a.g - b.g, a.b - b.b, a.a - b.a]
            .iter()
            .all(|d| d.abs() < 1e-4)
    }

    /// Selected: accent container, on-primary content and a full pill.
    fn assert_selected(name: &str, height: Pixels) {
        let paint = painted(name);
        assert!(
            same(paint.background, token(crate::theme::ACCENT)),
            "{name}: {:?}",
            paint
        );
        assert!(
            same(paint.foreground, token(crate::theme::ON_PRIMARY)),
            "{name}: {:?}",
            paint
        );
        assert_eq!(paint.left_radius, height / 2., "{name}");
        assert_eq!(paint.right_radius, height / 2., "{name}");
        // A state layer resolves over the selected container, never replaces it.
        assert!(!same(paint.hover, token(crate::theme::SURFACE_HIGH)));
        assert!(!same(paint.hover, paint.background));
    }

    /// Unselected: neutral container, ink content; only outer ends are round.
    fn assert_unselected(name: &str, index: usize, count: usize, height: Pixels) {
        let paint = painted(name);
        assert_eq!(
            paint.background,
            token(crate::theme::SURFACE_HIGH),
            "{name}"
        );
        assert!(
            same(paint.foreground, token(crate::theme::INK)),
            "{name}: {:?}",
            paint
        );
        let left = if index == 0 { height / 2. } else { px(8.) };
        let right = if index + 1 == count {
            height / 2.
        } else {
            px(8.)
        };
        assert_eq!(paint.left_radius, left, "{name}");
        assert_eq!(paint.right_radius, right, "{name}");
    }

    #[gpui::test]
    fn connected_segments_share_width_and_keep_labels_still_on_selection(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_reduce_motion(true));
        let (view, cx) = cx.add_window_view(|_, _| SegmentedHarness {
            selected: 0,
            changes: 0,
        });
        draw_choice(cx);
        let labels = [
            "full-choice-label-0",
            "full-choice-label-1",
            "full-choice-label-2",
        ];
        let before = labels.map(|selector| choice_bounds(cx, selector));
        let icons = [
            "full-choice-icon-0",
            "full-choice-icon-1",
            "full-choice-icon-2",
        ];
        let icon_before = icons.map(|selector| choice_bounds(cx, selector));
        let options = OPTION_SELECTORS[..3]
            .iter()
            .map(|selector| choice_bounds(cx, selector))
            .collect::<Vec<_>>();
        let lane = choice_bounds(cx, "full-choice-lane");
        for index in 0..3 {
            // Each label follows its icon; the cluster is centered in a 48px button.
            assert_eq!(options[index].size.height, px(48.));
            // Shares snap to the 2x device grid.
            assert!((options[index].size.width - options[0].size.width).abs() <= px(0.5));
            assert!(icon_before[index].right() <= before[index].left());
            let content_center = (icon_before[index].left() + before[index].right()) / 2.;
            assert!((content_center - options[index].center().x).abs() <= px(0.5));
            assert!((icon_before[index].center().y - options[index].center().y).abs() <= px(0.5));
            assert!((before[index].center().y - options[index].center().y).abs() <= px(0.5));
        }
        // Separate buttons with a 2px gap that together fill the parent.
        for pair in options.windows(2) {
            assert!((pair[1].left() - pair[0].right() - px(2.)).abs() <= px(0.5));
        }
        assert!((options[0].left() - lane.left()).abs() < px(0.5));
        assert!((options[2].right() - lane.right()).abs() < px(0.5));
        assert_eq!(lane.size.width, px(312.));
        assert_selected("full-0", px(48.));
        assert_unselected("full-1", 1, 3, px(48.));
        assert_unselected("full-2", 2, 3, px(48.));
        // A 360px panel minus two 24px insets must show the full label.
        let required = cx.update(|window, _| {
            let mut font = window.text_style().font();
            font.weight = FontWeight::MEDIUM;
            window
                .text_system()
                .shape_line(
                    "语音识别".into(),
                    px(14.),
                    &[TextRun {
                        len: "语音识别".len(),
                        font,
                        color: gpui::black(),
                        background_color: None,
                        underline: None,
                        strikethrough: None,
                    }],
                    None,
                )
                .width
                .ceil()
        });
        assert!(
            before[2].size.width >= required,
            "label bounds: {:?}, needed: {:?}",
            before[2],
            required
        );
        cx.simulate_click(options[2].center(), Modifiers::default());
        draw_choice(cx);
        cx.update(|_, cx| {
            assert_eq!(view.read(cx).selected, 2);
            assert_eq!(view.read(cx).changes, 1);
        });
        // Reduce motion: the new selection is fully drawn on the changed frame.
        assert_selected("full-2", px(48.));
        assert_unselected("full-0", 0, 3, px(48.));
        for (selector, bounds) in labels.into_iter().zip(before) {
            assert_eq!(choice_bounds(cx, selector), bounds);
        }
        for (selector, bounds) in icons.into_iter().zip(icon_before) {
            assert_eq!(choice_bounds(cx, selector), bounds);
        }
        cx.simulate_click(options[2].center(), Modifiers::default());
        cx.update(|_, cx| assert_eq!(view.read(cx).changes, 1));
    }

    const OPTION_SELECTORS: [&str; 4] = [
        "full-choice-option-0",
        "full-choice-option-1",
        "full-choice-option-2",
        "full-choice-option-3",
    ];

    fn choice_bounds(cx: &mut VisualTestContext, name: &'static str) -> Bounds<Pixels> {
        cx.debug_bounds(name)
            .unwrap_or_else(|| panic!("missing {name}"))
    }

    fn draw_choice(cx: &mut VisualTestContext) {
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        });
    }

    #[gpui::test]
    fn content_width_model_labels_keep_their_rounded_text_width(cx: &mut TestAppContext) {
        let (_, cx) = cx.add_window_view(|_, _| ModelWidthHarness {
            selected: MODEL_LABELS[0].into(),
        });
        draw_choice(cx);
        let required_widths = cx.update(|window, _| {
            let mut label_font = font(".SystemUIFont");
            label_font.weight = FontWeight::MEDIUM;
            MODEL_LABELS.map(|label| {
                window
                    .text_system()
                    .shape_line(
                        label.into(),
                        px(14.),
                        &[TextRun {
                            len: label.len(),
                            font: label_font.clone(),
                            ..Default::default()
                        }],
                        None,
                    )
                    .width
                    .ceil()
            })
        });
        let option_selectors = [
            "content-choice-option-0",
            "content-choice-option-1",
            "content-choice-option-2",
        ];
        let label_selectors = [
            "content-choice-label-0",
            "content-choice-label-1",
            "content-choice-label-2",
        ];
        let before = option_selectors.map(|selector| choice_bounds(cx, selector));
        // The native GPUI test window uses a 2x device scale. Check actual
        // laid-out labels, including the slightly longer 0.6B option.
        for index in 0..MODEL_LABELS.len() {
            let label = choice_bounds(cx, label_selectors[index]);
            assert!(
                label.size.width >= required_widths[index],
                "{} was narrowed below its full shaped width",
                MODEL_LABELS[index]
            );
            // Content-sized buttons: 16px from each edge to the label.
            let leading = label.left() - before[index].left();
            let trailing = before[index].right() - label.right();
            assert!((leading - px(16.)).abs() <= px(1.), "leading {leading:?}");
            assert!(
                (trailing - px(16.)).abs() <= px(1.),
                "trailing {trailing:?}"
            );
            assert_eq!(before[index].size.height, px(40.));
        }
        cx.simulate_click(before[1].center(), Modifiers::default());
        draw_choice(cx);
        for index in 0..MODEL_LABELS.len() {
            assert_eq!(choice_bounds(cx, option_selectors[index]), before[index]);
            assert!(choice_bounds(cx, label_selectors[index]).size.width >= required_widths[index]);
        }
    }

    #[gpui::test]
    fn content_width_choices_morph_in_place_and_settle(cx: &mut TestAppContext) {
        let (view, cx) = cx.add_window_view(|_, _| ContentWidthHarness {
            selected: "auto".into(),
            changes: 0,
        });
        draw_choice(cx);
        let selectors = [
            "content-choice-option-0",
            "content-choice-option-1",
            "content-choice-option-2",
        ];
        let labels = [
            "content-choice-label-0",
            "content-choice-label-1",
            "content-choice-label-2",
        ];
        let before = selectors.map(|selector| choice_bounds(cx, selector));
        let label_before = labels.map(|selector| choice_bounds(cx, selector));
        // Each option is sized to its own label and the group hugs the start.
        assert!(before[2].size.width > before[0].size.width + px(40.));
        assert!((before[1].left() - before[0].right() - px(2.)).abs() < px(0.5));
        assert_selected("content-0", px(40.));
        cx.simulate_click(before[2].center(), Modifiers::default());
        draw_choice(cx);
        // The click frame starts from the current presentation, not the endpoint.
        assert_eq!(
            painted("content-2").background,
            token(crate::theme::SURFACE_HIGH)
        );
        cx.executor().advance_clock(Duration::from_millis(60));
        draw_choice(cx);
        let entering = painted("content-2");
        let leaving = painted("content-0");
        assert!(entering.right_radius == px(20.) && entering.left_radius > px(8.));
        assert!(
            entering.left_radius < px(20.),
            "an intermediate shape is drawn"
        );
        assert!(leaving.right_radius > px(8.) && leaving.right_radius < px(20.));
        assert!(!same(entering.background, token(crate::theme::ACCENT)));
        for index in 0..3 {
            assert_eq!(choice_bounds(cx, selectors[index]), before[index]);
            assert_eq!(choice_bounds(cx, labels[index]), label_before[index]);
        }
        cx.executor().advance_clock(Duration::from_millis(300));
        draw_choice(cx);
        assert_selected("content-2", px(40.));
        assert_unselected("content-0", 0, 3, px(40.));
        cx.simulate_mouse_move(before[2].center(), None, Modifiers::default());
        draw_choice(cx);
        assert_eq!(choice_bounds(cx, selectors[2]), before[2]);
        cx.update(|window, cx| {
            assert_eq!(view.read(cx).changes, 1);
            window.simulate_next_frame(cx);
            window.refresh();
            window.draw(cx).clear(cx);
            assert_eq!(window.simulate_next_frame(cx), 0);
        });
    }

    fn assert_equal_options(cx: &mut VisualTestContext, count: usize, width: Pixels) {
        let track = choice_bounds(cx, "full-choice-track");
        let lane = choice_bounds(cx, "full-choice-lane");
        assert_eq!(track.size.width, width);
        assert_eq!(lane.size.width, width);
        let share = (width - px(2.) * (count - 1) as f32) / count as f32;
        for selector in &OPTION_SELECTORS[..count] {
            let option = choice_bounds(cx, selector);
            assert!((option.size.width - share).abs() <= px(0.5));
            assert_eq!(option.size.height, px(40.));
        }
    }

    fn exercise_full_width_motion(cx: &mut TestAppContext, count: usize) {
        let (view, cx) = cx.add_window_view(|_, _| FullWidthHarness {
            count,
            selected: 0,
            width: px(420.),
            changes: 0,
            disabled: false,
        });
        draw_choice(cx);
        assert_equal_options(cx, count, px(420.));
        let last = format!("full-{}", count - 1);
        assert_selected("full-0", px(40.));
        assert_unselected(&last, count - 1, count, px(40.));
        let options = (0..count)
            .map(|index| choice_bounds(cx, OPTION_SELECTORS[index]))
            .collect::<Vec<_>>();
        let destination = options[count - 1];

        cx.simulate_click(destination.center(), Modifiers::default());
        cx.update(|window, cx| {
            assert_eq!(
                view.read(cx).selected,
                count - 1,
                "click is accepted immediately"
            );
            assert_eq!(view.read(cx).changes, 1);
            window.draw(cx).clear(cx);
        });
        assert_eq!(painted(&last).left_radius, px(8.));
        cx.simulate_click(destination.center(), Modifiers::default());
        cx.update(|_, cx| assert_eq!(view.read(cx).changes, 1, "same choice does not save twice"));

        cx.executor().advance_clock(Duration::from_millis(60));
        draw_choice(cx);
        let middle = painted(&last);
        assert!(middle.left_radius > px(8.) && middle.left_radius < px(20.));
        assert!(!same(middle.background, token(crate::theme::SURFACE_HIGH)));
        assert!(!same(middle.background, token(crate::theme::ACCENT)));
        for (index, before) in options.iter().enumerate() {
            assert_eq!(choice_bounds(cx, OPTION_SELECTORS[index]), *before);
        }
        draw_choice(cx);
        assert_eq!(
            painted(&last),
            middle,
            "ordinary redraw must not restart motion"
        );

        // Reverse before settling, then resize without advancing time. The
        // shape continues from its current value instead of jumping.
        cx.simulate_click(options[0].center(), Modifiers::default());
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert_eq!(painted(&last).left_radius, middle.left_radius);
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.width = px(284.);
                cx.notify();
            });
            window.draw(cx).clear(cx);
        });
        assert_equal_options(cx, count, px(284.));
        cx.executor().advance_clock(Duration::from_millis(30));
        draw_choice(cx);
        let returning = painted(&last);
        assert!(returning.left_radius < middle.left_radius);
        assert!(returning.left_radius >= px(8.));

        cx.executor().advance_clock(Duration::from_millis(300));
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
            window.simulate_next_frame(cx);
            window.refresh();
            window.draw(cx).clear(cx);
            assert_eq!(
                window.simulate_next_frame(cx),
                0,
                "settled control must stop scheduling frames"
            );
        });
        assert_selected("full-0", px(40.));
        assert_unselected(&last, count - 1, count, px(40.));
    }

    #[gpui::test]
    fn full_width_choices_morph_inside_current_geometry_and_settle(cx: &mut TestAppContext) {
        for count in [3, 4] {
            exercise_full_width_motion(cx, count);
        }
    }

    #[gpui::test]
    fn full_width_choices_respect_reduce_motion_on_the_changed_frame(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_reduce_motion(true));
        let (view, cx) = cx.add_window_view(|_, _| FullWidthHarness {
            count: 4,
            selected: 0,
            width: px(420.),
            changes: 0,
            disabled: false,
        });
        draw_choice(cx);
        let destination = choice_bounds(cx, "full-choice-option-3");
        cx.simulate_click(destination.center(), Modifiers::default());
        cx.update(|window, cx| {
            assert_eq!(view.read(cx).selected, 3);
            window.draw(cx).clear(cx);
            assert_eq!(window.simulate_next_frame(cx), 0);
        });
        assert_selected("full-3", px(40.));
        assert_unselected("full-0", 0, 4, px(40.));
        assert_equal_options(cx, 4, px(420.));
    }

    #[gpui::test]
    fn arrow_keys_move_focus_and_selection_once_per_step(cx: &mut TestAppContext) {
        cx.update(|cx| {
            cx.set_reduce_motion(true);
            super::init(cx);
        });
        let (view, cx) = cx.add_window_view(|_, _| FullWidthHarness {
            count: 3,
            selected: 0,
            width: px(420.),
            changes: 0,
            disabled: false,
        });
        draw_choice(cx);
        // Clicking the selected option focuses it without a change.
        let first = choice_bounds(cx, "full-choice-option-0").center();
        cx.simulate_click(first, Modifiers::default());
        draw_choice(cx);
        cx.update(|_, cx| assert_eq!(view.read(cx).changes, 0));
        cx.simulate_keystrokes("right");
        draw_choice(cx);
        cx.update(|_, cx| {
            assert_eq!(view.read(cx).selected, 1);
            assert_eq!(view.read(cx).changes, 1);
        });
        cx.simulate_keystrokes("left left");
        draw_choice(cx);
        cx.update(|_, cx| {
            assert_eq!(
                view.read(cx).selected,
                2,
                "previous wraps to the last option"
            );
            assert_eq!(view.read(cx).changes, 3);
        });
        cx.simulate_keystrokes("end");
        cx.update(|_, cx| assert_eq!(view.read(cx).changes, 3, "already last"));
        cx.simulate_keystrokes("home");
        draw_choice(cx);
        cx.update(|_, cx| {
            assert_eq!(view.read(cx).selected, 0);
            assert_eq!(view.read(cx).changes, 4);
        });
        assert_selected("full-0", px(40.));
    }

    #[gpui::test]
    fn disabled_choices_use_disabled_roles_and_ignore_clicks(cx: &mut TestAppContext) {
        let (view, cx) = cx.add_window_view(|_, _| FullWidthHarness {
            count: 3,
            selected: 1,
            width: px(420.),
            changes: 0,
            disabled: true,
        });
        draw_choice(cx);
        let third = choice_bounds(cx, "full-choice-option-2").center();
        cx.simulate_click(third, Modifiers::default());
        draw_choice(cx);
        cx.update(|_, cx| assert_eq!(view.read(cx).changes, 0));
        let ink = token(crate::theme::INK);
        for index in 0..3 {
            let paint = painted(&format!("full-{index}"));
            assert_eq!(paint.background, ink.opacity(0.12));
            assert_eq!(paint.foreground, ink.opacity(0.38));
            assert_eq!(paint.hover, paint.background, "no hover layer");
        }
        // The current option keeps its pill shape.
        assert_eq!(painted("full-1").left_radius, px(20.));
    }

    #[test]
    fn arrows_wrap_and_start_when_selection_is_unknown() {
        assert_eq!(destination(3, Some(0), Direction::Next), Some(1));
        assert_eq!(destination(3, Some(2), Direction::Next), Some(0));
        assert_eq!(destination(3, Some(0), Direction::Previous), Some(2));
        assert_eq!(destination(3, None, Direction::Next), Some(0));
        assert_eq!(destination(3, None, Direction::Previous), Some(2));
        assert_eq!(destination(3, Some(1), Direction::First), Some(0));
        assert_eq!(destination(3, Some(1), Direction::Last), Some(2));
        assert_eq!(destination(0, Some(0), Direction::Next), None);
    }
}
