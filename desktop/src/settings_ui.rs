//! Settings: its navigation, the categories' pages, and how their saves report.
#[path = "generation_settings.rs"]
mod generation_settings;
#[path = "settings_legacy.rs"]
mod legacy;
#[path = "model_diagnostics.rs"]
pub(crate) mod model_diagnostics;
#[path = "service_editor.rs"]
mod service_editor;
#[path = "service_form.rs"]
pub(crate) mod service_form;
pub(crate) use legacy::*;

use crate::credentials::Secret;
use crate::preferences::{
    self, ApplicationPreferences, Authentication, BindingScope, GenerationPreferences,
    PreferenceGroup, ServiceDraft, ServiceProtocol, ServicePurpose, ServiceVersion,
};
use crate::service_test::{self, TestKind};
use crate::{ConversionOptions, Desktop, Page, symbols, ui};
use anyhow::{Context as _, Result, anyhow};
use course2md::execution::Component;
use gpui::{prelude::*, *};
use gpui_base::{h_flex, v_flex};
use gpui_kit::component::{
    input::{InputEvent, InputState, TextareaState},
    menu::{ContextMenuExt, PopupMenuItem},
    scroll::Scrollbar,
};
use gpui_kit_material::{
    icons::{Icon, SymbolIcon},
    motion,
    prelude::*,
};
use service_form::{FormField, FormOwner, ServiceForm};
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

actions!(
    course2md_settings,
    [
        NextSettingsTab,
        PreviousSettingsTab,
        FirstSettingsTab,
        LastSettingsTab
    ]
);

#[derive(Clone, Copy)]
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
}

pub struct OrdinaryPreferenceIssue {
    pub group: PreferenceGroup,
    pub message: String,
    pub can_retry: bool,
}

/// What continues once a typed key has been stored and the draft committed.
#[derive(Clone, Copy)]
enum AfterDraftSaved {
    Test,
    Publish { continue_repair: bool },
}

/// A settings menu's width in design px: its longest labels stay whole.
const MENU_WIDTH: f32 = 280.;

/// Pause after the last keystroke before typed settings text is written.
const TEXT_SAVE_DELAY_MS: u64 = 600;

