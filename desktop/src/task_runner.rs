//! Simple FIFO task runner and task action handlers for the 5-state in-place task model.
use super::*;
use crate::{
    notes::default_output,
    workspace::{Intent, RunMode, TaskPlan, TaskRecord, TaskStatus},
};
use anyhow::{Context as _, Result};
use desktop_tasks::import::{WorkbenchIssue, WorkbenchIssueKind};
use desktop_tasks::plan::*;
use desktop_tasks::run;
use desktop_tasks::task_view::{TaskAction, TaskGroup, task_group, task_status_label};
use std::collections::HashMap;

pub(crate) fn settings_tab_for_message(message: &str) -> settings_ui::SettingsTab {
    let lower = message.to_ascii_lowercase();
    let is_service_or_account = [
        "服务", "密钥", "账号", "登录", "鉴权", "授权", "接口", "地址", "cookie", "api", "key",
        "codex", "chatgpt", "ollama", "bilibili", "b站", "b 站", "youtube", "401", "403",
    ]
    .iter()
    .any(|needle| message.contains(needle) || lower.contains(needle));
    if is_service_or_account {
        settings_ui::SettingsTab::Services
    } else {
        settings_ui::SettingsTab::Generation
    }
}

impl Desktop {
    /// The current input form.
    pub(crate) fn draft<'a>(&self, cx: &'a App) -> Option<&'a workspace::Draft> {
        self.records.read(cx).draft()
    }

    /// Whether the current input is an online link rather than a local file.
    pub(crate) fn online(&self, cx: &App) -> bool {
        self.draft(cx).is_none_or(|draft| draft.online)
    }

    /// Where videos' text comes from: Settings' 文字来源.
    pub(crate) fn text_mode(&self, _cx: &App) -> course2md::config::TranscriptSource {
        self.preferences
            .generation()
            .options
            .transcript_source
            .unwrap_or_default()
    }

    /// One saved change to the workspace. A persistence failure shows in the shell as
    /// `failure`, whose `{}` takes the cause.
    pub(crate) fn transact<T>(
        &mut self,
        failure: &str,
        edit: impl FnOnce(&mut workspace::State) -> Result<T>,
        cx: &mut App,
    ) -> Option<T> {
        let result = self.records.change(cx, |records, _| {
            Some(records.workspace.as_mut()?.transaction(edit))
        })?;
        match result {
            Ok(value) => Some(value),
            Err(error) => {
                self.report(failure, error);
                None
            }
        }
    }

    fn report(&mut self, failure: &str, error: anyhow::Error) {
        self.workspace_error = Some(failure.replacen("{}", &format!("{error:#}"), 1));
    }

    pub(crate) fn set_task_page_notice(&mut self, notice: Option<String>, cx: &mut Context<Self>) {
        self.tasks
            .update(cx, |page, cx| page.set_notice(notice, cx));
    }

    /// Opens Settings on the tab relevant to `message`.
    pub(crate) fn open_settings_for_message(
        &mut self,
        message: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let tab = settings_tab_for_message(message);
        if self.page == Page::Settings {
            self.select_settings_tab(tab, window, cx);
        } else {
            self.settings_tab = tab;
            self.open_settings(window, cx);
        }
    }

    /// The task with this id, if the record is readable.
    pub(crate) fn task<'a>(&self, id: &str, cx: &'a App) -> Option<&'a TaskRecord> {
        self.records.read(cx).task(id)
    }

    /// Change the current draft and record it.
    pub(crate) fn update_draft(
        &mut self,
        edit: impl FnOnce(&mut workspace::Draft),
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(edited) = self.with_workspace(cx, |workspace| edit_current_draft(workspace, edit))
        else {
            return false;
        };
        match edited {
            Ok(_) => {
                self.workspace_error = None;
                true
            }
            Err(error) => {
                self.report(course2md::tr!("当前输入尚未保存：{}"), error);
                cx.notify();
                false
            }
        }
    }

    fn workspace_root(&self) -> PathBuf {
        self.config
            .defaults
            .out
            .clone()
            .unwrap_or_else(default_output)
    }

    /// Puts a freshly opened or rebuilt record in place and shows what it holds.
    fn install_workspace(
        &mut self,
        workspace: workspace::Workspace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.message = workspace.recovery.clone();
        self.records
            .change(cx, |records, _| records.workspace = Some(workspace));
        self.workspace_error = None;
        self.restore_draft(window, cx);
        self.refresh_library(cx);
    }

    pub fn retry_workspace_records(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(workspace) = &self.records.read(cx).workspace {
            self.workspace_error = None;
            if let Err(error) = workspace.save() {
                self.report(course2md::tr!("记录仍未保存：{}"), error);
            }
        } else {
            match workspace::Workspace::open(self.workspace_root()) {
                Ok(workspace) => self.install_workspace(workspace, window, cx),
                Err(error) => self.report(course2md::tr!("记录仍无法读取，原件已保留：{}"), error),
            }
        }
        cx.notify();
    }

    /// Called only by the explicit global recovery action; rebuilding never starts the queue.
    pub fn rebuild_workspace_records(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.work.read(cx).job.is_some() || self.storage_ui.busy {
            self.workspace_error = Some(
                course2md::tr!("当前还有处理或文件移动在进行，请等候停止后再重建记录。").into(),
            );
            cx.notify();
            return;
        }
        let root = self.workspace_root();
        let result = if let Some(workspace) = &self.records.read(cx).workspace {
            workspace::Workspace::rebuild_at(workspace.storage_path().to_owned(), root)
        } else {
            workspace::Workspace::rebuild(root)
        };
        match result {
            Ok(workspace) => self.install_workspace(workspace, window, cx),
            Err(error) => self.report(course2md::tr!("记录尚未重建，原件仍保留：{}"), error),
        }
        cx.notify();
    }

    pub fn recommended_local_provider(&self) -> course2md::config::AsrProvider {
        recommended_local_provider(self.environment.as_ref())
    }

    /// Whether this task holds the running background job.
    pub(crate) fn is_running_task(&self, id: &str, cx: &App) -> bool {
        self.work.read(cx).is_running(id)
    }

    /// The library location a task saves to.
    pub(crate) fn task_library<'a>(
        &self,
        task: &TaskRecord,
        cx: &'a App,
    ) -> Option<&'a workspace::LibraryLocation> {
        self.records.read(cx).task_library(task)
    }

    /// Record the text fields, preview and scroll position of the visible form.
    pub fn save_current_draft(&mut self, cx: &mut Context<Self>) -> bool {
        let input = self.value(Field::Source, cx);
        let source = self.import.source_preview.clone();
        let scroll = f32::from(self.scrolls[Page::New as usize].offset().y);
        self.work.update(cx, |work, _| work.draft_saved());
        self.update_draft(
            |draft| {
                update_draft_source(draft, input, source);
                draft.scroll = scroll;
            },
            cx,
        )
    }

    pub fn restore_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(draft) = self.workspace(cx).and_then(|w| w.state.draft()).cloned() else {
            return;
        };
        self.import.source_preview = draft.source.clone();
        self.import.issue = draft.source.as_ref().and_then(|source| {
            source
                .subtitle_read_error
                .as_ref()
                .map(|msg| WorkbenchIssue::subtitle(msg.to_string()))
                .or_else(|| {
                    source.subtitle_request.as_ref().map(|_| {
                        WorkbenchIssue::subtitle(course2md::tr!(
                            "所选字幕还未读取完成，请继续读取或选择其他文字来源。"
                        ))
                    })
                })
        });
        self.import.source_candidates.clear();
        self.import.source_collection_title = None;
        self.import.source_collection_name = None;
        self.import.selected_candidates.clear();
        self.import.subtitle_loading = false;
        let local_unread = !draft.online
            && draft.source.is_none()
            && std::path::Path::new(draft.input.trim()).is_file();
        self.inputs[&Field::Source].update(cx, |s, cx| s.set_value(draft.input, window, cx));
        self.scrolls[Page::New as usize].set_offset(point(px(0.), px(draft.scroll)));
        self.work.update(cx, |work, _| work.draft_saved());
        if local_unread {
            self.inspect_source(window, cx);
        }
        cx.notify();
    }

    pub fn switch_source_kind(
        &mut self,
        online: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.online(cx) == online {
            return;
        }
        if !self.save_current_draft(cx) {
            return;
        }
        let switched = self.transact(
            course2md::tr!("无法切换来源：{}"),
            |state| {
                state.switch_source_kind(online);
                Ok(())
            },
            cx,
        );
        if switched.is_some() {
            self.followed_task = None;
            self.invalidate_source();
            self.message = None;
            self.restore_draft(window, cx);
        }
    }

    fn plan_inputs<'a>(&'a self, cx: &'a App) -> PlanInputs<'a> {
        PlanInputs {
            preferences: &self.preferences,
            workspace: self.workspace(cx),
            environment: self.environment.as_ref(),
            import: &self.import,
            library: self.library(cx),
            locations: &self.storage_ui.location_checks,
            preferences_issue: self
                .ordinary_preferences_submit_issue()
                .map(|issue| issue.message),
        }
    }

    fn build_plan(&self, validation: PlanValidation, cx: &App) -> Result<TaskPlan> {
        self.plan_inputs(cx).build_plan(validation)
    }

    fn build_batch_plan(
        &self,
        candidate: &source::SourceCandidate,
        title: String,
        cx: &App,
    ) -> Result<TaskPlan> {
        self.plan_inputs(cx).build_batch_plan(candidate, title)
    }

    /// Queue the selected candidates at once in a single transaction, skipping any
    /// course that already has a `Running` or `Queued` task.
    pub fn enqueue_selected_candidates(&mut self, cx: &mut Context<Self>) {
        let selected: Vec<(usize, source::SourceCandidate)> = self
            .import
            .source_candidates
            .iter()
            .enumerate()
            .filter(|(_, candidate)| self.import.selected_candidates.contains(&candidate.input))
            .map(|(index, candidate)| (index, candidate.clone()))
            .collect();
        if selected.is_empty() {
            return;
        }
        self.message = None;
        let collection = self.import.source_collection_name.clone();
        let mut plans = Vec::new();
        let mut skipped = 0usize;
        for (index, candidate) in selected {
            if course2md::fetch::candidate_identity(&candidate).is_none() {
                skipped += 1;
                continue;
            }
            let title = batch_note_title(&candidate, collection.as_deref(), index);
            match self.build_batch_plan(&candidate, title, cx) {
                Ok(plan) => plans.push(plan),
                Err(error) => {
                    let message =
                        course2md::tr!("选中的视频尚未加入队列：{}", format!("{error:#}"));
                    let needs_settings = run::message_indicates_settings(&message);
                    self.import
                        .set_issue(WorkbenchIssue::plan(message, needs_settings));
                    cx.notify();
                    return;
                }
            }
        }
        if plans.is_empty() {
            let msg = if skipped > 0 {
                course2md::tr!("选中的视频无法识别，请单独打开后转换")
            } else {
                course2md::tr!("选中的视频尚未加入队列，请重试")
            };
            self.import.set_issue(WorkbenchIssue::plan(msg, false));
            cx.notify();
            return;
        }
        let total = plans.len();
        let queued = self.transact(
            course2md::tr!("任务尚未加入队列：{}"),
            |state| {
                let mut first_id = None;
                let mut duplicates = 0usize;
                for plan in plans {
                    let course_id = workspace::course_dir_name(&plan.source_id);
                    if let Some(existing) = state.task_for_course(&plan.library_id, &course_id)
                        && matches!(existing.status, TaskStatus::Running | TaskStatus::Queued)
                    {
                        first_id.get_or_insert_with(|| existing.id.clone());
                        duplicates += 1;
                        continue;
                    }
                    let id = state.enqueue(plan)?;
                    first_id.get_or_insert(id);
                }
                Ok((first_id, duplicates))
            },
            cx,
        );
        if let Some((first_id, duplicates)) = queued {
            self.import.issue = None;
            self.import.selected_candidates.clear();
            let created = total - duplicates;
            let mut parts = Vec::new();
            match created {
                0 => {}
                1 => parts.push(course2md::tr!("已把 1 个视频加入队列").to_owned()),
                created => parts.push(course2md::tr!("已把 {} 个视频加入队列", created)),
            }
            match duplicates {
                0 => {}
                1 => parts.push(course2md::tr!("1 个已有进行中的任务，未重复加入").to_owned()),
                duplicates => parts.push(course2md::tr!(
                    "{} 个已有进行中的任务，未重复加入",
                    duplicates
                )),
            }
            if skipped > 0 {
                parts.push(course2md::tr!("{} 个无法识别，已跳过", skipped));
            }
            if !parts.is_empty() {
                self.message = Some(parts.join(course2md::tr!("；")));
            }
            if let Some(id) = first_id {
                self.select_task(&id, cx);
            }
            self.start_next_task(cx);
        }
        cx.notify();
    }

    pub fn preview_plan(&self, cx: &App) -> Result<TaskPlan> {
        self.build_plan(PlanValidation::Preview, cx)
    }

    pub fn enqueue_current(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.enqueue_current_with_mode(RunMode::Resume, window, cx);
    }

    pub fn enqueue_current_with_mode(
        &mut self,
        mode: RunMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.save_current_draft(cx) {
            return;
        }
        self.message = None;
        let mut plan = match self.build_plan(PlanValidation::Submission, cx) {
            Ok(plan) => plan,
            Err(error) => {
                self.import
                    .set_issue(WorkbenchIssue::from_error(WorkbenchIssueKind::Plan, &error));
                if self.import.source_preview.is_none() && self.online(cx) {
                    let input = self.inputs[&Field::Source].clone();
                    window.on_next_frame(move |window, cx| {
                        input.update(cx, |input, cx| input.focus(window, cx));
                    });
                }
                cx.notify();
                return;
            }
        };
        plan.run_mode = mode;

        // If this exact course is already `Running` or `Queued`, select it without failing.
        let course_id = workspace::course_dir_name(&plan.source_id);
        if let Some(existing_id) = self.workspace(cx).and_then(|w| {
            w.state
                .task_for_course(&plan.library_id, &course_id)
                .filter(|t| matches!(t.status, TaskStatus::Running | TaskStatus::Queued))
                .map(|t| t.id.clone())
        }) {
            self.import.issue = None;
            self.followed_task = (self.page == Page::New).then(|| existing_id.clone());
            self.select_task(&existing_id, cx);
            self.start_next_task(cx);
            cx.notify();
            return;
        }

        if let Some(id) = self.transact(
            course2md::tr!("任务尚未加入队列：{}"),
            |state| state.submit_conversion(plan),
            cx,
        ) {
            self.import.issue = None;
            self.followed_task = (self.page == Page::New).then(|| id.clone());
            self.scrolls[Page::New as usize].set_offset(point(px(0.), px(0.)));
            self.select_task(&id, cx);
            self.start_next_task(cx);
        }
        cx.notify();
    }

    /// Upserts or retries a note's single course task in place (`RunMode::Resume` to
    /// complete unfinished parts, `RunMode::Fresh` to regenerate from scratch).
    pub(crate) fn retry_or_regenerate_course(
        &mut self,
        course: Course,
        mode: RunMode,
        cx: &mut Context<Self>,
    ) {
        let Some(location) = self.course_location(&course, cx).cloned() else {
            let msg = course2md::tr!("保存位置暂时不可用").to_owned();
            self.route_note_action_error(msg, cx);
            return;
        };
        let folder = self.course_folder(&course, cx);

        // Fast path: the course's task record is still in `workspace.state.tasks`.
        let existing_id = self.workspace(cx).and_then(|w| {
            let manifest = course.manifest.as_ref()?;
            w.state
                .task_for_course(&location.id, &manifest.course_id)
                .or_else(|| w.state.task_for_source(&location.id, &manifest.source_id))
                .map(|t| (t.id.clone(), t.status))
        });
        if let Some((id, status)) = existing_id {
            if matches!(status, TaskStatus::Running | TaskStatus::Queued) {
                self.select_task(&id, cx);
                if status == TaskStatus::Queued {
                    self.start_next_task(cx);
                }
                return;
            }
            let note_dir = course.dir.clone();
            let outcomes = course.manifest.as_ref().map(|m| m.outcomes.clone());
            if self
                .transact(
                    course2md::tr!("任务状态尚未保存：{}"),
                    |state| {
                        state.retry_task(&id, mode)?;
                        if let Some(task) = state.task_mut(&id) {
                            task.note_dir = Some(note_dir);
                            if mode == RunMode::Resume && task.outcomes.is_none() {
                                task.outcomes = outcomes;
                            }
                        }
                        Ok(())
                    },
                    cx,
                )
                .is_some()
            {
                self.select_task(&id, cx);
                self.start_next_task(cx);
            }
            cx.notify();
            return;
        }

        // Slow path: the completed task record was previously cleared (or the note came
        // from CLI). Reconstruct `NewTask` from the note's `manifest.json` + `meta.json`.
        let reader_sources = self
            .workspace(cx)
            .map(|w| w.state.reader_sources.clone())
            .unwrap_or_default();
        let locations = self
            .workspace(cx)
            .map(|w| {
                w.state
                    .libraries
                    .iter()
                    .map(|lib| (lib.root.clone(), lib.previous_roots.clone()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let work = crate::spawn_blocking_result(
            move || -> Result<(TaskPlan, course2md::artifact::Manifest)> {
                let manifest = match course.manifest.clone() {
                    Some(manifest) => manifest,
                    None => course2md::artifact::read_manifest(&course.dir.join("manifest.json"))?,
                };
                let meta: course2md::fetch::VideoMeta = serde_json::from_slice(
                    &std::fs::read(course.dir.join("meta.json"))
                        .context(course2md::tr!("缺少原视频信息文件 meta.json"))?,
                )?;
                let online = !meta.is_local() && !manifest.source_id.starts_with("local:");
                let source_key = if manifest.source_id.is_empty() {
                    manifest.course_id.clone()
                } else {
                    manifest.source_id.clone()
                };
                let input = if online {
                    meta.webpage_url.clone()
                } else if let Some(relocated) = reader_sources.get(&source_key) {
                    relocated.to_string_lossy().into_owned()
                } else {
                    let candidate = PathBuf::from(&meta.webpage_url);
                    desktop_reader_core::reader_navigation::relocated_source(&candidate, &locations)
                        .to_string_lossy()
                        .into_owned()
                };
                anyhow::ensure!(
                    !input.trim().is_empty(),
                    course2md::tr!("缺少原视频链接或文件路径，请从主页重新导入")
                );
                if !online {
                    anyhow::ensure!(
                        std::path::Path::new(&input).is_file(),
                        course2md::tr!("原视频文件不存在：{}", input)
                    );
                }
                let source_id = if manifest.source_id.trim().is_empty() {
                    manifest.course_id.clone()
                } else {
                    manifest.source_id.clone()
                };
                let source = workspace::Source {
                    input,
                    title: meta.title.clone(),
                    author: meta.uploader.clone(),
                    duration: meta.duration,
                    identity: source_id.clone(),
                    online,
                    ..Default::default()
                };
                let mut plan = TaskPlan::from_source(location.id, source, course.title, mode);
                plan.source_id = source_id;
                plan.target_folder = folder;
                plan.note_dir = Some(course.dir);
                Ok((plan, manifest))
            },
        );
        cx.spawn(async move |this, cx| {
            let built = work.await;
            let _ = this.update(cx, |this, cx| {
                match built {
                    Ok((plan, manifest)) => {
                        let course_id = manifest.course_id.clone();
                        let outcomes = manifest.outcomes;
                        if let Some(id) = this.transact(
                            course2md::tr!("任务尚未加入队列：{}"),
                            |state| {
                                let id = state.submit_conversion(plan)?;
                                if let Some(task) = state.task_mut(&id) {
                                    if !course_id.trim().is_empty() {
                                        task.course_id = course_id;
                                    }
                                    if mode == RunMode::Resume {
                                        task.outcomes = Some(outcomes);
                                    }
                                }
                                Ok(id)
                            },
                            cx,
                        ) {
                            this.select_task(&id, cx);
                            this.start_next_task(cx);
                        }
                    }
                    Err(error) => {
                        this.route_note_action_error(format!("{error:#}"), cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn route_note_action_error(&mut self, message: String, cx: &mut Context<Self>) {
        match self.page {
            Page::Result => {
                self.reader
                    .change(cx, |reader, cx| reader.set_inline_notice(Some(message), cx));
            }
            Page::Library => {
                self.library_ui.notice = Some(message);
                cx.notify();
            }
            Page::Task => {
                self.set_task_page_notice(Some(message), cx);
            }
            _ => {
                let needs_settings = run::message_indicates_settings(&message);
                self.import
                    .set_issue(WorkbenchIssue::plan(message, needs_settings));
                cx.notify();
            }
        }
    }

    pub fn start_next_task(&mut self, cx: &mut Context<Self>) {
        if self.storage_ui.busy {
            return;
        }
        if !self
            .detection_wait
            .may_start(self.environment.is_some(), Instant::now())
        {
            return;
        }
        let Some(task) = self
            .workspace(cx)
            .and_then(|w| w.state.next_task())
            .cloned()
        else {
            return;
        };
        if !self.work.update(cx, |work, _| work.claim_start(&task.id)) {
            return;
        }
        let fallback = self.recommended_local_provider();
        let needed = match run::needed_credentials(&self.preferences, &task, fallback) {
            Ok(needed) => needed,
            Err(error) => return self.fail_task_before_start(&task.id, error, cx),
        };
        if needed.is_empty() {
            self.finish_task_start(&task.id, HashMap::new(), cx);
            return;
        }
        let vault = self.preferences.vault();
        let task_id = task.id.clone();
        let secrets_rx = crate::spawn_blocking_io(move || -> Result<HashMap<String, String>> {
            let mut secrets = HashMap::new();
            for reference in needed {
                let secret = vault.resolve(&reference).map(|s| s.expose().to_owned())?;
                secrets.insert(reference, secret);
            }
            Ok(secrets)
        });
        cx.spawn(async move |this, cx| {
            let secrets = secrets_rx
                .await
                .unwrap_or_else(|| Err(anyhow::anyhow!(course2md::tr!("凭据读取意外中断"))));
            let _ = this.update(cx, |this, cx| {
                match secrets {
                    Ok(secrets) => this.finish_task_start(&task_id, secrets, cx),
                    Err(error) => this.fail_task_before_start(&task_id, error, cx),
                }
                this.start_next_task(cx);
            });
        })
        .detach();
    }

    fn finish_task_start(
        &mut self,
        task_id: &str,
        secrets: HashMap<String, String>,
        cx: &mut Context<Self>,
    ) {
        let Some(task) = self
            .workspace(cx)
            .filter(|workspace| {
                self.work.read(cx).is_starting(task_id)
                    && run::still_runnable(&workspace.state, task_id)
            })
            .and_then(|workspace| workspace.state.task(task_id))
            .cloned()
        else {
            self.work.update(cx, |work, _| work.end_start(task_id));
            return;
        };
        let start = match run::start_request(
            &self.preferences,
            self.workspace(cx),
            &task,
            &secrets,
            self.recommended_local_provider(),
        ) {
            Ok(start) => start,
            Err(error) => return self.fail_task_before_start(&task.id, error, cx),
        };
        let read_task = task.clone();
        let prepared = crate::spawn_blocking_result(move || start.prepare(&read_task));
        cx.spawn(async move |this, cx| {
            let prepared = prepared.await;
            let _ = this.update(cx, |this, cx| match prepared {
                Ok(prepared) => this.start_task_request(task, prepared, cx),
                Err(error) => {
                    this.fail_task_before_start(&task.id, error, cx);
                    this.start_next_task(cx);
                }
            });
        })
        .detach();
        cx.notify();
    }

    fn start_task_request(
        &mut self,
        task: TaskRecord,
        prepared: run::PreparedStart,
        cx: &mut Context<Self>,
    ) {
        let current = self
            .workspace(cx)
            .is_some_and(|workspace| run::still_runnable(&workspace.state, &task.id));
        let work = self.work.read(cx);
        if !work.is_starting(&task.id) || work.job.is_some() || work.closing || !current {
            self.work.update(cx, |work, _| work.end_start(&task.id));
            return;
        }
        let run::PreparedStart {
            request,
            keep_video,
            adopted_legacy_work_dir,
        } = prepared;
        let running = self
            .with_workspace(cx, |workspace| {
                workspace.transaction(|state| {
                    run::mark_running(state, &task.id, adopted_legacy_work_dir)
                })
            })
            .context(course2md::tr!("任务记录不可用"))
            .and_then(|running| running);
        if let Err(error) = running {
            return self.fail_task_before_start(&task.id, error, cx);
        }
        self.active_keep_video = keep_video;
        let wake = self.work.read(cx).waker();
        let started = crate::spawn_blocking_result(move || Job::start_task(&request, wake));
        cx.spawn(async move |this, cx| {
            let job = started.await;
            let _ = this.update(cx, |this, cx| match job {
                Ok(job) => {
                    let status = course2md::tr!("正在生成《{}》", task.title);
                    this.work.change(cx, |work, _| {
                        work.install(&task.id, job, status, Instant::now())
                    });
                    cx.notify();
                }
                Err(error) => {
                    this.fail_task_before_start(&task.id, error, cx);
                    this.start_next_task(cx);
                }
            });
        })
        .detach();
    }

    fn fail_task_before_start(
        &mut self,
        task_id: &str,
        error: anyhow::Error,
        cx: &mut Context<Self>,
    ) {
        self.work.update(cx, |work, _| work.end_start(task_id));
        if let Some(Err(error)) = self.with_workspace(cx, |workspace| {
            run::fail_before_start(workspace, task_id, &error)
        }) {
            self.report(course2md::tr!("任务状态尚未保存：{}"), error);
        }
        cx.notify();
    }

    pub fn finish_task(
        &mut self,
        id: &str,
        success: bool,
        cancelled: bool,
        cx: &mut Context<Self>,
    ) {
        let keep_video = std::mem::replace(&mut self.active_keep_video, false);
        let done = self
            .with_workspace(cx, |workspace| {
                workspace
                    .state
                    .task_mut(id)
                    .and_then(|task| task.events.done.take())
            })
            .flatten();
        let manifest = done.as_ref().and_then(|d| {
            course2md::artifact::read_manifest(&d.out_dir.join("manifest.json")).ok()
        });
        let page = self.page;
        let preferences = &self.preferences;
        let fallback = self.recommended_local_provider();
        let Some(result) = self.with_workspace(cx, |workspace| {
            let visible =
                page == Page::Task && workspace.state.selected_task.as_deref() == Some(id);
            workspace.transaction(|state| {
                run::record_finish(
                    state,
                    preferences,
                    fallback,
                    id,
                    done.as_ref(),
                    manifest.as_ref(),
                    keep_video,
                    visible,
                    success,
                    cancelled,
                )
            })
        }) else {
            return;
        };
        match result {
            Ok((task, cleanup)) => {
                if let Some(cleared) = cleanup {
                    drop(crate::spawn_blocking_io(move || {
                        workspace::clean_cleared_task_work(&cleared);
                    }));
                }
                let open_followed = self.page == Page::New
                    && !self.reader.read(cx).reader_viewer_open()
                    && self.followed_task.as_deref() == Some(id);
                if open_followed {
                    self.followed_task = None;
                }
                if let Some(done) = done {
                    self.work
                        .update(cx, |work, _| work.completed = Some(done.clone()));
                    let root = self.task_library(&task, cx).map(|l| l.root.clone());
                    let publication = run::Publication::new(&task, root, &done, manifest.as_ref());
                    let published = crate::spawn_blocking_io(move || publication.file());
                    cx.spawn(async move |this, cx| {
                        let Some((issues, thumbnail)) = published.await else {
                            return;
                        };
                        let _ = this.update(cx, |this, cx| {
                            if !issues.is_empty() {
                                let notice = course2md::tr!(
                                    "笔记已保存，{}",
                                    issues.join(course2md::tr!("；"))
                                );
                                this.library_ui.notice = Some(notice.clone());
                                if this.page == Page::Task {
                                    this.set_task_page_notice(Some(notice), cx);
                                }
                            }
                            if let Some(manifest) = &manifest {
                                this.records.change(cx, |records, _| {
                                    update_published_course(
                                        &mut records.library,
                                        &done,
                                        manifest,
                                        thumbnail,
                                    )
                                });
                            }
                            this.refresh_library(cx);
                            if open_followed {
                                this.open_course(Course::from_completed(&done), cx);
                            } else {
                                this.reload_replaced_note(&done.out_dir, cx);
                            }
                            cx.notify();
                        });
                    })
                    .detach();
                }
                let status_label = task_status_label(&task);
                self.work.change(cx, |work, _| {
                    work.task_status = status_label;
                });
            }
            Err(error) => self.report(
                course2md::tr!("完成状态尚未保存：{}。已发布的笔记仍保留。"),
                error,
            ),
        }
        cx.notify();
    }

    /// A note open in the reader that a task has just updated is read again in place.
    fn reload_replaced_note(&mut self, note: &std::path::Path, cx: &mut Context<Self>) {
        let shown = self
            .opened(cx)
            .preview
            .as_ref()
            .is_some_and(|preview| preview.course.dir == note);
        if !shown {
            return;
        }
        let course = self.records.read(cx).note_at(note).map_or_else(
            || {
                let mut course = self.opened(cx).preview.as_ref().unwrap().course.clone();
                course.manifest = None;
                course
            },
            |course| (*course).clone(),
        );
        self.reader.change(cx, |reader, cx| {
            reader.reload_note(course, crate::reader_ui::Reload::Replaced, cx)
        });
    }

    pub fn select_task(&mut self, id: &str, cx: &mut Context<Self>) {
        let previous = self
            .workspace(cx)
            .and_then(|workspace| workspace.state.selected_task.clone());
        if previous.as_deref() != Some(id)
            && let Some(previous) = previous
        {
            self.tasks
                .update(cx, |page, _| page.close_task_details(&previous));
        }
        self.transact(
            course2md::tr!("任务选择尚未保存：{}"),
            |state| {
                state.selected_task = Some(id.to_owned());
                if let Some(task) = state.task_mut(id) {
                    task.unread = false;
                }
                Ok(())
            },
            cx,
        );
        let waiting = self
            .task(id, cx)
            .is_some_and(|task| matches!(task_group(task), TaskGroup::Running | TaskGroup::Queued));
        let expanded = (!waiting).then(|| id.to_owned());
        self.tasks.update(cx, |page, cx| {
            page.task_list.expanded = expanded;
            cx.notify();
        });
        cx.notify();
    }

    pub(crate) fn open_task_export_location(&mut self, id: String, cx: &mut Context<Self>) {
        let destination = self
            .workspace(cx)
            .context(course2md::tr!("任务记录暂时不可用"))
            .and_then(|workspace| run::task_export_destination(&workspace.state, &id));
        match destination {
            Ok(path) => {
                self.set_task_page_notice(None, cx);
                cx.reveal_path(&path);
            }
            Err(error) => {
                let msg = course2md::tr!("无法打开导出位置：{}", format!("{error:#}"));
                self.set_task_page_notice(Some(msg), cx);
            }
        }
        cx.notify();
    }

    fn write_worker_control(work_dir: PathBuf, intent: Intent) {
        drop(crate::spawn_blocking_io(move || {
            let _ = run::write_control(&work_dir, intent);
        }));
    }

    pub(crate) fn pause_task_by_id(&mut self, id: &str, cx: &mut Context<Self>) {
        let work_dir = self
            .task(id, cx)
            .map(|task| task.effective_work_dir().to_path_buf());
        let target = id.to_owned();
        if let Some(was_running) = self.transact(
            course2md::tr!("任务状态尚未保存：{}"),
            |state| state.pause_task(&target),
            cx,
        ) {
            if (was_running || self.is_running_task(id, cx))
                && let Some(dir) = work_dir
            {
                Self::write_worker_control(dir, Intent::Pause);
            }
            self.start_next_task(cx);
        }
        cx.notify();
    }

    pub(crate) fn resume_task_by_id(&mut self, id: &str, cx: &mut Context<Self>) {
        let target = id.to_owned();
        if self
            .transact(
                course2md::tr!("任务状态尚未保存：{}"),
                |state| state.resume_task(&target),
                cx,
            )
            .is_some()
        {
            self.start_next_task(cx);
        }
        cx.notify();
    }

    pub(crate) fn retry_task_in_place(&mut self, id: &str, mode: RunMode, cx: &mut Context<Self>) {
        let target = id.to_owned();
        if self
            .transact(
                course2md::tr!("任务状态尚未保存：{}"),
                |state| state.retry_task(&target, mode),
                cx,
            )
            .is_some()
        {
            self.select_task(id, cx);
            self.start_next_task(cx);
        }
        cx.notify();
    }

    pub(crate) fn pause_all_tasks(&mut self, cx: &mut Context<Self>) {
        let work_dirs: HashMap<String, PathBuf> = self
            .workspace(cx)
            .map(|w| {
                w.state
                    .tasks
                    .iter()
                    .map(|t| (t.id.clone(), t.effective_work_dir().to_path_buf()))
                    .collect()
            })
            .unwrap_or_default();
        if let Some(paused) = self.transact(
            course2md::tr!("任务状态尚未保存：{}"),
            |state| Ok(state.pause_all()),
            cx,
        ) {
            for (id, was_running) in paused {
                if (was_running || self.is_running_task(&id, cx))
                    && let Some(dir) = work_dirs.get(&id).cloned()
                {
                    Self::write_worker_control(dir, Intent::Pause);
                }
            }
        }
        cx.notify();
    }

    pub(crate) fn resume_all_tasks(&mut self, cx: &mut Context<Self>) {
        if let Some(resumed) = self.transact(
            course2md::tr!("任务状态尚未保存：{}"),
            |state| Ok(state.resume_all()),
            cx,
        ) && !resumed.is_empty()
        {
            self.start_next_task(cx);
        }
        cx.notify();
    }

    pub(crate) fn move_waiting_task(&mut self, id: &str, position: usize, cx: &mut Context<Self>) {
        self.transact(
            course2md::tr!("任务顺序尚未保存：{}"),
            |state| state.move_queued_task(id, position),
            cx,
        );
        cx.notify();
    }

    pub(crate) fn cancel_or_remove_task(&mut self, id: &str, cx: &mut Context<Self>) {
        let active = self.is_running_task(id, cx);
        let target = id.to_owned();
        if let Some((cleared, was_running)) = self.transact(
            course2md::tr!("任务记录尚未保存：{}"),
            |state| state.cancel_or_remove_task(&target),
            cx,
        ) {
            if self.followed_task.as_deref() == Some(id) {
                self.followed_task = None;
            }
            if was_running || active {
                Self::write_worker_control(cleared.work_dir.clone(), Intent::Cancel);
                if let Some(job) = &self.work.read(cx).job
                    && self.work.read(cx).active_task.as_deref() == Some(id)
                {
                    job.cancel();
                }
            }
            self.tasks.update(cx, |page, _| {
                page.forget_tasks(std::slice::from_ref(&target))
            });
            drop(crate::spawn_blocking_io(move || {
                workspace::clean_cleared_task_work(&cleared);
            }));
            self.start_next_task(cx);
        }
        cx.notify();
    }

    pub(crate) fn clear_completed_tasks(&mut self, cx: &mut Context<Self>) {
        let completed_ids: Vec<String> = self
            .workspace(cx)
            .map(|w| {
                w.state
                    .tasks
                    .iter()
                    .filter(|t| t.status == TaskStatus::Completed)
                    .map(|t| t.id.clone())
                    .collect()
            })
            .unwrap_or_default();
        if let Some(cleared) = self.transact(
            course2md::tr!("任务记录尚未保存：{}"),
            |state| Ok(state.clear_completed_tasks()),
            cx,
        ) && !cleared.is_empty()
        {
            self.tasks
                .update(cx, |page, _| page.forget_tasks(&completed_ids));
            drop(crate::spawn_blocking_io(move || {
                for item in &cleared {
                    workspace::clean_cleared_task_work(item);
                }
            }));
        }
        cx.notify();
    }

    fn open_task_note(&mut self, task: &TaskRecord, cx: &mut Context<Self>) {
        let Some(path) = &task.note_dir else {
            return;
        };
        let course = self
            .library(cx)
            .courses
            .iter()
            .find(|course| &course.dir == path)
            .cloned()
            .unwrap_or_else(|| {
                Rc::new(Course::from_completed(&Done {
                    out_dir: path.clone(),
                    title: task.title.clone(),
                    ..Default::default()
                }))
            });
        self.open_course(course, cx);
    }

    /// Runs one of a task's actions, from its card, its menu or Home.
    pub(crate) fn run_task_action(
        &mut self,
        action: &TaskAction,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(task) = self.task(id, cx).cloned() else {
            return;
        };
        let id = id.to_owned();
        match action {
            TaskAction::OpenNote => self.open_task_note(&task, cx),
            TaskAction::OpenNoteFolder => {
                if let Some(path) = &task.note_dir {
                    cx.open_with_system(path);
                }
            }
            TaskAction::OpenExports => self.open_task_export_location(id, cx),
            TaskAction::OpenSource => cx.open_url(&task.source.input),
            TaskAction::CopyLink => {
                crate::clipboard::write(ClipboardItem::new_string(task.source.input.clone()), cx);
            }
            TaskAction::Details => self
                .tasks
                .update(cx, |page, cx| page.toggle_task_row(&id, cx)),
            TaskAction::Log => self
                .tasks
                .update(cx, |page, cx| page.toggle_task_log(&task, cx)),
            TaskAction::Move { to, .. } => {
                if let Some(to) = to {
                    self.move_waiting_task(&id, *to, cx);
                }
            }
            TaskAction::Pause => self.pause_task_by_id(&id, cx),
            TaskAction::Resume => self.resume_task_by_id(&id, cx),
            TaskAction::Retry => self.retry_task_in_place(&id, RunMode::Resume, cx),
            TaskAction::Regenerate => self.retry_task_in_place(&id, RunMode::Fresh, cx),
            TaskAction::PrepareModel => self.prepare_task_model(&task, cx),
            TaskAction::OpenSettings => {
                let reason = task
                    .error
                    .as_ref()
                    .map(|err| err.message.as_str())
                    .unwrap_or_default();
                self.open_settings_for_message(reason, window, cx);
            }
            TaskAction::Reassociate(library) => {
                self.begin_library_reassociation(library.clone(), window, cx)
            }
            TaskAction::Cancel | TaskAction::Remove => self.cancel_or_remove_task(&id, cx),
            TaskAction::ViewTask => {
                self.select_task(&id, cx);
                self.navigate(Page::Task, cx);
            }
        }
    }

    pub(crate) fn tasks_event(
        &mut self,
        _: &Entity<task_ui::TasksPage>,
        event: &task_ui::TasksEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use task_ui::TasksEvent;
        match event {
            TasksEvent::Run(id, action) => self.run_task_action(action, id, window, cx),
            TasksEvent::Select { id, expand } => {
                self.select_task(id, cx);
                if *expand {
                    let id = id.clone();
                    self.tasks.update(cx, |page, cx| {
                        page.task_list.expanded = Some(id);
                        cx.notify();
                    });
                }
            }
            TasksEvent::Move { id, position } => self.move_waiting_task(id, *position, cx),
            TasksEvent::PauseAll => self.pause_all_tasks(cx),
            TasksEvent::ResumeAll => self.resume_all_tasks(cx),
            TasksEvent::ClearCompleted => self.clear_completed_tasks(cx),
            TasksEvent::Navigate(page) => self.navigate(*page, cx),
            TasksEvent::Import => {
                if self.prepare_workbench_input(window, cx) {
                    self.navigate(Page::New, cx);
                }
            }
        }
        cx.notify();
    }

    pub(crate) fn shell_facts(&self, cx: &App) -> task_ui::ShellFacts {
        let records = self.records.read(cx);
        let reassociate = records
            .workspace
            .iter()
            .flat_map(|workspace| &workspace.state.libraries)
            .filter(|library| {
                self.cached_location_check(library)
                    .is_some_and(|check| check.needs_reassociation)
            })
            .map(|library| library.id.clone())
            .collect();
        let state = &self.settings_ui.model_diagnostics;
        let provider = self.recommended_local_provider();
        let models = records
            .workspace
            .iter()
            .flat_map(|workspace| &workspace.state.tasks)
            .filter(|task| {
                task.status == TaskStatus::Failed
                    && task
                        .error
                        .as_ref()
                        .is_some_and(|err| err.missing_model.is_some())
            })
            .filter_map(|task| {
                let key =
                    desktop_tasks::task_view::task_model_request(&self.preferences, task, provider)
                        .key();
                let preparation = if state.preparing_for_tasks
                    && state
                        .preparing
                        .as_ref()
                        .is_some_and(|request| request.key() == key)
                {
                    task_ui::ModelPreparation::Preparing
                } else {
                    let outcome = state
                        .outcome
                        .as_ref()
                        .filter(|outcome| outcome.error && outcome.key == key)?;
                    task_ui::ModelPreparation::Failed(outcome.message.clone())
                };
                Some((task.id.clone(), preparation))
            })
            .collect();
        task_ui::ShellFacts {
            reassociate,
            storage_busy: self.storage_ui.busy,
            models,
        }
    }
}
