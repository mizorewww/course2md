//! Task execution: draft persistence, plan construction, dispatch to the
//! engine process and recording its events. The task page lives in task_ui.
use super::*;
use crate::{
    notes::default_output,
    workspace::{Intent, TaskPlan, TaskRecord, TaskState},
};
use anyhow::{Context as _, Result, ensure};
use course2md::execution::Component;
use desktop_tasks::plan::*;
use desktop_tasks::run;
use desktop_tasks::task_view::{TaskGroup, task_group};
use std::collections::HashMap;

/// Options shown before the workspace has restored a draft.
static NO_DRAFT_OPTIONS: std::sync::LazyLock<ConversionOptions> =
    std::sync::LazyLock::new(ConversionOptions::default);

impl Desktop {
    /// The current input form. It is the only copy of its choices: views read
    /// it and option controls write through `update_draft`.
    pub(crate) fn draft<'a>(&self, cx: &'a App) -> Option<&'a workspace::Draft> {
        self.records.read(cx).draft()
    }

    /// Whether the current input is an online link (the default before a
    /// workspace is restored) rather than a local file.
    pub(crate) fn online(&self, cx: &App) -> bool {
        self.draft(cx).is_none_or(|draft| draft.online)
    }

    pub(crate) fn task_options<'a>(&self, cx: &'a App) -> &'a ConversionOptions {
        self.draft(cx)
            .map(|draft| &draft.options)
            .unwrap_or(&NO_DRAFT_OPTIONS)
    }

    pub(crate) fn target_folder(&self, cx: &App) -> Option<u64> {
        self.draft(cx).and_then(|draft| draft.folder)
    }

    /// One saved change to the workspace. A failure shows in the shell as `failure`,
    /// whose `{}` takes the cause; the caller only says what to do on success.
    pub(crate) fn transact<T>(
        &mut self,
        failure: &str,
        edit: impl FnOnce(&mut workspace::State) -> Result<T>,
        cx: &mut App,
    ) -> Option<T> {
        let result = self.records.update(cx, |records, _| {
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

    /// Shows a failure in the shell; `failure`'s `{}` takes the cause.
    fn report(&mut self, failure: &str, error: anyhow::Error) {
        self.workspace_error = Some(failure.replacen("{}", &format!("{error:#}"), 1));
    }

    /// The task with this id, if the record is readable.
    pub(crate) fn task<'a>(&self, id: &str, cx: &'a App) -> Option<&'a TaskRecord> {
        self.records.read(cx).task(id)
    }

    /// Change the current draft and record it. A failed write is reported
    /// beside the workspace and returns false.
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

    /// Change this task's processing options; changed choices become overrides.
    pub(crate) fn update_task_options(
        &mut self,
        edit: impl FnOnce(&mut ConversionOptions),
        cx: &mut Context<Self>,
    ) -> bool {
        self.update_draft(
            |draft| {
                let mut options = draft.options.clone();
                edit(&mut options);
                apply_draft_options(draft, options);
            },
            cx,
        )
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
            .update(cx, |records, _| records.workspace = Some(workspace));
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
            let options = ConversionOptions::from_config(&self.preferences.defaults_config());
            match workspace::Workspace::open(self.workspace_root(), options) {
                Ok(workspace) => self.install_workspace(workspace, window, cx),
                Err(error) => self.report(course2md::tr!("记录仍无法读取，原件已保留：{}"), error),
            }
        }
        if self.preference_defaults_pending {
            self.refresh_preference_defaults(cx);
        }
        if self.workspace_error.is_none() {
            self.advance_conversion_when_ready(cx);
        }
        cx.notify();
    }

    /// Called only by the explicit global recovery action; rebuilding never starts the queue.
    pub fn rebuild_workspace_records(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.work.job.is_some() || self.storage_ui.busy {
            self.workspace_error = Some(
                course2md::tr!("当前还有处理或文件移动在进行，请等候停止后再重建记录。").into(),
            );
            cx.notify();
            return;
        }
        let root = self.workspace_root();
        let options = ConversionOptions::from_config(&self.preferences.defaults_config());
        let result = if let Some(workspace) = &self.records.read(cx).workspace {
            workspace::Workspace::rebuild_at(workspace.storage_path().to_owned(), root, options)
        } else {
            workspace::Workspace::rebuild(root, options)
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

    /// Whether this task holds the running background job (its record may lag the event stream).
    pub(crate) fn is_running_task(&self, id: &str) -> bool {
        self.work.active_task.as_deref() == Some(id) && self.work.job.is_some()
    }

    /// The library location a task's plan saves to; the record may name a removed library.
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
        // A source edit must establish the next form's defaults before autosave
        // can detach the submitted task. Failed preparation cannot fall through
        // to saving the previous video's title and overrides against a new URL.
        if self
            .workspace(cx)
            .and_then(|workspace| workspace.state.draft())
            .is_some_and(|draft| draft.submitted_task.is_some() && draft.input != input)
        {
            return false;
        }
        let title = self.value(Field::Title, cx);
        let source = self.import.source_preview.clone();
        let scroll = f32::from(self.scrolls[Page::New as usize].offset().y);
        self.draft_deadline = None;
        self.update_draft(
            |draft| {
                update_draft_source_title(draft, input, title, source);
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
        self.import.preview_error = None;
        self.import.source_validation = None;
        self.import.title_required = false;
        self.import.source_candidates.clear();
        self.import.source_collection_title = None;
        self.import.source_collection_name = None;
        self.import.selected_candidates.clear();
        self.import.subtitle_error = draft.source.as_ref().and_then(|source| {
            source
                .subtitle_read_error
                .as_ref()
                .map(ToString::to_string)
                .or_else(|| {
                    source.subtitle_request.as_ref().map(|_| {
                        course2md::tr!("所选字幕还未读取完成，请继续读取或选择其他文字来源。")
                            .into()
                    })
                })
        });
        self.import.subtitle_loading = false;
        self.inputs[&Field::Source].update(cx, |s, cx| s.set_value(draft.input, window, cx));
        self.inputs[&Field::Title].update(cx, |s, cx| s.set_value(draft.title, window, cx));
        self.scrolls[Page::New as usize].set_offset(point(px(0.), px(draft.scroll)));
        self.draft_deadline = None;
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
        let defaults = ConversionOptions::from_config(&self.preferences.defaults_config());
        let switched = self.transact(
            course2md::tr!("无法切换来源：{}"),
            |state| {
                state.switch_source_kind(online, defaults);
                Ok(())
            },
            cx,
        );
        if switched.is_some() {
            self.import.following_conversion = None;
            self.invalidate_source();
            self.message = None;
            self.restore_draft(window, cx);
        }
    }

    /// What a plan is built from, as of now.
    fn plan_inputs<'a>(&'a self, cx: &'a App) -> PlanInputs<'a> {
        PlanInputs {
            preferences: &self.preferences,
            workspace: self.workspace(cx),
            environment: self.environment.as_ref(),
            import: &self.import,
            library: self.library(cx),
            locations: &self.storage_ui.location_checks,
            defaults_pending: self.preference_defaults_pending,
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

    /// Queue the selected candidates at once, reusing per-candidate deduplication (an identical task in
    /// progress is not created again), in one transaction; then select the first task and start the queue.
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
                    self.message = Some(course2md::tr!(
                        "选中的视频尚未加入队列：{}",
                        format!("{error:#}")
                    ));
                    cx.notify();
                    return;
                }
            }
        }
        if plans.is_empty() {
            self.message = Some(if skipped > 0 {
                course2md::tr!("选中的视频无法识别，请单独打开后转换").into()
            } else {
                course2md::tr!("选中的视频尚未加入队列，请重试").into()
            });
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
                    let (id, created) = state.enqueue(plan, None)?;
                    first_id.get_or_insert_with(|| id.clone());
                    if !created {
                        duplicates += 1;
                    }
                }
                Ok((first_id, duplicates))
            },
            cx,
        );
        if let Some((first_id, duplicates)) = queued {
            self.import.selected_candidates.clear();
            let created = total - duplicates;
            let mut parts = Vec::new();
            if created > 0 {
                parts.push(course2md::tr!("已把 {} 个视频加入队列", created));
            }
            if duplicates > 0 {
                parts.push(course2md::tr!(
                    "{} 个与进行中的任务相同，未重复加入",
                    duplicates
                ));
            }
            if skipped > 0 {
                parts.push(course2md::tr!("{} 个无法识别，已跳过", skipped));
            }
            self.message = Some(parts.join(course2md::tr!("；")));
            if let Some(id) = first_id {
                self.select_task(&id, cx);
            }
            self.start_next_task(cx);
        }
        cx.notify();
    }

    /// The plan the form would submit, checked against loaded snapshots.
    /// Submission rechecks live storage, local source and dispatch markers
    /// before a task can enter the queue.
    pub fn preview_plan(&self, cx: &App) -> Result<TaskPlan> {
        self.build_plan(PlanValidation::Preview, cx)
    }

    /// An unfinished task already doing the same work as `plan`; the task the
    /// current input retries is not one.
    pub fn matching_task<'a>(&self, plan: &TaskPlan, cx: &'a App) -> Option<&'a TaskRecord> {
        let state = &self.workspace(cx)?.state;
        let parent = state.draft().and_then(|draft| draft.retry_of.as_deref());
        state.duplicate_of(plan, parent)
    }

    pub fn enqueue_current(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.save_current_draft(cx) {
            return;
        }
        self.message = None;
        let plan = match self.build_plan(PlanValidation::Submission, cx) {
            Ok(plan) => plan,
            Err(error) => {
                self.import.title_required = error.downcast_ref::<TitleRequired>().is_some();
                self.import.source_validation = Some(format!("{error:#}"));
                self.import.validation_attempt = self.import.validation_attempt.wrapping_add(1);
                let subtitle_attention = self.subtitle_attention_required(cx);
                let field = if self.import.source_preview.is_some()
                    && !subtitle_attention
                    && self.value(Field::Title, cx).trim().is_empty()
                {
                    Some(Field::Title)
                } else if self.import.source_preview.is_none() && self.online(cx) {
                    Some(Field::Source)
                } else {
                    None
                };
                if let Some(field) = field {
                    if field == Field::Title {
                        // The name field is in the processing options; show
                        // them for this fix without changing the saved choice.
                        self.desktop_settings.home_options = true;
                        self.import.pending_conversion = None;
                        self.import.following_conversion = None;
                    }
                    let input = self.inputs[&field].clone();
                    window.on_next_frame(move |window, cx| {
                        input.update(cx, |input, cx| input.focus(window, cx));
                    });
                }
                cx.notify();
                return;
            }
        };
        // Starting again acknowledges only the terminal feedback belonging to
        // the current foreground input, before the new task replaces its link.
        let acknowledged_input = (self.page == Page::New)
            .then(|| self.current_input_task(cx))
            .flatten()
            .filter(|task| task.state.finished())
            .map(|task| task.id.clone());
        let parent = self.draft(cx).and_then(|draft| draft.retry_of.clone());
        if let Some((id, created)) = self.transact(
            course2md::tr!("任务尚未加入队列：{}"),
            |state| {
                let (id, created) = state.enqueue(plan, parent)?;
                if let Some(previous) = acknowledged_input
                    .as_deref()
                    .and_then(|id| state.task_mut(id))
                {
                    previous.unread = false;
                }
                if created && let Some(draft) = state.draft_mut() {
                    draft.submitted_task = Some(id.clone());
                }
                Ok((id, created))
            },
            cx,
        ) {
            self.import.pending_conversion = None;
            self.import.source_validation = None;
            self.import.title_required = false;
            self.message = (!created).then(|| course2md::tr!("已有相同任务，未重复加入").into());
            if created {
                self.import.following_conversion = self
                    .import
                    .following_conversion
                    .take()
                    .filter(|_| self.page == Page::New)
                    .and_then(|follow| {
                        follow.submitted(self.import.preview_generation, id.clone())
                    });
                self.import.source_editor_open = false;
                self.scrolls[Page::New as usize].set_offset(point(px(0.), px(0.)));
            }
            self.select_task(&id, cx);
            // Preparation may finish after navigation; keep the user's current page.
            self.start_next_task(cx);
        }
        cx.notify();
    }

    /// Sends a task its current instruction. A failure comes back through
    /// `take_write_error`, which stops the job rather than let it miss the instruction.
    fn send_task_control(&self, task: &TaskRecord, cx: &App) -> Result<()> {
        let workspace = self
            .workspace(cx)
            .context(course2md::tr!("任务记录不可用"))?;
        workspace.write_control(task, run::task_control(&self.preferences, task)?);
        Ok(())
    }

    pub fn refresh_dispatch_controls(&mut self, cx: &mut Context<Self>) {
        if let Some(task) = self
            .work
            .active_task
            .as_deref()
            .and_then(|id| self.task(id, cx))
            && let Err(error) = self.send_task_control(task, cx)
        {
            self.report(course2md::tr!("停止新请求的指令尚未保存：{}"), error);
            if let Some(job) = &self.work.job {
                job.cancel();
            }
            cx.notify();
        }
    }

    pub fn start_next_task(&mut self, cx: &mut Context<Self>) {
        if self.storage_ui.busy {
            return;
        }
        let Some(task) = self
            .workspace(cx)
            .and_then(|w| w.state.next_task())
            .cloned()
        else {
            return;
        };
        // Held until the job runs or the start fails: polls keep coming meanwhile.
        if !self.work.claim_start(&task.id) {
            return;
        }
        let needed = match run::needed_credentials(&self.preferences, &task) {
            Ok(needed) => needed,
            Err(error) => return self.fail_task_before_start(&task.id, error, cx),
        };
        if needed.is_empty() {
            self.finish_task_start(&task.id, HashMap::new(), cx);
            return;
        }
        // Keychain resolution may surface a system authorization dialog whose queue can even
        // be held by another application; resolving on the UI thread would freeze the whole
        // interface, so the secrets are gathered on a background executor first.
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
        // The queue may have changed while credentials were being resolved off-thread.
        let Some(task) = self
            .workspace(cx)
            .filter(|workspace| {
                self.work.is_starting(task_id) && run::still_runnable(&workspace.state, task_id)
            })
            .and_then(|workspace| workspace.state.task(task_id))
            .cloned()
        else {
            self.work.end_start(task_id);
            return;
        };
        let start = match run::start_request(&self.preferences, self.workspace(cx), &task, &secrets)
        {
            Ok(start) => start,
            Err(error) => return self.fail_task_before_start(&task.id, error, cx),
        };
        let read_task = task.clone();
        let read = crate::spawn_blocking_result(move || start.prepare(&read_task));
        cx.spawn(async move |this, cx| {
            let read = read.await;
            let _ = this.update(cx, |this, cx| match read {
                Ok(request) => this.start_task_request(task, request, cx),
                Err(error) => this.fail_task_before_start(&task.id, error, cx),
            });
        })
        .detach();
        cx.notify();
    }

    fn start_task_request(
        &mut self,
        task: TaskRecord,
        request: course2md::execution::Request,
        cx: &mut Context<Self>,
    ) {
        // What the task is now, not the copy taken before the library was read.
        let current = self
            .workspace(cx)
            .is_some_and(|workspace| run::still_runnable(&workspace.state, &task.id));
        if !self.work.is_starting(&task.id)
            || self.work.job.is_some()
            || self.work.closing
            || !current
        {
            self.work.end_start(&task.id);
            return;
        }
        let running = self
            .with_workspace(cx, |workspace| {
                workspace.transaction(|state| run::mark_running(state, &task.id))
            })
            .context(course2md::tr!("任务记录不可用"))
            .and_then(|running| running);
        if let Err(error) = running {
            return self.fail_task_before_start(&task.id, error, cx);
        }
        // Starting the child and writing the full request to stdin can block on a pipe;
        // keep it off the UI thread.
        let wake = self.event_wake_tx.clone();
        let started = crate::spawn_blocking_result(move || Job::start_task(&request, wake));
        cx.spawn(async move |this, cx| {
            let job = started.await;
            let _ = this.update(cx, |this, cx| match job {
                Ok(job) => {
                    this.work.install(&task.id, job);
                    this.work.kind = Kind::Convert;
                    this.work.cancelling = false;
                    this.work.job_started = Some(std::time::Instant::now());
                    this.work.task_status = course2md::tr!("正在生成《{}》", task.plan.title);
                    cx.notify();
                }
                Err(error) => this.fail_task_before_start(&task.id, error, cx),
            });
        })
        .detach();
    }

    /// A start that ended before its job ran: the task waits for the person.
    fn fail_task_before_start(
        &mut self,
        task_id: &str,
        error: anyhow::Error,
        cx: &mut Context<Self>,
    ) {
        self.work.end_start(task_id);
        if let Some(Err(error)) = self.with_workspace(cx, |workspace| {
            run::fail_before_start(workspace, task_id, &error)
        }) {
            self.report(course2md::tr!("任务状态尚未保存：{}"), error);
        }
        cx.notify();
    }

    pub fn set_task_intent(&mut self, id: String, intent: Intent, cx: &mut Context<Self>) {
        let active = self.is_running_task(&id);
        if active && intent == Intent::Run {
            self.message = Some(course2md::tr!("当前任务还在处理，请等候当前操作结束。").into());
            cx.notify();
            return;
        }
        let changed = self.transact(
            course2md::tr!("任务状态尚未保存：{}"),
            |state| {
                state.set_intent(&id, intent)?;
                if active && intent != Intent::Run {
                    state
                        .task_mut(&id)
                        .context(course2md::tr!("任务记录不存在"))?
                        .state = TaskState::Pausing;
                }
                Ok(())
            },
            cx,
        );
        if changed.is_some() {
            if intent == Intent::Run {
                self.follow_resumed_input_task(&id, cx);
            } else if intent == Intent::Cancel {
                self.import.following_conversion = None;
            }
            if let Some(task) = self.task(&id, cx).cloned()
                && let Err(error) = self.send_task_control(&task, cx)
            {
                self.report(course2md::tr!("任务指令尚未保存：{}"), error);
                if self.work.active_task.as_ref() == Some(&id)
                    && let Some(job) = &self.work.job
                {
                    job.cancel();
                }
            }
            self.start_next_task(cx);
        }
        cx.notify();
    }

    pub fn select_task(&mut self, id: &str, cx: &mut Context<Self>) {
        let previous = self
            .workspace(cx)
            .and_then(|workspace| workspace.state.selected_task.clone());
        if previous.as_deref() != Some(id)
            && let Some(previous) = previous
        {
            self.close_task_details(&previous);
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
        // Going to a task opens its details when there is something to read
        // or decide; new and running work stays a compact row.
        let waiting = self
            .task(id, cx)
            .is_some_and(|task| matches!(task_group(task), TaskGroup::Running | TaskGroup::Queued));
        self.task_list.expanded = (!waiting).then(|| id.to_owned());
        cx.notify();
    }

    fn follow_resumed_input_task(&mut self, id: &str, cx: &App) {
        if self.page == Page::New
            && self
                .current_input_task(cx)
                .is_some_and(|task| task.id == id)
        {
            self.import.following_conversion =
                Some(desktop_tasks::import::ConversionFollow::Task {
                    id: id.to_owned(),
                    source_revision: self.import.preview_generation,
                });
        } else if self.page == Page::Result && !self.opened(cx).reading {
            self.import.following_conversion = self
                .task(id, cx)
                .zip(self.opened(cx).preview.as_ref())
                .and_then(|(task, preview)| {
                    desktop_tasks::import::ConversionFollow::reader_reprocess(
                        task,
                        &preview.course.dir,
                        self.opened(cx).read_generation,
                    )
                });
        }
    }

    pub fn resend_uncertain(&mut self, id: String, cx: &mut Context<Self>) {
        if self.is_running_task(&id) {
            self.message = Some(
                course2md::tr!("正在保存当前结果，请等候当前任务停止后再选择重新发送。").into(),
            );
            cx.notify();
            return;
        }
        let Some(task) = self.task(&id, cx).cloned() else {
            return;
        };
        if let Some(followup) = &task.handled_by {
            self.select_task(followup, cx);
            return;
        }
        let requests = run::uncertain_requests(&task);
        if requests.is_empty() {
            return;
        }
        if task.artifact.is_some() {
            self.reprocess_task(id, run::uncertain_components(&task), requests, cx);
            return;
        }
        if self.records.read(cx).workspace.is_some()
            && self
                .transact(
                    course2md::tr!("重新发送的选择尚未保存：{}"),
                    |state| state.authorize_uncertain(&id, &requests),
                    cx,
                )
                .is_none()
        {
            return;
        }
        self.follow_resumed_input_task(&id, cx);
        self.start_next_task(cx);
        cx.notify();
    }

    pub fn reprocess_task(
        &mut self,
        id: String,
        mut components: Vec<Component>,
        resend: Vec<String>,
        cx: &mut Context<Self>,
    ) {
        if self.is_running_task(&id) {
            self.message =
                Some(course2md::tr!("正在保存当前结果，请等候当前任务停止后再补做。").into());
            cx.notify();
            return;
        }
        components.sort();
        components.dedup();
        if let Some(id) = self.transact(
            course2md::tr!("补做任务尚未建立：{}"),
            |state| state.reprocess(&id, components, resend),
            cx,
        ) {
            self.select_task(&id, cx);
            self.follow_resumed_input_task(&id, cx);
            self.start_next_task(cx);
        }
        cx.notify();
    }

    pub(super) fn repair_task_service(
        &mut self,
        id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A followed-up or unconfirmed task is decided on its card.
        if let Some(task) = self.task(&id, cx)
            && (task.handled_by.is_some() || task.has_uncertain())
        {
            let shown = task.handled_by.clone().unwrap_or_else(|| id.clone());
            self.select_task(&shown, cx);
            self.navigate(Page::Task, cx);
            return;
        }
        let components = self
            .task(&id, cx)
            .map(run::repairable_components)
            .unwrap_or_default();
        if components.is_empty() {
            self.message = Some(course2md::tr!("这份笔记没有需要修复的校对或摘要").into());
            cx.notify();
            return;
        }
        self.open_reprocess_service_editor(id, components, window, cx);
    }

    pub(crate) fn reprocess_task_with_service(
        &mut self,
        id: String,
        components: Vec<Component>,
        ai_service: String,
        cx: &mut Context<Self>,
    ) -> bool {
        let result = (|| -> Result<String> {
            ensure!(
                !self.is_running_task(&id),
                course2md::tr!("正在保存当前结果，请等待任务停止后再补做")
            );
            let task = self
                .task(&id, cx)
                .context(course2md::tr!("原任务记录暂时不可用"))?;
            let config = run::reprocess_config(&self.preferences, task, &components, &ai_service)?;
            self.with_workspace(cx, |workspace| {
                workspace.transaction(|state| {
                    state.reprocess_with_service(
                        &id,
                        components,
                        Vec::new(),
                        Some((ai_service, config)),
                    )
                })
            })
            .context(course2md::tr!("任务记录暂时不可用"))?
        })();
        match result {
            Ok(next) => {
                self.workspace_error = None;
                self.select_task(&next, cx);
                self.follow_resumed_input_task(&next, cx);
                self.start_next_task(cx);
                cx.notify();
                true
            }
            Err(error) => {
                self.report(course2md::tr!("服务已保存，补做任务尚未建立：{}"), error);
                cx.notify();
                false
            }
        }
    }

    pub fn adjust_task(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_running_task(&id) {
            self.message =
                Some(course2md::tr!("当前任务还在处理，请先暂停并等候当前步骤结束。").into());
            cx.notify();
            return;
        }
        if !self.save_current_draft(cx) {
            return;
        }
        let Some(task) = self.task(&id, cx).cloned() else {
            return;
        };
        let events =
            crate::spawn_blocking_io(move || workspace::task_subtitle_events(&task).ok().flatten());
        cx.spawn_in(window, async move |this, cx| {
            let events = events.await.flatten();
            let _ = this.update_in(cx, |this, window, cx| {
                let adjusted = this.transact(
                    course2md::tr!("尚未打开任务调整选项：{}"),
                    |state| state.adjust_task(&id, events),
                    cx,
                );
                if adjusted.is_some() {
                    this.import.following_conversion = None;
                    this.invalidate_source();
                    this.restore_draft(window, cx);
                    this.set_page(Page::New, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub fn finish_task(
        &mut self,
        id: &str,
        success: bool,
        cancelled: bool,
        cx: &mut Context<Self>,
    ) {
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
        let Some(result) = self.with_workspace(cx, |workspace| {
            let visible =
                page == Page::Task && workspace.state.selected_task.as_deref() == Some(id);
            workspace.transaction(|state| {
                run::record_finish(
                    state,
                    id,
                    done.as_ref(),
                    manifest.as_ref(),
                    visible,
                    success,
                    cancelled,
                )
            })
        }) else {
            return;
        };
        match result {
            Ok(task) => {
                if self.reader.read(cx).reader_viewer_open() {
                    desktop_tasks::import::ConversionFollow::interrupt_for_reader_viewer(
                        &mut self.import.following_conversion,
                    );
                }
                let follow_input_completion = self
                    .import
                    .following_conversion
                    .as_ref()
                    .zip(self.workspace(cx))
                    .and_then(|(follow, workspace)| {
                        follow.completed_task(
                            self.page == Page::New,
                            self.import.preview_generation,
                            workspace.state.draft()?,
                            &workspace.state.tasks,
                            &self.value(Field::Source, cx),
                        )
                    })
                    .is_some_and(|task| task.id == id);
                let follow_reader_completion = !self.opened(cx).reading
                    && self
                        .import
                        .following_conversion
                        .as_ref()
                        .is_some_and(|follow| {
                            follow.completed_reader_task(
                                &task,
                                self.page,
                                self.opened(cx).read_generation,
                                self.opened(cx)
                                    .preview
                                    .as_ref()
                                    .map(|preview| preview.course.dir.as_path()),
                            )
                        });
                if let Some(done) = done {
                    self.work.completed = Some(done.clone());
                    let root = self.task_library(&task, cx).map(|l| l.root.clone());
                    let open = !task.exports_only()
                        && (follow_input_completion || follow_reader_completion);
                    let publication = run::Publication::new(&task, root, &done, manifest.as_ref());
                    // Cover copy, folder assignment and thumbnail lookup touch the
                    // library disk; the record above is already committed.
                    let published = crate::spawn_blocking_io(move || publication.file());
                    cx.spawn(async move |this, cx| {
                        let Some((issues, thumbnail)) = published.await else {
                            return;
                        };
                        let _ = this.update(cx, |this, cx| {
                            if !issues.is_empty() {
                                this.message = Some(course2md::tr!(
                                    "笔记已保存，{}",
                                    issues.join(course2md::tr!("；"))
                                ));
                            }
                            if let Some(manifest) = &manifest {
                                this.records.update(cx, |records, _| {
                                    update_published_course(
                                        &mut records.library.courses,
                                        &done,
                                        manifest,
                                        thumbnail,
                                    )
                                });
                            }
                            this.refresh_library(cx);
                            if open {
                                this.open_completed_conversion(Course::from_completed(&done), cx);
                            }
                            cx.notify();
                        });
                    })
                    .detach();
                }
                self.work.task_status = task.status_label().into();
                self.transient_task_result = Some((id.to_owned(), std::time::Instant::now()));
                self.schedule_wake(std::time::Duration::from_secs(8));
            }
            Err(error) => self.report(
                course2md::tr!("完成状态尚未保存：{}。已发布的笔记仍保留。"),
                error,
            ),
        }
        cx.notify();
    }

    pub(crate) fn open_task_export_location(&mut self, id: String, cx: &mut Context<Self>) {
        let destination = self
            .workspace(cx)
            .context(course2md::tr!("任务记录暂时不可用"))
            .and_then(|workspace| run::task_export_destination(&workspace.state, &id));
        match destination {
            Ok(path) => cx.reveal_path(&path),
            Err(error) => {
                self.message = Some(course2md::tr!("无法打开导出位置：{}", format!("{error:#}")))
            }
        }
        cx.notify();
    }
}
