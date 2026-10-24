//! Four independently saved setting groups and a shared service editor.
use super::*;
#[path = "generation_settings.rs"]
mod generation_settings;
#[path = "model_diagnostics.rs"]
pub(crate) mod model_diagnostics;
#[path = "service_editor.rs"]
mod service_editor;
#[path = "service_form.rs"]
pub(crate) mod service_form;
use crate::credentials::Secret;
use crate::preferences::{
    ApplicationPreferences, Authentication, BindingScope, GenerationPreferences, PreferenceGroup,
    ServiceDraft, ServiceProtocol, ServicePurpose, ServiceVersion,
};
use crate::service_test::{self, TestKind};
use crate::theme::*;
use anyhow::{Context as _, anyhow};
use course2md::execution::Component;
use gpui_component::{
    button::*,
    input::{InputContentType, Textarea, TextareaState},
    menu::{ContextMenuExt, DropdownMenu, PopupMenu, PopupMenuItem},
    switch::Switch,
};
use service_form::{FormField, FormOwner, ServiceForm};
use std::rc::Rc;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

actions!(
    course2md_settings,
    [
        NextSettingsTab,
        PreviousSettingsTab,
        FirstSettingsTab,
        LastSettingsTab
    ]
);

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum EditField {
    LocalModel,
    Languages,
}

struct ServiceEditor {
    form: ServiceForm,
    target: Option<String>,
    repair_task: Option<(String, Vec<Component>)>,
    scroll: ScrollHandle,
    also_default: bool,
    return_focus: Option<FocusHandle>,
    status: Option<String>,
    test_kind: TestKind,
    test_running: Option<Arc<AtomicBool>>,
    evidence: Option<preferences::ServiceTestEvidence>,
    received_tests: BTreeMap<String, preferences::ServiceTestEvidence>,
    pending_binding: Option<ServiceVersion>,
    saved_configuration: Option<ServiceVersion>,
    test_details_open: bool,
    save_failed: bool,
    /// A credential is being written to or checked in the system vault.
    vault_busy: bool,
    /// The settings category the inline editor was opened from (e.g. 生成笔记);
    /// closing it returns there instead of leaving the user on 服务与账号.
    origin_tab: Option<SettingsTab>,
}

pub struct OrdinaryPreferenceIssue {
    pub group: PreferenceGroup,
    pub message: String,
    pub can_retry: bool,
}

/// What continues once a typed key has been stored and the draft committed.
#[derive(Clone, Copy)]
pub(crate) enum AfterDraftSaved {
    Test,
    Publish { continue_repair: bool },
}

/// How long a success notice such as 已保存 stays.
const SUCCESS_NOTICE_SECS: u64 = 6;

/// Pause after the last keystroke before typed settings text is written.
const TEXT_SAVE_DELAY_MS: u64 = 600;

pub(crate) struct State {
    model_diagnostics: model_diagnostics::State,
    tab_focus: [FocusHandle; SettingsTab::ALL.len()],
    /// Settings search in the navigation pane.
    search: Entity<InputState>,
    inputs: BTreeMap<EditField, Entity<InputState>>,
    prompt: Entity<TextareaState>,
    editor: Option<ServiceEditor>,
    initialized: bool,
    show_prompt: bool,
    language_details_open: bool,
    asr_details_open: bool,
    model_details_open: bool,
    diagnostics_details_open: bool,
    feedback: BTreeMap<PreferenceGroup, (String, bool)>,
    feedback_expiry: BTreeMap<PreferenceGroup, Task<()>>,
    feedback_details: BTreeMap<PreferenceGroup, String>,
    expanded_feedback: std::collections::BTreeSet<PreferenceGroup>,
    generation_block_messages: std::collections::BTreeSet<String>,
    pub(crate) pending_generation: Option<GenerationPreferences>,
    pub(crate) pending_application: Option<ApplicationPreferences>,
    /// Typed text not yet written to disk; flushed after a pause, on blur and on exit.
    unsaved_text: Option<GenerationPreferences>,
    text_flush: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl State {
    pub fn new(window: &mut Window, cx: &mut Context<Desktop>) -> Self {
        cx.bind_keys([
            KeyBinding::new("right", NextSettingsTab, Some("SettingsTabs")),
            KeyBinding::new("down", NextSettingsTab, Some("SettingsTabs")),
            KeyBinding::new("left", PreviousSettingsTab, Some("SettingsTabs")),
            KeyBinding::new("up", PreviousSettingsTab, Some("SettingsTabs")),
            KeyBinding::new("home", FirstSettingsTab, Some("SettingsTabs")),
            KeyBinding::new("end", LastSettingsTab, Some("SettingsTabs")),
        ]);
        let inputs: BTreeMap<_, _> = [EditField::LocalModel, EditField::Languages]
            .into_iter()
            .map(|field| {
                let placeholder = match field {
                    EditField::Languages => {
                        course2md::tr!("按优先顺序填写语言代码，如 zh-Hans, en")
                    }
                    EditField::LocalModel => "",
                };
                (
                    field,
                    cx.new(|cx| InputState::new(window, cx).placeholder(placeholder)),
                )
            })
            .collect();
        let mut subscriptions = Vec::new();
        for (field, input) in &inputs {
            let field = *field;
            subscriptions.push(
                cx.subscribe_in(input, window, move |this, _, event, _, cx| match event {
                    InputEvent::Change => this.setting_input_changed(field, cx),
                    InputEvent::Blur => this.flush_generation_text(cx),
                    _ => {}
                }),
            );
        }
        let search = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(course2md::tr!("搜索设置"))
                .clean_on_escape()
        });
        subscriptions.push(cx.subscribe_in(&search, window, |this, _, event, _, cx| {
            if let InputEvent::Change = event {
                this.settings_search_changed(cx);
            }
        }));
        let prompt = cx.new(|cx| TextareaState::new(window, cx).rows(6));
        subscriptions.push(cx.subscribe(&prompt, |this, _, event, cx| match event {
            InputEvent::Change => this.save_prompt_draft(cx),
            InputEvent::Blur => this.flush_generation_text(cx),
            _ => {}
        }));
        Self {
            model_diagnostics: Default::default(),
            tab_focus: std::array::from_fn(|_| cx.focus_handle()),
            search,
            inputs,
            prompt,
            editor: None,
            initialized: false,
            show_prompt: false,
            language_details_open: false,
            asr_details_open: false,
            model_details_open: false,
            diagnostics_details_open: false,
            feedback: BTreeMap::new(),
            feedback_expiry: BTreeMap::new(),
            feedback_details: BTreeMap::new(),
            expanded_feedback: Default::default(),
            generation_block_messages: Default::default(),
            pending_generation: None,
            pending_application: None,
            unsaved_text: None,
            text_flush: None,
            _subscriptions: subscriptions,
        }
    }
}

pub(super) fn field_label(
    id: impl Into<ElementId>,
    value: impl Into<SharedString>,
) -> Stateful<Div> {
    accessible_text(id, value)
        .text_size(TEXT_SETTINGS_BODY)
        .font_weight(FontWeight::SEMIBOLD)
}

fn setting_label(id: impl Into<ElementId>, value: impl Into<SharedString>) -> Div {
    div().child(tile_label(id, value))
}

/// The main label of a settings tile: 17px medium, as on the canvas.
pub(crate) const TEXT_TILE_LABEL: Rems = rems(17. / 14.);

