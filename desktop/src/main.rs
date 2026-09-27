#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
mod a11y;
mod about;
mod account_ui;
mod activity;
mod appearance_ui;
mod backend;
mod bounded_http;
mod codex_ui;
mod course_library;
mod credentials;
mod focus_scroll;
// Icon helpers ship ahead of the pages that reference them (M3+); same staged
// token allowance as the theme module.
#[allow(dead_code)]
mod icons;
mod import_ui;
mod library_ui;
mod model_discovery;
mod motion;
mod notes;
mod onboarding;
mod organize;
mod palettes;
#[cfg(feature = "performance")]
mod performance;
mod preferences;
mod reader_navigation;
mod reader_ui;
mod service_test;
mod settings_ui;
mod source;
mod storage;
mod storage_ui;
mod task_ui;
// Token items ship ahead of the page milestones that adopt them (M3+); the
// module-wide allowance keeps staged tokens from tripping the zero-warning bar.
#[allow(dead_code)]
mod theme;
mod views;
mod workspace;
use backend::{Completed, Course, Event, Job};
use gpui::{prelude::*, *};
use gpui_component::{
    input::{InputEvent, InputState},
    *,
};
use std::{
    collections::{BTreeMap, HashSet, VecDeque},
    path::PathBuf,
    time::{Duration, Instant},
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    New,
    Task,
    Library,
    Settings,
    Result,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Convert,
    Doctor,
    Models,
}
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Field {
    Source,
    Title,
    FolderName,
    Output,
    Search,
    AsrUrl,
    AsrKey,
    AsrModel,
    LlmUrl,
    LlmKey,
    LlmModel,
}
const PROVIDERS: [(&str, &str); 6] = [
    ("", "自动"),
    ("coreml", "Apple 原生"),
    ("gpu", "GPU"),
    ("cpu", "CPU"),
    ("npu", "Intel NPU"),
    ("api", "云端 API"),
];

/// 识别引擎的规范展示名：全桌面唯一来源（选择与描述场景共用）。
/// 与 PROVIDERS 表的 id 一一对应；None 表示「自动」。
/// Runs blocking IO on a dedicated OS thread and delivers the result through a channel.
///
/// GPUI's background executor dispatches to GCD global queues, and macOS may run those
/// blocks on the main thread via queue override; `cx.spawn` always polls on the main
/// thread. Blocking syscalls — keychain reads that can wait on an authorization dialog,
/// synchronous HTTP — therefore must never run inside executor tasks.
pub(crate) fn spawn_blocking_io<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> smol::channel::Receiver<T> {
    let (tx, rx) = smol::channel::bounded(1);
    std::thread::spawn(move || {
        let _ = tx.send_blocking(work());
    });
    rx
}

pub(crate) fn provider_label(provider: Option<course2md::config::AsrProvider>) -> &'static str {
    use course2md::config::AsrProvider;
    match provider {
        None => "自动",
        Some(AsrProvider::Coreml) => "Apple 原生",
        Some(AsrProvider::Gpu) => "GPU",
        Some(AsrProvider::Cpu) => "CPU",
        Some(AsrProvider::Npu) => "Intel NPU",
        Some(AsrProvider::Api) => "云端 API",
    }
}

/// PROVIDERS 下标 → AsrProvider：索引与枚举映射的唯一实现（与 PROVIDERS 顺序同源）。
pub(crate) fn asr_provider_from_index(index: usize) -> Option<course2md::config::AsrProvider> {
    use course2md::config::AsrProvider;
    match PROVIDERS.get(index)?.0 {
        "coreml" => Some(AsrProvider::Coreml),
        "gpu" => Some(AsrProvider::Gpu),
        "cpu" => Some(AsrProvider::Cpu),
        "npu" => Some(AsrProvider::Npu),
        "api" => Some(AsrProvider::Api),
        _ => None,
    }
}

/// 云端 API 在 PROVIDERS 表中的下标：PROVIDERS[..CLOUD_PROVIDER_INDEX] 即本机引擎集合。
/// 此前以裸数字 5 散落在 import_ui/task_ui/workspace，是「5 即云端」的无文档契约。
pub(crate) const CLOUD_PROVIDER_INDEX: usize = 5;