pub(crate) struct State {
    model_diagnostics: model_diagnostics::State,
    /// Arrow keys move among the categories while focus is in their list.
    navigation_focus: FocusHandle,
    /// Settings search in the navigation pane.
    search: Entity<InputState>,
    languages: Entity<InputState>,
    local_model: Entity<InputState>,
    prompt: Entity<TextareaState>,
    editor: Option<ServiceEditor>,
    /// The closed editor, kept until the dialog has played its exit.
    closed_editor: Option<ServiceEditor>,
    /// Which mode's palettes the chooser shows, and whether it is open.
    pub(crate) palette_picker: (bool, bool),
    pub(crate) palette_scroll: ScrollHandle,
    initialized: bool,
    show_prompt: bool,
    custom_languages: bool,
    model_details_open: bool,
    diagnostics_details_open: bool,
    /// The last failed save of each group, and what lies behind it.
    failures: BTreeMap<PreferenceGroup, String>,
    failure_details: BTreeMap<PreferenceGroup, String>,
    expanded_feedback: BTreeSet<PreferenceGroup>,
    generation_block_messages: BTreeSet<String>,
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
        let mut subscriptions = Vec::new();
        let mut text_input = |field: EditField, window: &mut Window, cx: &mut Context<Desktop>| {
            let input = cx.new(|cx| InputState::new(window, cx));
            subscriptions.push(
                cx.subscribe_in(&input, window, move |this, _, event, _, cx| match event {
                    InputEvent::Change => this.setting_input_changed(field, cx),
                    InputEvent::Blur => this.flush_generation_text(cx),
                    _ => {}
                }),
            );
            input
        };
        let languages = text_input(EditField::Languages, window, cx);
        let local_model = text_input(EditField::LocalModel, window, cx);
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
            navigation_focus: cx.focus_handle(),
            search,
            languages,
            local_model,
            prompt,
            editor: None,
            closed_editor: None,
            palette_picker: (false, false),
            palette_scroll: ScrollHandle::new(),
            initialized: false,
            show_prompt: false,
            custom_languages: false,
            model_details_open: false,
            diagnostics_details_open: false,
            failures: BTreeMap::new(),
            failure_details: BTreeMap::new(),
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

/// The room a dialog leaves its content: the window less the dialog's margins,
/// headline and actions. Titles and actions stay in view while the content scrolls.
pub(crate) fn dialog_body_height(window: &Window) -> Pixels {
    let chrome = rem(152.).to_pixels(window.rem_size()) + px(48.);
    (window.viewport_size().height - chrome).max(px(120.))
}

/// One thing the person keeps (an account or a service): what it is, how it stands
/// and its commands. A long name wraps within its column; when the column and the
/// commands no longer fit side by side, the commands wrap below as one group.
pub(crate) struct Resource {
    pub(crate) id: SharedString,
    pub(crate) icon: Icon,
    pub(crate) title: SharedString,
    pub(crate) status: Vec<(StatusKind, &'static str)>,
    pub(crate) busy: bool,
    /// Lines under the name, and whether each is a failure.
    pub(crate) lines: Vec<(String, bool)>,
    pub(crate) actions: Vec<AnyElement>,
}

impl Resource {
    pub(crate) fn row(self, cx: &App) -> Div {
        let id = self.id;
        let child = |name: &'static str| ElementId::Name(format!("{id}-{name}").into());
        let scale = TileScale::Regular;
        let mut text = v_flex().flex_1().min_w_0().gap(rem(2.)).child(
            h_flex()
                .min_w_0()
                .flex_wrap()
                .gap(rem(8.))
                .items_center()
                .child(scale.label(child("title"), self.title))
                .when(self.busy, |row| {
                    row.child(spinner(child("busy"), course2md::tr!("正在检查"), cx))
                })
                .children(
                    self.status
                        .into_iter()
                        .map(|(kind, label)| status_chip(kind).child(label)),
                ),
        );
        for (index, (line, failed)) in self.lines.into_iter().enumerate() {
            let line_id = ElementId::NamedChild(Arc::new(child("line")), index.to_string().into());
            text = text.child(if failed {
                error_text(line_id, line).min_w_0().whitespace_normal()
            } else {
                scale.support(line_id, line)
            });
        }
        h_flex()
            .w_full()
            .min_w_0()
            .flex_wrap()
            .items_center()
            .gap(rem(12.))
            .child(
                h_flex()
                    .flex_1()
                    .flex_basis(rem(240.))
                    .min_w_0()
                    .gap(rem(16.))
                    .items_center()
                    .child(
                        self.icon
                            .size(rem(24.))
                            .color(color(ColorRole::ON_SURFACE_VARIANT)),
                    )
                    .child(text),
            )
            .when(!self.actions.is_empty(), |row| {
                row.child(
                    h_flex()
                        .flex_shrink_0()
                        .max_w_full()
                        .ml_auto()
                        .flex_wrap()
                        .items_center()
                        .gap(rem(8.))
                        // A command in a row that opens something does only its command.
                        .id(child("actions"))
                        .on_click(|_, _, cx| cx.stop_propagation())
                        .children(self.actions),
                )
            })
    }
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
            Self::Application => course2md::tr!("应用 / Application"),
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
    fn icon(self) -> SymbolIcon {
        match self {
            Self::Appearance => symbols::palette(),
            Self::Generation => symbols::article(),
            Self::Services => symbols::cloud(),
            Self::Storage => symbols::storage(),
            Self::Application => symbols::info(),
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
                course2md::tr!("自定义优先级"),
                course2md::tr!("语音识别"),
                course2md::tr!("识别方式"),
                course2md::tr!("识别模型"),
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

impl Desktop {
    /// The dialogs settings owns. They live in the shell's tree, because any page can
    /// open a service editor or the login.
    pub(crate) fn settings_dialogs(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> [AnyElement; 3] {
        [
            self.service_editor_dialog(window, cx),
            self.palette_dialog(window, cx),
            self.account_dialog(window, cx),
        ]
    }

    /// One section of a settings page: its rows scroll into view as they take focus.
    pub(crate) fn settings_fields(
        &self,
        id: &'static str,
        title: &'static str,
        fields: Vec<ui::Field>,
    ) -> ui::FieldTable {
        ui::field_table(id, title, fields).reveal_in(self.scrolls[Page::Settings as usize].clone())
    }
    fn generation_edit_base(&self) -> GenerationPreferences {
        self.settings_ui
            .unsaved_text
            .as_ref()
            .or(self.settings_ui.pending_generation.as_ref())
            .unwrap_or_else(|| self.preferences.generation())
            .clone()
    }
    pub(crate) fn application_edit_base(&self) -> ApplicationPreferences {
        self.settings_ui
            .pending_application
            .as_ref()
            .unwrap_or_else(|| self.preferences.application())
            .clone()
    }
    fn setting_input(&self, field: EditField, cx: &App) -> String {
        match field {
            EditField::LocalModel => &self.settings_ui.local_model,
            EditField::Languages => &self.settings_ui.languages,
        }
        .read(cx)
        .value()
        .to_string()
    }
    fn hydrate_settings_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings_ui.initialized {
            return;
        }
        self.settings_ui.initialized = true;
        let value = self.preferences.generation().clone();
        let languages = value
            .subtitle_languages_draft
            .unwrap_or_else(|| value.preferred_subtitle_languages.join(", "));
        let model = value
            .local_model_draft
            .unwrap_or_else(|| value.options.asr_model.unwrap_or_default());
        self.settings_ui
            .languages
            .update(cx, |input, cx| input.set_value(languages, window, cx));
        self.settings_ui
            .local_model
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
            || self.settings_ui.pending_generation.is_some()
            || self.settings_ui.pending_application.is_some()
            || !self.settings_ui.failures.is_empty()
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
        // Event-time preparation, not a render side effect: first hydration of inputs
        // and the deduplicated model check.
        self.prepare_settings_view(window, cx);
        self.settings_ui.navigation_focus.focus(window, cx);
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
        self.hydrate_settings_inputs(window, cx);
        self.ensure_settings_model_diagnostic(cx);
        self.ensure_account_cards(cx);
    }

    pub(crate) fn select_settings_tab(
        &mut self,
        tab: SettingsTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settings_tab = tab;
        self.scrolls[Page::Settings as usize].set_offset(point(px(0.), px(0.)));
        // Capture builds can review the lower part of a category.
        #[cfg(feature = "capture")]
        if let Some(offset) = std::env::var("COURSE2MD_CAPTURE_SETTINGS_SCROLL")
            .ok()
            .and_then(|value| value.parse::<f32>().ok())
        {
            self.scrolls[Page::Settings as usize].set_offset(point(px(0.), px(-offset)));
        }
        // Capture builds can review every disclosure expanded.
        #[cfg(feature = "capture")]
        if std::env::var_os("COURSE2MD_CAPTURE_SETTINGS_EXPANDED").is_some() {
            let ui = &mut self.settings_ui;
            ui.diagnostics_details_open = true;
            ui.model_details_open = true;
            ui.custom_languages = true;
            ui.show_prompt = true;
        }
        #[cfg(feature = "capture")]
        if let Ok(query) = std::env::var("COURSE2MD_CAPTURE_SETTINGS_SEARCH") {
            self.settings_ui
                .search
                .update(cx, |search, cx| search.set_value(query, window, cx));
            self.settings_search_changed(cx);
        }
        self.settings_ui.navigation_focus.focus(window, cx);
        self.ensure_account_cards(cx);
        cx.notify();
    }

    /// Services and accounts: bring the ChatGPT account up to date (busy and checked guards avoid repeats).
    fn ensure_account_cards(&mut self, cx: &mut Context<Self>) {
        if self.settings_tab == SettingsTab::Services
            && self.codex.status.is_none()
            && !self.codex.busy()
        {
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

    fn settings_back_button(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let origin = self
            .settings_origin
            .as_ref()
            .map_or(Page::New, |(page, _)| *page);
        let name = match origin {
            Page::Library | Page::Result => course2md::tr!("笔记库"),
            Page::Task => course2md::tr!("任务"),
            _ => course2md::tr!("主页"),
        };
        let label = course2md::tr!("返回「{}」", name);
        MaterialTooltip::plain("settings-back-tip", label.clone()).trigger(
            IconButton::new("settings-back", symbols::arrow_back(), label)
                .variant(IconButtonVariant::Tonal)
                .on_click(cx.listener(|this, _, window, cx| this.leave_settings(window, cx))),
        )
    }

    /// The leading pane: the way back, the 设置 title, search and the categories as
    /// segmented lists grouped by relationship. It scrolls on its own.
    fn settings_navigation(&self, cx: &mut Context<Self>) -> AnyElement {
        let visible = self.visible_settings_tabs(cx);
        let groups: [&[SettingsTab]; 3] = [
            &[SettingsTab::Appearance, SettingsTab::Generation],
            &[SettingsTab::Services, SettingsTab::Storage],
            &[SettingsTab::Application],
        ];
        let lists = groups.into_iter().filter_map(|tabs| {
            let items: Vec<_> = tabs
                .iter()
                .filter(|tab| visible.contains(tab))
                .map(|&tab| {
                    ListItem::new(("settings-tab", tab.index()), tab.label())
                        .supporting(tab.subtitle())
                        .leading(ListLeading::Icon(tab.icon().into()))
                        .selected(self.settings_tab == tab)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.select_settings_tab(tab, window, cx)
                        }))
                })
                .collect();
            (!items.is_empty()).then(|| segmented_list(items))
        });
        let list = v_flex()
            .id("settings-categories")
            .w_full()
            .min_w_0()
            .gap(rem(16.))
            .key_context("SettingsTabs")
            .track_focus(&self.settings_ui.navigation_focus)
            .tab_stop(false)
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
            .children(lists)
            .when(visible.is_empty(), |list| {
                list.child(
                    TileScale::Regular
                        .support("settings-search-empty", course2md::tr!("没有匹配的设置"))
                        .px(ITEM_INSET),
                )
            });
        div()
            .id("settings-navigation")
            .size_full()
            .min_h_0()
            .overflow_y_scroll()
            .child(
                v_flex()
                    .w_full()
                    .min_w_0()
                    .gap(rem(16.))
                    .pb(rem(64.))
                    .child(
                        h_flex()
                            .min_h(rem(56.))
                            .items_center()
                            .gap(rem(16.))
                            .child(self.settings_back_button(cx))
                            .child(page_heading("settings-page-title", course2md::tr!("设置"))),
                    )
                    .child(
                        SearchField::new(&self.settings_ui.search)
                            .id("settings-search")
                            .leading(
                                Icon::from(symbols::search())
                                    .size(rem(24.))
                                    .color(color(ColorRole::ON_SURFACE_VARIANT)),
                            ),
                    )
                    .child(list),
            )
            .into_any_element()
    }

    /// Narrow windows keep the categories in a navigation rail beside the content.
    fn settings_rail(&self, cx: &mut Context<Self>) -> AnyElement {
        let current = self.settings_tab;
        NavigationRail::new("settings-rail", course2md::tr!("设置分类"))
            .menu(self.settings_back_button(cx))
            .items(SettingsTab::ALL.map(|tab| {
                NavItem::new(
                    tab.index().to_string(),
                    tab.short_label(),
                    NavIcon::symbol(tab.icon().symbol()),
                )
            }))
            .selected(current.index().to_string())
            .on_change(cx.listener(|this, value: &SharedString, window, cx| {
                let tab = value
                    .parse::<usize>()
                    .ok()
                    .and_then(|index| SettingsTab::ALL.get(index));
                if let Some(tab) = tab {
                    this.select_settings_tab(*tab, window, cx);
                }
            }))
            .into_any_element()
    }

    /// What is wrong with this category's saves, and a way to the others' failures.
    fn settings_notices(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut notices = Vec::new();
        for (group, target) in [
            (PreferenceGroup::Generation, SettingsTab::Generation),
            (PreferenceGroup::Services, SettingsTab::Services),
            (PreferenceGroup::Application, SettingsTab::Appearance),
        ] {
            let failed = self
                .settings_group_notice(group)
                .filter(|(_, error)| *error);
            if self.settings_tab.group() == Some(group) {
                if self.preferences.is_blocked(group) || failed.is_some() {
                    notices.push(self.group_feedback(group, cx).into_any_element());
                }
            } else if let Some((message, _)) = failed {
                notices.push(
                    notice_banner(
                        ("settings-failed-group", target.index()),
                        NoticeKind::Danger,
                        message,
                    )
                    .child(
                        material_button(
                            ("open-failed-settings-group", target.index()),
                            ButtonVariant::Text,
                        )
                        .child(course2md::tr!("前往「{}」", target.label()))
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                this.select_settings_tab(target, window, cx);
                            },
                        )),
                    )
                    .into_any_element(),
                );
            }
        }
        notices
    }

