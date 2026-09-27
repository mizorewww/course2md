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
    disabled: bool,
}

#[derive(IntoElement)]
pub struct SingleChoiceGroup {
    id: ElementId,
    label: SharedString,
    value: Option<SharedString>,
    options: Vec<OptionItem>,
    icons: BTreeMap<SharedString, Icon>,
    full_width: bool,
    tabs: bool,
    activate_selected: bool,
    vertical: bool,
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
            tabs: false,
            activate_selected: false,
            vertical: false,
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
                disabled: false,
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
    /// Navigation uses the same surfaces, with tab semantics and optional leading layout.
    pub fn tabs(mut self) -> Self {
        self.tabs = true;
        self
    }
    /// A top-level tab can also return from a detail view to its section root.
    pub fn activate_selected(mut self) -> Self {
        self.activate_selected = true;
        self
    }
    pub fn vertical(mut self) -> Self {
        self.vertical = true;
        self.full_width = true;
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
    /// 整组禁用（预留能力：尚无调用方；用于可用性门控场景，如环境不满足时禁用某组选择）
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
    /// 按值禁用单个选项（预留能力：尚无调用方；用于如「未检测到 GPU 时禁用 GPU 选项」）
    pub fn disable_option(mut self, value: impl AsRef<str>) -> Self {
        for option in &mut self.options {
            if option.value.as_ref() == value.as_ref() {
                option.disabled = true;
            }
        }
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
    options: Vec<OptionItem>,
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
fn destination(enabled: &[usize], current: Option<usize>, direction: Direction) -> Option<usize> {
    if enabled.is_empty() {
        return None;
    }
    let position = current.and_then(|current| enabled.iter().position(|index| *index == current));
    Some(match direction {
        Direction::First => enabled[0],
        Direction::Last => *enabled.last().unwrap(),
        Direction::Next => enabled[position.map_or(0, |index| (index + 1) % enabled.len())],
        Direction::Previous => {
            enabled[position.map_or(enabled.len() - 1, |index| {
                (index + enabled.len() - 1) % enabled.len()
            })]
        }
    })
}
impl Navigation {
    fn navigate(&self, direction: Direction, window: &mut Window, cx: &mut App) {
        let enabled = self
            .options
            .iter()
            .enumerate()
            .filter(|(_, option)| !option.disabled)
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        let current = self
            .focus
            .iter()
            .position(|focus| focus.is_focused(window))
            .or(self.selected);
        let Some(index) = destination(&enabled, current, direction) else {
            return;
        };
        self.focus[index].focus(window, cx);
        if self.selected != Some(index)
            && let Some(on_change) = &self.on_change
        {
            on_change(&self.options[index].value, window, cx);
        }
    }
}
/// Surfaces stay behind the interactive item. Hover/focus cannot erase selection.
fn item_style<T: Styled + StatefulInteractiveElement + gpui::prelude::FluentBuilder>(
    item: T,
    vertical: bool,
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
        .when(vertical, |item| item.w_full())
        .when(!vertical, |item| item.flex_1())
        .hover(|style| style.bg(super::color(super::INK).opacity(0.035)))
        .active(|style| style.bg(super::color(super::INK).opacity(0.075)))
        .focus_visible(|style| style.border_color(super::color(super::ACCENT)))
}
impl RenderOnce for SingleChoiceGroup {
    fn render(mut self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        if self.disabled {
            self.options
                .iter_mut()
                .for_each(|option| option.disabled = true);
        }
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
        let entry = selected
            .filter(|index| !self.options[*index].disabled)
            .or_else(|| self.options.iter().position(|option| !option.disabled));
        let navigation = Navigation {
            options: self.options.clone(),
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
        let inset = 2. * scale;
        let item_height = px(36. * scale);
        let vertical = self.vertical;
        let position = crate::motion::selection_value(
            ElementId::NamedChild(self.id.clone().into(), "selection-position".into()),
            selected.unwrap_or(0) as f32,
            window,
            cx,
        )
        .clamp(0., count.saturating_sub(1) as f32);
        // Text shaping uses the current font and size. Equal slots keep labels still
        // while the selected surface moves, including in content-sized toolbars.
        let mut font = window.text_style().font();
        font.weight = FontWeight::MEDIUM;
        let slot_width = self
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
                let width = window
                    .text_system()
                    .shape_line(option.label.clone(), window.rem_size(), &[run], None)
                    .width;
                let padding = f32::from(rems(12. / 14.).to_pixels(window.rem_size())) * 2.;
                let icon_and_gap = if self.icons.contains_key(&option.value) {
                    f32::from(rems(18. / 14.).to_pixels(window.rem_size()))
                        + f32::from(rems(8. / 14.).to_pixels(window.rem_size()))
                } else {
                    0.
                };
                // TextLayout rounds the shaped label up. Reserve that same
                // width, both actual paddings and the fixed 2px borders, then
                // close each slot before the track is split into equal shares.
                (f32::from(width.ceil()) + padding + 4. + icon_and_gap).ceil()
            })
            .fold(0_f32, f32::max);
        let mut lane = gpui_base::h_flex()
            .relative()
            .w_full()
            .min_w_0()
            .items_stretch()
            .when(vertical, |lane| lane.flex_col().gap(px(4. * scale)))
            .when(cfg!(test), |lane| {
                lane.debug_selector(|| "full-choice-lane".into())
            });
        if selected.is_some() && count > 0 {
            lane = lane.child(
                div()
                    .absolute()
                    .rounded_full()
                    .bg(super::color(super::SURFACE))
                    .border_1()
                    .border_color(super::blend(
                        super::color(super::SURFACE),
                        super::color(super::ACCENT),
                        0.45,
                    ))
                    .shadow(super::shadow_segment_selected())
                    .when(!vertical, |fill| {
                        fill.top_0()
                            .bottom_0()
                            .left(relative(position / count as f32))
                            .w(relative(1. / count as f32))
                    })
                    .when(vertical, |fill| {
                        fill.left_0()
                            .right_0()
                            .top(px(position * (40. * scale)))
                            .h(item_height)
                    })
                    .when(cfg!(test), |fill| {
                        fill.debug_selector(|| "full-choice-indicator".into())
                    }),
            );
        }
        for (index, option) in self.options.into_iter().enumerate() {
            let checked = selected == Some(index);
            let activate_selected = self.activate_selected;
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
                super::color(super::ACCENT_STRONG),
                coverage,
            );
            let debug_kind = if self.full_width { "full" } else { "content" };
            let content = gpui_base::h_flex()
                .min_w_0()
                .gap(rems(8. / 14.))
                .items_center()
                .when(vertical, |row| row.w_full())
                .when_some(self.icons.get(&value).cloned(), |row, icon| {
                    row.child(icon.size(rems(18. / 14.)).flex_shrink_0())
                })
                .child(
                    div()
                        .min_w_0()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .when(cfg!(test), |label| {
                            label.debug_selector(move || {
                                format!("{debug_kind}-choice-label-{index}").into()
                            })
                        })
                        .child(label.clone()),
                );
            if self.tabs {
                let click_focus = focus.clone();
                let tab = item_style(gpui_base::Tab::new(value.clone()), vertical, item_height)
                    .selected(checked)
                    .disabled(option.disabled)
                    .accessibility_label(label)
                    .set_position(index + 1, count)
                    .track_focus(&focus)
                    .text_color(text_color)
                    .child(content)
                    .on_click(move |_, window, cx| {
                        click_focus.focus(window, cx);
                        if (!checked || activate_selected)
                            && let Some(callback) = &callback
                        {
                            callback(&value, window, cx);
                        }
                    });
                lane = lane.child(tab);
            } else {
                let radio = item_style(gpui_base::Radio::new(value.clone()), vertical, item_height)
                    .checked(checked)
                    .disabled(option.disabled)
                    .accessibility_label(label)
                    .set_position(index + 1, count)
                    .track_focus(&focus)
                    .tab_stop(entry == Some(index))
                    .text_color(text_color)
                    .when(option.disabled, |radio| radio.opacity(0.5))
                    .child(content)
                    .when(cfg!(test), |radio| {
                        radio.debug_selector(move || {
                            format!("{debug_kind}-choice-option-{index}").into()
                        })
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
            // 轨道用内嵌面色（与输入框同一 recessed 语义）：4.5% 混合在深色下与卡片底无法区分，
            // 导致「选中段跳出轨道」的错觉（system.md：角色映射失败应在共享层修正）。
            // 页面底色与 INSET 几乎相同，单靠填充在页面背景上轨道会消失（reader 页签、
            // 笔记库筛选、工作台高级选项），补一道发丝边让轨道在任何底上都有定义。
            // 竖向导航没有轨道概念（settings.md：复用胶囊选中即可），轨道近乎方形时
            // rounded_full 会把发丝边画成一个完整圆圈，因此竖向不画轨道外框。
            .when(!vertical, |group| {
                group
                    .rounded_full()
                    .border_1()
                    .border_color(super::color(super::HAIRLINE))
            })
            .bg(if vertical {
                gpui::transparent_black()
            } else {
                super::color(super::INSET).into()
            })
            .when(self.full_width, |group| group.w_full())
            .when(!self.full_width, |group| {
                // 边框占入声明宽度：lane 可用宽度不被发丝边吃掉
                group.w(px(slot_width * count as f32 + inset * 2. + 2.))
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
            assert!(before[index].size.width - px(24. + 4.) >= required_widths[index]);
            assert!(label.left() >= before[index].left() + px(14.));
            assert!(label.right() <= before[index].right() - px(14.));
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
        // 轨道含 1px 发丝边：lane 36 + 内距 2×2 + 边 2×1 = 42
        assert_eq!(choice_bounds(cx, "full-choice-track").size.height, px(42.));
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
        assert_eq!(lane.left() - track.left(), px(3.));
        assert_eq!(track.right() - lane.right(), px(3.));
        assert_eq!(lane.top() - track.top(), px(3.));
        assert_eq!(track.bottom() - lane.bottom(), px(3.));
        assert!(indicator.left() >= lane.left() - px(0.5));
        assert!(indicator.right() <= lane.right() + px(0.5));
        assert_eq!(indicator.top(), lane.top());
        assert_eq!(indicator.bottom(), lane.bottom());
        for index in 0..count {
            let option = choice_bounds(cx, OPTION_SELECTORS[index]);
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
    fn arrows_skip_disabled_values_wrap_and_start_when_selection_is_unknown() {
        let enabled = [0, 2, 3];
        assert_eq!(destination(&enabled, Some(0), Direction::Next), Some(2));
        assert_eq!(destination(&enabled, Some(3), Direction::Next), Some(0));
        assert_eq!(destination(&enabled, Some(0), Direction::Previous), Some(3));
        assert_eq!(destination(&enabled, None, Direction::Next), Some(0));
        assert_eq!(destination(&enabled, None, Direction::Previous), Some(3));
        assert_eq!(destination(&enabled, Some(1), Direction::First), Some(0));
        assert_eq!(destination(&enabled, Some(1), Direction::Last), Some(3));
        assert_eq!(destination(&[], Some(0), Direction::Next), None);
    }
}