pub(crate) fn tile_label(
    id: impl Into<ElementId>,
    value: impl Into<SharedString>,
) -> Stateful<Div> {
    accessible_text(id, value)
        .text_size(TEXT_TILE_LABEL)
        .line_height(rems(24. / 14.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(color(INK))
}

/// Supporting text under a tile's label: 14px regular gray.
pub(crate) fn tile_support(
    id: impl Into<ElementId>,
    value: impl Into<SharedString>,
) -> Stateful<Div> {
    accessible_text(id, value)
        .min_w_0()
        .whitespace_normal()
        .text_size(TEXT_SETTINGS_AUX)
        .line_height(rems(20. / 14.))
        .font_weight(FontWeight::NORMAL)
        .text_color(color(GRAY))
}

/// A tile's text column: its label and, when present, the value or
/// explanation under it.
pub(crate) fn tile_text(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    support: Option<SharedString>,
) -> Div {
    let id = id.into();
    v_flex()
        .flex_1()
        .min_w_0()
        .gap(rems(2. / 14.))
        .child(tile_label(id.clone(), label))
        .when_some(support, |column, support| {
            column.child(tile_support(
                SharedString::from(format!("{id:?}-support")),
                support,
            ))
        })
}

/// The trailing chevron of a tile that opens something.
pub(crate) fn tile_chevron() -> Icon {
    icons::chevron_right()
        .size(rems(20. / 14.))
        .flex_shrink_0()
        .text_color(color(GRAY))
}

/// An explanation inside a tile: the information icon and 14px gray text.
pub(crate) fn tile_info(id: impl Into<ElementId>, text: impl Into<SharedString>) -> Div {
    h_flex()
        .w_full()
        .min_w_0()
        .items_start()
        .gap(rems(10. / 14.))
        .child(
            div()
                .flex()
                .flex_shrink_0()
                .h(rems(20. / 14.))
                .items_center()
                .child(icons::info().size(rems(18. / 14.)).text_color(color(GRAY))),
        )
        .child(tile_support(id, text).flex_1())
}

/// The corner radii of a tile at a position in its connected group.
fn tile_corners<E: Styled>(element: E, outer: [bool; 4]) -> E {
    let radius = |outside: bool| {
        if outside {
            TILE_RADIUS
        } else {
            TILE_INNER_RADIUS
        }
    };
    element
        .rounded_tl(radius(outer[0]))
        .rounded_tr(radius(outer[1]))
        .rounded_br(radius(outer[2]))
        .rounded_bl(radius(outer[3]))
}

/// A whole tile that acts: the surface with M3 state layers, the tile's
/// corners and its padding. Callers add the label column and trailing items.
pub(crate) fn tile_button(id: impl Into<ElementId>, outer: [bool; 4], cx: &App) -> Button {
    let surface = color(SURFACE);
    let ink = color(INK);
    tile_corners(
        Button::new(id)
            .custom(
                ButtonCustomVariant::new(cx)
                    .color(surface.into())
                    .foreground(ink.into())
                    .hover(theme::blend(surface, ink, 0.08).into())
                    .active(theme::blend(surface, ink, 0.10).into())
                    .shadow(false),
            )
            .bg(surface)
            .w_full()
            .h_auto()
            .min_h(rems(64. / 14.))
            .px(SETTING_ITEM_INSET)
            .py(rems(16. / 14.))
            .gap(rems(16. / 14.))
            .justify_start()
            .text_left()
            .whitespace_normal()
            .font_weight(FontWeight::NORMAL),
        outer,
    )
}

/// A complete preference owns its label, value, help and trailing actions.
/// Controls align to this item's trailing edge and reflow as a unit.
pub(super) fn settings_row(
    id: impl Into<ElementId>,
    label: &'static str,
    hint: &'static str,
    control: impl IntoElement,
) -> Div {
    let id = id.into();
    // Label and its hint read as one unit, centered against the control.
    let row = h_flex()
        .w_full()
        .min_w_0()
        .min_h(CONTROL_HEIGHT)
        .items_center()
        .flex_wrap()
        .gap_4()
        .child(
            tile_text(id.clone(), label, (!hint.is_empty()).then(|| hint.into()))
                .flex_basis(rems(120. / 14.))
                .max_w_full(),
        )
        .child(
            h_flex()
                .flex_1()
                .flex_grow(3.)
                .flex_basis(rems(400. / 14.))
                .max_w_full()
                .min_w_0()
                .min_h(CONTROL_HEIGHT)
                .items_center()
                .justify_end()
                .child(control),
        );
    setting_surface().child(row)
}

/// A tile that expands content in place below its row.
pub(crate) fn tile_disclosure(
    id: impl Into<ElementId>,
    outer: [bool; 4],
    open: bool,
    cx: &App,
) -> Button {
    tile_button(id, outer, cx).selected(open)
}

/// The chevron of an in-place disclosure; it turns over when open.
pub(crate) fn disclosure_chevron(open: bool) -> Icon {
    if open {
        icons::chevron_up()
    } else {
        icons::chevron_down()
    }
    .size(rems(20. / 14.))
    .flex_shrink_0()
    .text_color(color(GRAY))
}

/// A small outlined fact chip beside a tile's label, e.g. the local engine.
pub(crate) fn outlined_chip(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
) -> Stateful<Div> {
    accessible_text(id, label)
        .flex_shrink_0()
        .px(px(10.))
        .py(px(4.))
        .rounded(RADIUS_SMALL)
        .border_1()
        .border_color(color(HAIRLINE))
        .text_size(TEXT_AUX)
        .line_height(rems(16. / 14.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(color(GRAY))
        .whitespace_nowrap()
}

/// A setting's hint, identified after its row.
fn setting_hint(id: &ElementId, hint: &'static str) -> Stateful<Div> {
    theme::row_hint(
        SharedString::from(format!("{id:?}-hint")),
        hint,
        RowScale::Settings,
    )
}

/// A full-width control that labels itself (an outlined field), with an
/// optional hint below it.
pub(super) fn settings_outlined_row(
    id: impl Into<ElementId>,
    hint: &'static str,
    control: impl IntoElement,
) -> Div {
    let id = id.into();
    setting_surface().child(
        v_flex()
            .w_full()
            .min_w_0()
            .gap_2()
            .child(control)
            .when(!hint.is_empty(), |view| view.child(setting_hint(&id, hint))),
    )
}

/// Editing fields use a top label: errors and related actions stay with the
/// input without forcing long values into the preference label/control grid.
pub(super) fn settings_field_row(
    id: impl Into<ElementId>,
    label: &'static str,
    field: impl IntoElement,
) -> Div {
    let id = id.into();
    v_flex()
        .w_full()
        .min_w_0()
        .gap_2()
        .child(setting_label(id, label))
        .child(div().w_full().min_w_0().child(field))
}

/// A setting whose control sits under its label.
pub(super) fn settings_stacked_row(
    id: impl Into<ElementId>,
    label: &'static str,
    hint: &'static str,
    control: impl IntoElement,
) -> Div {
    let id = id.into();
    setting_surface().child(
        v_flex()
            .w_full()
            .min_w_0()
            .gap(rems(1.))
            .child(tile_label(id.clone(), label))
            .child(control)
            .when(!hint.is_empty(), |view| {
                view.child(tile_info(SharedString::from(format!("{id:?}-hint")), hint))
            }),
    )
}

/// A preference switch: label and explanation, the switch on the tile's edge.
pub(super) fn settings_switch_row(label: &'static str, hint: &'static str, control: Switch) -> Div {
    h_flex()
        .w_full()
        .min_w_0()
        .min_h(rems(32. / 14.))
        .items_center()
        .gap(rems(16. / 14.))
        .child(tile_text(
            SharedString::from(format!("switch-label-{label}")),
            label,
            (!hint.is_empty()).then(|| hint.into()),
        ))
        .child(
            div()
                .flex_shrink_0()
                .child(preference_switch(control).accessibility_label(label)),
        )
}

/// Compact outlined chips used for capabilities and license links in settings.
pub(super) fn settings_chip<T: Styled>(element: T) -> T {
    element
        .rounded(RADIUS_SMALL)
        .h(rems(32. / 14.))
        .min_h(rems(32. / 14.))
        .px(rems(8. / 14.))
        .border_1()
        .border_color(color(HAIRLINE))
        .bg(gpui::transparent_black())
        .font_weight(FontWeight::NORMAL)
}

/// Status rows inside an existing card: label on the shared axis, value trailing.
/// Do not wrap these in `settings_row`; that would give each item its own surface.
pub(super) fn settings_status_row(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    value: impl IntoElement,
) -> Div {
    h_flex()
        .w_full()
        .min_w_0()
        .min_h(CONTROL_HEIGHT)
        .items_center()
        .gap_4()
        .child(field_label(id, label).flex_1().min_w_0().max_w_full())
        .child(
            h_flex()
                .flex_shrink_0()
                .items_center()
                .justify_end()
                .child(value),
        )
}

/// One setting's content box inside a settings card; the card owns the surface.
pub(super) fn setting_surface() -> Div {
    v_flex()
        .w_full()
        .min_w_0()
        .flex_shrink_0()
        .px(SETTING_ITEM_INSET)
        .py(rems(16. / 14.))
        .gap_2()
}

/// Diagnostic facts as on the canvas: an accent heading over one inset block of
/// label/value rows separated by hairlines. `after` holds actions that follow
/// the block rather than belong to a row.
#[derive(IntoElement)]
pub(crate) struct DetailGroup {
    id: ElementId,
    icon: Icon,
    title: &'static str,
    rows: Vec<AnyElement>,
    after: Vec<AnyElement>,
}

pub(super) fn settings_detail_group(
    id: impl Into<ElementId>,
    icon: Icon,
    title: &'static str,
) -> DetailGroup {
    DetailGroup {
        id: id.into(),
        icon,
        title,
        rows: Vec::new(),
        after: Vec::new(),
    }
}

impl DetailGroup {
    /// One fact: a quiet label on a common ruler and its value.
    pub(crate) fn row(
        mut self,
        id: impl Into<ElementId>,
        label: impl Into<SharedString>,
        value: impl IntoElement,
    ) -> Self {
        self.rows.push(
            h_flex()
                .w_full()
                .min_w_0()
                .flex_wrap()
                .items_center()
                .gap_x_4()
                .gap_y_1()
                .child(
                    accessible_text(id, label)
                        .flex_basis(rems(180. / 14.))
                        .flex_shrink_0()
                        .text_size(TEXT_SETTINGS_AUX)
                        .text_color(color(GRAY)),
                )
                .child(
                    div()
                        .flex_1()
                        .flex_basis(rems(200. / 14.))
                        .min_w_0()
                        .text_size(TEXT_SETTINGS_AUX)
                        .child(value),
                )
                .into_any_element(),
        );
        self
    }

    pub(crate) fn after(mut self, element: impl IntoElement) -> Self {
        self.after.push(element.into_any_element());
        self
    }
}

impl ParentElement for DetailGroup {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.rows.extend(elements);
    }
}

impl RenderOnce for DetailGroup {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        // Label/value rows separated by hairlines, as on the canvas.
        let mut block = v_flex().w_full().min_w_0();
        for (index, row) in self.rows.into_iter().enumerate() {
            block = block.child(
                div()
                    .w_full()
                    .min_w_0()
                    .min_h(rems(48. / 14.))
                    .py(rems(8. / 14.))
                    .flex()
                    .items_center()
                    .when(index > 0, |row| {
                        row.border_t_1().border_color(color(HAIRLINE))
                    })
                    .child(row),
            );
        }
        v_flex()
            .w_full()
            .min_w_0()
            .gap_2()
            .child(theme::section_heading(self.id, self.icon, self.title))
            .child(block)
            .children(self.after)
    }
}

/// Shared skeleton of the Bilibili, Codex and Ollama account sections. The title
/// row owns one trailing slot (busy spinner, status badge, then actions) that stays
/// on the title line when it fits and wraps below as a unit when narrow. Each
/// connection's state machine stays in its module; this is render-only.
pub(crate) struct AccountCard {
    pub(crate) id: &'static str,
    pub(crate) icon: Icon,
    pub(crate) tint: Tint,
    pub(crate) title: &'static str,
    pub(crate) status: (BadgeKind, &'static str),
    pub(crate) detail: Option<String>,
    pub(crate) busy: bool,
    /// Lines under the status: element id suffix, text, and whether it is a failure.
    pub(crate) lines: Vec<(&'static str, String, bool)>,
    pub(crate) actions: Vec<AnyElement>,
}

impl AccountCard {
    pub(crate) fn render(self, cx: &App) -> Div {
        let id = self.id;
        let (kind, label) = self.status;
        let identity = h_flex()
            .gap_2()
            .items_center()
            .flex_wrap()
            .child(tile_label(
                SharedString::from(format!("{id}-heading")),
                self.title,
            ))
            .when(self.busy, |row| {
                row.child(crate::motion::spinner(
                    SharedString::from(format!("{id}-busy")),
                    cx,
                ))
            })
            .child(badge(kind).child(label));
        let line = move |suffix: &str, message: String, error: bool| {
            tile_support(SharedString::from(format!("{id}-{suffix}")), message)
                .text_color(color(if error { DANGER } else { GRAY }))
        };
        resource_layout(
            tinted_icon(self.icon, self.tint, px(48.)),
            v_flex()
                .child(identity)
                .when_some(self.detail, |view, detail| {
                    view.child(line("detail", detail, false))
                })
                .children(
                    self.lines
                        .into_iter()
                        .map(|(suffix, message, error)| line(suffix, message, error)),
                ),
            h_flex()
                .gap_2()
                .items_center()
                .flex_wrap()
                .max_w_full()
                .children(self.actions),
        )
    }
}

/// A resource item's layout (an account or a library location): its icon,
/// its text column, and trailing status or actions on the item's edge.
pub(crate) fn resource_layout(icon: impl IntoElement, text: Div, trailing: Div) -> Div {
    h_flex()
        .w_full()
        .min_w_0()
        .gap(px(16.))
        .items_center()
        .child(icon)
        .child(text.flex_1().min_w_0().gap_1())
        .child(trailing.flex_shrink_0())
}

/// One row of a resource list (an account or a saved service): icon, text,
/// status and a ⋯ menu, on the shared insets that line the menus up.
fn resource_row(id: impl Into<ElementId>) -> Stateful<Div> {
    h_flex()
        .id(id)
        .w_full()
        .min_w_0()
        .gap(rems(16. / 14.))
        .items_center()
        .py(rems(16. / 14.))
        .pl(SETTING_ITEM_INSET)
        .pr(rems(12. / 14.))
}

/// Diagnostic values begin on a common reading axis. They can wrap and grow
/// vertically without moving short values to the far edge of the window.
pub(super) fn settings_detail_row(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    value: impl IntoElement,
) -> Div {
    h_flex()
        .w_full()
        .min_w_0()
        .min_h(rems(28. / 14.))
        .items_start()
        .flex_wrap()
        .gap_x_4()
        .gap_y_1()
        .child(
            field_label(id, label)
                .min_w(rems(96. / 14.))
                .max_w_full()
                .flex_shrink_0()
                .whitespace_normal(),
        )
        .child(
            v_flex()
                .flex_1()
                .flex_basis(rems(240. / 14.))
                .min_w_0()
                .items_start()
                .child(value),
        )
}

pub(super) fn settings_value(
    id: impl Into<ElementId>,
    value: impl Into<SharedString>,
) -> Stateful<Div> {
    accessible_text(id, value)
        .w_full()
        .min_w_0()
        .whitespace_normal()
        .text_size(TEXT_SETTINGS_BODY)
        .text_color(color(INK))
}
/// Settings categories in navigation order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SettingsTab {
    Appearance,
    Generation,
    Services,
    Storage,
    Application,
}

impl SettingsTab {
    pub(crate) const ALL: [Self; 5] = [
        Self::Appearance,
        Self::Generation,
        Self::Services,
        Self::Storage,
        Self::Application,
    ];

    fn index(self) -> usize {
        self as usize
    }
    fn label(self) -> &'static str {
        match self {
            Self::Appearance => course2md::tr!("外观"),
            Self::Generation => course2md::tr!("生成笔记"),
            Self::Services => course2md::tr!("服务与账号"),
            Self::Storage => course2md::tr!("存储"),
            Self::Application => course2md::tr!("应用"),
        }
    }
    /// The narrow rail has room for two characters.
    fn short_label(self) -> &'static str {
        match self {
            Self::Generation => course2md::tr!("笔记"),
            Self::Services => course2md::tr!("服务"),
            other => other.label(),
        }
    }
    fn icon(self) -> Icon {
        match self {
            Self::Appearance => icons::palette(),
            Self::Generation => icons::article(),
            Self::Services => icons::cloud(),
            Self::Storage => icons::storage(),
            Self::Application => icons::info(),
        }
    }
    /// What the category holds, under its name in the navigation.
    fn subtitle(self) -> &'static str {
        match self {
            Self::Appearance => course2md::tr!("主题、语言、文字大小"),
            Self::Generation => course2md::tr!("字幕、语音识别、AI 处理"),
            Self::Services => course2md::tr!("来源账号、AI 与语音服务"),
            Self::Storage => course2md::tr!("笔记库位置"),
            Self::Application => course2md::tr!("运行检查、版本、许可"),
        }
    }
    fn tint(self) -> Tint {
        match self {
            Self::Appearance => Tint::Orange,
            Self::Generation => Tint::Blue,
            Self::Services => Tint::Teal,
            Self::Storage => Tint::Amber,
            Self::Application => Tint::Green,
        }
    }
    /// The labels of the settings a category contains, for settings search.
    fn keywords(self) -> Vec<&'static str> {
        match self {
            Self::Appearance => vec![
                course2md::tr!("主题"),
                course2md::tr!("外观模式"),
                course2md::tr!("浅色主题"),
                course2md::tr!("深色主题"),
                course2md::tr!("界面偏好"),
                course2md::tr!("界面语言"),
                course2md::tr!("界面文字大小"),
                course2md::tr!("减少动态效果"),
                course2md::tr!("主页"),
                course2md::tr!("显示最近的笔记"),
            ],
            Self::Generation => vec![
                course2md::tr!("字幕与识别"),
                course2md::tr!("文字来源"),
                course2md::tr!("字幕语言"),
                course2md::tr!("自定义语言优先级"),
                course2md::tr!("语音识别"),
                course2md::tr!("识别方式"),
                course2md::tr!("识别模型"),
                course2md::tr!("高级识别设置"),
                course2md::tr!("本机引擎"),
                course2md::tr!("AI 处理"),
                course2md::tr!("校对文字和标点"),
                course2md::tr!("在开头生成摘要"),
                course2md::tr!("校对时附带对应截图"),
                course2md::tr!("双语翻译"),
                course2md::tr!("自定义校对规则"),
                course2md::tr!("保存与导出"),
                course2md::tr!("保存位置"),
                course2md::tr!("同时导出"),
                course2md::tr!("保留下载的视频"),
            ],
            Self::Services => vec![
                course2md::tr!("来源账号"),
                course2md::tr!("Bilibili 账号"),
                course2md::tr!("AI 服务"),
                course2md::tr!("添加 AI 服务"),
                course2md::tr!("语音服务"),
                course2md::tr!("添加语音服务"),
                course2md::tr!("ChatGPT 账号"),
            ],
            Self::Storage => vec![
                course2md::tr!("添加笔记库位置"),
                course2md::tr!("移动笔记库…"),
                course2md::tr!("设为默认保存位置"),
                course2md::tr!("打开位置"),
            ],
            Self::Application => vec![
                course2md::tr!("运行检查"),
                course2md::tr!("诊断详情"),
                course2md::tr!("关于"),
                course2md::tr!("项目主页"),
                course2md::tr!("发送反馈"),
                course2md::tr!("开源许可"),
                course2md::tr!("首次设置"),
            ],
        }
    }
    /// Whether settings search for `query` finds this category.
    fn matches(self, query: &str) -> bool {
        let query = query.trim().to_lowercase();
        query.is_empty()
            || [self.label(), self.subtitle()]
                .into_iter()
                .chain(self.keywords())
                .any(|text| text.to_lowercase().contains(&query))
    }
    fn group(self) -> Option<PreferenceGroup> {
        match self {
            Self::Generation => Some(PreferenceGroup::Generation),
            Self::Services => Some(PreferenceGroup::Services),
            Self::Appearance | Self::Application => Some(PreferenceGroup::Application),
            Self::Storage => None,
        }
    }
}

fn group(id: &'static str, title: &'static str) -> SettingsGroup {
    SettingsGroup::new(id, title)
}

/// Builds a tile's content once the group knows which corners are outside.
type TileBuilder = Box<dyn FnOnce([bool; 4]) -> AnyElement>;

enum TileMain {
    /// Content on the plain surface; the group draws the tile.
    Content(AnyElement),
    /// A tile that draws itself (a button), given its corners.
    Built(TileBuilder),
    /// Two tiles side by side in one row of the group.
    Pair(TileBuilder, TileBuilder),
}

struct GroupTile {
    main: TileMain,
    details: Vec<AnyElement>,
}

/// One settings section as on the M3 canvas: a quiet title above a connected
/// group of tiles, each setting on its own surface with 24px outer and 6px
/// inner corners and 2px between tiles.
#[derive(IntoElement)]
pub(crate) struct SettingsGroup {
    id: &'static str,
    title: &'static str,
    tiles: Vec<GroupTile>,
}

impl SettingsGroup {
    pub(crate) fn new(id: &'static str, title: &'static str) -> Self {
        Self {
            id,
            title,
            tiles: Vec::new(),
        }
    }

