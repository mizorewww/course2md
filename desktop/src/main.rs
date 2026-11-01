#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
// GPUI's macro output and this crate name its types through `gpui`; the Kit selects
// the engine behind that name, as gpui-kit-material does.
extern crate gpui_kit as gpui;

mod a11y;
mod about;
mod account_ui;
mod appearance_ui;
#[cfg(feature = "capture")]
mod capture;
mod codex_ui;
mod course_library;
mod folder_tree;
mod i18n;
mod import_ui;
mod library_ui;
mod ollama_ui;
mod onboarding;
#[cfg(feature = "performance")]
mod performance;
mod reader_images;
mod reader_ui;
mod settings_ui;
mod storage_ui;
mod task_runner;
mod task_ui;
mod views;

use desktop_core::{
    activity, backend, conversion, credentials, interrupted, model_discovery, notes, organize,
    preferences, reader_navigation, service_test, source, spawn_blocking_io, storage, workspace,
};

use desktop_ui::{note_text, symbols, theme, ui};

use desktop_core::conversion::{provider_from_id, provider_label, providers};
use desktop_core::job::{earliest_deadline, job_exit_status};
use desktop_core::library::FolderFilter;
use desktop_core::page::{Kind, Page};
use desktop_core::reader::note_read_failure;
use desktop_core::reader::{carry_repaired_reading_positions, settle_reader_notice};

use backend::{Done, EngineEvent, Event, Job};
use conversion::ConversionOptions;
use course2md::progress::ModelOperation;
use gpui::{prelude::*, *};
use gpui_kit::component::{
    input::{InputEvent, InputState},
    *,
};
use notes::Course;
use std::{
    collections::{BTreeMap, HashSet},
    path::PathBuf,
    rc::Rc,
    time::{Duration, Instant},
};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Field {
    Source,
    Title,
    /// The name dialog's field: a folder's or a note's name.
    Name,
    Search,
}
/// Debug builds: open a note in a given reader state at startup, so native
/// layout can be checked without synthetic input. Release builds ignore it.
struct ValidationOpen {
    /// Matched against note titles and version directories.
    note: Option<String>,
    /// Comma-separated reader panels: `find`, `info`.
    panels: String,
}

impl ValidationOpen {
    fn from_env() -> Option<Self> {
        if !cfg!(debug_assertions) {
            return None;
        }
        let note = std::env::var("COURSE2MD_VALIDATION_OPEN_NOTE").ok()?;
        Some(Self {
            note: Some(note),
            panels: std::env::var("COURSE2MD_VALIDATION_READER").unwrap_or_default(),
        })
    }
}

struct Desktop {
    preferences: preferences::Store,
    settings_ui: settings_ui::State,
    onboarding: onboarding::State,
    storage_ui: desktop_core::locations::State,
    reader_ui: reader_ui::State,
    workspace: Option<workspace::Workspace>,
    workspace_error: Option<String>,
    import: desktop_core::import::ImportSession,
    /// The running job: a task's or a model's.
    work: desktop_core::job::WorkState,
    /// The note being read, or on its way.
    opened: desktop_core::reader::OpenedNote,
    task_list: task_ui::TaskListUi,
    theme_frame: views::ThemeFrame,
    library: desktop_core::library::LibraryState,
    library_ui: library_ui::LibraryUi,
    preference_defaults_pending: bool,
    transient_task_result: Option<(String, Instant)>,
    draft_deadline: Option<Instant>,
    account: desktop_core::account::AccountUi,
    codex: desktop_core::codex::CodexUi,
    ollama: desktop_core::ollama::OllamaUi,
    /// A startup reconciliation of the task record is running.
    workspace_reconciling: bool,
    import_submit_focus: FocusHandle,
    root_focus: FocusHandle,
    /// The window this view lives in; deferred work returns to it.
    window: AnyWindowHandle,
    page: Page,
    /// Where Settings was opened from, and the control to refocus on return.
    settings_origin: Option<(Page, Option<FocusHandle>)>,
    settings_tab: settings_ui::SettingsTab,
    environment: Option<backend::Environment>,
    scrolls: [ScrollHandle; 5],
    inputs: BTreeMap<Field, Entity<InputState>>,
    config: course2md::settings::ConfigFile,
    settings_options: ConversionOptions,
    desktop_settings: course2md::settings::DesktopSettings,
    /// Debug validation only: a note to open after the first library scan,
    /// and the reader panels to show once it has loaded.
    validation: Option<ValidationOpen>,
    candidate_scroll: ScrollHandle,
    event_wake_tx: smol::channel::Sender<()>,
    event_wake_rx: smol::channel::Receiver<()>,
    // Entrance animations already played for logical content that a
    // virtualized list may unmount and remount while scrolling.
    entered: HashSet<String>,
    message: Option<String>,
    _subscriptions: Vec<Subscription>,
    _poll: Task<()>,
}

