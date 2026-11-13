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
use desktop_tasks::task_view::{TaskAction, TaskGroup, request_scope, resumable, task_group};
use std::collections::HashMap;

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

    /// Where videos' text comes from: Settings' 文字来源.
    pub(crate) fn text_mode(&self, _cx: &App) -> course2md::config::TranscriptSource {
        self.preferences
            .generation()
            .options
            .transcript_source
            .unwrap_or_default()
    }

    /// One saved change to the workspace. A failure shows in the shell as `failure`,
    /// whose `{}` takes the cause; the caller only says what to do on success.
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
        if self.workspace_error.is_none() {
            self.advance_conversion_when_ready(cx);
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

    /// Whether this task holds the running background job (its record may lag the event stream).
    pub(crate) fn is_running_task(&self, id: &str, cx: &App) -> bool {
        self.work.read(cx).is_running(id)
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
        // A source edit must establish the next input before autosave can detach
        // the submitted task. Failed preparation cannot fall through to saving the
        // previous video's choices against a new URL.
        if self
            .workspace(cx)
            .and_then(|workspace| workspace.state.draft())
            .is_some_and(|draft| draft.submitted_task.is_some() && draft.input != input)
        {
            return false;
        }
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
        self.import.preview_error = None;
        self.import.source_validation = None;
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
        let local_unread = !draft.online
            && draft.source.is_none()
            && std::path::Path::new(draft.input.trim()).is_file();
        self.inputs[&Field::Source].update(cx, |s, cx| s.set_value(draft.input, window, cx));
        self.scrolls[Page::New as usize].set_offset(point(px(0.), px(draft.scroll)));
        self.work.update(cx, |work, _| work.draft_saved());
        // A local file chosen but not read yet is read now: reading it is local
        // and quick, and only previews; nothing is submitted.
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
            // English counts its noun; the catalog keeps the singular apart.
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
            self.message = Some(parts.join(course2md::tr!("；")));
            if let Some(id) = first_id {
                self.select_task(&id, cx);
            }
            self.start_next_task(cx);
        }
        cx.notify();
    }

    /// The plan the form would submit, checked against what is loaded now.
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
                self.import.source_validation = Some(format!("{error:#}"));
                self.import.validation_attempt = self.import.validation_attempt.wrapping_add(1);
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
            self.message =
                (!created).then(|| course2md::tr!("这份笔记已有进行中的任务，已为你打开").into());
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
            .read(cx)
            .active_task
            .as_deref()
            .and_then(|id| self.task(id, cx))
            && let Err(error) = self.send_task_control(task, cx)
        {
            self.report(course2md::tr!("停止新请求的指令尚未保存：{}"), error);
            if let Some(job) = &self.work.read(cx).job {
                job.cancel();
            }
            cx.notify();
        }
    }

    pub fn start_next_task(&mut self, cx: &mut Context<Self>) {
        if self.storage_ui.busy {
            return;
        }
        // Until this machine is checked, an automatic speech engine would be a guess,
        // and a wrong one would discard the speech progress saved with the right one;
        // a check that failed or takes too long no longer holds the queue.
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
        // Held until the job runs or the start fails: polls keep coming meanwhile.
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
        let read = crate::spawn_blocking_result(move || start.prepare(&read_task));
        cx.spawn(async move |this, cx| {
            let read = read.await;
            let _ = this.update(cx, |this, cx| match read {
                Ok((request, ran_with)) => this.start_task_request(task, request, ran_with, cx),
                Err(error) => match error.downcast::<run::NeedsRework>() {
                    Ok(run::NeedsRework(rework)) => this.await_rework(&task.id, rework, cx),
                    Err(error) => this.fail_task_before_start(&task.id, error, cx),
                },
            });
        })
        .detach();
        cx.notify();
    }

    fn start_task_request(
        &mut self,
        task: TaskRecord,
        request: course2md::execution::Request,
        ran_with: workspace::RunSettings,
        cx: &mut Context<Self>,
    ) {
        // What the task is now, not the copy taken before the library was read.
        let current = self
            .workspace(cx)
            .is_some_and(|workspace| run::still_runnable(&workspace.state, &task.id));
        let work = self.work.read(cx);
        if !work.is_starting(&task.id) || work.job.is_some() || work.closing || !current {
            self.work.update(cx, |work, _| work.end_start(&task.id));
            return;
        }
        let running = self
            .with_workspace(cx, |workspace| {
                workspace.transaction(|state| run::mark_running(state, &task.id, ran_with))
            })
            .context(course2md::tr!("任务记录不可用"))
            .and_then(|running| running);
        if let Err(error) = running {
            return self.fail_task_before_start(&task.id, error, cx);
        }
        // Starting the child and writing the full request to stdin can block on a pipe;
        // keep it off the UI thread.
        let wake = self.work.read(cx).waker();
        let started = crate::spawn_blocking_result(move || Job::start_task(&request, wake));
        cx.spawn(async move |this, cx| {
            let job = started.await;
            let _ = this.update(cx, |this, cx| match job {
                Ok(job) => {
                    let status = course2md::tr!("正在生成《{}》", task.plan.title);
                    this.work.change(cx, |work, _| {
                        work.install(&task.id, job, status, Instant::now())
                    });
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
        self.work.update(cx, |work, _| work.end_start(task_id));
        if let Some(Err(error)) = self.with_workspace(cx, |workspace| {
            run::fail_before_start(workspace, task_id, &error)
        }) {
            self.report(course2md::tr!("任务状态尚未保存：{}"), error);
        }
        cx.notify();
    }

    /// Continues a paused or stopped task with the current settings. One that stopped
    /// because that would redo paid work its earlier run finished continues only once
    /// the person agrees here; every other way a task resumes (all at once, after a
    /// move, when its model is ready, on relaunch) leaves it waiting for this.
    /// The video reopened from task `id` goes back to being that task's: the card
    /// shows it again, and it continues.
    pub(crate) fn continue_retried_task(
        &mut self,
        id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let linked = self.update_draft(
            |draft| {
                draft.submitted_task = Some(id.clone());
                draft.retry_of = None;
            },
            cx,
        );
        if linked {
            self.resume_task(id, window, cx);
        }
    }

    fn resume_task(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(task) = self.task(&id, cx).cloned() else {
            return;
        };
        if let Some(rework) = task.rework.clone() {
            self.ask_rework(id, rework, window, cx);
            return;
        }
        // Asked at once when continuing would pay again; the check reads the task's
        // request records, so it runs off the UI thread.
        let Some(check) =
            run::resume_check(&self.preferences, &task, self.recommended_local_provider())
        else {
            self.set_task_intent(id, Intent::Run, cx);
            return;
        };
        let check = crate::spawn_blocking_io(move || check.run());
        cx.spawn_in(window, async move |this, cx| {
            let rework = check.await.flatten();
            let _ = this.update_in(cx, |this, window, cx| match rework {
                Some(rework) => {
                    let asked = this.transact(
                        course2md::tr!("任务状态尚未保存：{}"),
                        |state| run::ask_rework(state, &id, rework.clone()),
                        cx,
                    );
                    if asked.is_some() {
                        this.ask_rework(id, rework, window, cx);
                    }
                }
                None => this.set_task_intent(id, Intent::Run, cx),
            });
        })
        .detach();
    }

    /// Asks whether to redo the paid work `rework` names, and continues on yes.
    fn ask_rework(
        &mut self,
        id: String,
        rework: workspace::Rework,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut question = ui::Confirm::new(
            course2md::tr!("用当前设置继续生成？"),
            course2md::tr!("暂不继续"),
            course2md::tr!("继续生成"),
        );
        for change in rework.changes {
            question = question.text(change);
        }
        question
            .text(course2md::tr!(
                "继续生成会按当前设置重新处理上次已完成的部分，可能产生额外费用。"
            ))
            .ask(
                cx.listener(move |this, _, _, cx| this.accept_rework(id.clone(), cx)),
                window,
                cx,
            );
    }

    fn accept_rework(&mut self, id: String, cx: &mut Context<Self>) {
        if self
            .transact(
                course2md::tr!("任务状态尚未保存：{}"),
                |state| run::accept_rework(state, &id),
                cx,
            )
            .is_some()
        {
            self.follow_resumed_input_task(&id, cx);
            self.start_next_task(cx);
        }
        cx.notify();
    }

    /// A start that would redo paid work the person has not agreed to redo: the task
    /// waits, paused, and the queue goes on.
    fn await_rework(&mut self, task_id: &str, rework: workspace::Rework, cx: &mut Context<Self>) {
        self.work.update(cx, |work, _| work.end_start(task_id));
        if let Some(Err(error)) = self.with_workspace(cx, |workspace| {
            workspace.transaction(|state| run::await_rework(state, task_id, rework))
        }) {
            self.report(course2md::tr!("任务状态尚未保存：{}"), error);
        }
        self.start_next_task(cx);
        cx.notify();
    }

    pub fn set_task_intent(&mut self, id: String, intent: Intent, cx: &mut Context<Self>) {
        let active = self.is_running_task(&id, cx);
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
                let work = self.work.read(cx);
                if work.active_task.as_ref() == Some(&id)
                    && let Some(job) = &work.job
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
        // Going to a task opens its details when there is something to read
        // or decide; new and running work stays a compact row.
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
        if self.is_running_task(&id, cx) {
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
            match task.uncertain_components() {
                Ok(components) => self.reprocess_task(id, components, requests, cx),
                Err(error) => {
                    self.message = Some(format!("{error:#}"));
                    cx.notify();
                }
            }
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
        if self.is_running_task(&id, cx) {
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

    /// Repairs the AI parts of a task's note once its service is saved as the default,
    /// which is what the repair runs with.
    pub(crate) fn reprocess_task_after_service(
        &mut self,
        id: String,
        components: Vec<Component>,
        cx: &mut Context<Self>,
    ) -> bool {
        let result = (|| -> Result<String> {
            ensure!(
                !self.is_running_task(&id, cx),
                course2md::tr!("正在保存当前结果，请等待任务停止后再补做")
            );
            self.with_workspace(cx, |workspace| {
                workspace.transaction(|state| state.reprocess(&id, components, Vec::new()))
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
        if self.is_running_task(&id, cx) {
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
                let adjusted = this
                    .records
                    .change(cx, |records, _| {
                        Some(
                            records
                                .workspace
                                .as_mut()?
                                .transaction(|state| state.adjust_task(&id, events)),
                        )
                    })
                    .and_then(|result| {
                        result
                            .map_err(|error| {
                                this.report(course2md::tr!("尚未在主页重新打开这个视频：{}"), error)
                            })
                            .ok()
                    });
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
                            let preview = self.opened(cx).preview.as_ref();
                            follow.completed_reader_task(
                                &task,
                                self.page,
                                self.opened(cx).read_generation,
                                preview.map(|preview| preview.course.dir.as_path()),
                                preview
                                    .and_then(|preview| preview.course.manifest.as_ref())
                                    .map(|manifest| manifest.task_id.as_str()),
                            )
                        });
                if let Some(done) = done {
                    self.work
                        .update(cx, |work, _| work.completed = Some(done.clone()));
                    let root = self.task_library(&task, cx).map(|l| l.root.clone());
                    let open = !task.exports_only()
                        && (follow_input_completion || follow_reader_completion);
                    let exports_only = task.exports_only();
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
                            if open {
                                this.open_completed_conversion(Course::from_completed(&done), cx);
                            } else {
                                this.reload_replaced_note(&done.out_dir, exports_only, cx);
                            }
                            cx.notify();
                        });
                    })
                    .detach();
                }
                self.work.change(cx, |work, _| {
                    work.task_status = task.status_label().into();
                    work.show_result(id, Instant::now());
                });
            }
            Err(error) => self.report(
                course2md::tr!("完成状态尚未保存：{}。已发布的笔记仍保留。"),
                error,
            ),
        }
        cx.notify();
    }

    /// A note open in the reader that a task has just replaced is read again in place,
    /// at the same moment of the video, wherever the person is.
    fn reload_replaced_note(
        &mut self,
        note: &std::path::Path,
        exports_only: bool,
        cx: &mut Context<Self>,
    ) {
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
        let reload = if exports_only {
            crate::reader_ui::Reload::Reread
        } else {
            crate::reader_ui::Reload::Replaced
        };
        self.reader
            .change(cx, |reader, cx| reader.reload_note(course, reload, cx));
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

    /// Stop the running task first, so its stop starts nothing new, then hold
    /// the queue in one step, so no queued task starts between two pauses.
    pub(crate) fn pause_all_tasks(&mut self, cx: &mut Context<Self>) {
        let Some(workspace) = &self.records.read(cx).workspace else {
            return;
        };
        let pausable = |task: &&TaskRecord| task.handled_by.is_none() && task.intent == Intent::Run;
        let running: Vec<String> = workspace
            .state
            .tasks
            .iter()
            .filter(|task| task.state == TaskState::Running)
            .filter(pausable)
            .map(|task| task.id.clone())
            .collect();
        let queued: Vec<String> = workspace
            .state
            .queued_tasks()
            .filter(pausable)
            .map(|task| task.id.clone())
            .collect();
        for id in running {
            self.set_task_intent(id, Intent::Pause, cx);
        }
        // Queued work has no worker to tell; its record is the whole state.
        self.transact(
            course2md::tr!("任务状态尚未保存：{}"),
            |state| {
                queued
                    .iter()
                    .try_for_each(|id| state.set_intent(id, Intent::Pause))
            },
            cx,
        );
        cx.notify();
    }

    pub(crate) fn resume_all_tasks(&mut self, cx: &mut Context<Self>) {
        let Some(workspace) = &self.records.read(cx).workspace else {
            return;
        };
        let ids: Vec<String> = workspace
            .state
            .tasks
            .iter()
            .filter(|task| resumable(task))
            .map(|task| task.id.clone())
            .collect();
        for id in ids {
            self.set_task_intent(id, Intent::Run, cx);
        }
    }

    pub(crate) fn move_waiting_task(&mut self, id: &str, position: usize, cx: &mut Context<Self>) {
        self.transact(
            course2md::tr!("任务顺序尚未保存：{}"),
            |state| state.move_waiting_task(id, position),
            cx,
        );
        cx.notify();
    }

    fn open_task_note(&mut self, task: &TaskRecord, cx: &mut Context<Self>) {
        let Some(path) = &task.artifact else {
            return;
        };
        // The note opens from its path even before the library has listed it.
        let course = self
            .library(cx)
            .courses
            .iter()
            .find(|course| &course.dir == path)
            .cloned()
            .unwrap_or_else(|| {
                Rc::new(Course::from_completed(&Done {
                    out_dir: path.clone(),
                    title: task.plan.title.clone(),
                    ..Default::default()
                }))
            });
        self.open_course(course, cx);
    }

    /// Runs one of a task's actions, from its card, its menu or a library row.
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
            TaskAction::Read => self.open_task_note(&task, cx),
            TaskAction::OpenNoteFolder => {
                if let Some(path) = &task.artifact {
                    cx.open_with_system(path);
                }
            }
            TaskAction::OpenRecoveryFolder => {
                if let Some(folder) = task.events.blocked_folder() {
                    cx.open_with_system(folder);
                }
            }
            TaskAction::OpenExports => self.open_task_export_location(id, cx),
            TaskAction::OpenSource => cx.open_url(&task.plan.source.input),
            TaskAction::CopyLink => crate::clipboard::write(
                ClipboardItem::new_string(task.plan.source.input.clone()),
                cx,
            ),
            TaskAction::Details => self
                .tasks
                .update(cx, |page, cx| page.toggle_task_row(&id, cx)),
            TaskAction::Log => self
                .tasks
                .update(cx, |page, cx| page.toggle_task_log(&task, cx)),
            TaskAction::Followup(next) => self.select_task(next, cx),
            TaskAction::Move { to, .. } => {
                if let Some(to) = to {
                    self.move_waiting_task(&id, *to, cx);
                }
            }
            TaskAction::Pause => self.set_task_intent(id, Intent::Pause, cx),
            TaskAction::Resume => self.resume_task(id, window, cx),
            TaskAction::PrepareModel => self.prepare_task_model(&task, cx),
            TaskAction::Cancel => self.set_task_intent(id, Intent::Cancel, cx),
            TaskAction::Repair => self.repair_task_service(id, window, cx),
            TaskAction::Retry(component) => {
                let part = Component::parse(component).unwrap_or(Component::Proofreading);
                self.reprocess_task(id, vec![part], Vec::new(), cx)
            }
            TaskAction::Reassociate(library) => {
                self.begin_library_reassociation(library.clone(), window, cx)
            }
            // A menu has no room for the consequence, so it is asked here. The card's
            // own button stands beside it and sends directly.
            TaskAction::Resend => {
                let mut question = ui::Confirm::new(
                    course2md::tr!("重新发送以上内容"),
                    course2md::tr!("暂不发送"),
                    course2md::tr!("重新发送以上内容"),
                );
                for request in task.events.blocked.iter().filter(|b| b.is_uncertain()) {
                    question = question.text(request_scope(request));
                }
                for also in task.resend_also_sends() {
                    question = question.text(also);
                }
                question
                    .text(course2md::tr!(
                        "服务可能已处理这些内容，重新发送可能产生额外费用"
                    ))
                    .ask(
                        cx.listener(move |this, _, _, cx| this.resend_uncertain(id.clone(), cx)),
                        window,
                        cx,
                    );
            }
            TaskAction::Adjust => self.adjust_task(id, window, cx),
            TaskAction::ViewTask => {
                self.select_task(&id, cx);
                self.navigate(Page::Task, cx);
            }
        }
    }

    /// What the task page asks of the window.
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
            TasksEvent::Resend(id) => self.resend_uncertain(id.clone(), cx),
            TasksEvent::Move { id, position } => self.move_waiting_task(id, *position, cx),
            TasksEvent::PauseAll => self.pause_all_tasks(cx),
            TasksEvent::ResumeAll => self.resume_all_tasks(cx),
            TasksEvent::Navigate(page) => self.navigate(*page, cx),
            TasksEvent::Import => {
                if self.prepare_workbench_input(window, cx) {
                    self.navigate(Page::New, cx);
                }
            }
        }
        cx.notify();
    }

    /// What the task cards show that only the window knows.
    pub(crate) fn shell_facts(&self, cx: &App) -> task_ui::ShellFacts {
        let records = self.records.read(cx);
        let services = records
            .workspace
            .iter()
            .flat_map(|workspace| &workspace.state.tasks)
            .flat_map(desktop_tasks::task_view::ai_producers)
            .filter_map(|id| {
                let version = self.preferences.version(id)?;
                Some((id.to_owned(), version.config.name.clone()))
            })
            .collect();
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
        // A card that asked for its model says how that went: the outcome lives in
        // settings' model state until another preparation starts.
        let state = &self.settings_ui.model_diagnostics;
        let provider = self.recommended_local_provider();
        let models = records
            .workspace
            .iter()
            .flat_map(|workspace| &workspace.state.tasks)
            .filter(|task| task.state == TaskState::NeedsAttention && task.events.model_required)
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
        let services_missing = records
            .workspace
            .iter()
            .flat_map(|workspace| &workspace.state.tasks)
            // A paused task resumes with the default services, so it is affected too.
            .filter(|task| matches!(task.state, TaskState::NeedsAttention | TaskState::Paused))
            .filter(|task| desktop_tasks::run::services_missing(&self.preferences, task, provider))
            .map(|task| task.id.clone())
            .collect();
        task_ui::ShellFacts {
            services,
            reassociate,
            storage_busy: self.storage_ui.busy,
            models,
            services_missing,
        }
    }
}