impl ConversionOptions {
    /// 当前是否使用云端识别服务（provider == CLOUD_PROVIDER_INDEX）。
    pub(crate) fn uses_cloud_provider(&self) -> bool {
        self.provider == CLOUD_PROVIDER_INDEX
    }
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
struct ConversionOptions {
    provider: usize,
    source_mode: usize,
    llm: bool,
    #[serde(default)]
    summarize: bool,
    #[serde(default)]
    vision: bool,
    keep_video: bool,
    resume: bool,
    formats: [bool; 3],
}

impl ConversionOptions {
    fn from_config(config: &course2md::settings::ConfigFile) -> Self {
        let provider = config
            .defaults
            .provider
            .map(|p| {
                PROVIDERS
                    .iter()
                    .position(|(id, _)| *id == p.as_str())
                    .unwrap_or(0)
            })
            .unwrap_or(0);
        let source_mode = match config.defaults.transcript_source.unwrap_or_default() {
            course2md::config::TranscriptSource::Auto => 0,
            course2md::config::TranscriptSource::Subtitle => 1,
            course2md::config::TranscriptSource::Asr => 2,
        };
        let llm = config.llm.enabled;
        let keep_video = config.defaults.keep_video.unwrap_or(false);
        let resume = true;
        let formats = config
            .defaults
            .formats
            .as_ref()
            .map(|formats| {
                use course2md::config::OutputFormat::*;
                [
                    formats.contains(&Md),
                    formats.contains(&Html),
                    formats.contains(&Json),
                ]
            })
            .unwrap_or([true, true, false]);
        Self {
            provider,
            source_mode,
            llm,
            summarize: config.llm.summarize,
            vision: config.llm.vision,
            keep_video,
            resume,
            formats,
        }
    }
}

impl Default for ConversionOptions {
    fn default() -> Self {
        let mut value = Self::from_config(&Default::default());
        value.formats = [false; 3];
        value
    }
}

struct Desktop {
    preferences: preferences::Store,
    settings_ui: settings_ui::State,
    onboarding: onboarding::State,
    storage_ui: storage_ui::State,
    reader_ui: reader_ui::State,
    workspace: Option<workspace::Workspace>,
    workspace_error: Option<String>,
    preference_defaults_pending: bool,
    active_task: Option<String>,
    transient_task_result: Option<(String, Instant)>,
    reader_opened_notice_at: Option<Instant>,
    draft_loading: bool,
    draft_deadline: Option<Instant>,
    quit_deadline: Option<Instant>,
    account: account_ui::AccountUi,
    codex: codex_ui::CodexUi,
    ollama: ollama_ui::OllamaUi,
    online: bool,
    last_source_input: String,
    completed_source: Option<String>,
    source_preview: Option<source::Source>,
    source_candidates: Vec<source::SourceCandidate>,
    source_collection_title: Option<String>,
    /// 候选行预览图的本地缓存（candidate.input → 缓存文件）；后台按 URL 去重下载
    source_candidate_covers: std::collections::HashMap<String, PathBuf>,
    source_covers_cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    subtitle_loading: bool,
    subtitle_cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    subtitle_error: Option<String>,
    subtitle_generation: u64,
    preview_cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    preview_generation: u64,
    pending_conversion: Option<u64>,
    following_conversion: Option<import_ui::ConversionFollow>,
    preview_workers: usize,
    preview_error: Option<String>,
    source_validation: Option<String>,
    import_submit_focus: FocusHandle,
    root_focus: FocusHandle,
    show_preview_details: bool,
    expanded_subtitle_issue: Option<SharedString>,
    library: organize::Library,
    library_root: PathBuf,
    library_error: Option<String>,
    library_issues: Vec<String>,
    library_indexes: BTreeMap<PathBuf, organize::Library>,
    library_view_cache: course_library::LibraryViewCache,
    library_generation: u64,
    folder_filter: Option<u64>, // None = all; 0 = unfiled
    target_folder: Option<u64>,
    folder_editor: Option<Option<u64>>,
    folder_origin: Option<library_ui::FolderOrigin>,
    folder_error: Option<String>,
    delete_folder: Option<u64>,
    page: Page,
    result_origin: Page,
    settings_origin: Option<Page>,
    settings_return_focus: Option<FocusHandle>,
    result_tab: usize,
    settings_tab: usize,
    source_editor_open: bool,
    generation_options_open: bool,
    validation_attempt: usize,
    show_options: bool,
    show_export_options: bool,
    show_logs: bool,
    environment: Option<backend::Environment>,
    scrolls: [ScrollHandle; 5],
    inputs: BTreeMap<Field, Entity<InputState>>,
    config: course2md::settings::ConfigFile,
    task_options: ConversionOptions,
    settings_options: ConversionOptions,
    job: Option<Job>,
    kind: Kind,
    cancelling: bool,
    closing: bool,
    /// A queued task's credentials are being resolved off the UI thread before it can start.
    start_pending: bool,
    task_status: String,
    task_error: Option<String>,
    progress: BTreeMap<String, activity::Activity>,
    settings_snapshot: course2md::settings::ConfigFile,
    settings_deadline: Option<Instant>,
    settings_status: String,
    desktop_settings: course2md::settings::DesktopSettings,
    last_tick: Instant,
    logs: VecDeque<String>,
    pending_done: Option<Completed>,
    completed: Option<Completed>,
    courses: Vec<Course>,
    loading: bool,
    preview: Option<backend::Preview>,
    read_generation: u64,
    reading: bool,
    opening_course: Option<PathBuf>,
    reader_course_error: Option<(PathBuf, String)>,
    reader_failure_notice: Option<String>,
    reader_scroll: ScrollHandle,
    reader_saved_offset: f32,
    reader_position_saved_at: Option<Instant>,
    // Virtualized long pages: the task queue and the course library each keep
    // a persistent list state plus the identity keys and focus containers of
    // the items the state currently describes.
    queue_list: ListState,
    queue_keys: Vec<String>,
    queue_focus: Vec<FocusHandle>,
    queue_rem: f32,
    library_list: ListState,
    library_keys: Vec<String>,
    library_focus: Vec<FocusHandle>,
    library_rem: f32,
    // Entrance animations already played for logical content that a
    // virtualized list may unmount and remount while scrolling.
    entered: HashSet<String>,
    // Disclosure open state that outlives a virtualized row's unmount.
    task_panels_open: HashSet<String>,
    event_repaint_pending: bool,
    exporting: bool,
    message: Option<String>,
    _subscriptions: Vec<Subscription>,
    _poll: Task<()>,
}

actions!(
    course2md_desktop,
    [Quit, OpenSettings, OpenAbout, ImportVideo, SearchContent]
);

/// Task log/progress events coalesce into at most one repaint per interval;
/// stage, error and completion transitions still repaint immediately.
const EVENT_REPAINT_INTERVAL: Duration = Duration::from_millis(250);

impl Desktop {
    /// Splice a virtualized page list to a new item identity sequence, keeping
    /// the measured heights and scroll anchor of the unchanged prefix/suffix.
    fn reconcile_list_items(
        state: &ListState,
        keys: &mut Vec<String>,
        focus: &mut Vec<FocusHandle>,
        new_keys: Vec<String>,
        cx: &mut App,
    ) {
        if *keys == new_keys {
            return;
        }
        let prefix = keys
            .iter()
            .zip(new_keys.iter())
            .take_while(|(old, new)| old == new)
            .count();
        let suffix = keys
            .iter()
            .rev()
            .zip(new_keys.iter().rev())
            .take_while(|(old, new)| old == new)
            .count()
            .min(keys.len() - prefix)
            .min(new_keys.len() - prefix);
        let old_end = keys.len() - suffix;
        let new_mid = new_keys.len() - prefix - suffix;
        let handles: Vec<FocusHandle> = (0..new_mid).map(|_| cx.focus_handle()).collect();
        state.splice_focusable(prefix..old_end, handles.iter().cloned().map(Some));
        keys.splice(
            prefix..old_end,
            new_keys[prefix..prefix + new_mid].iter().cloned(),
        );
        focus.splice(prefix..old_end, handles);
    }

    /// Entrance motion plays once per logical mount. A virtualized list
    /// remounts rows as they re-enter the viewport, which must not replay it.
    fn enter_once(&mut self, id: String) -> bool {
        self.entered.insert(id)
    }