    /// A tile that draws its own surface (a button row), given its corners as
    /// top-left, top-right, bottom-right, bottom-left (true = outside).
    pub(crate) fn tile(mut self, build: impl FnOnce([bool; 4]) -> AnyElement + 'static) -> Self {
        self.tiles.push(GroupTile {
            main: TileMain::Built(Box::new(build)),
            details: Vec::new(),
        });
        self
    }

    /// Two tiles sharing one row of the group.
    pub(crate) fn pair(
        mut self,
        leading: impl FnOnce([bool; 4]) -> AnyElement + 'static,
        trailing: impl FnOnce([bool; 4]) -> AnyElement + 'static,
    ) -> Self {
        self.tiles.push(GroupTile {
            main: TileMain::Pair(Box::new(leading), Box::new(trailing)),
            details: Vec::new(),
        });
        self
    }

    /// Content that belongs to the previous item (a disclosure, a summary
    /// line): it stays inside that item's tile.
    pub(crate) fn detail(mut self, element: impl IntoElement) -> Self {
        match self.tiles.last_mut() {
            Some(tile) => tile.details.push(element.into_any_element()),
            None => self.tiles.push(GroupTile {
                main: TileMain::Content(element.into_any_element()),
                details: Vec::new(),
            }),
        }
        self
    }
}

impl ParentElement for SettingsGroup {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.tiles
            .extend(elements.into_iter().map(|element| GroupTile {
                main: TileMain::Content(element),
                details: Vec::new(),
            }));
    }
}

impl RenderOnce for SettingsGroup {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let count = self.tiles.len();
        let mut group = v_flex()
            .w_full()
            .min_w_0()
            .flex_shrink_0()
            .gap(TILE_GAP)
            .child(
                theme::section_title(self.id, self.title)
                    .pl(SETTING_ITEM_INSET)
                    .mb(px(8.)),
            );
        for (index, tile) in self.tiles.into_iter().enumerate() {
            let first = index == 0;
            let last = index + 1 == count;
            let outer = [first, first, last, last];
            let element = match tile.main {
                TileMain::Content(content) => settings_tile(outer)
                    .child(content)
                    .children(tile.details)
                    .into_any_element(),
                TileMain::Built(build) if tile.details.is_empty() => build(outer),
                // Details continue below the built tile inside the same
                // surface, so its own bottom corners stay inner.
                TileMain::Built(build) => settings_tile(outer)
                    .child(build([outer[0], outer[1], false, false]))
                    .children(tile.details)
                    .into_any_element(),
                TileMain::Pair(leading, trailing) => v_flex()
                    .w_full()
                    .min_w_0()
                    .child(
                        div()
                            .grid()
                            .grid_cols(2)
                            .w_full()
                            .min_w_0()
                            .gap(TILE_GAP)
                            .child(leading([first, false, false, last]))
                            .child(trailing([false, first, last, false])),
                    )
                    .children(tile.details)
                    .into_any_element(),
            };
            group = group.child(element);
        }
        group
    }
}

/// One tile of a settings group: the surface and the corners for its
/// position (top-left, top-right, bottom-right, bottom-left; true = outside).
pub(crate) fn settings_tile(outer: [bool; 4]) -> Div {
    tile_corners(
        v_flex()
            .w_full()
            .min_w_0()
            .flex_shrink_0()
            .bg(color(SURFACE))
            .text_size(TEXT_SETTINGS_BODY),
        outer,
    )
}

/// A standalone settings surface outside a group (a single tile).
pub(crate) fn settings_card() -> Div {
    settings_tile([true; 4])
}

/// The settings arrangement for the current window: a navigation pane (or a
/// compact rail), the gap after it and the content width. As on the canvas,
/// the whole layout is centered and at most 1180px wide with 24px gutters;
/// the pane takes 300–340px and the content at most 760px.
pub(crate) struct SettingsLayout {
    pub(crate) compact: bool,
    pub(crate) nav: f32,
    pub(crate) gap: f32,
    pub(crate) content: f32,
}

pub(crate) fn settings_layout(window: &Window) -> SettingsLayout {
    let scale = f32::from(window.rem_size()) / 14.;
    let width = f32::from(window.bounds().size.width);
    let compact = is_compact(window);
    let gutter = if compact { 16. } else { 24. };
    let available = (width.min(1180. * scale) - gutter * 2.).max(0.);
    if compact {
        let nav = 88. * scale;
        let gap = 16.;
        SettingsLayout {
            compact,
            nav,
            gap,
            content: (available - nav - gap).max(0.),
        }
    } else {
        let gap = 40.;
        let nav = (available - gap - 760. * scale).clamp(300. * scale, 340. * scale);
        SettingsLayout {
            compact,
            nav,
            gap,
            content: (available - nav - gap).clamp(0., 760. * scale),
        }
    }
}

