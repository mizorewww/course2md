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
/// Surfaces stay behind the interactive item. Hover/focus cannot erase selection.
fn item_style<T: Styled + StatefulInteractiveElement + gpui::prelude::FluentBuilder>(
    item: T,
    height: Pixels,
) -> T {
    item.relative()
        .flex()
        .items_center()
        .justify_center()
        .min_w_0()
        .h(height)
        .min_h(height)
        .px(rems(12. / 14.))
        .py_0()
        .rounded_full()
        .border_2()
        .border_color(gpui::transparent_black())
        .bg(gpui::transparent_black())
        .text_size(super::TEXT_BODY)
        .font_weight(FontWeight::MEDIUM)
        .flex_1()
        .hover(|style| style.bg(super::color(super::INK).opacity(0.035)))
        .active(|style| style.bg(super::color(super::INK).opacity(0.075)))
        .focus_visible(|style| style.border_color(super::color(super::ACCENT)))
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
        let segmented = self.segmented;
        let inset = if segmented { 0. } else { 2. * scale };
        let item_height = px(if segmented { 40. * scale } else { 36. * scale });
        let position = crate::motion::selection_value(
            ElementId::NamedChild(self.id.clone().into(), "selection-position".into()),
            selected.unwrap_or(0) as f32,
            window,
            cx,
        )
        .clamp(0., count.saturating_sub(1) as f32);
        let icon_size = rems(18. / 14.);
        // A content-sized group gives every option the slot of its longest
        // label, so labels stay still while the selected surface moves.
        // Full-width groups divide the parent instead and need no measurement.
        let content_width = (!self.full_width).then(|| {
            let mut font = window.text_style().font();
            font.weight = FontWeight::MEDIUM;
            let size = self
                .label_text_size
                .unwrap_or(super::TEXT_BODY)
                .to_pixels(window.rem_size());
            let label_width = self
                .options
                .iter()
                .map(|option| {
                    let run = TextRun {
                        len: option.label.len(),
                        font: font.clone(),
                        color: super::color(super::INK).into(),
                        background_color: None,
                        underline: None,
                        strikethrough: None,
                    };
                    let shaped =
                        window
                            .text_system()
                            .shape_line(option.label.clone(), size, &[run], None);
                    f32::from(shaped.width.ceil())
                })
                .fold(0_f32, f32::max);
            let icon_and_gap = f32::from(rems(26. / 14.).to_pixels(window.rem_size()));
            let padding = f32::from(
                rems(if segmented { 8. / 14. } else { 12. / 14. }).to_pixels(window.rem_size()),
            ) * 2.;
            // The 2px focus border on each side belongs to the slot.
            ((label_width + icon_and_gap + padding + 4.).ceil()) * count as f32 + inset * 2. + 2.
        });
        let mut lane = gpui_base::h_flex()
            .relative()
            .w_full()
            .min_w_0()
            .items_stretch()
            .when(segmented, |lane| lane.rounded_full().overflow_hidden())
            .when(cfg!(test), |lane| {
                lane.debug_selector(|| "full-choice-lane".into())
            });
        if selected.is_some() && count > 0 {
            lane = lane.child(
                div()
                    .absolute()
                    .when(!segmented, |fill| fill.rounded_full())
                    // GPUI overflow clips the rectangle, not rounded corners. Paint
                    // the indicator's end caps explicitly; use the animated position
                    // so leaving/entering an end segment never flashes a square cap.
                    .when(segmented, |fill| {
                        let radius = f32::from(item_height) / 2.;
                        let left = (1. - position).clamp(0., 1.);
                        let right =
                            (1. - (count.saturating_sub(1) as f32 - position)).clamp(0., 1.);
                        fill.rounded_l(px(radius * left))
                            .rounded_r(px(radius * right))
                    })
                    // M3 segmented buttons and navigation share the tonal
                    // secondary container as the selected surface.
                    .bg(super::color(super::SECONDARY_CONTAINER))
                    .top_0()
                    .bottom_0()
                    .left(relative(position / count as f32))
                    .w(relative(1. / count as f32))
                    .when(cfg!(test), |fill| {
                        fill.debug_selector(|| "full-choice-indicator".into())
                    }),
            );
        }
        if segmented && count > 1 {
            for index in 1..count {
                lane = lane.child(
                    div()
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .left(relative(index as f32 / count as f32))
                        .w(px(1.))
                        .bg(super::color(super::CONTROL)),
                );
            }
        }
        for (index, option) in self.options.into_iter().enumerate() {
            let checked = selected == Some(index);
            let focus = handles[index].clone().tab_stop(entry == Some(index));
            let callback = self.on_change.clone();
            let value = option.value.clone();
            let label = option.label.clone();
            let coverage = if selected.is_some() {
                (1. - (position - index as f32).abs()).clamp(0., 1.)
            } else {
                0.
            };
            let text_color = super::blend(
                super::color(super::GRAY),
                super::color(super::ON_SECONDARY_CONTAINER),
                coverage,
            );
            let debug_kind = if self.full_width { "full" } else { "content" };
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
                let tab = item_style(gpui_base::Tab::new(value.clone()), item_height)
                    .when_some(self.label_text_size, |item, size| item.text_size(size))
                    .when(segmented, |item| {
                        item.px(rems(8. / 14.))
                            .rounded_none()
                            .when(index == 0, |item| item.rounded_l_full())
                            .when(index + 1 == count, |item| item.rounded_r_full())
                    })
                    .selected(checked)
                    .disabled(disabled)
                    .accessibility_label(label)
                    .set_position(index + 1, count)
                    .track_focus(&focus)
                    .text_color(text_color)
                    .child(content)
                    .on_click(move |_, window, cx| {
                        click_focus.focus(window, cx);
                        if !checked && let Some(callback) = &callback {
                            callback(&value, window, cx);
                        }
                    });
                lane = lane.child(tab);
            } else {
                let radio = item_style(gpui_base::Radio::new(value.clone()), item_height)
                    .when_some(self.label_text_size, |item, size| item.text_size(size))
                    .when(segmented, |item| {
                        item.px(rems(8. / 14.))
                            .rounded_none()
                            .when(index == 0, |item| item.rounded_l_full())
                            .when(index + 1 == count, |item| item.rounded_r_full())
                    })
                    .checked(checked)
                    .disabled(disabled)
                    .accessibility_label(label)
                    .set_position(index + 1, count)
                    .track_focus(&focus)
                    .text_color(text_color)
                    .when(disabled, |radio| radio.opacity(0.5))
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
            .p(px(inset))
            .rounded_full()
            .border_1()
            .border_color(super::color(if segmented {
                super::CONTROL
            } else {
                super::HAIRLINE
            }))
            .map(|group| match content_width {
                Some(width) => group.w(px(width)),
                None => group.w_full(),
            })
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
            if self.full_width {
                reveal
            } else {
                reveal.inline()
            }
            .into_any_element()
        } else {
            group.into_any_element()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Direction, SingleChoiceGroup, destination};
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

    #[gpui::test]
    fn connected_segments_fill_track_and_keep_labels_still_on_selection(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_reduce_motion(true));
        let (view, cx) = cx.add_window_view(|_, _| SegmentedHarness {
            selected: 0,
            changes: 0,
        });
        draw_choice(cx);
        let lane = choice_bounds(cx, "full-choice-lane");
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
        let options = [
            "full-choice-option-0",
            "full-choice-option-1",
            "full-choice-option-2",
        ]
        .map(|selector| choice_bounds(cx, selector));
        for index in 0..3 {
            // Each label follows its icon, and every segment has the same height.
            assert!(icon_before[index].right() <= before[index].left());
            assert!((options[index].size.height - options[0].size.height).abs() < px(0.5));
            let content_center = (icon_before[index].left() + before[index].right()) / 2.;
            assert!((content_center - options[index].center().x).abs() <= px(0.5));
            assert!((icon_before[index].center().y - options[index].center().y).abs() <= px(0.5));
            assert!((before[index].center().y - options[index].center().y).abs() <= px(0.5));
            assert!((before[index].top() - before[0].top()).abs() < px(0.5));
        }
        // Medium reference: a 360px panel minus two 24px insets must show the full label.
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
        let last = choice_bounds(cx, "full-choice-option-2");
        let first = choice_bounds(cx, "full-choice-option-0");
        assert!((first.size.width - last.size.width).abs() < px(0.5));
        cx.simulate_click(last.center(), Modifiers::default());
        draw_choice(cx);
        cx.update(|_, cx| {
            assert_eq!(view.read(cx).selected, 2);
            assert_eq!(view.read(cx).changes, 1);
        });
        let fill = choice_bounds(cx, "full-choice-indicator");
        assert!((fill.left() - last.left()).abs() < px(0.5));
        assert_eq!(fill.top(), lane.top());
        assert_eq!(fill.bottom(), lane.bottom());
        for (selector, bounds) in labels.into_iter().zip(before) {
            assert_eq!(choice_bounds(cx, selector), bounds);
        }
        for (selector, bounds) in icons.into_iter().zip(icon_before) {
            assert_eq!(choice_bounds(cx, selector), bounds);
        }
        cx.simulate_click(last.center(), Modifiers::default());
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
        // laid-out labels, including the slightly longer 0.6B option that used
        // to lose a fraction of a pixel when the track width was rounded.
        for index in 0..MODEL_LABELS.len() {
            let label = choice_bounds(cx, label_selectors[index]);
            assert!(
                label.size.width >= required_widths[index],
                "{} was narrowed below its full shaped width",
                MODEL_LABELS[index]
            );
            // The label sits inside its segment with equal room on both sides,
            // whatever the segment padding is.
            let leading = label.left() - before[index].left();
            let trailing = before[index].right() - label.right();
            assert!(leading > px(0.) && trailing > px(0.));
            assert!((leading - trailing).abs() <= px(1.));
        }
        cx.simulate_click(before[1].center(), Modifiers::default());
        draw_choice(cx);
        for index in 0..MODEL_LABELS.len() {
            assert_eq!(choice_bounds(cx, option_selectors[index]), before[index]);
            assert!(choice_bounds(cx, label_selectors[index]).size.width >= required_widths[index]);
        }
    }

    #[gpui::test]
    fn content_width_choices_move_the_indicator_without_shifting_labels(cx: &mut TestAppContext) {
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
        let before = selectors.map(|selector| choice_bounds(cx, selector));
        let initial = choice_bounds(cx, "full-choice-indicator");
        assert!((before[0].size.width - before[2].size.width).abs() < px(0.5));
        cx.simulate_click(before[2].center(), Modifiers::default());
        draw_choice(cx);
        assert_eq!(choice_bounds(cx, "full-choice-indicator"), initial);
        cx.executor().advance_clock(Duration::from_millis(70));
        draw_choice(cx);
        let intermediate = choice_bounds(cx, "full-choice-indicator");
        assert!(intermediate.left() > initial.left());
        assert!(intermediate.left() < before[2].left());
        for (selector, bounds) in selectors.into_iter().zip(before) {
            assert_eq!(choice_bounds(cx, selector), bounds);
        }
        cx.executor().advance_clock(Duration::from_millis(230));
        draw_choice(cx);
        assert!(
            (choice_bounds(cx, "full-choice-indicator").left() - before[2].left()).abs() < px(0.5)
        );
        cx.simulate_mouse_move(before[2].center(), None, Modifiers::default());
        draw_choice(cx);
        assert_eq!(
            choice_bounds(cx, "full-choice-indicator").size,
            initial.size
        );
        cx.update(|window, cx| {
            assert_eq!(view.read(cx).changes, 1);
            window.simulate_next_frame(cx);
            window.refresh();
            window.draw(cx).clear(cx);
            assert_eq!(window.simulate_next_frame(cx), 0);
        });
    }

    fn assert_inside_lane(cx: &mut VisualTestContext, count: usize, width: Pixels) {
        let track = choice_bounds(cx, "full-choice-track");
        let lane = choice_bounds(cx, "full-choice-lane");
        let indicator = choice_bounds(cx, "full-choice-indicator");
        assert_eq!(track.size.width, width);
        assert!(track.contains(&lane.origin) && track.contains(&lane.bottom_right()));
        assert!(indicator.left() >= lane.left() - px(0.5));
        assert!(indicator.right() <= lane.right() + px(0.5));
        assert_eq!(indicator.top(), lane.top());
        assert_eq!(indicator.bottom(), lane.bottom());
        for selector in &OPTION_SELECTORS[..count] {
            let option = choice_bounds(cx, selector);
            assert!((option.size.width - lane.size.width / count as f32).abs() <= px(0.5));
            assert!((indicator.size.width - option.size.width).abs() <= px(0.5));
        }
    }

    fn exercise_full_width_motion(cx: &mut TestAppContext, count: usize) {
        let (view, cx) = cx.add_window_view(|_, _| FullWidthHarness {
            count,
            selected: 0,
            width: px(420.),
            changes: 0,
        });
        draw_choice(cx);
        assert_inside_lane(cx, count, px(420.));
        let start = choice_bounds(cx, "full-choice-indicator");
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
        assert_eq!(
            choice_bounds(cx, "full-choice-indicator").left(),
            start.left()
        );
        cx.simulate_click(destination.center(), Modifiers::default());
        cx.update(|_, cx| assert_eq!(view.read(cx).changes, 1, "same choice does not save twice"));

        cx.executor().advance_clock(Duration::from_millis(70));
        draw_choice(cx);
        let middle = choice_bounds(cx, "full-choice-indicator");
        assert!(
            middle.left() > start.left(),
            "selection must leave its starting cell"
        );
        assert!(
            middle.left() < destination.left(),
            "selection must expose an intermediate frame"
        );
        for (index, before) in options.into_iter().enumerate() {
            assert_eq!(choice_bounds(cx, OPTION_SELECTORS[index]), before);
        }
        draw_choice(cx);
        assert_eq!(
            choice_bounds(cx, "full-choice-indicator"),
            middle,
            "ordinary redraw must not restart motion"
        );

        // Reverse before settling, then resize without advancing time. Neither
        // operation may teleport the current selection or use old geometry.
        cx.simulate_click(start.center(), Modifiers::default());
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert_eq!(
            choice_bounds(cx, "full-choice-indicator").left(),
            middle.left()
        );
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.width = px(284.);
                cx.notify();
            });
            window.draw(cx).clear(cx);
        });
        assert_inside_lane(cx, count, px(284.));
        let resized = choice_bounds(cx, "full-choice-indicator");
        cx.executor().advance_clock(Duration::from_millis(35));
        draw_choice(cx);
        let returning = choice_bounds(cx, "full-choice-indicator");
        assert!(returning.left() < resized.left());
        assert!(returning.left() > choice_bounds(cx, "full-choice-lane").left());
        assert_inside_lane(cx, count, px(284.));

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
        assert_eq!(
            choice_bounds(cx, "full-choice-indicator").left(),
            choice_bounds(cx, "full-choice-lane").left()
        );
    }

    #[gpui::test]
    fn full_width_choices_move_inside_current_geometry_and_settle(cx: &mut TestAppContext) {
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
        });
        draw_choice(cx);
        let destination = choice_bounds(cx, "full-choice-option-3");
        cx.simulate_click(destination.center(), Modifiers::default());
        cx.update(|window, cx| {
            assert_eq!(view.read(cx).selected, 3);
            window.draw(cx).clear(cx);
            assert_eq!(window.simulate_next_frame(cx), 0);
        });
        assert_eq!(
            choice_bounds(cx, "full-choice-indicator").left(),
            destination.left()
        );
        assert_inside_lane(cx, 4, px(420.));
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