    fn request_close(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.flush_settings_for_exit(cx) || !self.save_current_draft(cx) {
            return false;
        }
        self.save_reading_position(cx);
        self.cancel_storage_for_close();
        if let Some(workspace) = &mut self.workspace {
            if let Err(error) = workspace.transaction(|state| {
                state.stop_session();
                Ok(())
            }) {
                self.workspace_error = Some(format!("退出意图尚未保存：{error:#}。窗口保持打开。"));
                cx.notify();
                return false;
            }
        }
        if let Some(cancel) = &self.preview_cancel {
            cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        if let Some(cancel) = &self.subtitle_cancel {
            cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        self.closing = true;
        self.quit_deadline = Some(Instant::now() + Duration::from_secs(10));
        self.refresh_dispatch_controls(cx);
        if self.active_task.is_none()
            && let Some(job) = &self.job
        {
            job.cancel();
        }
        self.message = Some("正在保存进度并退出…".into());
        cx.notify();
        self.job.is_none() && self.preview_workers == 0
    }
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let configuration_directory = course2md::config::config_dir();
        let message = None;
        // Fresh installs bootstrap managed local storage so the welcome screen does not
        // synchronously access a protected Documents folder before the user chooses a location.
        let output = configuration_directory.join("desktop-local-library");
        let preferences = preferences::Store::open(
            configuration_directory.join("desktop-preferences"),
            credentials::system_vault(configuration_directory.join("desktop-credentials.json")),
        );
        let mut config = preferences.defaults_config();
        config.defaults.out = Some(output.clone());
        let fields = [
            (
                Field::Source,
                "粘贴 YouTube 或 Bilibili 视频链接",
                String::new(),
            ),
            (Field::Title, "", String::new()),
            (
                Field::Output,
                "课程笔记保存位置",
                output.display().to_string(),
            ),
            (Field::Search, "搜索笔记标题", String::new()),
            (Field::FolderName, "文件夹名称", String::new()),
            (Field::AsrUrl, "", config.asr_api.base_url.clone()),
            (Field::AsrKey, "API Key", config.asr_api.api_key.clone()),
            (Field::AsrModel, "转写模型", config.asr_api.model.clone()),
            (Field::LlmUrl, "", config.llm.base_url.clone()),
            (Field::LlmKey, "API Key", config.llm.api_key.clone()),
            (Field::LlmModel, "模型名称", config.llm.model.clone()),
        ];
        let inputs: BTreeMap<_, _> = fields
            .into_iter()
            .map(|(field, placeholder, value)| {
                (
                    field,
                    cx.new(|cx| {
                        InputState::new(window, cx)
                            .placeholder(placeholder)
                            .default_value(value)
                            .masked(matches!(field, Field::AsrKey | Field::LlmKey))
                    }),
                )
            })
            .collect();
        let mut subscriptions: Vec<Subscription> = inputs
            .iter()
            .map(|(field, input)| {
                let field = *field;
                cx.observe_in(input, window, move |this: &mut Self, _, window, cx| {
                    if this.draft_loading {
                        return;
                    }
                    if field == Field::Source {
                        let value = this.value(Field::Source, cx);
                        if value != this.last_source_input {
                            this.following_conversion = None;
                            if this.reading && this.page == Page::New {
                                this.read_generation = this.read_generation.wrapping_add(1);
                                this.reading = false;
                            }
                            if !this.prepare_next_import(&value, window, cx) {
                                return;
                            }
                            this.last_source_input = value;
                            this.invalidate_source();
                        }
                    }
                    if matches!(field, Field::Source | Field::Title) {
                        this.draft_deadline = Some(Instant::now() + Duration::from_millis(350));
                    }
                    if field == Field::Title
                        && !this.value(Field::Title, cx).is_empty()
                        && this.source_validation.as_deref() == Some("请填写笔记名称")
                    {
                        this.source_validation = None;
                    }
                    if field == Field::Search {
                        this.library_list.scroll_to(ListOffset {
                            item_ix: 0,
                            offset_in_item: px(0.),
                        });
                    }
                    cx.notify();
                })
            })
            .collect();
        subscriptions.push(cx.subscribe_in(
            &inputs[&Field::Source],
            window,
            |this: &mut Self, _, event, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. })
                    && this.page == Page::New
                    && this.online
                {
                    this.start_conversion(window, cx);
                }
            },
        ));
        subscriptions.push(cx.subscribe(
            &inputs[&Field::FolderName],
            |this: &mut Self, _, event, cx| {
                if matches!(event, InputEvent::Change) {
                    this.folder_error = None;
                    cx.notify();
                }
                if matches!(event, InputEvent::PressEnter { .. }) {
                    this.save_folder(cx);
                }
            },
        ));
        let options = ConversionOptions::from_config(&config);
        let poll = cx.spawn_in(window, async |this, cx| {
            let mut busy = false;
            loop {
                smol::Timer::after(Duration::from_millis(if busy { 100 } else { 500 })).await;
                match this.update_in(cx, |this, _, cx| {
                    this.poll(cx);
                    if this
                        .settings_deadline
                        .is_some_and(|deadline| Instant::now() >= deadline)
                    {
                        this.settings_deadline = None;
                        this.save_settings(cx);
                    }
                    this.job.is_some() || this.settings_deadline.is_some()
                }) {
                    Ok(active) => busy = active,
                    Err(_) => break,
                }
            }
        });
        let (workspace, workspace_error) =
            match workspace::Workspace::open(output.clone(), options.clone()) {
                Ok(workspace) => (Some(workspace), None),
                Err(error) => (
                    None,
                    Some(format!("输入与任务记录无法读取，原文件已保留：{error:#}")),
                ),
            };
        let settings_ui = settings_ui::State::new(window, cx);
        let onboarding = onboarding::State::new(window, cx);
        let reader_ui = reader_ui::State::new(window, cx);
        let mut this = Self {
            preferences,
            settings_ui,
            onboarding,
            reader_ui,
            storage_ui: Default::default(),
            workspace,
            workspace_error,
            preference_defaults_pending: false,
            active_task: None,
            transient_task_result: None,
            reader_opened_notice_at: None,
            draft_loading: false,
            draft_deadline: None,
            quit_deadline: None,
            page: Page::New,
            online: true,
            last_source_input: String::new(),
            completed_source: None,
            source_preview: None,
            source_candidates: Vec::new(),
            source_collection_title: None,
            source_candidate_covers: std::collections::HashMap::new(),
            source_covers_cancel: None,
            subtitle_loading: false,
            subtitle_cancel: None,
            subtitle_error: None,
            subtitle_generation: 0,
            preview_cancel: None,
            preview_generation: 0,
            pending_conversion: None,
            following_conversion: None,
            preview_workers: 0,
            preview_error: None,
            source_validation: None,
            import_submit_focus: cx.focus_handle(),
            root_focus: Self::install_root_focus(window, cx),
            show_preview_details: false,
            expanded_subtitle_issue: None,
            account: account_ui::AccountUi::default(),
            codex: codex_ui::CodexUi::default(),
            library: Default::default(),
            library_root: output,
            library_error: None,
            library_issues: Vec::new(),
            library_indexes: BTreeMap::new(),
            library_view_cache: Default::default(),
            library_generation: 0,
            folder_filter: None,
            target_folder: None,
            folder_editor: None,
            folder_origin: None,
            folder_error: None,
            delete_folder: None,
            result_origin: Page::Library,
            settings_origin: None,
            settings_return_focus: None,
            result_tab: 0,
            settings_tab: 4,
            source_editor_open: false,
            generation_options_open: false,
            validation_attempt: 0,
            show_options: false,
            show_export_options: false,
            show_logs: false,
            environment: None,
            scrolls: std::array::from_fn(|_| ScrollHandle::new()),
            inputs,
            settings_snapshot: config.clone(),
            desktop_settings: config.desktop.clone(),
            settings_deadline: None,
            settings_status: String::new(),
            last_tick: Instant::now(),
            config,
            task_options: options.clone(),
            settings_options: options,
            job: None,
            kind: Kind::Convert,
            cancelling: false,
            closing: false,
            start_pending: false,
            task_error: None,
            task_status: String::new(),
            progress: BTreeMap::new(),
            logs: VecDeque::new(),
            pending_done: None,
            completed: None,
            courses: vec![],
            loading: false,
            preview: None,
            read_generation: 0,
            reading: false,
            opening_course: None,
            reader_course_error: None,
            reader_failure_notice: None,
            reader_scroll: ScrollHandle::new(),
            reader_saved_offset: f32::NAN,
            reader_position_saved_at: None,
            queue_list: ListState::new(0, ListAlignment::Top, px(1000.)),
            queue_keys: Vec::new(),
            queue_focus: Vec::new(),
            queue_rem: f32::NAN,
            library_list: ListState::new(0, ListAlignment::Top, px(1000.)),
            library_keys: Vec::new(),
            library_focus: Vec::new(),
            library_rem: f32::NAN,
            entered: HashSet::new(),
            task_panels_open: HashSet::new(),
            event_repaint_pending: false,
            exporting: false,
            message,
            _subscriptions: subscriptions,
            _poll: poll,
        };
        this.settings_snapshot = this.edited_settings(cx);
        // Preferences and workspace records are saved separately. Reconcile
        // inherited options after an interrupted save before restoring input.
        this.refresh_preference_defaults(cx);
        this.restore_draft(window, cx);
        this.prepare_workbench_input(window, cx);
        this.restore_storage_state(cx);
        cx.set_reduce_motion(this.desktop_settings.reduce_motion);
        this.refresh_account(cx);
        this.refresh_environment(cx);
        this.refresh_library(cx);
        if !this.preferences.application().desktop.setup_completed {
            this.start_onboarding(window, cx);
        }
        this
    }
    fn refresh_environment(&mut self, cx: &mut Context<Self>) {
        self.environment = None;
        // 环境探测会同步启动子进程（Metal 枚举可超过十秒），必须离开 executor；
        // 见 spawn_blocking_io 的说明
        let task = spawn_blocking_io(backend::Environment::detect);
        cx.spawn(async move |this, cx| {
            let Ok(environment) = task.recv().await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                this.environment = Some(environment);
                this.refresh_model_diagnostics(cx);
                this.advance_conversion_when_ready(cx);
                cx.notify();
            });
        })
        .detach();
    }
    fn install_root_focus(window: &mut Window, cx: &mut Context<Self>) -> FocusHandle {
        let root = cx.focus_handle().tab_stop(false);
        // This is a real dispatch ancestor, not another stop in the control order.
        if window.focused(cx).is_none() {
            root.focus(window, cx);
        }
        cx.on_focus_lost(window, |this, window, cx| {
            let ancestor = window.focus_lost_restore_target(cx);
            if window.has_active_dialog(cx) {
                // A dialog can replace its own controls. Restore only inside its
                // active trap; never send focus back to the obscured main page.
                if let Some(trap) = gpui_base::active_focus_trap(window, cx) {
                    let target = ancestor
                        .filter(|focus| trap.contains(focus, window))
                        .unwrap_or(trap);
                    target.focus(window, cx);
                }
            } else {
                ancestor
                    .unwrap_or_else(|| this.root_focus.clone())
                    .focus(window, cx);
            }
        })
        .detach();
        root
    }

    fn prepare_next_import(
        &mut self,
        value: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let defaults = ConversionOptions::from_config(&self.preferences.defaults_config());
        let Some(workspace) = &mut self.workspace else {
            return false;
        };
        if !workspace
            .state
            .draft()
            .is_some_and(|draft| draft.submitted_task.is_some() && draft.input != value)
        {
            return true;
        }
        match workspace.transaction(|state| Ok(state.prepare_next_import(value, defaults))) {
            Ok(true) => {
                if let Some(draft) = workspace.state.draft() {
                    self.task_options = draft.options.clone();
                    self.target_folder = draft.folder;
                }
                self.completed_source = None;
                self.invalidate_source();
                self.message = None;
                self.draft_loading = true;
                self.inputs[&Field::Title].update(cx, |state, cx| state.set_value("", window, cx));
                self.draft_loading = false;
                self.scrolls[Page::New as usize].set_offset(point(px(0.), px(0.)));
                true
            }
            Err(error) => {
                self.workspace_error = Some(format!("新的视频输入尚未保存：{error:#}"));
                cx.notify();
                false
            }
            Ok(false) => true,
        }
    }

    fn import_video_from_action(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.prepare_workbench_input(window, cx) {
            return;
        }
        self.source_editor_open = true;
        self.navigate(Page::New, cx);
        if self.online {
            self.inputs[&Field::Source].update(cx, |input, cx| input.focus(window, cx));
        }
    }

    fn navigate(&mut self, page: Page, cx: &mut Context<Self>) {
        self.transient_task_result = None;
        if page != Page::New
            || matches!(
                self.following_conversion,
                Some(import_ui::ConversionFollow::ReaderTask { .. })
            )
        {
            self.following_conversion = None;
        }
        self.save_reading_position(cx);
        self.read_generation = self.read_generation.wrapping_add(1);
        self.reading = false;
        self.opening_course = None;
        if self.page == Page::New {
            self.save_current_draft(cx);
        }
        self.page = page;
        self.message = None;
        if page == Page::Library {
            self.refresh_library(cx);
        }
        if page == Page::Task {
            let visible_unread = self.workspace.as_ref().and_then(|workspace| {
                let id = workspace.state.selected_task.as_ref()?;
                workspace
                    .state
                    .task(id)
                    .filter(|task| task.unread)
                    .map(|_| id.clone())
            });
            if let Some(id) = visible_unread {
                self.select_task(&id, cx);
            }
        }
        cx.notify();
    }
    fn value(&self, field: Field, cx: &App) -> String {
        self.inputs[&field].read(cx).value().trim().to_string()
    }
    fn field_error(&self, field: Field, cx: &App) -> Option<&'static str> {
        self.settings_status
            .starts_with("未保存：")
            .then(|| self.invalid_setting(cx))
            .flatten()
            .filter(|(invalid, _)| *invalid == field)
            .map(|(_, message)| message)
    }
    fn input(&self, field: Field, label: &'static str, cx: &App) -> Div {
        let error = self.field_error(field, cx);
        v_flex()
            .gap_2()
            .w_full()
            .child(
                theme::accessible_text(("field-label", field as usize), label)
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM),
            )
            .child(
                theme::text_input(&self.inputs[&field])
                    .aria_label(label)
                    .when(error.is_some(), |input| {
                        input.border_color(theme::color(theme::DANGER))
                    }),
            )
            .when_some(error, |v, message| {
                v.child(
                    div()
                        .text_sm()
                        .text_color(theme::color(theme::DANGER))
                        .child(message),
                )
            })
    }
    fn output(&self, _cx: &App) -> PathBuf {
        self.workspace
            .as_ref()
            .and_then(|w| w.state.library(&w.state.default_library))
            .map(|l| l.root.clone())
            .unwrap_or_else(|| self.library_root.clone())
    }
    fn pick(&mut self, directory: bool, window: &mut Window, cx: &mut Context<Self>) {
        let prompt = cx.prompt_for_paths(PathPromptOptions {
            files: !directory,
            directories: directory,
            multiple: false,
            prompt: Some(
                if directory {
                    "选择保存目录"
                } else {
                    "选择课程视频"
                }
                .into(),
            ),
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = prompt.await;
            let _ = this.update_in(cx, |this, window, cx| {
                match result {
                    Ok(Ok(Some(paths))) => {
                        if let Some(path) = paths.first() {
                            let field = if directory {
                                Field::Output
                            } else {
                                Field::Source
                            };
                            this.inputs[&field].update(cx, |state, cx| {
                                state.set_value(path.display().to_string(), window, cx)
                            });
                        }
                    }
                    Ok(Ok(None)) => {}
                    Ok(Err(error)) => this.message = Some(format!("无法打开文件选择器：{error:#}")),
                    Err(error) => this.message = Some(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }
    fn start(&mut self, kind: Kind, cx: &mut Context<Self>) {
        if kind == Kind::Convert {
            self.navigate(Page::New, cx);
            return;
        }
        if self.job.is_some() {
            self.message = Some("当前任务正在处理，结束后可以进行这项操作。".into());
            cx.notify();
            return;
        }
        let args = match kind {
            Kind::Doctor => vec!["doctor".into()],
            Kind::Models => self.model_preparation_args(),
            Kind::Convert => unreachable!(),
        };
        match Job::start(args) {
            Ok(job) => {
                self.job = Some(job);
                self.active_task = None;
                self.kind = kind;
                self.cancelling = false;
                self.logs.clear();
                self.progress.clear();
                self.pending_done = None;
                self.task_error = None;
                self.task_status = if kind == Kind::Doctor {
                    "正在检查运行环境"
                } else {
                    "正在准备识别模型"
                }
                .into();
            }
            Err(error) => {
                self.task_error = Some(format!("{error:#}"));
                if kind == Kind::Models {
                    self.model_preparation_finished(false, false, cx);
                } else {
                    self.message = self.task_error.clone();
                }
            }
        }
        cx.notify();
    }
    fn poll(&mut self, cx: &mut Context<Self>) {
        if self
            .reader_opened_notice_at
            .is_some_and(|shown| shown.elapsed() >= Duration::from_secs(5))
        {
            self.reader_opened_notice_at = None;
            if self.message.as_deref() == Some("笔记已打开") {
                self.message = None;
                cx.notify();
            }
        }
        if self
            .transient_task_result
            .as_ref()
            .is_some_and(|(_, shown)| shown.elapsed() >= Duration::from_secs(8))
        {
            self.transient_task_result = None;
            cx.notify();
        }
        self.poll_storage(cx);
        self.poll_reading_position(cx);
        if self
            .draft_deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            self.save_current_draft(cx);
        }
        let events: Vec<_> = self
            .job
            .as_ref()
            .map(|job| job.events.try_iter().take(512).collect())
            .unwrap_or_default();
        let mut save = false;
        // Stage transitions persist immediately; steady progress is throttled.
        let mut flush = false;
        // Visible state transitions repaint now; log/progress lines coalesce.
        let mut immediate = false;
        for event in events {
            let recording = self.active_task.is_some();
            if recording {
                self.record_task_event(&event);
                save = true;
            }
            match event {
                Event::Log { message } => self.logs.push_back(message),
                Event::Stage { stage, status } => {
                    if status == "start" {
                        self.progress.insert(stage, activity::Activity::new());
                    } else if status == "done" {
                        self.progress
                            .entry(stage)
                            .or_insert_with(activity::Activity::new)
                            .done = true;
                    }
                    flush = recording;
                    immediate = true;
                }
                Event::Progress {
                    stage,
                    current,
                    total,
                    message,
                } => self
                    .progress
                    .entry(stage)
                    .or_insert_with(activity::Activity::new)
                    .update(current, total, message),
                Event::Tokens {
                    stage,
                    prompt,
                    completion,
                } => self
                    .progress
                    .entry(stage)
                    .or_insert_with(activity::Activity::new)
                    .note_tokens(prompt, completion),
                Event::Workers { stage, workers } => {
                    self.progress
                        .entry(stage)
                        .or_insert_with(activity::Activity::new)
                        .workers = workers
                }
                Event::Error { message } => {
                    self.task_error = Some(message.clone());
                    self.logs.push_back(message);
                    flush = recording;
                    immediate = true;
                }
                Event::Blocked { message, .. } => {
                    self.task_error = Some(message);
                    flush = recording;
                    immediate = true;
                }
                Event::Done(done) => {
                    self.pending_done = Some(done);
                    immediate = true;
                }
                Event::Exit { success, cancelled } => {
                    immediate = true;
                    self.job = None;
                    self.cancelling = false;
                    if let Some(id) = self.active_task.take() {
                        self.finish_task(&id, success, cancelled, cx);
                        self.refresh_model_diagnostics(cx);
                    } else {
                        self.task_status = if success {
                            "已完成"
                        } else if cancelled {
                            "已停止"
                        } else {
                            "未完成"
                        }
                        .into();
                        if self.kind == Kind::Models {
                            self.model_preparation_finished(success, cancelled, cx);
                        } else if success {
                            self.refresh_environment(cx);
                        }
                        if self.kind != Kind::Models {
                            self.message = Some(
                                self.task_error
                                    .clone()
                                    .unwrap_or_else(|| self.task_status.clone()),
                            );
                        }
                    }
                }
            }
            while self.logs.len() > 400 {
                self.logs.pop_front();
            }
        }
        if save && let Some(workspace) = &self.workspace {
            let result = if flush {
                workspace.save().map(|_| ())
            } else {
                workspace.save_progress().map(|_| ())
            };
            if let Err(error) = result {
                self.workspace_error = Some(format!("任务进度尚未保存：{error:#}"));
            }
        }
        if self.closing {
            if self.job.is_none() && self.preview_workers == 0 {
                cx.quit();
                return;
            }
            if self
                .quit_deadline
                .is_some_and(|deadline| Instant::now() >= deadline)
            {
                if let Some(job) = &self.job {
                    job.cancel();
                }
                cx.quit();
                return;
            }
        } else {
            self.start_next_task(cx);
        }
        let ticking = save || immediate || self.job.is_some() || self.event_repaint_pending;
        // 模型下载与任务同一合帧节奏（此前 1s，设置页进度条明显滞后）
        let interval = if save || self.event_repaint_pending || self.kind == Kind::Models {
            EVENT_REPAINT_INTERVAL
        } else {
            Duration::from_secs(1)
        };
        if ticking && (immediate || self.last_tick.elapsed() >= interval) {
            self.last_tick = Instant::now();
            self.event_repaint_pending = false;
            cx.notify();
        } else if save {
            self.event_repaint_pending = true;
        }
    }
    fn refresh_library(&mut self, cx: &mut Context<Self>) {
        let locations = self.registered_storage_locations();
        self.library_generation = self.library_generation.wrapping_add(1);
        let generation = self.library_generation;
        self.storage_ui
            .begin_location_checks(generation, &locations);
        self.loading = true;
        let task = storage_ui::scan_locations(locations, move |location| {
            let scan = backend::scan_library(&location.root);
            let organization = organize::Library::load(&location.root);
            let cache = course_library::LibraryViewCache::inspect(location, scan.as_ref().ok());
            (scan, organization, cache)
        });
        cx.spawn(async move |this, cx| {
            let results = task.await;
            let _ = this.update(cx, |this, cx| {
                if this.library_generation != generation {
                    return;
                }
                let current = this.registered_storage_locations();
                let checks = results
                    .iter()
                    .map(|(location, check, _)| (location.clone(), check.clone()))
                    .collect();
                if !this
                    .storage_ui
                    .finish_location_checks(generation, &current, checks)
                {
                    this.refresh_library(cx);
                    return;
                }
                this.loading = false;
                this.courses.clear();
                this.library_issues.clear();
                this.library_indexes.clear();
                this.library_view_cache = Default::default();
                for (location, _, (scan, organization, cache)) in results {
                    this.library_view_cache.merge(cache);
                    match scan {
                        Ok(scan) => {
                            this.courses.extend(scan.courses);
                            this.library_issues.extend(scan.issues);
                        }
                        Err(error) => this
                            .library_issues
                            .push(format!("{}暂时无法读取：{error:#}", location.name)),
                    }
                    match organization {
                        Ok(library) => {
                            this.library_indexes.insert(location.root, library);
                        }
                        Err(error) => this.library_issues.push(format!(
                            "{}的文件夹记录暂时无法读取：{error:#}",
                            location.name
                        )),
                    }
                }
                this.courses.sort_by_key(|c| std::cmp::Reverse(c.modified));
                this.apply_course_title_aliases();
                if let Some(library) = this.library_indexes.get(&this.library_root) {
                    this.library = library.clone();
                    this.library_error = None;
                } else if let Some((root, library)) = this
                    .workspace
                    .as_ref()
                    .and_then(|workspace| workspace.state.libraries.first())
                    .and_then(|location| {
                        this.library_indexes
                            .get(&location.root)
                            .map(|library| (location.root.clone(), library.clone()))
                    })
                {
                    // The configured default is not a registered library; follow
                    // the workspace's first readable location instead.
                    this.library_root = root;
                    this.library = library;
                    this.library_error = None;
                } else {
                    this.library_error = Some("当前库的文件夹信息暂时无法读取".into());
                }
                this.advance_conversion_when_ready(cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn course_location(&self, course: &Course) -> Option<&workspace::LibraryLocation> {
        let cached = self
            .library_view_cache
            .locations
            .get(&course.storage_dir())?;
        self.workspace
            .as_ref()?
            .state
            .libraries
            .iter()
            .find(|lib| lib.id == cached.id && lib.root == cached.root)
    }
    fn course_folder(&self, course: &Course) -> Option<u64> {
        let root = &self.course_location(course)?.root;
        let cached = self
            .library_view_cache
            .locations
            .get(&course.storage_dir())?;
        self.library_indexes.get(root)?.folder_key(&cached.relative)
    }
    fn open_course(&mut self, course: Course, cx: &mut Context<Self>) {
        self.following_conversion = None;
        self.open_course_with_conversion_guard(course, None, cx);
    }

    fn open_completed_conversion(&mut self, course: Course, cx: &mut Context<Self>) {
        let follow = self.following_conversion.clone();
        self.open_course_with_conversion_guard(course, follow, cx);
    }

    fn open_course_with_conversion_guard(
        &mut self,
        course: Course,
        follow: Option<import_ui::ConversionFollow>,
        cx: &mut Context<Self>,
    ) {
        self.save_reading_position(cx);
        let reader_revision = self.read_generation;
        let reader_origin = follow.as_ref().and_then(|follow| {
            if !matches!(follow, import_ui::ConversionFollow::ReaderTask { .. }) {
                return None;
            }
            let manifest = self.preview.as_ref()?.course.manifest.as_ref()?;
            Some((manifest.course_id.clone(), manifest.version_id.clone()))
        });
        cx.notify();
        self.reading = true;
        let reading_path = course.dir.clone();
        self.opening_course = Some(reading_path.clone());
        self.reader_course_error = None;
        self.read_generation = self.read_generation.wrapping_add(1);
        let generation = self.read_generation;
        let origin = self.page;
        let followed_notice = follow.as_ref().and_then(|_| {
            self.message
                .as_ref()
                .filter(|message| message.starts_with("已加入任务"))
                .cloned()
        });
        self.result_origin = if origin == Page::Result {
            if self.result_origin == Page::Result {
                Page::Library
            } else {
                self.result_origin
            }
        } else {
            origin
        };
        let task = spawn_blocking_io(move || backend::read_preview(course));
        cx.spawn(async move |this, cx| {
            let result = task
                .recv()
                .await
                .unwrap_or_else(|_| Err(anyhow::anyhow!("读取笔记的工作线程意外结束")));
            let _ = this.update(cx, |this, cx| {
                if this.read_generation != generation || this.page != origin {
                    return;
                }
                this.reading = false;
                this.opening_course = None;
                if this.reader_viewer_open() {
                    import_ui::ConversionFollow::interrupt_for_reader_viewer(
                        &mut this.following_conversion,
                    );
                }
                if follow.as_ref().is_some_and(|follow| {
                    this.following_conversion.as_ref() != Some(follow)
                        || !follow.context_is_current(
                            this.page,
                            this.preview_generation,
                            reader_revision,
                            this.preview.as_ref().map(|preview| preview.course.dir.as_path()),
                        )
                }) {
                    cx.notify();
                    return;
                }
                match result {
                    Ok(preview) => {
                        // 与 navigate() 相同的离页义务：离开工作台先存草稿；
                        // 主动打开另一篇笔记时取消无关的转换跟随，后台完成不得抢占当前位置
                        if this.page == Page::New {
                            this.save_current_draft(cx);
                        }
                        if follow.is_none() {
                            this.following_conversion = None;
                        }
                        // The old note remains scrollable during the read. Its
                        // current anchor, after all ownership guards pass, is the
                        // one that belongs in the repaired version.
                        if reader_origin.is_some() {
                            this.save_reading_position(cx);
                        }
                        if let Some(workspace) = &mut this.workspace {
                            let artifact = preview.course.dir.clone();
                            if let Err(error) = workspace.transaction(|state| {
                                // A repair publishes a new version of the same note.
                                // Carry its existing anchors into both reading modes;
                                // the reader resolves changed paragraphs by timestamp.
                                if let (Some((course_id, previous)), Some(manifest)) =
                                    (&reader_origin, &preview.course.manifest)
                                {
                                    carry_repaired_reading_positions(
                                        &mut state.positions,
                                        (course_id, previous),
                                        (&manifest.course_id, &manifest.version_id),
                                    );
                                }
                                for task in &mut state.tasks {
                                    if task.artifact.as_ref() == Some(&artifact) {
                                        task.unread = false;
                                    }
                                }
                                Ok(())
                            }) {
                                this.workspace_error = Some(format!("阅读状态尚未保存：{error:#}"));
                            }
                        }
                        settle_reader_notice(
                            &mut this.message,
                            &mut this.reader_failure_notice,
                            followed_notice.as_deref(),
                        );
                        if reader_origin.is_none() {
                            this.result_tab = 0;
                            this.sync_reader_tab_stops();
                        }
                        this.preview = Some(preview);
                        // 事件路径触发阅读页数据加载（渲染不再负责）
                        this.ensure_reader_data(cx);
                        this.apply_course_title_aliases();
                        this.restore_reading_position(cx);
                        this.page = Page::Result;
                        if follow.is_some() {
                            this.following_conversion = None;
                            if this.message.is_none() {
                                this.message = Some("笔记已打开".into());
                                this.reader_opened_notice_at = Some(Instant::now());
                            }
                        }
                    }
                    Err(error) => {
                        let io_kind = error.chain().find_map(|cause| {
                            cause.downcast_ref::<std::io::Error>().map(std::io::Error::kind)
                        });
                        let message = match io_kind {
                            Some(std::io::ErrorKind::NotFound) =>
                                "笔记文件暂时无法访问。请重新连接保存位置或恢复文件后再次阅读，也可刷新课程库。",
                            Some(std::io::ErrorKind::PermissionDenied) =>
                                "暂时无法读取这份笔记。请检查保存位置的访问权限后再次阅读，也可刷新课程库。",
                            _ =>
                                "这份笔记暂时无法读取。请检查保存位置中的文件，恢复可读版本后再次阅读，也可刷新课程库。",
                        }.to_owned();
                        this.reader_course_error = Some((reading_path, message.clone()));
                        if origin != Page::Library {
                            this.message = Some(message);
                            this.reader_failure_notice = this.message.clone();
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}

fn carry_repaired_reading_positions(
    positions: &mut BTreeMap<String, workspace::ReadingPosition>,
    previous: (&str, &str),
    next: (&str, &str),
) {
    if previous.0 != next.0 || previous.1 == next.1 {
        return;
    }
    for tab in 0..=1 {
        let previous_key = format!("{}:{}:{tab}", previous.0, previous.1);
        let next_key = format!("{}:{}:{tab}", next.0, next.1);
        if let Some(position) = positions.get(&previous_key).cloned() {
            positions.entry(next_key).or_insert(position);
        }
    }
}

/// A completed read owns only its previous failure and the notice captured
/// when following this conversion. Newer, unrelated feedback stays visible.
fn settle_reader_notice(
    current: &mut Option<String>,
    failure: &mut Option<String>,
    followed_notice: Option<&str>,
) {
    let failed_notice = failure.take();
    if current.as_deref().is_some_and(|message| {
        Some(message) == failed_notice.as_deref() || Some(message) == followed_notice
    }) {
        *current = None;
    }
}

#[cfg(test)]
mod reader_notice_tests {
    use super::settle_reader_notice;

    #[test]
    fn repair_position_handoff_uses_the_latest_old_position_and_keeps_existing_versions() {
        use super::{BTreeMap, carry_repaired_reading_positions, workspace::ReadingPosition};
        let start = ReadingPosition {
            seconds: Some(12.),
            offset: -120.,
            ..Default::default()
        };
        let latest = ReadingPosition {
            paragraph: Some("section-80".into()),
            seconds: Some(80.),
            offset: -840.,
            fraction: Some(0.4),
            ..Default::default()
        };
        let gallery = ReadingPosition {
            seconds: Some(36.),
            offset: -300.,
            ..Default::default()
        };
        let mut positions = BTreeMap::from([
            ("course:old:0".into(), start.clone()),
            ("course:old:1".into(), gallery.clone()),
        ]);
        // The user keeps scrolling while the new version is read. Completion
        // saves this latest old-note position before performing the handoff.
        positions.insert("course:old:0".into(), latest.clone());
        carry_repaired_reading_positions(&mut positions, ("course", "old"), ("course", "new"));
        assert_eq!(positions["course:new:0"], latest);
        assert_eq!(positions["course:new:1"], gallery);
        assert_eq!(positions["course:old:0"], latest);
        assert_eq!(positions["course:old:1"], gallery);

        positions.insert("course:new:0".into(), start.clone());
        carry_repaired_reading_positions(&mut positions, ("course", "old"), ("course", "new"));
        assert_eq!(positions["course:new:0"], start);
        let snapshot = positions.clone();
        carry_repaired_reading_positions(&mut positions, ("course", "old"), ("different", "new"));
        carry_repaired_reading_positions(&mut positions, ("course", "old"), ("course", "old"));
        assert_eq!(positions, snapshot);
    }

    #[test]
    fn successful_retry_clears_its_failed_read_notice() {
        let mut message = Some("笔记文件暂时无法访问".into());
        let mut failure = message.clone();
        settle_reader_notice(&mut message, &mut failure, None);
        assert!(message.is_none());
        assert!(failure.is_none());
    }

    #[test]
    fn successful_retry_preserves_newer_unrelated_feedback() {
        for newer in ["分类已恢复", "已加入任务：另一份笔记"] {
            let mut message = Some(newer.into());
            let mut failure = Some("笔记文件暂时无法访问".into());
            settle_reader_notice(&mut message, &mut failure, None);
            assert_eq!(message.as_deref(), Some(newer));
            assert!(failure.is_none());
        }
    }

    #[test]
    fn followed_completion_only_clears_its_captured_notice() {
        let followed = "已加入任务：当前笔记";
        for (current, expected) in [
            (followed, None),
            ("已加入任务：另一份笔记", Some("已加入任务：另一份笔记")),
        ] {
            let mut message = Some(current.into());
            settle_reader_notice(&mut message, &mut None, Some(followed));
            assert_eq!(message.as_deref(), expected);
        }
    }

    #[test]
    fn success_does_not_recreate_dismissed_or_clear_unowned_messages() {
        let mut dismissed = None;
        let mut failure = Some("笔记文件暂时无法访问".into());
        settle_reader_notice(&mut dismissed, &mut failure, None);
        assert!(dismissed.is_none());
        assert!(failure.is_none());

        let mut unowned = Some("笔记文件暂时无法访问".into());
        settle_reader_notice(&mut unowned, &mut None, None);
        assert!(unowned.is_some());
    }
}

impl Drop for Desktop {
    fn drop(&mut self) {
        if let Some(cancel) = &self.preview_cancel {
            cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        // 与 request_close 一致：字幕读取也要停
        if let Some(cancel) = &self.subtitle_cancel {
            cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }
}

fn dispatch_desktop_action(
    view: &WeakEntity<Desktop>,
    cx: &mut App,
    action: impl FnOnce(&mut Desktop, &mut Window, &mut Context<Desktop>) + 'static,
) {
    let Some(handle) = cx.active_window() else {
        return;
    };
    let view = view.clone();
    // Global action listeners run while the dispatching window is borrowed.
    // Update it after dispatch completes so the native menu fallback can run too.
    cx.defer(move |cx| {
        let _ = handle.update(cx, |_, window, cx| {
            if !window.has_active_dialog(cx) {
                let _ = view.update(cx, |this, cx| action(this, window, cx));
            }
        });
    });
}

fn main() {
    a11y::init_validation_diagnostics();
    let app = gpui_platform::application().with_assets(icons::Assets);
    app.on_reopen(|cx| {
        cx.activate(true);
        for handle in cx.windows() {
            let _ = cx.update_window(handle, |_, window, _| window.activate_window());
        }
    });
    app.run(|cx| {
        gpui_component::init(cx);
        gpui_component::set_locale("zh-CN");
        theme::init(cx);
        // Debug validation uses the same native window and render path at an exact size.
        // Release builds always use the ordinary initial window size.
        let initial_size = if cfg!(debug_assertions) {
            std::env::var("COURSE2MD_VALIDATION_WINDOW")
                .ok()
                .and_then(|value| {
                    let (width, height) = value.split_once('x')?;
                    let width = width.parse::<f32>().ok()?;
                    let height = height.parse::<f32>().ok()?;
                    (width.is_finite() && height.is_finite() && width >= 860. && height >= 620.)
                        .then_some(size(px(width), px(height)))
                })
                .unwrap_or(size(px(1140.), px(820.)))
        } else {
            size(px(1140.), px(820.))
        };
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::centered(initial_size, cx)),
                window_min_size: Some(size(px(860.), px(620.))),
                titlebar: Some(TitlebarOptions {
                    traffic_light_position: Some(point(px(16.), px(19.))),
                    ..TitleBar::title_bar_options()
                }),
                ..TitleBar::window_options()
            },
            |window, cx| {
                window.set_window_title("course2md");
                #[cfg(feature = "performance")]
                performance::start(window, cx);
                window
                    .observe_window_appearance(|window, _| window.refresh())
                    .detach();
                let view = cx.new(|cx| Desktop::new(window, cx));
                let weak = view.downgrade();
                let quit_view = weak.clone();
                let settings_view = weak.clone();
                let about_view = weak.clone();
                let new_view = weak.clone();
                let search_view = weak.clone();
                cx.on_action(move |_: &OpenAbout, cx| {
                    dispatch_desktop_action(&about_view, cx, |this, window, cx| {
                        this.settings_tab = 3;
                        this.open_settings(window, cx);
                    });
                });
                cx.on_action(move |_: &OpenSettings, cx| {
                    dispatch_desktop_action(&settings_view, cx, |this, window, cx| {
                        this.open_settings(window, cx);
                    });
                });
                cx.on_action(move |_: &ImportVideo, cx| {
                    dispatch_desktop_action(&new_view, cx, |this, window, cx| {
                        this.import_video_from_action(window, cx);
                    });
                });
                cx.on_action(move |_: &SearchContent, cx| {
                    dispatch_desktop_action(&search_view, cx, |this, window, cx| {
                        if this.page == Page::Result {
                            this.open_reader_find(window, cx);
                        } else {
                            this.navigate(Page::Library, cx);
                            this.inputs[&Field::Search]
                                .update(cx, |input, cx| input.focus(window, cx));
                        }
                    });
                });
                cx.on_action(move |_: &Quit, cx| {
                    if quit_view
                        .update(cx, |this, cx| this.request_close(cx))
                        .unwrap_or(true)
                    {
                        cx.quit();
                    }
                });
                window.on_window_should_close(cx, move |_, cx| {
                    let saved = weak
                        .update(cx, |this, cx| {
                            this.save_reading_position(cx);
                            this.flush_settings_for_exit(cx) && this.save_current_draft(cx)
                        })
                        .unwrap_or(true);
                    if saved {
                        cx.hide();
                    }
                    false
                });
                cx.new(|cx| Root::new(view, window, cx))
            },
        )
        .expect("无法创建 course2md 窗口");
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        cx.bind_keys([
            // Focused controls activate on key-up. The dialog's generic Enter
            // confirmation otherwise closes it on key-down before that action
            // can run (including validation, cancellation, and image controls).
            KeyBinding::new("enter", NoAction, Some("Dialog")),
            KeyBinding::new("secondary-q", Quit, None),
            KeyBinding::new("secondary-,", OpenSettings, None),
            KeyBinding::new("secondary-n", ImportVideo, None),
            KeyBinding::new("secondary-f", SearchContent, None),
        ]);
        cx.set_menus([
            gpui::Menu::new("course2md").items([
                gpui::MenuItem::action("关于 course2md", OpenAbout),
                gpui::MenuItem::action("设置…", OpenSettings),
                gpui::MenuItem::action("退出 course2md", Quit),
            ]),
            gpui::Menu::new("文件").items([gpui::MenuItem::action("导入视频", ImportVideo)]),
            gpui::Menu::new("查找")
                .items([gpui::MenuItem::action("搜索课程或当前笔记", SearchContent)]),
        ]);
        cx.activate(true);
    });
}

#[cfg(test)]
mod list_reconcile_tests {
    use super::Desktop;
    use gpui::{App, FocusHandle, ListAlignment, ListOffset, ListState, TestAppContext, px};

    #[gpui::test]
    fn list_splice_keeps_the_scroll_anchor_on_unchanged_content(cx: &mut TestAppContext) {
        cx.update(splice_keeps_anchor);
    }

    fn splice_keeps_anchor(cx: &mut App) {
        let state = ListState::new(4, ListAlignment::Top, px(100.));
        let mut keys = vec!["a".to_owned(), "b".into(), "c".into(), "d".into()];
        let mut focus: Vec<FocusHandle> = (0..4).map(|_| cx.focus_handle()).collect();
        state.scroll_to(ListOffset {
            item_ix: 3,
            offset_in_item: px(7.),
        });
        // Two rows arrive after the first row: the anchor item keeps its place.
        Desktop::reconcile_list_items(
            &state,
            &mut keys,
            &mut focus,
            vec![
                "a".into(),
                "x".into(),
                "y".into(),
                "b".into(),
                "c".into(),
                "d".into(),
            ],
            cx,
        );
        assert_eq!(state.item_count(), 6);
        assert_eq!(state.logical_scroll_top().item_ix, 5);
        assert_eq!(state.logical_scroll_top().offset_in_item, px(7.));
        assert_eq!(keys, ["a", "x", "y", "b", "c", "d"]);
        assert_eq!(focus.len(), 6);
        // An identical frame is a no-op.
        Desktop::reconcile_list_items(
            &state,
            &mut keys,
            &mut focus,
            vec![
                "a".into(),
                "x".into(),
                "y".into(),
                "b".into(),
                "c".into(),
                "d".into(),
            ],
            cx,
        );
        assert_eq!(state.logical_scroll_top().item_ix, 5);
        // Removing the rows above the anchor shifts it back with its content.
        Desktop::reconcile_list_items(
            &state,
            &mut keys,
            &mut focus,
            vec!["a".into(), "b".into(), "c".into(), "d".into()],
            cx,
        );
        assert_eq!(state.item_count(), 4);
        assert_eq!(state.logical_scroll_top().item_ix, 3);
    }
}