impl Desktop {
    fn generation_edit_base(&self) -> GenerationPreferences {
        self.settings_ui
            .unsaved_text
            .as_ref()
            .or(self.settings_ui.pending_generation.as_ref())
            .or_else(|| self.preferences.generation_intent())
            .unwrap_or_else(|| self.preferences.generation())
            .clone()
    }
    pub(crate) fn application_edit_base(&self) -> ApplicationPreferences {
        self.settings_ui
            .pending_application
            .as_ref()
            .or_else(|| self.preferences.application_intent())
            .unwrap_or_else(|| self.preferences.application())
            .clone()
    }
    fn setting_value(&self, field: EditField, cx: &App) -> String {
        self.settings_ui.inputs[&field].read(cx).value().to_string()
    }
    /// Scroll handle owning the active settings editor (or the settings page
    /// itself) when reveal-on-focus applies.
    fn active_editor_scroll(&self) -> (bool, Option<ScrollHandle>) {
        let editor = self.settings_ui.editor.as_ref().filter(|editor| {
            editor.target.is_some()
                || editor.repair_task.is_some()
                || (self.page == Page::Settings && self.settings_tab == SettingsTab::Services)
        });
        (
            self.page == Page::Settings || editor.is_some(),
            editor.map(|editor| editor.scroll.clone()),
        )
    }
    fn reveal_setting(&self, id: impl Into<ElementId>, child: impl IntoElement) -> Div {
        let child = child.into_any_element();
        let (reveals, editor_scroll) = self.active_editor_scroll();
        if reveals {
            div()
                .w_full()
                .min_w_0()
                .flex_shrink_0()
                .child(crate::focus_scroll::RevealFocus::new(
                    id,
                    child,
                    editor_scroll.unwrap_or_else(|| self.scrolls[Page::Settings as usize].clone()),
                ))
        } else {
            div().w_full().min_w_0().flex_shrink_0().child(child)
        }
    }
    fn setting_choices(
        &self,
        id: impl Into<ElementId>,
        label: impl Into<SharedString>,
    ) -> SingleChoiceGroup {
        let (reveals, editor_scroll) = self.active_editor_scroll();
        SingleChoiceGroup::new(id, label)
            .settings_text()
            .segmented()
            .when(reveals, |group| {
                group.reveal_in(
                    editor_scroll.unwrap_or_else(|| self.scrolls[Page::Settings as usize].clone()),
                )
            })
    }
    fn setting_field(&self, field: EditField, label: &'static str) -> Div {
        let input = text_input(&self.settings_ui.inputs[&field]);
        self.input_field(field as usize, label, input, None, None)
    }
    /// A service editor input with its field error and an optional trailing action.
    /// A connection field on the service form: outlined, its label floating
    /// on the outline, an optional trailing action and its error below.
    fn service_field(
        &self,
        field: FormField,
        label: &'static str,
        action: Option<AnyElement>,
        window: &Window,
        cx: &App,
    ) -> AnyElement {
        let Some(editor) = &self.settings_ui.editor else {
            return div().into_any_element();
        };
        let state = editor.form.input(field);
        let error = editor.form.error(field).map(str::to_owned);
        let input = text_input(state)
            .settings_text()
            .w_full()
            .readonly(editor.pending_binding.is_some())
            .when(field == FormField::Key, |input| {
                input.content_type(InputContentType::Password)
            })
            .aria_label(
                error
                    .as_ref()
                    .map(|error| course2md::tr!("{}，{}", label, error))
                    .unwrap_or_else(|| label.to_owned()),
            )
            .when(error.is_some(), |input| input.border_color(color(DANGER)));
        let index = 8 + field as usize;
        let field = labeled_input(
            ("setting-field-label", index),
            label,
            state,
            input,
            window,
            cx,
        );
        self.reveal_setting(
            ("setting-field-reveal", index),
            v_flex()
                .w_full()
                .min_w_0()
                .gap_1()
                .child(match action {
                    Some(action) => h_flex()
                        .w_full()
                        .min_w_0()
                        .gap_2()
                        .items_end()
                        .child(div().flex_1().min_w_0().child(field))
                        .child(div().pb(rems(8. / 14.)).child(action))
                        .into_any_element(),
                    None => field.into_any_element(),
                })
                .when_some(error, |view, error| {
                    view.child(
                        accessible_text(("setting-error", index), error)
                            .px(rems(16. / 14.))
                            .text_size(TEXT_SETTINGS_AUX)
                            .text_color(color(DANGER)),
                    )
                }),
        )
        .into_any_element()
    }
    fn input_field(
        &self,
        index: usize,
        label: &'static str,
        input: gpui_component::input::Input,
        error: Option<String>,
        action: Option<AnyElement>,
    ) -> Div {
        let input = input
            .settings_text()
            .w_full()
            .aria_label(
                error
                    .as_ref()
                    .map(|error| course2md::tr!("{}，{}", label, error))
                    .unwrap_or_else(|| label.to_owned()),
            )
            .when(error.is_some(), |input| input.border_color(color(DANGER)));
        self.reveal_setting(
            ("setting-field-reveal", index),
            settings_field_row(
                ("setting-field-label", index),
                label,
                v_flex()
                    .w_full()
                    .gap_2()
                    .child(match action {
                        Some(action) => h_flex()
                            .w_full()
                            .min_w_0()
                            .gap_2()
                            .items_center()
                            .child(div().flex_1().min_w_0().child(input))
                            .child(action)
                            .into_any_element(),
                        None => input.into_any_element(),
                    })
                    .when_some(error, |view, error| {
                        view.child(
                            accessible_text(("setting-error", index), error)
                                .text_size(TEXT_SETTINGS_AUX)
                                .text_color(color(DANGER)),
                        )
                    }),
            ),
        )
    }
    fn hydrate_settings_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings_ui.initialized {
            return;
        }
        self.settings_ui.initialized = true;
        let value = self
            .preferences
            .generation_intent()
            .unwrap_or_else(|| self.preferences.generation())
            .clone();
        if self.preferences.generation_intent().is_some() {
            self.settings_ui.pending_generation = Some(value.clone());
            self.set_settings_feedback(
                PreferenceGroup::Generation,
                course2md::tr!("已恢复未保存的修改").into(),
                true,
                cx,
            );
        }
        if let Some(intent) = self.preferences.application_intent().cloned() {
            self.settings_ui.pending_application = Some(intent);
            self.set_settings_feedback(
                PreferenceGroup::Application,
                course2md::tr!("已恢复未保存的修改").into(),
                true,
                cx,
            );
        }
        let languages = value
            .subtitle_languages_draft
            .unwrap_or_else(|| value.preferred_subtitle_languages.join(", "));
        let model = value
            .local_model_draft
            .unwrap_or_else(|| value.options.asr_model.unwrap_or_default());
        self.settings_ui.inputs[&EditField::Languages]
            .update(cx, |input, cx| input.set_value(languages, window, cx));
        self.settings_ui.inputs[&EditField::LocalModel]
            .update(cx, |input, cx| input.set_value(model, window, cx));
        self.settings_ui.prompt.update(cx, |input, cx| {
            input.set_value(
                value.prompt_draft.or(value.ai.prompt).unwrap_or_default(),
                window,
                cx,
            )
        });
    }
    pub(crate) fn settings_have_problem(&self) -> bool {
        [
            PreferenceGroup::Generation,
            PreferenceGroup::Application,
            PreferenceGroup::Services,
        ]
        .into_iter()
        .any(|group| self.preferences.is_blocked(group))
            || self.preferences.generation_intent().is_some()
            || self.preferences.application_intent().is_some()
            || self.settings_ui.pending_generation.is_some()
            || self.settings_ui.pending_application.is_some()
            || self.settings_ui.feedback.values().any(|(_, error)| *error)
            || self
                .settings_ui
                .editor
                .as_ref()
                .is_some_and(|editor| editor.save_failed)
    }

    pub(crate) fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.page != Page::Settings {
            self.settings_origin = Some((self.page, window.focused(cx)));
        }
        self.navigate(Page::Settings, cx);
        // Event-time preparation, not a render side effect: tab stops, first
        // hydration of inputs and the deduplicated model check.
        self.prepare_settings_view(window, cx);
        self.settings_ui.tab_focus[self.settings_tab.index()].focus(window, cx);
    }

    /// Settings returns to where it was opened from, refocusing what opened it.
    pub(crate) fn leave_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (page, focus) = self.settings_origin.take().unwrap_or((Page::New, None));
        if page == Page::New && !self.prepare_workbench_input(window, cx) {
            return;
        }
        self.navigate(page, cx);
        if let Some(focus) = focus {
            window.on_next_frame(move |window, cx| focus.focus(window, cx));
        }
    }

    pub(crate) fn prepare_settings_view(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_settings_tab_stops();
        self.hydrate_settings_inputs(window, cx);
        self.ensure_settings_model_diagnostic(cx);
        self.ensure_account_cards(cx);
    }

    fn sync_settings_tab_stops(&mut self) {
        for (index, focus) in self.settings_ui.tab_focus.iter_mut().enumerate() {
            *focus = focus.clone().tab_stop(index == self.settings_tab.index());
        }
    }
    pub(crate) fn select_settings_tab(
        &mut self,
        tab: SettingsTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settings_tab = tab;
        self.sync_settings_tab_stops();
        self.scrolls[Page::Settings as usize].set_offset(point(px(0.), px(0.)));
        // Capture builds can review the lower part of a category.
        #[cfg(feature = "capture")]
        if let Some(offset) = std::env::var("COURSE2MD_CAPTURE_SETTINGS_SCROLL")
            .ok()
            .and_then(|value| value.parse::<f32>().ok())
        {
            self.scrolls[Page::Settings as usize].set_offset(point(px(0.), px(-offset)));
        }
        #[cfg(feature = "capture")]
        if let Ok(query) = std::env::var("COURSE2MD_CAPTURE_SETTINGS_SEARCH") {
            self.settings_ui
                .search
                .update(cx, |search, cx| search.set_value(query, window, cx));
        }
        self.settings_ui.tab_focus[tab.index()].focus(window, cx);
        self.ensure_account_cards(cx);
        cx.notify();
    }

    /// Services and accounts: bring both account cards up to date (busy and checked guards avoid repeats).
    fn ensure_account_cards(&mut self, cx: &mut Context<Self>) {
        if self.settings_tab != SettingsTab::Services {
            return;
        }
        if self.codex.status.is_none() && !self.codex.busy() {
            self.codex_refresh_status(crate::codex_ui::CodexSurface::Settings, cx);
        }
    }

    /// The categories settings search currently shows, in navigation order.
    fn visible_settings_tabs(&self, cx: &App) -> Vec<SettingsTab> {
        let query = self.settings_ui.search.read(cx).value().to_string();
        SettingsTab::ALL
            .into_iter()
            .filter(|tab| tab.matches(&query))
            .collect()
    }

    /// Search narrows the categories and moves to the first one that matches
    /// when the open category no longer does. Focus stays in the field.
    fn settings_search_changed(&mut self, cx: &mut Context<Self>) {
        let visible = self.visible_settings_tabs(cx);
        if !visible.contains(&self.settings_tab)
            && let Some(first) = visible.first()
        {
            self.settings_tab = *first;
            self.sync_settings_tab_stops();
            self.scrolls[Page::Settings as usize].set_offset(point(px(0.), px(0.)));
            self.ensure_account_cards(cx);
        }
        cx.notify();
    }

    /// Arrow keys move between the categories search leaves visible.
    fn step_settings_tab(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let visible = self.visible_settings_tabs(cx);
        if visible.is_empty() {
            return;
        }
        let position = visible
            .iter()
            .position(|tab| *tab == self.settings_tab)
            .unwrap_or(0);
        let next = if forward {
            (position + 1) % visible.len()
        } else {
            (position + visible.len() - 1) % visible.len()
        };
        self.select_settings_tab(visible[next], window, cx);
    }

    fn settings_back_button(&self, cx: &mut Context<Self>) -> Button {
        let origin = self
            .settings_origin
            .as_ref()
            .map_or(Page::New, |(page, _)| *page);
        let name = match origin {
            Page::Library | Page::Result => course2md::tr!("笔记库"),
            Page::Task => course2md::tr!("任务"),
            _ => course2md::tr!("主页"),
        };
        icon_button("settings-back", BUTTON_MEDIUM, true, cx)
            .icon(icons::arrow_left().size(rems(22. / 14.)))
            .accessibility_label(course2md::tr!("返回「{}」", name))
            .tooltip(course2md::tr!("返回「{}」", name))
            .on_click(cx.listener(|this, _, window, cx| this.leave_settings(window, cx)))
    }

    /// The outlined settings search field of the navigation pane.
    fn settings_search_field(&self, window: &Window, cx: &App) -> Div {
        let focused = self
            .settings_ui
            .search
            .read(cx)
            .focus_handle(cx)
            .is_focused(window);
        h_flex()
            .w_full()
            .min_w_0()
            .h(BUTTON_LARGE)
            .flex_shrink_0()
            .pl(rems(22. / 14.))
            .pr(rems(12. / 14.))
            .gap(rems(1.))
            .items_center()
            .rounded(RADIUS_PILL)
            .bg(color(SURFACE))
            .border_1()
            .border_color(color(if focused { ACCENT } else { HAIRLINE }))
            .child(
                icons::search()
                    .size(rems(22. / 14.))
                    .flex_shrink_0()
                    .text_color(color(GRAY)),
            )
            .child(
                div().flex_1().min_w_0().child(
                    gpui_component::input::Input::new(&self.settings_ui.search)
                        .appearance(false)
                        .aria_label(course2md::tr!("搜索设置"))
                        .w_full()
                        .px_0()
                        .text_size(TEXT_TILE_LABEL)
                        .cleanable(true),
                ),
            )
    }

    fn settings_navigation_actions(
        &self,
        view: Stateful<Div>,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        view.key_context("SettingsTabs")
            .on_action(cx.listener(|this, _: &NextSettingsTab, window, cx| {
                this.step_settings_tab(true, window, cx);
            }))
            .on_action(cx.listener(|this, _: &PreviousSettingsTab, window, cx| {
                this.step_settings_tab(false, window, cx);
            }))
            .on_action(cx.listener(|this, _: &FirstSettingsTab, window, cx| {
                if let Some(first) = this.visible_settings_tabs(cx).first() {
                    this.select_settings_tab(*first, window, cx)
                }
            }))
            .on_action(cx.listener(|this, _: &LastSettingsTab, window, cx| {
                if let Some(last) = this.visible_settings_tabs(cx).last() {
                    this.select_settings_tab(*last, window, cx)
                }
            }))
    }

    /// One category tile: tinted icon circle, name and what it holds. The
    /// open category fills with the secondary container and its circle turns
    /// to the surface.
    fn settings_category_tile(
        &self,
        tab: SettingsTab,
        outer: [bool; 4],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let index = tab.index();
        let selected = self.settings_tab == tab;
        let fill = color(if selected {
            SECONDARY_CONTAINER
        } else {
            SURFACE
        });
        let hover = theme::blend(fill, color(INK), 0.08);
        let icon = tinted_icon(tab.icon(), tab.tint(), px(40.))
            .when(selected, |circle| circle.bg(color(SURFACE)));
        tile_corners(
            gpui_base::Tab::new(("settings-tab", index))
                .selected(selected)
                .track_focus(&self.settings_ui.tab_focus[index].clone().tab_stop(selected))
                .accessibility_label(tab.label())
                .w_full()
                .h_auto()
                .min_h(rems(72. / 14.))
                .px(rems(16. / 14.))
                .py(rems(1.))
                .justify_start()
                .bg(fill)
                .border_2()
                .border_color(gpui::transparent_black())
                .cursor_pointer()
                .hover(move |style| style.bg(hover))
                .focus_visible(|style| style.border_color(color(ACCENT)))
                .child(
                    h_flex()
                        .w_full()
                        .min_w_0()
                        .items_center()
                        .gap(rems(16. / 14.))
                        .child(icon)
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .gap(rems(2. / 14.))
                                .child(
                                    div()
                                        .text_size(TEXT_SETTINGS_BODY)
                                        .line_height(rems(22. / 14.))
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(color(INK))
                                        .child(tab.label()),
                                )
                                .child(
                                    div()
                                        .whitespace_normal()
                                        .text_size(rems(13. / 14.))
                                        .line_height(rems(18. / 14.))
                                        .text_color(color(GRAY))
                                        .child(tab.subtitle()),
                                ),
                        ),
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.select_settings_tab(tab, window, cx)
                })),
            outer,
        )
        .into_any_element()
    }

    /// The leading pane: the way back, the 设置 title, search and the
    /// categories as connected tile groups. It scrolls on its own.
    fn settings_navigation(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let visible = self.visible_settings_tabs(cx);
        let mut navigation = v_flex()
            .w_full()
            .min_w_0()
            .gap(rems(16. / 14.))
            .pb(px(64.))
            .child(
                h_flex()
                    .h(BUTTON_LARGE)
                    .flex_shrink_0()
                    .items_center()
                    .gap(rems(16. / 14.))
                    .child(self.settings_back_button(cx))
                    .child(theme::page_heading(
                        "settings-page-title",
                        course2md::tr!("设置"),
                    )),
            )
            .child(self.settings_search_field(window, cx));
        let groups: [&[SettingsTab]; 3] = [
            &[SettingsTab::Appearance, SettingsTab::Generation],
            &[SettingsTab::Services, SettingsTab::Storage],
            &[SettingsTab::Application],
        ];
        let mut first = true;
        for tabs in groups {
            let shown: Vec<_> = tabs
                .iter()
                .copied()
                .filter(|tab| visible.contains(tab))
                .collect();
            if shown.is_empty() {
                continue;
            }
            let count = shown.len();
            navigation = navigation.child(
                v_flex()
                    .w_full()
                    .min_w_0()
                    .gap(TILE_GAP)
                    .when(first, |group| group.mt(rems(4. / 14.)))
                    .children(shown.into_iter().enumerate().map(|(index, tab)| {
                        let top = index == 0;
                        let bottom = index + 1 == count;
                        self.settings_category_tile(tab, [top, top, bottom, bottom], cx)
                    })),
            );
            first = false;
        }
        if visible.is_empty() {
            navigation = navigation.child(
                tile_support("settings-search-empty", course2md::tr!("没有匹配的设置"))
                    .px(SETTING_ITEM_INSET)
                    .mt(rems(4. / 14.)),
            );
        }
        let view = div()
            .id("settings-navigation")
            .size_full()
            .min_h_0()
            .overflow_y_scroll()
            .role(Role::TabList)
            .aria_label(course2md::tr!("设置分类"))
            .child(navigation);
        self.settings_navigation_actions(view, cx)
            .into_any_element()
    }

    /// Narrow windows keep the categories in a compact rail beside the content.
    fn settings_rail(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut rail = v_flex()
            .w_full()
            .items_center()
            .gap(rems(4. / 14.))
            .pb(px(24.))
            .child(
                div()
                    .h(BUTTON_LARGE)
                    .flex()
                    .items_center()
                    .mb(rems(8. / 14.))
                    .child(self.settings_back_button(cx)),
            );
        for tab in SettingsTab::ALL {
            let index = tab.index();
            let selected = self.settings_tab == tab;
            let fill = color(if selected {
                SECONDARY_CONTAINER
            } else {
                CANVAS
            });
            let hover = theme::blend(fill, color(INK), 0.08);
            rail = rail.child(
                gpui_base::Tab::new(("settings-tab", index))
                    .selected(selected)
                    .track_focus(&self.settings_ui.tab_focus[index].clone().tab_stop(selected))
                    .accessibility_label(tab.label())
                    .w_full()
                    .h_auto()
                    .py(rems(8. / 14.))
                    .px(rems(2. / 14.))
                    .rounded(px(16.))
                    .bg(fill)
                    .border_2()
                    .border_color(gpui::transparent_black())
                    .cursor_pointer()
                    .hover(move |style| style.bg(hover))
                    .focus_visible(|style| style.border_color(color(ACCENT)))
                    .child(
                        v_flex()
                            .w_full()
                            .items_center()
                            .gap(rems(4. / 14.))
                            .child(
                                tinted_icon(tab.icon(), tab.tint(), px(40.))
                                    .when(selected, |circle| circle.bg(color(SURFACE))),
                            )
                            .child(
                                div()
                                    .w_full()
                                    .text_center()
                                    .whitespace_normal()
                                    .text_size(TEXT_AUX)
                                    .line_height(rems(16. / 14.))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(color(INK))
                                    .child(tab.short_label()),
                            ),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.select_settings_tab(tab, window, cx)
                    })),
            );
        }
        let view = div()
            .id("settings-navigation")
            .size_full()
            .min_h_0()
            .overflow_y_scroll()
            .role(Role::TabList)
            .aria_label(course2md::tr!("设置分类"))
            .child(rail);
        self.settings_navigation_actions(view, cx)
            .into_any_element()
    }

    pub fn settings_page(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        // Rendering has no side effects: hydration and diagnostics run from event entries such as
        // open_settings, select_settings_tab and restore_settings_group.
        let layout = settings_layout(window);
        let navigation = if layout.compact {
            self.settings_rail(cx)
        } else {
            self.settings_navigation(window, cx)
        };
        let mut panel = v_flex()
            .relative()
            .id("settings-panel")
            .role(Role::TabPanel)
            .aria_label(self.settings_tab.label())
            .flex_shrink_0()
            .min_w_0()
            .min_h_0()
            .h_full()
            .w(px(layout.content))
            .gap(px(16.));
        for group in [
            PreferenceGroup::Generation,
            PreferenceGroup::Services,
            PreferenceGroup::Application,
        ] {
            if self.settings_tab.group() == Some(group) {
                if self.preferences.is_blocked(group)
                    || self
                        .settings_group_notice(group)
                        .is_some_and(|(_, error)| error)
                {
                    panel = panel.child(
                        div()
                            .id(("fixed-settings-feedback", group as usize))
                            .w_full()
                            .min_w_0()
                            .flex_shrink_0()
                            .p(px(16.))
                            .rounded(TILE_RADIUS)
                            .bg(color(DANGER_BG))
                            .child(self.group_feedback(group, cx)),
                    );
                }
                continue;
            }
            let target = match group {
                PreferenceGroup::Generation => SettingsTab::Generation,
                PreferenceGroup::Services => SettingsTab::Services,
                PreferenceGroup::Application => SettingsTab::Appearance,
            };
            if let Some((message, true)) = self.settings_group_notice(group) {
                panel = panel.child(
                    h_flex()
                        .w_full()
                        .min_w_0()
                        .flex_shrink_0()
                        .gap_2()
                        .flex_wrap()
                        .py(px(8.))
                        .pl(px(20.))
                        .pr(px(8.))
                        .items_center()
                        .rounded(TILE_RADIUS)
                        .bg(color(DANGER_BG))
                        .child(
                            accessible_text(("settings-failed-group", target.index()), message)
                                .role(Role::Alert)
                                .flex_1()
                                .min_w_0()
                                .text_color(color(DANGER))
                                .text_size(TEXT_SETTINGS_AUX),
                        )
                        .child(
                            text_button(("open-failed-settings-group", target.index()), cx)
                                .settings_text()
                                .label(course2md::tr!("前往「{}」", target.label()))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.select_settings_tab(target, window, cx);
                                })),
                        ),
                );
            }
        }
        let content = match self.settings_tab {
            SettingsTab::Generation => self.generation_settings_page(window, cx),
            SettingsTab::Services => self.services_settings_page(window, cx),
            SettingsTab::Storage => self.storage_settings_page(cx),
            SettingsTab::Application => self.application_settings_page(cx),
            SettingsTab::Appearance => self.appearance_page(window, cx),
        };
        let editing_service = self.settings_tab == SettingsTab::Services
            && self
                .settings_ui
                .editor
                .as_ref()
                .is_some_and(|editor| editor.target.is_none() && editor.repair_task.is_none());
        let heading = v_flex()
            .w_full()
            .min_w_0()
            .gap(rems(6. / 14.))
            .child(
                div()
                    .h(BUTTON_LARGE)
                    .flex()
                    .items_center()
                    .child(theme::page_heading(
                        "settings-category-heading",
                        self.settings_tab.label(),
                    )),
            )
            .when(self.settings_tab == SettingsTab::Generation, |view| {
                view.child(
                    accessible_text(
                        "generation-default-explanation",
                        course2md::tr!("这里是长期默认值，主页的「处理选项」可以只对单次生效"),
                    )
                    .px(rems(2. / 14.))
                    .whitespace_normal()
                    .text_size(rems(15. / 14.))
                    .text_color(color(GRAY)),
                )
            });
        let panel = panel
            .child(
                div()
                    .id("settings-content-scroll")
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .w_full()
                    .when(!editing_service, |view| view.overflow_y_scroll())
                    .track_scroll(&self.scrolls[Page::Settings as usize])
                    .child(
                        div()
                            .w_full()
                            .min_w_0()
                            .when(editing_service, |view| view.h_full().min_h_0())
                            .when(!editing_service, |view| view.pb(px(64.)))
                            // A category, or the secondary page inside it, arrives as one unit.
                            .child(crate::motion::page_enter(
                                (
                                    "settings-category",
                                    self.settings_tab.index() * 2 + editing_service as usize,
                                ),
                                v_flex()
                                    .w_full()
                                    .min_w_0()
                                    .gap(px(28.))
                                    .when(editing_service, |view| view.h_full().min_h_0())
                                    .when(!editing_service, |view| view.child(heading))
                                    .child(content),
                            )),
                    ),
            )
            .when(!editing_service, |panel| {
                panel.child(crate::backend::vertical_scrollbar(
                    &self.scrolls[Page::Settings as usize],
                ))
            });
        let body = h_flex()
            .w(px(layout.nav + layout.gap + layout.content))
            .max_w_full()
            .mx_auto()
            .h_full()
            .min_h_0()
            .items_start()
            .gap(px(layout.gap))
            .child(
                div()
                    .w(px(layout.nav))
                    .flex_shrink_0()
                    .h_full()
                    .min_h_0()
                    .child(navigation),
            )
            .child(panel);
        v_flex()
            .w_full()
            .h_full()
            .min_w_0()
            .min_h_0()
            .pt(px(8.))
            .child(div().flex_1().min_h_0().w_full().child(body))
            .into_any_element()
    }

    fn services_settings_page(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let mut view = v_flex().w_full().min_w_0().flex_shrink_0().gap(px(28.));
        // Adding or editing a service is a secondary page of this category.
        if self
            .settings_ui
            .editor
            .as_ref()
            .is_some_and(|editor| editor.target.is_none() && editor.repair_task.is_none())
        {
            return view
                .h_full()
                .min_h_0()
                .child(self.service_editor_card(window, cx))
                .into_any_element();
        }
        // The account is a resource row like the services below, on the same
        // insets, so every row's ⋯ sits on one vertical line.
        view = view.child(
            group("account-settings-heading", course2md::tr!("来源账号"))
                .child(resource_row("bilibili-account-row").child(self.account_settings_page(cx))),
        );
        let latest = self.preferences.latest_versions();
        for (purpose, heading_id, heading, add_label) in [
            (
                ServicePurpose::Ai,
                "ai-services-heading",
                course2md::tr!("AI 服务"),
                course2md::tr!("添加 AI 服务"),
            ),
            (
                ServicePurpose::Speech,
                "speech-services-heading",
                course2md::tr!("语音服务"),
                course2md::tr!("添加语音服务"),
            ),
        ] {
            let services: Vec<_> = latest
                .values()
                .filter(|version| {
                    version.config.protocol.purpose() == purpose
                        && !self
                            .preferences
                            .service_retired_in_snapshot(&version.service_id)
                })
                .collect();
            let count = services.len() + 1;
            let mut section = group(heading_id, heading);
            for (index, version) in services.iter().enumerate() {
                let first = index == 0;
                let row = self.service_row(version, [first, first, false, false], cx);
                section = section.tile(move |_| row);
            }
            let first = count == 1;
            // Without a speech service, recognition runs on this device.
            let hint = (purpose == ServicePurpose::Speech && services.is_empty())
                .then(|| course2md::tr!("未添加时使用本机识别").into());
            let add = tile_button(
                ("add-service", purpose as usize),
                [first, first, true, true],
                cx,
            )
            .min_h(rems(72. / 14.))
            .accessibility_label(add_label)
            // The plus sits in the rows' icon column, the label on their text ruler.
            .child(
                h_flex()
                    .size(px(48.))
                    .flex_shrink_0()
                    .items_center()
                    .justify_center()
                    .text_color(color(INK))
                    .child(icons::plus().size(rems(24. / 14.))),
            )
            .child(tile_text(
                SharedString::from(format!("add-service-label-{}", purpose as usize)),
                add_label,
                hint,
            ))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.open_settings_service_editor(purpose, None, window, cx);
            }));
            section = section.tile(move |_| add.into_any_element());
            view = view.child(section);
        }
        view.into_any_element()
    }

    /// One saved service: what it is, whether it works, and its commands in a
    /// menu that the row's ⋯ button and a right-click open alike.
    fn service_row(
        &self,
        version: &ServiceVersion,
        outer: [bool; 4],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = version.id.clone();
        let protocol = version.config.protocol;
        let purpose = protocol.purpose();
        let defaults = self.preferences.default_refs();
        let default_version = match purpose {
            ServicePurpose::Speech => defaults.asr.as_deref(),
            ServicePurpose::Ai => defaults.llm.as_deref(),
        }
        .and_then(|id| self.preferences.version(id))
        .filter(|default| default.service_id == version.service_id);
        let is_default = default_version.is_some_and(|default| default.id == version.id);
        let outdated_default = default_version.filter(|default| default.id != version.id);
        let tests = self.service_tests(version);
        let (kind, status) = self.service_status(version, &tests);
        // An account action started here reports here.
        let notice = (protocol == ServiceProtocol::CodexResponses)
            .then(|| {
                self.codex
                    .notice_on(crate::codex_ui::CodexSurface::Settings)
            })
            .flatten();
        let mut detail = vec![protocol.choice_label().to_owned()];
        let host = version.config.host();
        if protocol != ServiceProtocol::CodexResponses && !host.is_empty() {
            detail.push(host);
        }
        detail.push(version.config.model.clone());
        let failures: Vec<_> = tests
            .iter()
            .filter(|(_, evidence)| evidence.outcome != preferences::TestOutcome::Passed)
            .map(|(kind, evidence)| course2md::tr!("{}：{}", kind.label(), evidence.message))
            .collect();
        let menu = self.service_menu(version, is_default, cx);
        let row_menu = menu.clone();
        let edit_id = id.clone();
        let hover = theme::blend(color(SURFACE), color(INK), 0.08);
        tile_corners(
            resource_row(SharedString::from(format!("saved-service-{id}"))),
            outer,
        )
        .bg(color(SURFACE))
        .min_h(rems(88. / 14.))
        // The whole row opens the editor, as「编辑…」in its menu does.
        .cursor_pointer()
        .hover(move |style| style.bg(hover))
        .on_click(cx.listener(move |this, _, window, cx| {
            this.open_settings_service_editor(purpose, Some(edit_id.clone()), window, cx);
        }))
        .child(tinted_icon(
            service_form::protocol_icon(protocol),
            if purpose == ServicePurpose::Ai {
                Tint::Purple
            } else {
                Tint::Teal
            },
            px(48.),
        ))
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap(rems(2. / 14.))
                .child(
                    h_flex()
                        .min_w_0()
                        .gap_2()
                        .items_center()
                        .flex_wrap()
                        .child(tile_label(
                            SharedString::from(format!("saved-service-title-{id}")),
                            version.config.name.clone(),
                        ))
                        .when(is_default, |row| {
                            row.child(badge(BadgeKind::Tonal).child(course2md::tr!("默认")))
                        }),
                )
                .child(
                    accessible_text(
                        SharedString::from(format!("saved-service-description-{id}")),
                        detail.join(" · "),
                    )
                    .truncate()
                    .text_size(TEXT_SETTINGS_AUX)
                    .text_color(color(GRAY)),
                )
                .when_some(outdated_default, |column, default| {
                    column.child(
                        accessible_text(
                            SharedString::from(format!("saved-service-old-default-{id}")),
                            course2md::tr!(
                                "以后生成的笔记仍使用修改前的配置 · {}",
                                default.config.model
                            ),
                        )
                        .whitespace_normal()
                        .text_size(TEXT_SETTINGS_AUX)
                        .text_color(color(GRAY)),
                    )
                })
                .when_some(notice, |column, (message, failed)| {
                    column.child(
                        accessible_text(
                            SharedString::from(format!("saved-service-notice-{id}")),
                            message,
                        )
                        .whitespace_normal()
                        .text_size(TEXT_SETTINGS_AUX)
                        .text_color(color(if failed {
                            DANGER
                        } else {
                            GRAY
                        })),
                    )
                })
                .when(!failures.is_empty(), |column| {
                    column.child(
                        accessible_text(
                            SharedString::from(format!("saved-service-test-status-{id}")),
                            failures.join("\n"),
                        )
                        .whitespace_normal()
                        .text_size(TEXT_SETTINGS_AUX)
                        .text_color(color(GRAY)),
                    )
                }),
        )
        .child(
            badge(kind)
                .flex_shrink_0()
                .text_size(TEXT_AUX)
                .child(status),
        )
        .child(
            // The menu button keeps its click from also opening the row.
            div()
                .id(SharedString::from(format!("service-actions-slot-{id}")))
                .flex_shrink_0()
                .on_click(|_, _, cx| cx.stop_propagation())
                .child(
                    quiet(SharedString::from(format!("service-actions-{id}")))
                        .w(CONTROL_HEIGHT)
                        .px_0()
                        .icon(icons::ellipsis())
                        .accessibility_label(course2md::tr!("{} 操作", version.config.name))
                        .tooltip(course2md::tr!("服务操作"))
                        .dropdown_menu_with_anchor(Anchor::TopRight, move |popup, window, cx| {
                            menu(popup, window, cx)
                        }),
                ),
        )
        .context_menu(move |popup, window, cx| row_menu(popup, window, cx))
        .into_any_element()
    }

    fn service_tests(
        &self,
        version: &ServiceVersion,
    ) -> Vec<(TestKind, &preferences::ServiceTestEvidence)> {
        let kinds: &[TestKind] = if version.config.protocol.purpose() == ServicePurpose::Speech {
            &[TestKind::Speech]
        } else {
            &[TestKind::Proofread, TestKind::Summary, TestKind::Vision]
        };
        kinds
            .iter()
            .filter_map(|kind| {
                self.preferences
                    .test_evidence(&version.config, kind.contract())
                    .map(|evidence| (*kind, evidence))
            })
            .collect()
    }

    /// One badge per service: an account's sign-in state, otherwise what its
    /// tests established.
    fn service_status(
        &self,
        version: &ServiceVersion,
        tests: &[(TestKind, &preferences::ServiceTestEvidence)],
    ) -> (BadgeKind, &'static str) {
        if version.config.protocol == ServiceProtocol::CodexResponses {
            return self.codex.badge();
        }
        if tests.is_empty() {
            (BadgeKind::Neutral, course2md::tr!("尚未测试"))
        } else if tests
            .iter()
            .all(|(_, evidence)| evidence.outcome == preferences::TestOutcome::Passed)
        {
            (BadgeKind::Success, course2md::tr!("测试通过"))
        } else {
            (BadgeKind::Warning, course2md::tr!("测试未通过"))
        }
    }

    /// 打开 → 处理 → 管理 → 危险, the order every context menu follows.
    fn service_menu(
        &self,
        version: &ServiceVersion,
        is_default: bool,
        cx: &mut Context<Self>,
    ) -> MenuBuilder {
        let entity = cx.weak_entity();
        let id = version.id.clone();
        let service_id = version.service_id.clone();
        let purpose = version.config.protocol.purpose();
        let codex_connected = version.config.protocol == ServiceProtocol::CodexResponses
            && matches!(
                self.codex.status,
                Some(course2md::login::codex::LoginStatus::Connected)
            );
        let busy = self.codex.busy();
        Rc::new(move |menu, _, _| {
            let open = |test: bool| {
                let entity = entity.clone();
                let id = id.clone();
                move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                    let _ = entity.update(cx, |this, cx| {
                        if this.open_settings_service_editor(purpose, Some(id.clone()), window, cx)
                            && test
                        {
                            this.start_service_test(window, cx);
                        }
                    });
                }
            };
            let default = {
                let entity = entity.clone();
                let id = id.clone();
                move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                    let _ = entity.update(cx, |this, cx| {
                        this.confirm_default_service(id.clone(), window, cx)
                    });
                }
            };
            let delete = {
                let entity = entity.clone();
                let service_id = service_id.clone();
                move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                    let _ = entity.update(cx, |this, cx| {
                        this.confirm_delete_service(service_id.clone(), window, cx)
                    });
                }
            };
            let logout = entity.clone();
            menu.item(
                PopupMenuItem::new(course2md::tr!("测试连接"))
                    .icon(icons::science())
                    .on_click(open(true)),
            )
            .item(
                PopupMenuItem::new(course2md::tr!("编辑…"))
                    .icon(icons::edit())
                    .on_click(open(false)),
            )
            .item(
                PopupMenuItem::new(if is_default {
                    course2md::tr!("已是默认服务")
                } else {
                    course2md::tr!("设为默认服务…")
                })
                .icon(icons::check_circle())
                .disabled(is_default)
                .on_click(default),
            )
            .separator()
            .when(codex_connected, |menu| {
                menu.item(
                    PopupMenuItem::new(course2md::tr!("退出 ChatGPT 账号"))
                        .icon(icons::logout())
                        .disabled(busy)
                        .on_click(move |_, _, cx| {
                            let _ = logout.update(cx, |this, cx| {
                                this.codex_logout(crate::codex_ui::CodexSurface::Settings, cx)
                            });
                        }),
                )
            })
            .item(
                PopupMenuItem::new(course2md::tr!("删除服务…"))
                    .icon(icons::delete())
                    .danger(true)
                    .on_click(delete),
            )
        })
    }

    fn setting_input_changed(&mut self, field: EditField, cx: &mut Context<Self>) {
        if !self.settings_ui.initialized {
            return;
        }
        {
            let input = self.setting_value(field, cx);
            let mut next = self.generation_edit_base();
            let previous = if field == EditField::LocalModel {
                next.local_model_draft
                    .clone()
                    .unwrap_or_else(|| next.options.asr_model.clone().unwrap_or_default())
            } else {
                next.subtitle_languages_draft
                    .clone()
                    .unwrap_or_else(|| next.preferred_subtitle_languages.join(", "))
            };
            if previous == input {
                return;
            }
            if field == EditField::LocalModel {
                next.local_model_draft = Some(input);
            } else {
                next.subtitle_languages_draft = Some(input);
            }
            self.stage_generation_text(next, cx);
        }
    }
    fn save_prompt_draft(&mut self, cx: &mut Context<Self>) {
        if !self.settings_ui.initialized {
            return;
        }
        let text = self.settings_ui.prompt.read(cx).value().to_string();
        let mut next = self.generation_edit_base();
        if next
            .prompt_draft
            .as_ref()
            .or(next.ai.prompt.as_ref())
            .map(String::as_str)
            .unwrap_or("")
            == text
            // The prefilled standard rule is not an unsaved edit.
            || (next.ai.prompt.is_none()
                && next.prompt_draft.is_none()
                && text.trim() == course2md::llm::DEFAULT_PROMPT.trim())
        {
            return;
        }
        next.prompt_draft = Some(text);
        self.stage_generation_text(next, cx);
    }
    fn stage_generation_text(&mut self, next: GenerationPreferences, cx: &mut Context<Self>) {
        self.settings_ui.unsaved_text = Some(next);
        self.settings_ui.text_flush = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(TEXT_SAVE_DELAY_MS))
                .await;
            let _ = this.update(cx, |this, cx| this.flush_generation_text(cx));
        }));
    }
    /// Writes typed text that is still only in memory.
    pub(crate) fn flush_generation_text(&mut self, cx: &mut Context<Self>) {
        self.settings_ui.text_flush = None;
        if let Some(next) = self.settings_ui.unsaved_text.take() {
            self.save_generation_text_draft(next, cx);
        }
    }
    /// Whole-value commits include any typed text, so the staged copy is superseded.
    pub(crate) fn discard_staged_generation_text(&mut self) {
        self.settings_ui.text_flush = None;
        self.settings_ui.unsaved_text = None;
    }
    fn save_generation_text_draft(&mut self, next: GenerationPreferences, cx: &mut Context<Self>) {
        let previous = self
            .ordinary_preferences_submit_issue()
            .map(|issue| issue.message);
        let had_pending = self.settings_ui.pending_generation.is_some()
            || self.preferences.generation_intent().is_some();
        let result = self.preferences.save_generation(next.clone());
        self.settings_ui.pending_generation = pending_after_save(&result, next);
        match result {
            Ok(()) => {
                self.set_settings_feedback(
                    PreferenceGroup::Generation,
                    course2md::tr!("已暂存，点击「应用」后生效").into(),
                    false,
                    cx,
                );
                if had_pending {
                    self.refresh_preference_defaults(cx);
                }
                self.clear_resolved_generation_errors(previous);
            }
            Err(error) => {
                self.set_settings_save_failure(PreferenceGroup::Generation, &error, cx);
            }
        }
        cx.notify();
    }
    /// Runs a blocking credential step off the UI thread, then continues in
    /// this window with its result. The system vault can block on a prompt or
    /// a locked keychain, so the interface never waits on it.
    pub(crate) fn run_vault_step<T: Send + 'static>(
        &self,
        cx: &mut Context<Self>,
        work: impl FnOnce(&dyn crate::credentials::CredentialVault) -> T + Send + 'static,
        then: impl FnOnce(&mut Self, T, &mut Window, &mut Context<Self>) + 'static,
    ) {
        let vault = self.preferences.vault();
        let task = crate::spawn_blocking_io(move || work(vault.as_ref()));
        let handle = self.window;
        cx.spawn(async move |this, cx| {
            let Ok(result) = task.recv().await else {
                return;
            };
            let _ = cx.update_window(handle, |_, window, cx| {
                let _ = this.update(cx, |this, cx| then(this, result, window, cx));
            });
        })
        .detach();
    }

    /// Forgets credentials no configuration uses, off the UI thread. A
    /// failure leaves an orphaned entry that no configuration refers to.
    pub(crate) fn forget_credentials(&self, references: Vec<crate::credentials::CredentialRef>) {
        if references.is_empty() {
            return;
        }
        let vault = self.preferences.vault();
        drop(crate::spawn_blocking_io(move || {
            preferences::Store::remove_credentials(vault.as_ref(), &references)
        }));
    }

    /// Records the open editor's fields. A typed key is first stored in the
    /// vault off the UI thread; the call then returns false and `after`
    /// continues once the draft has been committed with it.
    fn save_open_service_draft(
        &mut self,
        include_key: bool,
        after: AfterDraftSaved,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(editor) = &self.settings_ui.editor else {
            return true;
        };
        if editor.vault_busy {
            return false;
        }
        if editor.pending_binding.is_some() || editor.saved_configuration.is_some() {
            return true;
        }
        let draft = editor.form.current_draft(cx);
        let key = include_key
            .then(|| editor.form.typed_key(cx))
            .filter(|key| !key.trim().is_empty());
        if key.is_none()
            && draft.revision == 0
            && draft.name.is_empty()
            && draft.address.is_empty()
            && draft.model.is_empty()
            && draft.credential.is_none()
        {
            return true;
        }
        if key.is_none() && self.preferences.draft(&draft.id) == Some(&draft) {
            return true;
        }
        if let Some(key) = key {
            if let Some(editor) = &mut self.settings_ui.editor {
                editor.vault_busy = true;
                editor.status = Some(course2md::tr!("正在保存密钥…").into());
            }
            let secret = Secret::new(key);
            self.run_vault_step(
                cx,
                move |vault| preferences::Store::insert_credential(vault, secret),
                move |this, stored, window, cx| {
                    this.finish_service_key(draft, stored, after, window, cx)
                },
            );
            cx.notify();
            return false;
        }
        match self.preferences.save_service_draft(draft, None) {
            Ok(draft) => {
                if let Some(editor) = &mut self.settings_ui.editor {
                    editor.form.draft = draft;
                    editor.save_failed = false;
                    editor.status = None;
                    if include_key {
                        editor.form.clear_key(window, cx);
                    }
                }
                cx.notify();
                true
            }
            Err(error) => {
                if let Some(editor) = &mut self.settings_ui.editor {
                    editor.status =
                        Some(course2md::tr!("这些修改尚未保存：{}", format!("{error:#}")));
                    editor.save_failed = true;
                }
                cx.notify();
                false
            }
        }
    }
    /// The commit step after a typed key reached the vault.
    fn finish_service_key(
        &mut self,
        draft: ServiceDraft,
        stored: Result<crate::credentials::CredentialRef>,
        after: AfterDraftSaved,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let open = self
            .settings_ui
            .editor
            .as_mut()
            .filter(|editor| editor.form.draft.id == draft.id);
        let Some(editor) = open else {
            // The editor closed meanwhile; nothing refers to the stored key.
            if let Ok(reference) = stored {
                self.forget_credentials(vec![reference]);
            }
            return;
        };
        editor.vault_busy = false;
        let committed = stored.and_then(|reference| {
            self.preferences
                .commit_service_draft(draft, Some(reference.clone()))
                .inspect_err(|_| self.forget_credentials(vec![reference]))
        });
        let Some(editor) = &mut self.settings_ui.editor else {
            return;
        };
        match committed {
            Ok(draft) => {
                editor.form.draft = draft;
                editor.save_failed = false;
                editor.status = None;
                editor.form.clear_key(window, cx);
                match after {
                    AfterDraftSaved::Test => self.start_service_test(window, cx),
                    AfterDraftSaved::Publish { continue_repair } => {
                        self.publish_service_editor(continue_repair, window, cx)
                    }
                }
            }
            Err(error) => {
                editor.status = Some(course2md::tr!("这些修改尚未保存：{}", format!("{error:#}")));
                editor.save_failed = true;
            }
        }
        cx.notify();
    }
    fn apply_language_preferences(&mut self, cx: &mut Context<Self>) {
        let raw = self.setting_value(EditField::Languages, cx);
        let languages: Vec<String> = raw
            .split([',', '，', '\n'])
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
            .collect();
        if languages
            .iter()
            .any(|v| v.len() > 35 || !v.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
        {
            self.set_settings_feedback(
                PreferenceGroup::Generation,
                course2md::tr!("语言代码只能包含字母、数字和连字符，请用逗号分隔各语言").into(),
                true,
                cx,
            );
            cx.notify();
            return;
        }
        let mut next = self.generation_edit_base();
        next.preferred_subtitle_languages = languages;
        next.subtitle_languages_draft = None;
        self.commit_generation(next, cx);
    }
    /// Shows a group's result. A success notice clears itself after a few seconds.
    pub(crate) fn set_settings_feedback(
        &mut self,
        group: PreferenceGroup,
        message: String,
        error: bool,
        cx: &mut Context<Self>,
    ) {
        if group == PreferenceGroup::Generation && error {
            self.settings_ui
                .generation_block_messages
                .insert(message.clone());
        }
        self.settings_ui.feedback_details.remove(&group);
        if !error {
            self.settings_ui.expanded_feedback.remove(&group);
        }
        self.settings_ui.feedback_expiry.remove(&group);
        if !error {
            let shown = message.clone();
            let expiry = cx.spawn(async move |this, cx| {
                cx.background_executor()
                    .timer(Duration::from_secs(SUCCESS_NOTICE_SECS))
                    .await;
                let _ = this.update(cx, |this, cx| {
                    if success_notice_current(this.settings_ui.feedback.get(&group), &shown) {
                        this.settings_ui.feedback.remove(&group);
                        cx.notify();
                    }
                });
            });
            self.settings_ui.feedback_expiry.insert(group, expiry);
        }
        self.settings_ui.feedback.insert(group, (message, error));
    }
    pub(crate) fn apply_settings_write_results(
        &mut self,
        results: Vec<(PreferenceGroup, Result<(), String>)>,
        cx: &mut Context<Self>,
    ) -> bool {
        let mut all_ok = true;
        for (group, result) in results {
            match result {
                Ok(()) => {
                    self.set_settings_feedback(group, course2md::tr!("已保存").into(), false, cx)
                }
                Err(error) => {
                    all_ok = false;
                    match group {
                        PreferenceGroup::Generation => {
                            if let Some(intent) = self.preferences.generation_intent().cloned() {
                                self.settings_ui.pending_generation = Some(intent);
                            }
                        }
                        PreferenceGroup::Application => {
                            if let Some(intent) = self.preferences.application_intent().cloned() {
                                self.settings_ui.pending_application = Some(intent);
                            }
                        }
                        PreferenceGroup::Services => {}
                    }
                    self.set_settings_save_failure(group, &anyhow::anyhow!(error), cx);
                }
            }
        }
        all_ok
    }

    pub(crate) fn set_settings_save_failure(
        &mut self,
        group: PreferenceGroup,
        error: &anyhow::Error,
        cx: &mut Context<Self>,
    ) {
        self.set_settings_feedback(
            group,
            preferences::save_failure_message(group, error),
            true,
            cx,
        );
        self.settings_ui.feedback_details.insert(
            group,
            course2md::tr!(
                "设置目录：{}\n{}",
                self.preferences.root().display(),
                format!("{error:#}")
            ),
        );
    }
    pub(crate) fn settings_group_notice(&self, group: PreferenceGroup) -> Option<(String, bool)> {
        if let Some((message, error)) = self.settings_ui.feedback.get(&group) {
            return Some((message.clone(), *error));
        }
        self.preferences
            .issues()
            .iter()
            .rev()
            .find(|issue| issue.group == group)
            .map(|issue| {
                (
                    issue.message.clone(),
                    issue.kind != preferences::SettingsIssueKind::Recovered,
                )
            })
    }
    pub(crate) fn group_feedback(&self, group: PreferenceGroup, cx: &mut Context<Self>) -> Div {
        self.group_feedback_with_retry_emphasis(group, false, cx)
    }
    pub(crate) fn group_feedback_with_retry_emphasis(
        &self,
        group: PreferenceGroup,
        primary_retry: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        let mut view = v_flex().w_full().min_w_0().gap_2();
        if let Some((message, error)) = self.settings_group_notice(group) {
            view = view.child(crate::motion::enter(
                SharedString::from(format!("preference-feedback-{}-{message}", group as usize)),
                h_flex()
                    .w_full()
                    .min_w_0()
                    .gap_2()
                    .items_center()
                    .child(
                        if error {
                            icons::warning()
                        } else {
                            icons::check_circle()
                        }
                        .size_4()
                        .flex_shrink_0()
                        .text_color(color(if error {
                            DANGER
                        } else {
                            SUCCESS
                        })),
                    )
                    .child(
                        accessible_text(
                            ("settings-group-feedback", group as usize),
                            message.clone(),
                        )
                        .flex_1()
                        .min_w_0()
                        .text_size(TEXT_SETTINGS_BODY)
                        .text_color(color(if error {
                            DANGER
                        } else {
                            GRAY
                        })),
                    ),
            ));
            let has_pending = match group {
                PreferenceGroup::Generation => self.settings_ui.pending_generation.is_some(),
                PreferenceGroup::Application => self.settings_ui.pending_application.is_some(),
                PreferenceGroup::Services => self.preferences.has_pending_services(),
            };
            if error && has_pending {
                let id = ("retry-settings-save", group as usize);
                let retry = if primary_retry {
                    primary_pill(id)
                } else {
                    control(id).settings_text()
                };
                view = view.child(
                    retry
                        .icon(icons::refresh())
                        .label(course2md::tr!("重试保存"))
                        .self_start()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.retry_ordinary_preferences(group, cx);
                        })),
                );
            }
            let detail = self
                .settings_ui
                .feedback_details
                .get(&group)
                .cloned()
                .or_else(|| {
                    self.preferences
                        .issues()
                        .iter()
                        .rev()
                        .find(|issue| issue.group == group)
                        .and_then(|issue| issue.detail.clone())
                });
            if let Some(detail) = detail {
                let expanded = self.settings_ui.expanded_feedback.contains(&group);
                view = view.child(
                    control(("settings-feedback-details", group as usize))
                        .settings_text()
                        .icon(icons::info())
                        .ghost()
                        .self_start()
                        .label(if expanded {
                            course2md::tr!("收起技术详情")
                        } else {
                            course2md::tr!("查看技术详情")
                        })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if !this.settings_ui.expanded_feedback.remove(&group) {
                                this.settings_ui.expanded_feedback.insert(group);
                            }
                            cx.notify();
                        })),
                );
                if expanded {
                    view = view.child(
                        div()
                            .id(("settings-feedback-technical-scroll", group as usize))
                            .w_full()
                            .min_w_0()
                            .max_h(px(140.))
                            .overflow_y_scroll()
                            .p_2()
                            .child(
                                accessible_text(
                                    ("settings-feedback-technical", group as usize),
                                    detail,
                                )
                                .w_full()
                                .min_w_0()
                                .whitespace_normal()
                                .text_size(TEXT_SETTINGS_AUX)
                                .text_color(color(GRAY)),
                            ),
                    );
                }
            }
        }
        if self.preferences.is_blocked(group) {
            view = view.child(
                control(("reset-settings-group", group as usize))
                    .settings_text()
                    .icon(icons::refresh())
                    .label(course2md::tr!("保留原文件并重置这些设置"))
                    .self_start()
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.restore_settings_group(group, window, cx);
                    })),
            );
        }
        view
    }
    pub(super) fn register_storage_location(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let prompt = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some(course2md::tr!("添加笔记库位置").into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = prompt.await;
            let path = match result {
                Ok(Ok(Some(mut paths))) => paths.drain(..).next(),
                Ok(Ok(None)) => return,
                Ok(Err(error)) => {
                    let _ = this.update(cx, |this, cx| {
                        this.workspace_error =
                            Some(course2md::tr!("无法选择文件夹：{}", format!("{error:#}")));
                        cx.notify();
                    });
                    return;
                }
                Err(error) => {
                    let _ = this.update(cx, |this, cx| {
                        this.workspace_error =
                            Some(course2md::tr!("无法打开文件选择器：{}", error));
                        cx.notify();
                    });
                    return;
                }
            };
            let Some(path) = path else { return };
            let name = path
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or(course2md::tr!("笔记库"))
                .to_owned();
            let lists = this.update(cx, |this, _| {
                let workspace = this
                    .workspace
                    .as_ref()
                    .ok_or_else(|| anyhow!(course2md::tr!("笔记库记录暂时不可用")))?;
                Ok::<_, anyhow::Error>((
                    workspace
                        .state
                        .libraries
                        .iter()
                        .map(|l| l.root.clone())
                        .collect::<Vec<_>>(),
                    workspace
                        .state
                        .libraries
                        .iter()
                        .map(|l| l.id.clone())
                        .collect::<Vec<_>>(),
                ))
            });
            let Ok(Ok((roots, ids))) = lists else { return };
            let task = crate::spawn_blocking_io(move || {
                crate::workspace::Workspace::prepare_library(path, name, false, &roots, &ids)
            });
            let prepared = task
                .recv()
                .await
                .unwrap_or_else(|_| Err(anyhow!(crate::interrupted())));
            let _ = this.update(cx, |this, cx| {
                match prepared {
                    Ok(prepared) => {
                        let duplicate = prepared.duplicate;
                        let registered = this
                            .workspace
                            .as_mut()
                            .ok_or_else(|| anyhow!(course2md::tr!("笔记库记录暂时不可用")))
                            .and_then(|workspace| workspace.register_prepared_library(&prepared));
                        match registered {
                            Ok(_) => {
                                this.message = Some(
                                    if duplicate {
                                        course2md::tr!("这个文件夹已是笔记库位置")
                                    } else {
                                        course2md::tr!("已添加笔记库位置")
                                    }
                                    .into(),
                                );
                                this.refresh_library(cx);
                            }
                            Err(error) => {
                                this.workspace_error = Some(course2md::tr!(
                                    "笔记库位置尚未添加：{}",
                                    format!("{error:#}")
                                ))
                            }
                        }
                    }
                    Err(error) => {
                        this.workspace_error = Some(course2md::tr!(
                            "笔记库位置尚未添加：{}",
                            format!("{error:#}")
                        ))
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
    pub(crate) fn appearance_controls(&self, cx: &mut Context<Self>) -> Div {
        use crate::preferences::InterfaceLanguage;
        let current = self.application_edit_base().language;
        let label = |language: InterfaceLanguage| match language {
            InterfaceLanguage::System => course2md::tr!("跟随系统"),
            other => other.resolve().native_name(),
        };
        let value: SharedString = if current == InterfaceLanguage::System {
            format!("{} · {}", label(current), current.resolve().native_name()).into()
        } else {
            label(current).into()
        };
        let entity = cx.weak_entity();
        let language_menu = move |menu: PopupMenu, _: &mut Window, _: &mut Context<PopupMenu>| {
            InterfaceLanguage::ALL
                .into_iter()
                .fold(menu.min_w(px(240.)), |menu, language| {
                    let entity = entity.clone();
                    let menu = menu.item(
                        PopupMenuItem::new(label(language))
                            .checked(language == current)
                            .on_click(move |_, _, cx| {
                                let _ = entity.update(cx, |this, cx| {
                                    let mut next = this.application_edit_base();
                                    next.language = language;
                                    if this.commit_application(next, cx) {
                                        crate::i18n::apply(language);
                                        cx.notify();
                                    }
                                });
                            }),
                    );
                    // 跟随系统 stands apart from the fixed languages.
                    if language == InterfaceLanguage::System {
                        menu.separator()
                    } else {
                        menu
                    }
                })
        };
        let language = tile_button("app-language", [true, true, false, false], cx)
            .min_h(rems(76. / 14.))
            .accessibility_label(course2md::tr!("{}：{}", course2md::tr!("界面语言"), value))
            .child(tile_text(
                "app-language-label",
                course2md::tr!("界面语言"),
                Some(value),
            ))
            .child(tile_chevron())
            .dropdown_menu_with_anchor(Anchor::TopRight, language_menu);
        v_flex()
            .w_full()
            .gap(px(28.))
            .child(
                group("appearance-motion", course2md::tr!("界面偏好"))
                    .tile(move |_| language.into_any_element())
                    .child(settings_stacked_row(
                        "app-font-scale-label",
                        course2md::tr!("界面文字大小"),
                        "",
                        self.setting_choices("app-font-scale", course2md::tr!("应用文字大小"))
                            .options(crate::preferences::FONT_SCALES.into_iter().map(|scale| {
                                (
                                    (scale * 100.).round().to_string(),
                                    format!("{}%", (scale * 100.) as u32),
                                )
                            }))
                            .full_width()
                            .selected(
                                (self.preferences.application().font_scale * 100.)
                                    .round()
                                    .to_string(),
                            )
                            .on_change(cx.listener(
                                move |this, selected: &SharedString, window, cx| {
                                    let Ok(percent) = selected.parse::<f32>() else {
                                        return;
                                    };
                                    let scale = percent / 100.;
                                    let mut next = this.application_edit_base();
                                    next.font_scale = scale;
                                    if this.commit_application(next, cx) {
                                        theme::apply_scale(scale, window, cx);
                                    }
                                },
                            )),
                    ))
                    .child(
                        setting_surface().child(settings_switch_row(
                            course2md::tr!("减少动态效果"),
                            course2md::tr!("关闭过渡动画和进度条的流动效果"),
                            Switch::new("app-reduce-motion")
                                .checked(self.preferences.application().desktop.reduce_motion)
                                .on_click(cx.listener(|this, enabled, _, cx| {
                                    let mut next = this.application_edit_base();
                                    next.desktop.reduce_motion = *enabled;
                                    this.commit_application(next, cx);
                                })),
                        )),
                    ),
            )
            .child(
                group("appearance-home", course2md::tr!("主页")).child(
                    setting_surface().child(settings_switch_row(
                        course2md::tr!("显示最近的笔记"),
                        course2md::tr!("关闭后主页不显示已完成的笔记"),
                        Switch::new("app-home-recent-notes")
                            .checked(self.preferences.application().desktop.home_recent_notes)
                            .on_click(cx.listener(|this, enabled, _, cx| {
                                let mut next = this.application_edit_base();
                                next.desktop.home_recent_notes = *enabled;
                                this.commit_application(next, cx);
                            })),
                    )),
                ),
            )
    }
    fn application_settings_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let project = self.about_link(
            "about-project",
            icons::external_link(),
            course2md::tr!("项目主页"),
            "https://github.com/mizorewww/course2md",
            [false; 4],
            cx,
        );
        let feedback = self.about_link(
            "about-issue",
            icons::message_square(),
            course2md::tr!("发送反馈"),
            "https://github.com/mizorewww/course2md/issues",
            [false; 4],
            cx,
        );
        let licenses = self.about_licenses([false; 4], cx);
        v_flex()
            .w_full()
            .min_w_0()
            .gap(px(28.))
            .child(self.environment_page(cx))
            .child(
                group("about-heading", course2md::tr!("关于"))
                    .child(self.about_page())
                    .pair(move |_| project, move |_| feedback)
                    .tile(move |_| licenses)
                    .child(
                        setting_surface().py(rems(12. / 14.)).child(
                            h_flex()
                                .w_full()
                                .min_w_0()
                                .min_h(rems(48. / 14.))
                                .gap_4()
                                .items_center()
                                .child(tile_text(
                                    "restart-onboarding-label",
                                    course2md::tr!("首次设置"),
                                    None,
                                ))
                                .child(
                                    outline_pill("restart-onboarding", cx)
                                        .settings_text()
                                        .pl(rems(12. / 14.))
                                        .label(course2md::tr!("重新运行"))
                                        .icon(icons::restart())
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.start_onboarding(window, cx);
                                        })),
                                ),
                        ),
                    ),
            )
            .into_any_element()
    }
    pub(crate) fn commit_application(
        &mut self,
        next: ApplicationPreferences,
        cx: &mut Context<Self>,
    ) -> bool {
        match self.preferences.save_application(next.clone()) {
            Ok(()) => {
                self.settings_ui.pending_application = None;
                self.desktop_settings = self.preferences.application().desktop.clone();
                cx.set_reduce_motion(self.desktop_settings.reduce_motion);
                self.set_settings_feedback(
                    PreferenceGroup::Application,
                    if self.preferences.is_pending(PreferenceGroup::Application) {
                        course2md::tr!("正在保存…")
                    } else {
                        course2md::tr!("已保存")
                    }
                    .into(),
                    false,
                    cx,
                );
                cx.notify();
                true
            }
            Err(error) => {
                self.settings_ui.pending_application = Some(next);
                self.set_settings_save_failure(PreferenceGroup::Application, &error, cx);
                cx.notify();
                false
            }
        }
    }
    /// Programs the app runs, and whether each was found.
    fn program_facts(
        &self,
        environment: &crate::backend::Environment,
    ) -> Vec<(&'static str, bool)> {
        let mut tools = vec![
            (course2md::tr!("转换程序"), environment.engine),
            ("ffmpeg", environment.ffmpeg),
            ("ffprobe", environment.ffprobe),
            ("yt-dlp", environment.ytdlp),
        ];
        if matches!(
            self.preferences
                .generation()
                .options
                .provider
                .unwrap_or_else(|| self.recommended_local_provider()),
            course2md::config::AsrProvider::Cpu | course2md::config::AsrProvider::Gpu
        ) {
            tools.push(("llama-server", environment.llama));
        }
        tools
    }

    /// 运行检查 as on the canvas: one conclusion with the capabilities it
    /// covers, then 诊断详情 with the facts behind it. Failures stay visible:
    /// the conclusion turns to a warning and each capability says whether it
    /// is ready.
    fn environment_page(&self, cx: &mut Context<Self>) -> SettingsGroup {
        let checks = group("diagnostics-heading", course2md::tr!("运行检查"));
        let Some(e) = &self.environment else {
            return checks.child(
                setting_surface().p(px(20.)).child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(crate::motion::spinner("diagnostic-checking-spinner", cx))
                        .child(settings_value(
                            "diagnostic-checking",
                            course2md::tr!("正在检查本机能力…"),
                        )),
                ),
            );
        };
        let provider = self
            .preferences
            .generation()
            .options
            .provider
            .unwrap_or_else(|| self.recommended_local_provider());
        let speech_service = provider == course2md::config::AsrProvider::Api;
        let local_ready = e.engine
            && match provider {
                course2md::config::AsrProvider::Coreml => e.apple,
                course2md::config::AsrProvider::Gpu => e.llama && e.gpu.is_some(),
                course2md::config::AsrProvider::Cpu => e.llama,
                course2md::config::AsrProvider::Npu => e.npu(),
                course2md::config::AsrProvider::Api => false,
            };
        let capabilities = [
            (
                course2md::tr!("视频处理与导出"),
                e.engine && e.ffmpeg && e.ffprobe,
                false,
            ),
            (course2md::tr!("读取在线视频"), e.ytdlp, false),
            (course2md::tr!("本机识别环境"), local_ready, speech_service),
        ];
        let checked = capabilities
            .iter()
            .filter(|(_, _, optional)| !optional)
            .count();
        let failed = capabilities
            .iter()
            .filter(|(_, ready, optional)| !ready && !optional)
            .count();
        let all_ready = failed == 0;
        let recheck = tonal_pill("refresh-environment", cx)
            .settings_text()
            .pl(rems(12. / 14.))
            .icon(icons::refresh())
            .label(course2md::tr!("重新检查"))
            .on_click(cx.listener(|this, _, _, cx| this.refresh_environment(cx)));
        let summary = setting_surface()
            .p(px(20.))
            .gap(px(16.))
            .child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .items_center()
                    .flex_wrap()
                    .gap(rems(16. / 14.))
                    .child(if all_ready {
                        tinted_icon(icons::check_circle(), Tint::Green, px(48.))
                    } else {
                        tinted_icon(icons::warning(), Tint::Amber, px(48.))
                    })
                    .child(
                        tile_text(
                            "diagnostic-summary",
                            if all_ready {
                                course2md::tr!("一切正常")
                            } else {
                                course2md::tr!("需要修复")
                            },
                            Some(
                                if all_ready {
                                    course2md::tr!("{} 项检查全部通过", checked)
                                } else {
                                    course2md::tr!("{} 项检查未通过", failed)
                                }
                                .into(),
                            ),
                        )
                        .flex_basis(rems(200. / 14.)),
                    )
                    .child(recheck),
            )
            // The capabilities sit on the summary's text ruler.
            .child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .pl(rems(64. / 14.))
                    .gap_2()
                    .flex_wrap()
                    .children(capabilities.iter().enumerate().map(
                        |(index, (label, ready, optional))| {
                            let (icon, tone, state) = if *optional {
                                (icons::info(), GRAY, course2md::tr!("当前未使用"))
                            } else if *ready {
                                (icons::check(), SUCCESS, course2md::tr!("可用"))
                            } else {
                                (icons::warning(), WARNING, course2md::tr!("待修复"))
                            };
                            settings_chip(h_flex())
                                .id(("diagnostic-capability", index))
                                .role(Role::Label)
                                .aria_label(course2md::tr!("{}：{}", *label, state))
                                .pr(rems(12. / 14.))
                                .gap(rems(6. / 14.))
                                .items_center()
                                .text_size(TEXT_SETTINGS_AUX)
                                .text_color(color(if *ready || *optional { INK } else { WARNING }))
                                .when(!*ready && !*optional, |chip| {
                                    chip.border_color(color(WARNING)).bg(color(WARNING_BG))
                                })
                                .child(icon.size(px(16.)).text_color(color(tone)))
                                .child(*label)
                                .when(!*ready || *optional, |chip| {
                                    chip.child(
                                        div()
                                            .text_color(color(if *optional {
                                                GRAY
                                            } else {
                                                WARNING
                                            }))
                                            .child(format!("· {state}")),
                                    )
                                })
                        },
                    )),
            );
        let mut checks = checks.child(summary);
        if !e.engine {
            checks = checks.child(
                setting_surface()
                    .child(tile_info(
                        "repair-bundled-engine",
                        course2md::tr!("转换程序无法运行，重新安装应用后笔记仍会保留"),
                    ))
                    .child(
                        outline_pill("download-repair-app", cx)
                            .settings_text()
                            .icon(icons::download())
                            .label(course2md::tr!("下载安装包"))
                            .self_start()
                            .on_click(|_, _, cx| {
                                cx.open_url("https://github.com/mizorewww/course2md/releases")
                            }),
                    ),
            );
        }
        let open = self.settings_ui.diagnostics_details_open;
        let toggle = tile_disclosure(
            "toggle-diagnostics-details",
            [false, false, !open, !open],
            open,
            cx,
        )
        .accessibility_label(course2md::tr!("诊断详情"))
        .child(tile_text(
            "diagnostics-details-label",
            course2md::tr!("诊断详情"),
            None,
        ))
        .child(disclosure_chevron(open))
        .on_click(cx.listener(|this, _, _, cx| {
            this.settings_ui.diagnostics_details_open = !this.settings_ui.diagnostics_details_open;
            cx.notify();
        }));
        checks = checks.tile(move |_| toggle.into_any_element());
        if open {
            checks = checks.child(crate::motion::disclosure(
                "diagnostics-detail-content",
                true,
                self.diagnostic_details(e, cx)
                    .px(SETTING_ITEM_INSET)
                    .pt(rems(16. / 14.))
                    .pb(rems(16. / 14.)),
            ));
        }
        checks
    }

    fn diagnostic_details(
        &self,
        environment: &crate::backend::Environment,
        cx: &mut Context<Self>,
    ) -> Div {
        let tools = self.program_facts(environment);
        let mut programs = tools.iter().enumerate().fold(
            settings_detail_group(
                "diagnostic-programs-heading",
                icons::code(),
                course2md::tr!("所需程序"),
            ),
            |group, (index, (name, found))| {
                let (icon, tone, state) = if *found {
                    (icons::check(), SUCCESS, course2md::tr!("已安装"))
                } else {
                    (icons::warning(), WARNING, course2md::tr!("未安装"))
                };
                group.row(
                    ("diagnostic-tool-label", index),
                    *name,
                    h_flex()
                        .gap(rems(6. / 14.))
                        .items_center()
                        .text_color(color(tone))
                        .child(icon.size(px(16.)))
                        .child(accessible_text(("diagnostic-tool", index), state)),
                )
            },
        );
        if !environment.ffmpeg || !environment.ffprobe || !environment.ytdlp {
            let (help, command) = if cfg!(target_os = "macos") {
                (course2md::tr!("在终端运行"), "brew install ffmpeg yt-dlp")
            } else if cfg!(target_os = "windows") {
                (
                    course2md::tr!("在 PowerShell 运行"),
                    "winget install Gyan.FFmpeg; winget install yt-dlp.yt-dlp",
                )
            } else {
                (
                    course2md::tr!("用系统包管理器安装 ffmpeg，再运行"),
                    "pipx install yt-dlp",
                )
            };
            programs = programs.row(
                "media-tools-repair-label",
                course2md::tr!("修复方式"),
                v_flex()
                    .w_full()
                    .min_w_0()
                    .gap_2()
                    .child(settings_value("install-media-tools-help", help))
                    .child(settings_value("install-media-tools-command", command))
                    .child(
                        quiet("copy-media-install-command")
                            .settings_text()
                            .ml(-QUIET_INSET)
                            .icon(icons::content_copy())
                            .label(course2md::tr!("复制安装命令"))
                            .self_start()
                            .on_click(move |_, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(command.into()))
                            }),
                    ),
            );
        }
        // The copied text is what the page shows, one fact per line.
        let hardware = self.hardware_facts();
        let mut report = vec![course2md::tr!("所需程序").to_owned()];
        report.extend(tools.iter().map(|(name, found)| {
            format!(
                "{name}: {}",
                if *found {
                    course2md::tr!("已安装")
                } else {
                    course2md::tr!("未安装")
                }
            )
        }));
        report.push(course2md::tr!("设备与运行时").to_owned());
        report.extend(
            hardware
                .iter()
                .map(|(_, label, value)| format!("{label}: {value}")),
        );
        let report = report.join("\n");
        v_flex()
            .w_full()
            .min_w_0()
            .gap_5()
            .pt_1()
            .child(programs)
            .children(self.model_hardware_details(cx))
            .child(
                h_flex().w_full().justify_end().child(
                    quiet("copy-diagnostics")
                        .settings_text()
                        .mr(-QUIET_INSET)
                        .text_color(color(ACCENT))
                        .icon(icons::content_copy())
                        .label(course2md::tr!("复制诊断信息"))
                        .on_click(move |_, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(report.clone()))
                        }),
                ),
            )
    }

    /// Service editor drafts are deliberately excluded: an existing published service
    /// continues to be usable. An unapplied failed ordinary commit must never be silently
    /// replaced by the older effective defaults when submitting a task.
    pub fn ordinary_preferences_ready_for_submit(&self) -> Result<()> {
        if let Some(issue) = self.ordinary_preferences_submit_issue() {
            return Err(anyhow!(issue.message));
        }
        Ok(())
    }
    pub fn ordinary_preferences_submit_issue(&self) -> Option<OrdinaryPreferenceIssue> {
        let group = PreferenceGroup::Generation;
        if self.preferences.is_blocked(PreferenceGroup::Generation) {
            return Some(OrdinaryPreferenceIssue {
                group,
                message: course2md::tr!("生成选项暂时无法读取，原文件与当前输入已保留").into(),
                can_retry: false,
            });
        }
        if self.settings_ui.pending_generation.is_some()
            || self.preferences.generation_intent().is_some()
        {
            return Some(OrdinaryPreferenceIssue {
                group,
                message: self
                    .settings_group_notice(group)
                    .filter(|(_, error)| *error)
                    .map(|(message, _)| message)
                    .unwrap_or_else(|| {
                        course2md::tr!("生成选项的修改尚未保存，当前输入已保留").into()
                    }),
                can_retry: true,
            });
        }
        None
    }
    pub fn retry_ordinary_preferences(
        &mut self,
        group: PreferenceGroup,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.preferences.is_blocked(group) {
            return false;
        }
        match group {
            PreferenceGroup::Generation => {
                if let Some(next) = self
                    .settings_ui
                    .pending_generation
                    .as_ref()
                    .or_else(|| self.preferences.generation_intent())
                    .cloned()
                {
                    self.commit_generation(next, cx)
                } else {
                    true
                }
            }
            PreferenceGroup::Application => {
                if let Some(next) = self
                    .settings_ui
                    .pending_application
                    .as_ref()
                    .or_else(|| self.preferences.application_intent())
                    .cloned()
                {
                    self.commit_application(next, cx)
                } else {
                    true
                }
            }
            PreferenceGroup::Services => {
                if self.preferences.retry_services().is_ok() {
                    self.set_settings_feedback(
                        PreferenceGroup::Services,
                        course2md::tr!("正在保存…").into(),
                        false,
                        cx,
                    );
                    true
                } else {
                    false
                }
            }
        }
    }
    /// Only the explicitly named group is reset; the form and its source stay in place.
    pub fn restore_ordinary_preferences(
        &mut self,
        group: PreferenceGroup,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if group == PreferenceGroup::Services {
            return false;
        }
        self.restore_settings_group(group, window, cx)
    }
    fn restore_settings_group(
        &mut self,
        group: PreferenceGroup,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let previous = self
            .ordinary_preferences_submit_issue()
            .map(|issue| issue.message);
        match self.preferences.reset_group(group) {
            Ok(()) => {
                match group {
                    PreferenceGroup::Generation => {
                        self.discard_staged_generation_text();
                        self.settings_ui.pending_generation = None;
                    }
                    PreferenceGroup::Application => self.settings_ui.pending_application = None,
                    PreferenceGroup::Services => {}
                }
                self.settings_ui.initialized = false;
                // Rehydrate on the event path now rather than on the next visit; rendering never does this.
                self.hydrate_settings_inputs(window, cx);
                self.set_settings_feedback(
                    group,
                    course2md::tr!("原文件已保留，这些设置已重置").into(),
                    false,
                    cx,
                );
                if group == PreferenceGroup::Application {
                    self.desktop_settings = self.preferences.application().desktop.clone();
                    cx.set_reduce_motion(self.desktop_settings.reduce_motion);
                } else {
                    self.refresh_preference_defaults(cx);
                }
                if group == PreferenceGroup::Generation {
                    self.clear_resolved_generation_errors(previous);
                }
                if !self.preference_defaults_pending {
                    self.advance_conversion_when_ready(cx);
                }
                cx.notify();
                true
            }
            Err(error) => {
                self.set_settings_save_failure(group, &error, cx);
                cx.notify();
                false
            }
        }
    }
    fn clear_resolved_generation_errors(&mut self, previous: Option<String>) {
        if self.ordinary_preferences_submit_issue().is_some() {
            return;
        }
        let mut messages = std::mem::take(&mut self.settings_ui.generation_block_messages);
        if let Some(previous) = previous {
            messages.insert(previous);
        }
        for error in [&mut self.source_validation, &mut self.message] {
            if error.as_ref().is_some_and(|error| messages.contains(error)) {
                *error = None;
            }
        }
    }
    pub fn relocate_settings_paths(
        &mut self,
        old: &std::path::Path,
        new: &std::path::Path,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        self.flush_generation_text(cx);
        let previous = self
            .ordinary_preferences_submit_issue()
            .map(|issue| issue.message);
        let result = self.preferences.relocate_generation_paths(old, new);
        self.settings_ui.pending_generation = self.preferences.generation_intent().cloned();
        if let Err(error) = &result {
            self.set_settings_feedback(
                PreferenceGroup::Generation,
                course2md::tr!(
                    "模型位置尚未更新，修改已保留，可重试保存：{}",
                    format!("{error:#}")
                ),
                true,
                cx,
            );
        } else {
            self.refresh_preference_defaults(cx);
            self.clear_resolved_generation_errors(previous);
        }
        cx.notify();
        result
    }
    /// Ordinary preferences save immediately. Service form edits become active
    /// only through Save and do not create a separate quit-time save workflow.
    pub fn flush_settings_for_exit(&mut self, cx: &mut Context<Self>) -> bool {
        if let Some(cancel) = self
            .settings_ui
            .editor
            .as_ref()
            .and_then(|editor| editor.test_running.as_ref())
        {
            cancel.store(true, Ordering::Release);
        }
        self.flush_generation_text(cx);
        if let Some(next) = self.settings_ui.pending_generation.clone() {
            self.commit_generation(next, cx);
        }
        if let Some(next) = self.settings_ui.pending_application.clone() {
            self.commit_application(next, cx);
        }
        self.preferences.flush();
        let results = self.preferences.process_writes();
        let failed = !self.apply_settings_write_results(results, cx);
        self.preferences.flush();
        if failed || !self.preferences.unsaved_intents_are_preserved() {
            self.message = Some(course2md::tr!("修改暂时无法保存，请重试").into());
            cx.notify();
            return false;
        }
        true
    }
}