    pub fn settings_page(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        // Rendering has no side effects: hydration and diagnostics run from event entries such as
        // open_settings, select_settings_tab and restore_settings_group.
        let compact = is_compact(window);
        let navigation = if compact {
            self.settings_rail(cx)
        } else {
            self.settings_navigation(cx)
        };
        let content = match self.settings_tab {
            SettingsTab::Generation => self.generation_settings_page(window, cx),
            SettingsTab::Services => self.services_settings_page(window, cx),
            SettingsTab::Storage => self.storage_settings_page(window, cx),
            SettingsTab::Application => self.application_settings_page(cx),
            SettingsTab::Appearance => self.appearance_page(window, cx),
        };
        let heading = v_flex()
            .w_full()
            .min_w_0()
            .gap(rem(6.))
            .child(h_flex().min_h(rem(56.)).child(page_heading(
                "settings-category-heading",
                self.settings_tab.label(),
            )))
            .when(self.settings_tab == SettingsTab::Generation, |view| {
                view.child(TileScale::Regular.support(
                    "generation-default-explanation",
                    course2md::tr!("这里是长期默认值，主页的「处理选项」可以只对单次生效"),
                ))
            });
        let scroll = &self.scrolls[Page::Settings as usize];
        let panel = v_flex()
            .relative()
            .id("settings-panel")
            .flex_1()
            .max_w(rem(760.))
            .min_w_0()
            .min_h_0()
            .h_full()
            .gap(rem(16.))
            .child(
                v_flex()
                    .w_full()
                    .min_w_0()
                    .flex_shrink_0()
                    .pr(rem(12.))
                    .gap(rem(8.))
                    .children(self.settings_notices(cx)),
            )
            .child(
                div()
                    .id("settings-content-scroll")
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .w_full()
                    .overflow_y_scroll()
                    .track_scroll(scroll)
                    .child(
                        // A category arrives as one unit; the navigation stays a landmark.
                        // The right inset is the scrollbar's gutter, so it never covers a tile.
                        div().w_full().min_w_0().pr(rem(12.)).pb(rem(64.)).child(
                            motion::page_enter(
                                ("settings-category", self.settings_tab.index()),
                                v_flex()
                                    .w_full()
                                    .min_w_0()
                                    .gap(rem(28.))
                                    .child(heading)
                                    .child(content),
                            ),
                        ),
                    ),
            )
            .child(Scrollbar::vertical(scroll));
        h_flex()
            .size_full()
            .max_w(rem(1120.))
            .mx_auto()
            .min_h_0()
            .items_start()
            .pt(rem(8.))
            .gap(rem(if compact { 8. } else { 40. }))
            .child(
                div()
                    .when(!compact, |pane| pane.w(rem(320.)))
                    .flex_shrink_0()
                    .h_full()
                    .min_h_0()
                    .child(navigation),
            )
            .child(panel)
            .into_any_element()
    }