actions!(
    course2md_desktop,
    [Quit, OpenSettings, OpenAbout, ImportVideo, SearchContent]
);

/// Scrollbars fade after scrolling, or stay visible when the system asks for that.
/// `Theme::sync_scrollbar_appearance` would map the system's choice to hover.
fn sync_scrollbars(cx: &mut App) {
    use gpui_kit::component::scroll::ScrollbarMode;
    Theme::set_scrollbar_mode(
        if cx.should_auto_hide_scrollbars() {
            ScrollbarMode::Scrolling
        } else {
            ScrollbarMode::Always
        },
        cx,
    );
}

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
        if let Some(workspace) = &mut self.workspace
            && let Err(error) = workspace.transaction(|state| {
                state.stop_session();
                Ok(())
            })
        {
            self.workspace_error = Some(course2md::tr!(
                "无法保存退出前的状态，窗口暂不关闭：{}",
                format!("{error:#}")
            ));
            cx.notify();
            return false;
        }
        self.import.cancel_background_reads();
        self.codex.cancel_authorization();
        self.work.closing = true;
        self.work.quit_deadline = Some(Instant::now() + Duration::from_secs(10));
        self.refresh_dispatch_controls(cx);
        if self.work.active_task.is_none()
            && let Some(job) = &self.work.job
        {
            job.cancel();
        }
        if let Some(workspace) = &self.workspace
            && let Err(error) = workspace.save()
        {
            self.workspace_error = Some(course2md::tr!(
                "无法保存退出前的状态，窗口暂不关闭：{}",
                format!("{error:#}")
            ));
            cx.notify();
            return false;
        }
        self.message = Some(course2md::tr!("正在保存进度并退出…").into());
        cx.notify();
        self.work.job.is_none() && self.import.preview_workers == 0
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
        // Inputs and the first frame read the catalog immediately, so the
        // persisted language must be active before they are constructed.
        i18n::apply(preferences.application().language);
        let mut config = preferences.defaults_config();
        config.defaults.out = Some(output.clone());
        let fields = [
            (
                Field::Source,
                course2md::tr!("粘贴 YouTube 或 Bilibili 视频链接"),
                String::new(),
            ),
            (Field::Title, "", String::new()),
            (Field::Search, course2md::tr!("搜索笔记标题"), String::new()),
            (Field::Name, "", String::new()),
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
                    }),
                )
            })
            .collect();
        let mut subscriptions: Vec<Subscription> = inputs
            .iter()
            .map(|(field, input)| {
                let field = *field;
                cx.observe_in(input, window, move |this: &mut Self, _, window, cx| {
                    if field == Field::Source {
                        let value = this.value(Field::Source, cx);
                        // The draft records the input once autosave or a
                        // preview accepts it; anything else is a new edit.
                        if this.draft().is_some_and(|draft| draft.input != value) {
                            this.import.following_conversion = None;
                            if this.opened.reading && this.page == Page::New {
                                this.opened.stop_reading();
                            }
                            if !this.prepare_next_import(&value, window, cx) {
                                return;
                            }
                            this.invalidate_source();
                        }
                    }
                    if matches!(field, Field::Source | Field::Title) {
                        this.draft_deadline = Some(Instant::now() + Duration::from_millis(350));
                        this.schedule_wake(Duration::from_millis(350));
                    }
                    if field == Field::Title
                        && !this.value(Field::Title, cx).is_empty()
                        && this.import.title_required
                    {
                        this.import.source_validation = None;
                        this.import.title_required = false;
                    }
                    if field == Field::Search {
                        this.library_ui.list.scroll_to(ListOffset {
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
                    && this.online()
                {
                    this.inspect_source(window, cx);
                }
            },
        ));
        subscriptions.push(
            cx.subscribe(&inputs[&Field::Name], |this: &mut Self, _, event, cx| {
                if matches!(event, InputEvent::Change) {
                    this.name_edited(cx);
                }
                if matches!(event, InputEvent::PressEnter { .. }) {
                    this.save_name(cx);
                }
            }),
        );
        // Appearance and scale reach the theme before Root renders, not during it: a
        // preference change notifies this view, and the system's appearance tells the window.
        subscriptions.push(cx.observe_in(
            &cx.entity(),
            window,
            |this: &mut Self, _, window, cx| {
                this.prepare_theme(window, cx);
            },
        ));
        subscriptions.push(cx.observe_window_appearance(window, |this, window, cx| {
            this.prepare_theme(window, cx);
        }));
        // The system's scrollbar preference can change while the window is away.
        subscriptions.push(cx.observe_window_activation(window, |_, _, cx| {
            sync_scrollbars(cx);
        }));
        let options = ConversionOptions::from_config(&config);
        let poll = cx.spawn_in(window, async |this, cx| {
            loop {
                let Ok((wake, deadline)) = this.update_in(cx, |this, _, _| {
                    (this.event_wake_rx.clone(), this.next_poll_deadline())
                }) else {
                    break;
                };
                let wait = async {
                    let _ = wake.recv().await;
                };
                let timeout = deadline
                    .map(|deadline| deadline.saturating_duration_since(Instant::now()))
                    .unwrap_or(Duration::from_secs(3600));
                let timer = cx.background_executor().timer(timeout);
                smol::future::or(wait, timer).await;
                match this.update_in(cx, |this, _, cx| {
                    this.poll(cx);
                }) {
                    Ok(()) => {}
                    Err(_) => break,
                }
            }
        });
        let (workspace, workspace_error) =
            match workspace::Workspace::open(output.clone(), options.clone()) {
                Ok(workspace) => (Some(workspace), None),
                Err(error) => (
                    None,
                    Some(course2md::tr!("任务记录读取失败：{}", format!("{error:#}"))),
                ),
            };
        let (event_wake_tx, event_wake_rx) = smol::channel::bounded(1);
        if let Some(workspace) = &workspace {
            workspace.set_wake_sender(event_wake_tx.clone());
        }
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
            import: Default::default(),
            work: desktop_core::job::WorkState::new(),
            opened: Default::default(),
            task_list: task_ui::TaskListUi::new(),
            theme_frame: Default::default(),
            library: desktop_core::library::LibraryState::new(output),
            library_ui: library_ui::LibraryUi::new(),
            preference_defaults_pending: false,
            transient_task_result: None,
            draft_deadline: None,
            page: Page::New,
            workspace_reconciling: false,
            import_submit_focus: cx.focus_handle(),
            root_focus: Self::install_root_focus(window, cx),
            account: desktop_core::account::AccountUi::default(),
            codex: desktop_core::codex::CodexUi::default(),
            ollama: desktop_core::ollama::OllamaUi::default(),
            window: window.window_handle(),
            settings_origin: None,
            settings_tab: settings_ui::SettingsTab::Appearance,
            environment: None,
            scrolls: std::array::from_fn(|_| ScrollHandle::new()),
            inputs,
            desktop_settings: config.desktop.clone(),
            config,
            settings_options: options,
            validation: ValidationOpen::from_env(),
            candidate_scroll: ScrollHandle::new(),
            event_wake_tx,
            event_wake_rx,
            entered: HashSet::new(),
            message,
            _subscriptions: subscriptions,
            _poll: poll,
        };
        // Preferences and workspace records are saved separately. Reconcile
        // inherited options after an interrupted save before restoring input.
        this.refresh_preference_defaults(cx);
        this.restore_draft(window, cx);
        this.prepare_workbench_input(window, cx);
        this.restore_storage_state(cx);
        std::thread::spawn(source::prune_cache);
        cx.set_reduce_motion(this.desktop_settings.reduce_motion);
        this.refresh_account(cx);
        this.refresh_environment(cx);
        this.refresh_library(cx);
        this.poll_workspace(cx);
        if !this.preferences.application().desktop.setup_completed {
            this.start_onboarding(window, cx);
        }
        this
    }
    /// Reports record writes that failed after the event loop moved on, and
    /// reconciles a freshly opened record with the libraries off the UI thread.
    fn poll_workspace(&mut self, cx: &mut Context<Self>) {
        let Some(workspace) = &self.workspace else {
            return;
        };
        if let Some(failure) = workspace.take_write_error() {
            self.workspace_error = Some(if failure.control {
                course2md::tr!("任务指令尚未保存：{}", failure.message)
            } else {
                course2md::tr!("任务进度尚未保存：{}", failure.message)
            });
            // A job that missed its instruction must not send anything new.
            if failure.control
                && let Some(job) = &self.work.job
            {
                job.cancel();
            }
            cx.notify();
        }
        if self.workspace_reconciling {
            return;
        }
        let Some(reconciliation) = workspace.reconciliation() else {
            return;
        };
        // A record reopened elsewhere needs the wake sender too.
        workspace.set_wake_sender(self.event_wake_tx.clone());
        self.workspace_reconciling = true;
        let task = spawn_blocking_io(move || reconciliation.run());
        cx.spawn(async move |this, cx| {
            let reconciled = task.recv().await;
            let _ = this.update(cx, |this, cx| {
                let Ok(reconciled) = reconciled else {
                    // Leave dispatch held: the checks did not finish.
                    this.workspace_error = Some(interrupted().into());
                    cx.notify();
                    return;
                };
                this.workspace_reconciling = false;
                if let Some(workspace) = &mut this.workspace
                    && let Err(error) = workspace.apply_reconciliation(reconciled)
                {
                    this.workspace_error =
                        Some(course2md::tr!("任务进度尚未保存：{}", format!("{error:#}")));
                }
                this.start_next_task(cx);
                cx.notify();
            });
        })
        .detach();
    }
    fn refresh_environment(&mut self, cx: &mut Context<Self>) {
        self.environment = None;
        // Environment detection starts subprocesses synchronously (Metal enumeration can exceed ten seconds),
        // so it runs off the executor; see spawn_blocking_io.
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
            if ui::modal_open(window, cx) {
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
                self.import.completed_source = None;
                self.invalidate_source();
                self.message = None;
                self.inputs[&Field::Title].update(cx, |state, cx| state.set_value("", window, cx));
                self.scrolls[Page::New as usize].set_offset(point(px(0.), px(0.)));
                true
            }
            Err(error) => {
                self.workspace_error =
                    Some(course2md::tr!("新链接未能保存：{}", format!("{error:#}")));
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
        self.import.source_editor_open = true;
        self.navigate(Page::New, cx);
        if self.online() {
            self.inputs[&Field::Source].update(cx, |input, cx| input.focus(window, cx));
        }
    }

    fn open_about(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_settings(window, cx);
        self.select_settings_tab(settings_ui::SettingsTab::Application, window, cx);
    }

    /// Find inside the open note, otherwise search the library.
    fn search_content(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.page == Page::Result {
            self.open_reader_find(window, cx);
        } else {
            self.navigate(Page::Library, cx);
            self.inputs[&Field::Search].update(cx, |input, cx| input.focus(window, cx));
        }
    }

    fn navigate(&mut self, page: Page, cx: &mut Context<Self>) {
        self.transient_task_result = None;
        if page != Page::New
            || matches!(
                self.import.following_conversion,
                Some(desktop_core::import::ConversionFollow::ReaderTask { .. })
            )
        {
            self.import.following_conversion = None;
        }
        self.save_reading_position(cx);
        self.opened.stop_reading();
        self.opened.opening_course = None;
        if self.page == Page::New {
            self.save_current_draft(cx);
        }
        if page != Page::Result {
            self.release_reader_images(cx);
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
    fn output(&self) -> PathBuf {
        self.workspace
            .as_ref()
            .and_then(|w| w.state.library(&w.state.default_library))
            .map(|l| l.root.clone())
            .unwrap_or_else(|| self.library.root.clone())
    }
    fn pick_video(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let prompt = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(course2md::tr!("选择课程视频").into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = prompt.await;
            let _ = this.update_in(cx, |this, window, cx| {
                match result {
                    Ok(Ok(Some(paths))) => {
                        if let Some(path) = paths.first() {
                            this.inputs[&Field::Source].update(cx, |state, cx| {
                                state.set_value(path.display().to_string(), window, cx)
                            });
                            this.inspect_source(window, cx);
                        }
                    }
                    Ok(Ok(None)) => {}
                    Ok(Err(_)) | Err(_) => {
                        this.message = Some(course2md::tr!("无法打开文件选择器").into())
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
    fn start_model_preparation(&mut self, cx: &mut Context<Self>) {
        match Job::start(self.model_preparation_args(), self.event_wake_tx.clone()) {
            Ok(job) => {
                self.work.job = Some(job);
                self.work.active_task = None;
                self.work.kind = Kind::Models;
                self.work.cancelling = false;
                self.work.job_events = Default::default();
                self.work.task_status = course2md::tr!("正在准备识别模型").into();
            }
            Err(error) => {
                self.work.job_events.error = Some(format!("{error:#}"));
                self.model_preparation_finished(false, false, cx);
            }
        }
        cx.notify();
    }
    fn wake_poll(&self) {
        let _ = self.event_wake_tx.try_send(());
    }
    /// Makes the event loop recompute its next deadline now. Every delayed
    /// wake (the draft save, a transient task result) is a deadline in
    /// `next_poll_deadline`, which the loop sleeps toward on an async timer, so
    /// no thread has to sleep for `_after`.
    fn schedule_wake(&self, _after: Duration) {
        self.wake_poll();
    }
    /// The next instant the UI must run `poll` even without a job event.
    /// Returning `None` lets the loop sleep until an explicit wake.
    fn next_poll_deadline(&self) -> Option<Instant> {
        let mut deadline = earliest_deadline(self.draft_deadline, self.work.quit_deadline);
        if let Some((_, shown)) = &self.transient_task_result {
            deadline = earliest_deadline(deadline, Some(*shown + Duration::from_secs(8)));
        }
        // Running work shows a live clock.
        if self.work.job.is_some() {
            deadline = earliest_deadline(deadline, Some(Instant::now() + Duration::from_secs(1)));
        }
        if self.storage_ui.resume_ready() {
            deadline = earliest_deadline(deadline, Some(Instant::now()));
        }
        deadline
    }
    fn poll(&mut self, cx: &mut Context<Self>) {
        if self
            .transient_task_result
            .as_ref()
            .is_some_and(|(_, shown)| shown.elapsed() >= Duration::from_secs(8))
        {
            self.transient_task_result = None;
            cx.notify();
        }
        self.poll_storage(cx);
        self.poll_workspace(cx);
        if self
            .draft_deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            self.save_current_draft(cx);
        }
        let events: Vec<_> = self
            .work
            .job
            .as_ref()
            .map(|job| job.events.try_iter().take(512).collect())
            .unwrap_or_default();
        let streaming = !events.is_empty();
        // Visible state transitions repaint now; log/progress lines coalesce.
        let mut immediate = false;
        let mut commit = false;
        for event in events {
            let event = match event {
                Event::Exit { success, cancelled } => {
                    immediate = true;
                    self.work.job = None;
                    self.storage_ui.job_exited();
                    self.work.cancelling = false;
                    if let Some(id) = self.work.active_task.take() {
                        self.finish_task(&id, success, cancelled, cx);
                        self.refresh_model_diagnostics(cx);
                    } else {
                        self.work.task_status = job_exit_status(success, cancelled).into();
                        self.model_preparation_finished(success, cancelled, cx);
                    }
                    continue;
                }
                Event::Engine(event) => event,
            };
            if let EngineEvent::ModelReady {
                operation, model, ..
            } = &event
            {
                let name = model
                    .as_ref()
                    .map(|model| format!("{model} · "))
                    .unwrap_or_default();
                self.work.task_status = format!(
                    "{name}{}",
                    match operation {
                        ModelOperation::Prepare => course2md::tr!("模型已就绪"),
                        ModelOperation::Download => course2md::tr!("模型已下载"),
                    }
                );
            }
            immediate |= match &self.work.active_task {
                Some(id) => {
                    // What the task said lives in `events.jsonl`. Only what changes
                    // its record needs committing.
                    commit |= matches!(
                        event,
                        EngineEvent::Error { .. } | EngineEvent::Blocked { .. }
                    );
                    self.workspace
                        .as_mut()
                        .and_then(|workspace| workspace.state.task_mut(id))
                        .is_some_and(|task| task.apply_event(&event))
                }
                None => self.work.job_events.apply(&event),
            };
        }
        if commit
            && let Some(workspace) = &self.workspace
            && let Err(error) = workspace.commit()
        {
            self.workspace_error =
                Some(course2md::tr!("任务进度尚未保存：{}", format!("{error:#}")));
        }
        if self.work.closing {
            if self.work.job.is_none() && self.import.preview_workers == 0 {
                cx.quit();
                return;
            }
            if self
                .work
                .quit_deadline
                .is_some_and(|deadline| Instant::now() >= deadline)
            {
                if let Some(job) = &self.work.job {
                    job.cancel();
                }
                cx.quit();
                return;
            }
        } else {
            self.start_next_task(cx);
        }
        if self.work.repaint_due(streaming, immediate) {
            cx.notify();
        }
    }
    pub(crate) fn refresh_library(&mut self, cx: &mut Context<Self>) {
        let locations = self.registered_storage_locations();
        let generation = self.library.begin_scan();
        self.storage_ui
            .location_checks
            .begin(generation, &locations);
        let task = desktop_core::locations::scan_locations(locations, move |location| {
            let scan = notes::scan_library(&location.root);
            let organization = organize::Library::load(&location.root);
            let cache =
                desktop_core::library::LibraryViewCache::inspect(location, scan.as_ref().ok());
            (scan, organization, cache)
        });
        cx.spawn(async move |this, cx| {
            let results = task.await;
            let _ = this.update(cx, |this, cx| {
                if this.library.generation != generation {
                    return;
                }
                let current = this.registered_storage_locations();
                let checks = results
                    .iter()
                    .map(|(location, check, _)| (location.clone(), check.clone()))
                    .collect();
                if !this
                    .storage_ui
                    .location_checks
                    .finish(generation, &current, checks)
                {
                    this.refresh_library(cx);
                    return;
                }
                // Tasks of a library that has come back can read their events now.
                if let Some(workspace) = &mut this.workspace {
                    for (location, check, _) in &results {
                        if check.available && !check.needs_reassociation && check.problem.is_none()
                        {
                            workspace.recheck_library(&location.id);
                        }
                    }
                    if workspace.state.reconciling {
                        this.wake_poll();
                    }
                }
                this.library.apply_scan(results);
                this.apply_course_title_aliases();
                this.open_validation_note(cx);
                let registered = this
                    .workspace
                    .as_ref()
                    .map(|workspace| workspace.state.libraries.as_slice())
                    .unwrap_or_default();
                this.library.settle_root(registered);
                this.advance_conversion_when_ready(cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn course_location(&self, course: &Course) -> Option<&workspace::LibraryLocation> {
        self.library
            .course_location(&self.workspace.as_ref()?.state.libraries, course)
    }
    fn course_folder(&self, course: &Course) -> Option<u64> {
        self.library
            .course_folder(&self.workspace.as_ref()?.state.libraries, course)
    }
    fn open_course(&mut self, course: impl Into<Rc<Course>>, cx: &mut Context<Self>) {
        self.import.following_conversion = None;
        self.open_course_with_conversion_guard(course.into(), None, cx);
    }

    fn open_completed_conversion(&mut self, course: Course, cx: &mut Context<Self>) {
        let follow = self.import.following_conversion.clone();
        self.open_course_with_conversion_guard(Rc::new(course), follow, cx);
    }

    fn open_course_with_conversion_guard(
        &mut self,
        course: Rc<Course>,
        follow: Option<desktop_core::import::ConversionFollow>,
        cx: &mut Context<Self>,
    ) {
        // Reading needs its own copy on the worker; rendering shares the rest.
        let course = Rc::unwrap_or_clone(course);
        self.save_reading_position(cx);
        let reader_revision = self.opened.read_generation;
        let reader_origin = follow.as_ref().and_then(|follow| {
            if !matches!(
                follow,
                desktop_core::import::ConversionFollow::ReaderTask { .. }
            ) {
                return None;
            }
            let manifest = self.opened.preview.as_ref()?.course.manifest.as_ref()?;
            Some((manifest.course_id.clone(), manifest.version_id.clone()))
        });
        cx.notify();
        let reading_path = course.dir.clone();
        let origin = self.page;
        let generation = self.opened.begin_read(reading_path.clone(), origin);
        let task = spawn_blocking_io(move || notes::read_preview(course));
        cx.spawn(async move |this, cx| {
            let result = task
                .recv()
                .await
                .unwrap_or_else(|_| Err(anyhow::anyhow!(course2md::tr!("读取笔记时出错，请重试"))));
            let _ = this.update(cx, |this, cx| {
                if this.opened.read_generation != generation || this.page != origin {
                    return;
                }
                this.opened.reading = false;
                this.opened.opening_course = None;
                if this.reader_viewer_open() {
                    desktop_core::import::ConversionFollow::interrupt_for_reader_viewer(
                        &mut this.import.following_conversion,
                    );
                }
                if follow.as_ref().is_some_and(|follow| {
                    this.import.following_conversion.as_ref() != Some(follow)
                        || !follow.context_is_current(
                            this.page,
                            this.import.preview_generation,
                            reader_revision,
                            this.opened
                                .preview
                                .as_ref()
                                .map(|preview| preview.course.dir.as_path()),
                        )
                }) {
                    cx.notify();
                    return;
                }
                match result {
                    Ok(preview) => {
                        // The same duties as navigate() when leaving a page: save the draft when leaving the workbench;
                        // opening another note stops following an unrelated conversion, and a background completion never takes over.
                        if this.page == Page::New {
                            this.save_current_draft(cx);
                        }
                        if follow.is_none() {
                            this.import.following_conversion = None;
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
                                this.workspace_error = Some(course2md::tr!(
                                    "阅读进度未能保存：{}",
                                    format!("{error:#}")
                                ));
                            }
                        }
                        settle_reader_notice(
                            &mut this.message,
                            &mut this.opened.reader_failure_notice,
                        );
                        if reader_origin.is_none() {
                            this.opened.result_tab = 0;
                        }
                        this.opened.preview = Some(Rc::new(preview));
                        this.ensure_reader_data(cx);
                        this.apply_course_title_aliases();
                        this.restore_reading_position(cx);
                        this.page = Page::Result;
                        this.show_validation_panels();
                        if follow.is_some() {
                            this.import.following_conversion = None;
                        }
                    }
                    Err(error) => {
                        let message = note_read_failure(&error).to_owned();
                        this.opened.reader_course_error = Some((reading_path, message.clone()));
                        if origin != Page::Library {
                            this.message = Some(message);
                            this.opened.reader_failure_notice = this.message.clone();
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}

impl Drop for Desktop {
    fn drop(&mut self) {
        self.import.cancel_background_reads();
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
            if !ui::modal_open(window, cx) {
                let _ = view.update(cx, |this, cx| action(this, window, cx));
            }
        });
    });
}

/// The first window fits the display's usable area (without the taskbar or
/// dock), centered in it with a margin, so its title bar, the only place to
/// move it from, is never off screen. It still keeps its minimum size, but on
/// a display smaller than that it starts at the area's top-left corner.
fn initial_window_bounds(
    wanted: gpui::Size<Pixels>,
    min: gpui::Size<Pixels>,
    area: gpui::Bounds<Pixels>,
) -> gpui::Bounds<Pixels> {
    let margin = px(16.);
    let fit = |wanted: Pixels, min: Pixels, room: Pixels| wanted.min(room - margin * 2.).max(min);
    let size = size(
        fit(wanted.width, min.width, area.size.width),
        fit(wanted.height, min.height, area.size.height),
    );
    let mut bounds = gpui::Bounds::centered_at(area.center(), size);
    bounds.origin.x = bounds.origin.x.max(area.origin.x);
    bounds.origin.y = bounds.origin.y.max(area.origin.y);
    bounds
}

/// A binding on the platform's primary modifier. GPUI resolves `secondary-` only
/// through `load`, which knows the host OS; `KeyBinding::new` panics on it.
pub(crate) fn secondary_binding<A: gpui::Action>(
    keystrokes: &str,
    action: A,
    context: Option<&str>,
    cx: &App,
) -> KeyBinding {
    let context = context.map(|context| {
        gpui::KeyBindingContextPredicate::parse(context)
            .expect("a valid key context")
            .into()
    });
    KeyBinding::load(keystrokes, Box::new(action), context, false, None, cx)
        .expect("a valid keystroke")
}

fn main() {
    a11y::init_validation_diagnostics();
    let app = gpui_kit::application().with_assets(symbols::Assets);
    app.on_reopen(|cx| {
        cx.activate(true);
        for handle in cx.windows() {
            let _ = cx.update_window(handle, |_, window, _| window.activate_window());
        }
    });
    app.run(|cx| {
        symbols::register_fonts(cx);
        gpui_kit::init(cx);
        gpui_kit_material::init(cx);
        sync_scrollbars(cx);
        // Debug validation uses the same native window and render path at an exact size.
        // Release builds always use the ordinary initial window size.
        // Debug validation asks for an exact size, which it keeps even where
        // it does not fit, so captures are comparable across displays.
        let validation_size = if cfg!(debug_assertions) {
            std::env::var("COURSE2MD_VALIDATION_WINDOW")
                .ok()
                .and_then(|value| {
                    let (width, height) = value.split_once('x')?;
                    let width = width.parse::<f32>().ok()?;
                    let height = height.parse::<f32>().ok()?;
                    (width.is_finite() && height.is_finite() && width >= 720. && height >= 620.)
                        .then_some(size(px(width), px(height)))
                })
        } else {
            None
        };
        let min_size = size(px(720.), px(620.));
        let window_bounds = match (validation_size, cx.primary_display()) {
            (Some(exact), _) => WindowBounds::centered(exact, cx),
            (None, Some(display)) => WindowBounds::Windowed(initial_window_bounds(
                size(px(1140.), px(820.)),
                min_size,
                display.visible_bounds(),
            )),
            (None, None) => WindowBounds::centered(size(px(1140.), px(820.)), cx),
        };
        gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(window_bounds),
                window_min_size: Some(min_size),
                ..gpui_kit_material::components::title_bar_window_options(
                    "course2md",
                    views::APP_BAR_HEIGHT,
                    min_size,
                    cx,
                )
            },
            cx,
            |window, cx| {
                window.set_window_title("course2md");
                #[cfg(feature = "performance")]
                performance::start(window, cx);
                let view = cx.new(|cx| Desktop::new(window, cx));
                #[cfg(feature = "capture")]
                capture::start(view.downgrade(), window, cx);
                view.update(cx, |this, cx| this.init_theme(window, cx));
                let weak = view.downgrade();
                let quit_view = weak.clone();
                let settings_view = weak.clone();
                let about_view = weak.clone();
                let new_view = weak.clone();
                let search_view = weak.clone();
                cx.on_action(move |_: &OpenAbout, cx| {
                    dispatch_desktop_action(&about_view, cx, Desktop::open_about);
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
                    dispatch_desktop_action(&search_view, cx, Desktop::search_content);
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
                    // macOS keeps the app running with its window hidden. Other
                    // platforms cannot hide, so closing the window quits the app.
                    if !cfg!(target_os = "macos") {
                        return weak
                            .update(cx, |this, cx| this.request_close(cx))
                            .unwrap_or(true);
                    }
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
                view
            },
        )
        .expect(course2md::tr!("无法创建 course2md 窗口"));
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
            secondary_binding("secondary-q", Quit, None, cx),
            secondary_binding("secondary-,", OpenSettings, None, cx),
            secondary_binding("secondary-n", ImportVideo, None, cx),
            secondary_binding("secondary-f", SearchContent, None, cx),
        ]);
        cx.set_menus([
            gpui::Menu::new("course2md").items([
                gpui::MenuItem::action(course2md::tr!("关于 course2md"), OpenAbout),
                gpui::MenuItem::action(course2md::tr!("设置…"), OpenSettings),
                gpui::MenuItem::action(course2md::tr!("退出 course2md"), Quit),
            ]),
            gpui::Menu::new(course2md::tr!("文件")).items([gpui::MenuItem::action(
                course2md::tr!("导入视频"),
                ImportVideo,
            )]),
            gpui::Menu::new(course2md::tr!("查找")).items([gpui::MenuItem::action(
                course2md::tr!("搜索笔记"),
                SearchContent,
            )]),
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

#[cfg(test)]
mod initial_window_tests {
    use super::initial_window_bounds;
    use gpui::{Bounds, point, px, size};

    fn area(x: f32, y: f32, width: f32, height: f32) -> Bounds<gpui::Pixels> {
        Bounds::new(point(px(x), px(y)), size(px(width), px(height)))
    }

    #[test]
    fn a_large_display_centers_the_wanted_size_in_its_usable_area() {
        let bounds = initial_window_bounds(
            size(px(1140.), px(820.)),
            size(px(720.), px(620.)),
            area(0., 25., 1920., 1055.),
        );
        assert_eq!(bounds.size, size(px(1140.), px(820.)));
        assert_eq!(bounds.origin, point(px(390.), px(142.5)));
    }

    #[test]
    fn a_small_display_shrinks_the_window_into_its_usable_area() {
        // 1024×768 with a 48px taskbar: the window used to open at
        // (-66, -30), its title bar above the screen.
        let usable = area(0., 0., 1024., 720.);
        let bounds =
            initial_window_bounds(size(px(1140.), px(820.)), size(px(720.), px(620.)), usable);
        assert_eq!(bounds.size, size(px(992.), px(688.)));
        assert_eq!(bounds.origin, point(px(16.), px(16.)));
    }

    #[test]
    fn a_display_below_the_minimum_keeps_the_title_bar_on_screen() {
        let bounds = initial_window_bounds(
            size(px(1140.), px(820.)),
            size(px(720.), px(620.)),
            area(0., 0., 800., 600.),
        );
        assert_eq!(bounds.size, size(px(768.), px(620.)));
        assert_eq!(bounds.origin, point(px(16.), px(0.)));
    }
}

#[cfg(test)]
mod key_binding_tests {
    use super::{Quit, secondary_binding};

    #[gpui::test]
    fn secondary_bindings_resolve_on_the_host(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            for keystrokes in ["secondary-f", "secondary-,", "secondary-shift-g"] {
                let binding = secondary_binding(keystrokes, Quit, Some("CourseReader"), cx);
                assert_eq!(binding.keystrokes().len(), 1);
            }
        });
    }
}