/// A success notice clears itself only while it is still the group's notice;
/// a newer message, or an error that replaced it, stays.
fn success_notice_current(current: Option<&(String, bool)>, shown: &str) -> bool {
    matches!(current, Some((message, false)) if message == shown)
}

/// The staged edit after a save attempt: cleared once the store accepted it,
/// kept for retry and exit when the save failed.
fn pending_after_save<T, E>(result: &std::result::Result<(), E>, next: T) -> Option<T> {
    result.is_err().then_some(next)
}

impl Desktop {
    /// Settings is a place of its own: as on the canvas, its title bar is
    /// empty (window controls and dragging only); the way back and the 设置
    /// title lead the navigation pane instead.
    pub(crate) fn settings_title_bar(
        &self,
        bar: Div,
        window: &mut Window,
        _: &mut Context<Self>,
    ) -> TitleBar {
        crate::views::apply_shell_title_bar_chrome(TitleBar::new().h(title_bar_height(window)))
            .child(bar)
    }
}

#[cfg(test)]
mod tests {
    use super::{pending_after_save, success_notice_current};

    #[test]
    fn a_success_notice_expires_only_while_it_is_still_shown() {
        let shown = "已保存".to_owned();
        assert!(success_notice_current(
            Some(&(shown.clone(), false)),
            &shown
        ));
        // Cleared already, or replaced by a newer notice or by an error.
        assert!(!success_notice_current(None, &shown));
        assert!(!success_notice_current(
            Some(&("服务已保存".to_owned(), false)),
            &shown
        ));
        assert!(!success_notice_current(
            Some(&(shown.clone(), true)),
            &shown
        ));
    }

    #[test]
    fn a_failed_save_keeps_the_staged_edit_and_a_successful_one_clears_it() {
        assert_eq!(pending_after_save::<_, ()>(&Ok(()), 7), None);
        assert_eq!(pending_after_save(&Err("disk full"), 7), Some(7));
    }
}