    fn services_settings_page(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let mut view = v_flex().w_full().min_w_0().gap(rem(28.));
        let account = self.account_row(course2md::tr!("Bilibili 账号"), window, cx);
        view = view.child(self.settings_fields(
            "account-settings-heading",
            course2md::tr!("来源账号"),
            vec![ui::Field::content("bilibili-account-row", account)],
        ));
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
            // Without a speech service, recognition runs on this device.
            let hint = if purpose == ServicePurpose::Speech && services.is_empty() {
                course2md::tr!("未添加时使用本机识别")
            } else {
                ""
            };
            let mut fields: Vec<_> = services
                .iter()
                .map(|version| self.service_row(version, window, cx))
                .collect();
            fields.push(
                ui::Field::link(
                    format!("add-service-{}", purpose as usize),
                    add_label,
                    cx.listener(move |this, _, window, cx| {
                        this.open_settings_service_editor(purpose, None, window, cx);
                    }),
                )
                .icon(symbols::add())
                .hint(hint),
            );
            view = view.child(self.settings_fields(heading_id, heading, fields));
        }
        view.into_any_element()
    }

    /// One saved service: what it is, whether it works, and its commands in a menu that
    /// the row's ⋯ button and a right-click open alike. The row itself opens the editor.
    fn service_row(
        &self,
        version: &ServiceVersion,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> ui::Field {
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
        let tests = self.service_tests(version);
        let mut detail = vec![protocol.choice_label().to_owned()];
        let host = version.config.host();
        if protocol != ServiceProtocol::CodexResponses && !host.is_empty() {
            detail.push(host);
        }
        detail.push(version.config.model.clone());
        let mut lines = vec![(detail.join(" · "), false)];
        if let Some(default) = default_version.filter(|default| default.id != version.id) {
            lines.push((
                course2md::tr!(
                    "以后生成的笔记仍使用修改前的配置 · {}",
                    default.config.model
                ),
                false,
            ));
        }
        // An account action started here reports here.
        if protocol == ServiceProtocol::CodexResponses {
            lines.extend(
                self.codex
                    .notice_on(crate::codex_ui::CodexSurface::Settings),
            );
        }
        lines.extend(
            tests
                .iter()
                .filter(|(_, evidence)| evidence.outcome != preferences::TestOutcome::Passed)
                .map(|(kind, evidence)| {
                    (
                        course2md::tr!("{}：{}", kind.label(), evidence.message),
                        false,
                    )
                }),
        );
        let mut status = vec![self.service_status(version, &tests)];
        if is_default {
            status.insert(0, (StatusKind::Tonal, course2md::tr!("默认")));
        }
        let actions = self.service_actions(version, is_default);
        let menu_id = ElementId::Name(format!("service-actions-{id}").into());
        let trigger = MaterialTooltip::plain(
            SharedString::from(format!("service-actions-tip-{id}")),
            course2md::tr!("服务操作"),
        )
        .trigger(
            IconButton::new(
                SharedString::from(format!("service-actions-button-{id}")),
                symbols::more_vert(),
                course2md::tr!("{} 操作", version.config.name),
            )
            .on_click(ui::menu_key_toggle(&menu_id, window, cx)),
        );
        let items = actions.iter().map(|action| {
            let (kind, version) = (action.kind, version.clone());
            let icon = Icon::from(action.icon);
            MaterialMenuItem::new(action.label)
                .leading(if action.danger {
                    icon.color(color(ColorRole::ERROR))
                } else {
                    icon
                })
                .disabled(action.disabled)
                .divider(action.divider)
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.run_service_action(kind, &version, window, cx)
                }))
        });
        let menu = ui::menu(
            menu_id,
            trigger,
            MaterialMenu::new(SharedString::from(format!("service-action-items-{id}")))
                .width(MENU_WIDTH)
                .items(items),
            window,
            cx,
        )
        .anchor(Anchor::TopRight);
        let row = Resource {
            id: SharedString::from(format!("saved-service-{id}")),
            icon: service_form::protocol_icon(protocol).into(),
            title: version.config.name.clone().into(),
            status,
            busy: false,
            lines,
            actions: vec![menu.into_any_element()],
        }
        .row(cx);
        let this = cx.weak_entity();
        let context_version = version.clone();
        let open_version = id.clone();
        let open = cx.listener(move |this, _, window, cx| {
            this.open_settings_service_editor(purpose, Some(open_version.clone()), window, cx);
        });
        ui::Field::tile(format!("saved-service-row-{id}"), move |outer| {
            div()
                .id(SharedString::from(format!("service-context-{id}")))
                .w_full()
                .min_w_0()
                .child(
                    TileScale::Regular
                        .button(SharedString::from(format!("saved-service-{id}")), outer)
                        .on_click(open)
                        .child(row),
                )
                .context_menu(move |menu, _, _| {
                    actions.iter().fold(menu, |menu, action| {
                        let (kind, version, this) =
                            (action.kind, context_version.clone(), this.clone());
                        let item = menu.item(
                            PopupMenuItem::new(action.label)
                                .icon(action.icon.component())
                                .disabled(action.disabled)
                                .on_click(move |_, window, cx| {
                                    let _ = this.update(cx, |this, cx| {
                                        this.run_service_action(kind, &version, window, cx)
                                    });
                                }),
                        );
                        if action.divider {
                            item.separator()
                        } else {
                            item
                        }
                    })
                })
                .into_any_element()
        })
    }

    /// 打开 → 处理 → 管理 → 危险, the order every context menu follows.
    fn service_actions(
        &self,
        version: &ServiceVersion,
        is_default: bool,
    ) -> Rc<Vec<ServiceAction>> {
        let action = |kind, label, icon| ServiceAction {
            kind,
            label,
            icon,
            disabled: false,
            divider: false,
            danger: false,
        };
        let mut actions = vec![
            action(
                ServiceCommand::Test,
                course2md::tr!("测试连接"),
                symbols::science(),
            ),
            action(
                ServiceCommand::Edit,
                course2md::tr!("编辑…"),
                symbols::edit(),
            ),
            ServiceAction {
                disabled: is_default,
                divider: true,
                ..action(
                    ServiceCommand::MakeDefault,
                    if is_default {
                        course2md::tr!("已是默认服务")
                    } else {
                        course2md::tr!("设为默认服务…")
                    },
                    symbols::check_circle(),
                )
            },
        ];
        if version.config.protocol == ServiceProtocol::CodexResponses
            && matches!(
                self.codex.status,
                Some(course2md::login::codex::LoginStatus::Connected)
            )
        {
            actions.push(ServiceAction {
                disabled: self.codex.busy(),
                ..action(
                    ServiceCommand::SignOut,
                    course2md::tr!("退出 ChatGPT 账号"),
                    symbols::logout(),
                )
            });
        }
        actions.push(ServiceAction {
            danger: true,
            ..action(
                ServiceCommand::Delete,
                course2md::tr!("删除服务…"),
                symbols::delete(),
            )
        });
        Rc::new(actions)
    }

    fn run_service_action(
        &mut self,
        command: ServiceCommand,
        version: &ServiceVersion,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let purpose = version.config.protocol.purpose();
        match command {
            ServiceCommand::Test | ServiceCommand::Edit => {
                let opened = self.open_settings_service_editor(
                    purpose,
                    Some(version.id.clone()),
                    window,
                    cx,
                );
                if opened && matches!(command, ServiceCommand::Test) {
                    self.start_service_test(window, cx);
                }
            }
            ServiceCommand::MakeDefault => {
                self.confirm_default_service(version.id.clone(), window, cx)
            }
            ServiceCommand::SignOut => {
                self.codex_logout(crate::codex_ui::CodexSurface::Settings, cx)
            }
            ServiceCommand::Delete => {
                self.confirm_delete_service(version.service_id.clone(), window, cx)
            }
        }
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

    /// One status per service: an account's sign-in state, otherwise what its tests
    /// established.
    fn service_status(
        &self,
        version: &ServiceVersion,
        tests: &[(TestKind, &preferences::ServiceTestEvidence)],
    ) -> (StatusKind, &'static str) {
        if version.config.protocol == ServiceProtocol::CodexResponses {
            return self.codex.badge();
        }
        if tests.is_empty() {
            (StatusKind::Neutral, course2md::tr!("尚未测试"))
        } else if tests
            .iter()
            .all(|(_, evidence)| evidence.outcome == preferences::TestOutcome::Passed)
        {
            (StatusKind::Success, course2md::tr!("测试通过"))
        } else {
            (StatusKind::Warning, course2md::tr!("测试未通过"))
        }
    }

    fn setting_input_changed(&mut self, field: EditField, cx: &mut Context<Self>) {
        if !self.settings_ui.initialized {
            return;
        }
        let input = self.setting_input(field, cx);
        let mut next = self.generation_edit_base();
        let draft = match field {
            EditField::LocalModel => {
                let saved = next.options.asr_model.clone().unwrap_or_default();
                (&mut next.local_model_draft, saved)
            }
            EditField::Languages => {
                let saved = next.preferred_subtitle_languages.join(", ");
                (&mut next.subtitle_languages_draft, saved)
            }
        };
        if draft.0.as_ref().unwrap_or(&draft.1) == &input {
            return;
        }
        *draft.0 = Some(input);
        self.stage_generation_text(next, cx);
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
        let had_pending = self.settings_ui.pending_generation.is_some();
        let result = self.preferences.save_generation(next.clone());
        self.settings_ui.pending_generation = pending_after_save(&result, next);
        match result {
            Ok(()) => {
                self.clear_settings_failure(PreferenceGroup::Generation);
                if had_pending {
                    self.refresh_preference_defaults(cx);
                }
                self.clear_resolved_generation_errors(previous);
            }
            Err(error) => {
                self.set_settings_save_failure(PreferenceGroup::Generation, &error);
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

    fn apply_language_preferences(&mut self, cx: &mut Context<Self>) {
        let raw = self.setting_input(EditField::Languages, cx);
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
            self.set_settings_failure(
                PreferenceGroup::Generation,
                course2md::tr!("语言代码只能包含字母、数字和连字符，请用逗号分隔各语言").into(),
            );
            cx.notify();
            return;
        }
        let mut next = self.generation_edit_base();
        next.preferred_subtitle_languages = languages;
        next.subtitle_languages_draft = None;
        self.commit_generation(next, cx);
    }
    /// Records why a group's save failed; it shows until the next save succeeds.
    pub(crate) fn set_settings_failure(&mut self, group: PreferenceGroup, message: String) {
        if group == PreferenceGroup::Generation {
            self.settings_ui
                .generation_block_messages
                .insert(message.clone());
        }
        self.settings_ui.failure_details.remove(&group);
        self.settings_ui.failures.insert(group, message);
    }
    pub(crate) fn clear_settings_failure(&mut self, group: PreferenceGroup) {
        self.settings_ui.failures.remove(&group);
        self.settings_ui.failure_details.remove(&group);
        self.settings_ui.expanded_feedback.remove(&group);
    }
    pub(crate) fn set_settings_save_failure(
        &mut self,
        group: PreferenceGroup,
        error: &anyhow::Error,
    ) {
        self.set_settings_failure(group, preferences::save_failure_message(group, error));
        self.settings_ui.failure_details.insert(
            group,
            course2md::tr!(
                "设置目录：{}\n{}",
                self.preferences.root().display(),
                format!("{error:#}")
            ),
        );
    }
    pub(crate) fn settings_group_notice(&self, group: PreferenceGroup) -> Option<(String, bool)> {
        if let Some(message) = self.settings_ui.failures.get(&group) {
            return Some((message.clone(), true));
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
    /// A group's failure with its recovery: retry a kept edit, read what went wrong,
    /// or set aside a file that cannot be read. The edit stays until one of them works.
    pub(crate) fn group_feedback(&self, group: PreferenceGroup, cx: &mut Context<Self>) -> Div {
        let index = group as usize;
        let (message, error) = self.settings_group_notice(group).unwrap_or_default();
        let pending = match group {
            PreferenceGroup::Generation => self.settings_ui.pending_generation.is_some(),
            PreferenceGroup::Application => self.settings_ui.pending_application.is_some(),
            PreferenceGroup::Services => false,
        };
        let detail = self
            .settings_ui
            .failure_details
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
        let expanded = self.settings_ui.expanded_feedback.contains(&group);
        let toggle = detail.is_some().then(|| {
            material_button(("settings-feedback-details", index), ButtonVariant::Text)
                .icon(symbols::info())
                .child(if expanded {
                    course2md::tr!("收起技术详情")
                } else {
                    course2md::tr!("查看技术详情")
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    if !this.settings_ui.expanded_feedback.remove(&group) {
                        this.settings_ui.expanded_feedback.insert(group);
                    }
                    cx.notify();
                }))
        });
        let banner = notice_banner(
            ("settings-group-feedback", index),
            if error {
                NoticeKind::Danger
            } else {
                NoticeKind::Info
            },
            message,
        )
        .when(error && pending, |banner| {
            banner.child(
                material_button(("retry-settings-save", index), ButtonVariant::Tonal)
                    .icon(symbols::refresh())
                    .child(course2md::tr!("重试保存"))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.retry_ordinary_preferences(group, cx);
                    })),
            )
        })
        .children(toggle)
        .when(self.preferences.is_blocked(group), |banner| {
            banner.child(
                material_button(("reset-settings-group", index), ButtonVariant::Text)
                    .icon(symbols::refresh())
                    .child(course2md::tr!("保留原文件并重置这些设置"))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.restore_settings_group(group, window, cx);
                    })),
            )
        });
        v_flex()
            .w_full()
            .min_w_0()
            .flex_shrink_0()
            .gap(rem(8.))
            .child(banner)
            .when_some(detail.filter(|_| expanded), |view, detail| {
                view.child(
                    div()
                        .id(("settings-feedback-technical-scroll", index))
                        .w_full()
                        .min_w_0()
                        .max_h(rem(140.))
                        .overflow_y_scroll()
                        .px(rem(12.))
                        .child(
                            TileScale::Regular
                                .support(("settings-feedback-technical", index), detail),
                        ),
                )
            })
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
            let path = match prompt.await {
                Ok(Ok(Some(mut paths))) => paths.drain(..).next(),
                Ok(Ok(None)) => None,
                Ok(Err(error)) => {
                    let message = course2md::tr!("无法选择文件夹：{}", format!("{error:#}"));
                    let _ = this.update(cx, |this, cx| this.storage_location_failed(message, cx));
                    None
                }
                Err(error) => {
                    let message = course2md::tr!("无法打开文件选择器：{}", error);
                    let _ = this.update(cx, |this, cx| this.storage_location_failed(message, cx));
                    None
                }
            };
            let Some(path) = path else { return };
            let name = path
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or(course2md::tr!("笔记库"))
                .to_owned();
            let lists = this.update(cx, |this, _| {
                let libraries = &this.workspace.as_ref()?.state.libraries;
                Some((
                    libraries.iter().map(|l| l.root.clone()).collect::<Vec<_>>(),
                    libraries.iter().map(|l| l.id.clone()).collect::<Vec<_>>(),
                ))
            });
            let Ok(Some((roots, ids))) = lists else {
                return;
            };
            let task = crate::spawn_blocking_io(move || {
                crate::workspace::Workspace::prepare_library(path, name, false, &roots, &ids)
            });
            let prepared = task
                .recv()
                .await
                .unwrap_or_else(|_| Err(anyhow!(crate::interrupted())));
            let _ = this.update(cx, |this, cx| {
                let registered = prepared.and_then(|prepared| {
                    let workspace = this
                        .workspace
                        .as_mut()
                        .ok_or_else(|| anyhow!(course2md::tr!("笔记库记录暂时不可用")))?;
                    workspace.register_prepared_library(&prepared)?;
                    Ok(prepared.duplicate)
                });
                match registered {
                    Ok(duplicate) => {
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
                    Err(error) => this.storage_location_failed(
                        course2md::tr!("笔记库位置尚未添加：{}", format!("{error:#}")),
                        cx,
                    ),
                }
                cx.notify();
            });
        })
        .detach();
    }
    fn storage_location_failed(&mut self, message: String, cx: &mut Context<Self>) {
        self.workspace_error = Some(message);
        cx.notify();
    }
    fn application_settings_page(&self, cx: &mut Context<Self>) -> AnyElement {
        v_flex()
            .w_full()
            .min_w_0()
            .gap(rem(28.))
            .child(self.settings_fields(
                "diagnostics-heading",
                course2md::tr!("运行检查"),
                self.environment_fields(cx),
            ))
            .child(
                self.settings_fields(
                    "about-heading",
                    course2md::tr!("关于"),
                    crate::about::fields(ui::Field::inline(
                        "restart-onboarding-row",
                        course2md::tr!("首次设置"),
                        material_button("restart-onboarding", ButtonVariant::Outlined)
                            .icon(symbols::restart_alt())
                            .child(course2md::tr!("重新运行"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.start_onboarding(window, cx);
                            })),
                    )),
                ),
            )
            .into_any_element()
    }
    pub(crate) fn commit_application(
        &mut self,
        next: ApplicationPreferences,
        cx: &mut Context<Self>,
    ) -> bool {
        let saved = match self.preferences.save_application(next.clone()) {
            Ok(()) => {
                self.settings_ui.pending_application = None;
                self.desktop_settings = self.preferences.application().desktop.clone();
                cx.set_reduce_motion(self.desktop_settings.reduce_motion);
                self.clear_settings_failure(PreferenceGroup::Application);
                true
            }
            Err(error) => {
                self.settings_ui.pending_application = Some(next);
                self.set_settings_save_failure(PreferenceGroup::Application, &error);
                false
            }
        };
        cx.notify();
        saved
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
            if cfg!(windows) {
                tools.push((
                    course2md::tr!("Microsoft Visual C++ 运行库"),
                    environment.vc_runtime,
                ));
            }
        }
        tools
    }

    /// 运行检查: one conclusion, the capabilities it covers, then 诊断详情 with the facts
    /// behind it. Failures stay visible: the conclusion says so and each capability says
    /// whether it is ready.
    fn environment_fields(&self, cx: &mut Context<Self>) -> Vec<ui::Field> {
        let Some(e) = &self.environment else {
            let status = ui::AsyncStatus::Busy(course2md::tr!("正在检查本机能力…").into());
            return vec![ui::Field::content(
                "diagnostic-checking",
                div().children(ui::async_status(
                    "diagnostic-checking-status",
                    &status,
                    None,
                    cx,
                )),
            )];
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
                course2md::config::AsrProvider::Gpu => e.llama && e.vc_runtime && e.gpu.is_some(),
                course2md::config::AsrProvider::Cpu => e.llama && e.vc_runtime,
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
        let checked = capabilities.iter().filter(|(_, _, unused)| !unused).count();
        let failed = capabilities
            .iter()
            .filter(|(_, ready, unused)| !ready && !unused)
            .count();
        let recheck = material_button("refresh-environment", ButtonVariant::Tonal)
            .icon(symbols::refresh())
            .child(course2md::tr!("重新检查"))
            .on_click(cx.listener(|this, _, _, cx| this.refresh_environment(cx)));
        let conclusion = ui::async_status(
            "diagnostic-summary",
            &if failed == 0 {
                ui::AsyncStatus::Success(course2md::tr!("{} 项检查全部通过", checked).into())
            } else {
                ui::AsyncStatus::Warning(course2md::tr!("{} 项检查未通过", failed).into())
            },
            Some(recheck.into_any_element()),
            cx,
        );
        let mut fields = vec![ui::Field::content(
            "diagnostic-conclusion",
            div().children(conclusion),
        )];
        fields.extend(
            capabilities
                .iter()
                .enumerate()
                .map(|(index, (label, ready, unused))| {
                    let (kind, state) = if *unused {
                        (StatusKind::Neutral, course2md::tr!("当前未使用"))
                    } else if *ready {
                        (StatusKind::Success, course2md::tr!("可用"))
                    } else {
                        (StatusKind::Warning, course2md::tr!("待修复"))
                    };
                    ui::Field::inline(
                        format!("diagnostic-capability-{index}"),
                        *label,
                        status_chip(kind).child(state),
                    )
                }),
        );
        if !speech_service
            && !local_ready
            && let Some(issue) = self
                .engine_issue(provider)
                .filter(|issue| *issue != model_diagnostics::DeviceIssue::Engine)
        {
            fields.push(ui::Field::content(
                "diagnostic-engine-obstacle-row",
                self.engine_obstacle_with("diagnostic-engine-obstacle", issue, false, cx),
            ));
        }
        if !e.engine {
            fields.push(ui::Field::inline(
                "repair-bundled-engine",
                course2md::tr!("转换程序无法运行，重新安装应用后笔记仍会保留"),
                material_button("download-repair-app", ButtonVariant::Outlined)
                    .icon(symbols::download())
                    .child(course2md::tr!("下载安装包"))
                    .on_click(|_, _, cx| {
                        cx.open_url("https://github.com/mizorewww/course2md/releases")
                    }),
            ));
        }
        fields.push(ui::Field::disclosure(
            "toggle-diagnostics-details",
            course2md::tr!("诊断详情"),
            self.settings_ui.diagnostics_details_open,
            cx.listener(|this, _, _, cx| {
                this.settings_ui.diagnostics_details_open =
                    !this.settings_ui.diagnostics_details_open;
                cx.notify();
            }),
            self.diagnostic_details(e, cx),
        ));
        fields
    }

    fn diagnostic_details(
        &self,
        environment: &crate::backend::Environment,
        cx: &mut Context<Self>,
    ) -> Div {
        let tools = self.program_facts(environment);
        // A found tool says where it runs from: shipped with the app or installed.
        let programs = tools.iter().enumerate().map(|(index, (name, found))| {
            let state = if !*found {
                course2md::tr!("未安装")
            } else {
                match environment.origins.get(name) {
                    Some(course2md::tools::Origin::Bundled) => {
                        course2md::tools::Origin::Bundled.label()
                    }
                    Some(course2md::tools::Origin::Override) => {
                        course2md::tools::Origin::Override.label()
                    }
                    _ => course2md::tr!("已安装"),
                }
            };
            let icon = if *found {
                Icon::from(symbols::check()).color(color(ColorRole::SUCCESS))
            } else {
                Icon::from(symbols::warning()).color(color(ColorRole::WARNING))
            };
            ListItem::new(("diagnostic-tool", index), *name)
                .density(ListDensity::Compact)
                .leading(ListLeading::Icon(icon))
                .trailing_text(state)
        });
        let repair =
            (!environment.ffmpeg || !environment.ffprobe || !environment.ytdlp).then(|| {
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
                v_flex()
                    .w_full()
                    .min_w_0()
                    .gap(rem(4.))
                    .items_start()
                    .child(TileScale::Regular.support("install-media-tools-help", help))
                    .child(accessible_text("install-media-tools-command", command))
                    .child(
                        material_button("copy-media-install-command", ButtonVariant::Text)
                            .icon(symbols::content_copy())
                            .child(course2md::tr!("复制安装命令"))
                            .on_click(move |_, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(command.into()))
                            }),
                    )
            });
        // The copied text is what the page shows, one fact per line.
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
            self.hardware_facts()
                .iter()
                .map(|(_, label, value)| format!("{label}: {value}")),
        );
        let report = report.join("\n");
        v_flex()
            .gap(rem(20.))
            .child(
                v_flex()
                    .w_full()
                    .min_w_0()
                    .gap(rem(8.))
                    .child(section_heading(
                        "diagnostic-programs-heading",
                        symbols::code(),
                        course2md::tr!("所需程序"),
                    ))
                    // A list item's own inset would break the group's left edge.
                    .child(v_flex().mx(rem(-16.)).children(programs))
                    .children(repair),
            )
            .children(self.model_hardware_details(cx))
            .child(
                h_flex().w_full().justify_end().child(
                    material_button("copy-diagnostics", ButtonVariant::Text)
                        .icon(symbols::content_copy())
                        .child(course2md::tr!("复制诊断信息"))
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
        if self.settings_ui.pending_generation.is_some() {
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
                if let Some(next) = self.settings_ui.pending_generation.clone() {
                    self.commit_generation(next, cx)
                } else {
                    true
                }
            }
            PreferenceGroup::Application => {
                if let Some(next) = self.settings_ui.pending_application.clone() {
                    self.commit_application(next, cx)
                } else {
                    true
                }
            }
            PreferenceGroup::Services => false,
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
                self.clear_settings_failure(group);
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
                self.set_settings_save_failure(group, &error);
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
        if let Err(error) = &result {
            self.set_settings_failure(
                PreferenceGroup::Generation,
                course2md::tr!(
                    "模型位置尚未更新，修改已保留，可重试保存：{}",
                    format!("{error:#}")
                ),
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
        if self.settings_ui.pending_generation.is_some()
            || self.settings_ui.pending_application.is_some()
        {
            self.message = Some(course2md::tr!("修改暂时无法保存，请重试").into());
            cx.notify();
            return false;
        }
        true
    }
}

/// The staged edit after a save attempt: cleared once the store accepted it,
/// kept for retry and exit when the save failed.
fn pending_after_save<T, E>(result: &std::result::Result<(), E>, next: T) -> Option<T> {
    result.is_err().then_some(next)
}

/// The commands of a saved service, drawn by its ⋯ menu and its context menu alike.
#[derive(Clone, Copy)]
enum ServiceCommand {
    Test,
    Edit,
    MakeDefault,
    SignOut,
    Delete,
}

struct ServiceAction {
    kind: ServiceCommand,
    label: &'static str,
    icon: SymbolIcon,
    disabled: bool,
    divider: bool,
    danger: bool,
}

#[cfg(test)]
mod tests {
    use super::pending_after_save;

    #[test]
    fn a_failed_save_keeps_the_staged_edit_and_a_successful_one_clears_it() {
        assert_eq!(pending_after_save::<_, ()>(&Ok(()), 7), None);
        assert_eq!(pending_after_save(&Err("disk full"), 7), Some(7));
    }
}
