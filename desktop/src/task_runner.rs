//! Task execution: draft persistence, plan construction, dispatch to the
//! engine process and recording its events. The task page lives in task_ui.
use super::*;
use crate::{
    notes::default_output,
    preferences::ServiceRefs,
    workspace::{Intent, TaskPlan, TaskRecord, TaskState},
};
use anyhow::{Context as _, Result, ensure};
use course2md::config::TranscriptSource;
use course2md::execution::Component;
use desktop_core::plan::*;
use desktop_core::task_view::{TaskGroup, ai_component, task_component_failures, task_group};
use std::collections::HashMap;

/// Options shown before the workspace has restored a draft.
static NO_DRAFT_OPTIONS: std::sync::LazyLock<ConversionOptions> =
    std::sync::LazyLock::new(ConversionOptions::default);

impl Desktop {
    /// The current input form. It is the only copy of its choices: views read
    /// it and option controls write through `update_draft`.
    pub(crate) fn draft(&self) -> Option<&workspace::Draft> {
        self.workspace.as_ref().and_then(|w| w.state.draft())
    }

    /// Whether the current input is an online link (the default before a
    /// workspace is restored) rather than a local file.
    pub(crate) fn online(&self) -> bool {
        self.draft().is_none_or(|draft| draft.online)
    }

    pub(crate) fn task_options(&self) -> &ConversionOptions {
        self.draft()
            .map(|draft| &draft.options)
            .unwrap_or(&NO_DRAFT_OPTIONS)
    }

    pub(crate) fn target_folder(&self) -> Option<u64> {
        self.draft().and_then(|draft| draft.folder)
    }

    /// Change the current draft and record it. A failed write is reported
    /// beside the workspace and returns false.
    /// One saved change to the workspace. A failure shows in the shell as `failure`,
    /// whose `{}` takes the cause; the caller only says what to do on success.
    pub(crate) fn transact<T>(
        &mut self,
        failure: &str,
        edit: impl FnOnce(&mut workspace::State) -> Result<T>,
    ) -> Option<T> {
        match self.workspace.as_mut()?.transaction(edit) {
            Ok(value) => Some(value),
            Err(error) => {
                self.workspace_error = Some(failure.replacen("{}", &format!("{error:#}"), 1));
                None
            }
        }
    }

    pub(crate) fn update_draft(
        &mut self,
        edit: impl FnOnce(&mut workspace::Draft),
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(workspace) = &mut self.workspace else {
            return false;
        };
        match edit_current_draft(workspace, edit) {
            Ok(_) => {
                self.workspace_error = None;
                true
            }
            Err(error) => {
                self.workspace_error =
                    Some(course2md::tr!("当前输入尚未保存：{}", format!("{error:#}")));
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
        self.workspace = Some(workspace);
        self.workspace_error = None;
        self.restore_draft(window, cx);
        self.refresh_library(cx);
    }

    pub fn retry_workspace_records(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(workspace) = &self.workspace {
            self.workspace_error = workspace
                .save()
                .err()
                .map(|error| course2md::tr!("记录仍未保存：{}", format!("{error:#}")));
        } else {
            let options = ConversionOptions::from_config(&self.preferences.defaults_config());
            match workspace::Workspace::open(self.workspace_root(), options) {
                Ok(workspace) => self.install_workspace(workspace, window, cx),
                Err(error) => {
                    self.workspace_error = Some(course2md::tr!(
                        "记录仍无法读取，原件已保留：{}",
                        format!("{error:#}")
                    ))
                }
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
        let result = if let Some(workspace) = &self.workspace {
            workspace::Workspace::rebuild_at(workspace.storage_path().to_owned(), root, options)
        } else {
            workspace::Workspace::rebuild(root, options)
        };
        match result {
            Ok(workspace) => self.install_workspace(workspace, window, cx),
            Err(error) => {
                self.workspace_error = Some(course2md::tr!(
                    "记录尚未重建，原件仍保留：{}",
                    format!("{error:#}")
                ))
            }
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
    pub(crate) fn task_library(&self, task: &TaskRecord) -> Option<&workspace::LibraryLocation> {
        self.workspace
            .as_ref()
            .and_then(|w| w.state.library(&task.plan.library_id))
    }

    /// Record the text fields, preview and scroll position of the visible form.
    pub fn save_current_draft(&mut self, cx: &mut Context<Self>) -> bool {
        let input = self.value(Field::Source, cx);
        // A source edit must establish the next form's defaults before autosave
        // can detach the submitted task. Failed preparation cannot fall through
        // to saving the previous video's title and overrides against a new URL.
        if self
            .workspace
            .as_ref()
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
                let (options, folder) = (draft.options.clone(), draft.folder);
                update_input_form(draft, input, title, source, options, folder, scroll);
            },
            cx,
        )
    }

    pub fn restore_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(draft) = self
            .workspace
            .as_ref()
            .and_then(|w| w.state.draft())
            .cloned()
        else {
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
        if self.online() == online {
            return;
        }
        if !self.save_current_draft(cx) {
            return;
        }
        let defaults = ConversionOptions::from_config(&self.preferences.defaults_config());
        let switched = self.transact(course2md::tr!("无法切换来源：{}"), |state| {
            state.switch_source_kind(online, defaults);
            Ok(())
        });
        if switched.is_some() {
            self.import.following_conversion = None;
            self.invalidate_source();
            self.import.completed_source = None;
            self.message = None;
            self.restore_draft(window, cx);
        }
    }

    /// What a plan is built from, as of now.
    fn plan_inputs(&self) -> PlanInputs<'_> {
        PlanInputs {
            preferences: &self.preferences,
            workspace: self.workspace.as_ref(),
            environment: self.environment.as_ref(),
            import: &self.import,
            library: &self.library,
            locations: &self.storage_ui.location_checks,
            defaults_pending: self.preference_defaults_pending,
            preferences_issue: self
                .ordinary_preferences_submit_issue()
                .map(|issue| issue.message),
        }
    }

    fn build_plan(&self, validation: PlanValidation) -> Result<TaskPlan> {
        self.plan_inputs().build_plan(validation)
    }

    fn build_batch_plan(
        &self,
        candidate: &source::SourceCandidate,
        title: String,
    ) -> Result<TaskPlan> {
        self.plan_inputs().build_batch_plan(candidate, title)
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
            match self.build_batch_plan(&candidate, title) {
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
        let queued = self.transact(course2md::tr!("任务尚未加入队列：{}"), |state| {
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
        });
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
    pub fn preview_plan(&self) -> Result<TaskPlan> {
        self.build_plan(PlanValidation::Preview)
    }

    /// An unfinished task already doing the same work as `plan`.
    pub fn matching_task(&self, plan: &TaskPlan) -> Option<&TaskRecord> {
        self.workspace.as_ref()?.state.tasks.iter().find(|task| {
            !task.state.finished() && task.handled_by.is_none() && task.plan.same_work(plan)
        })
    }

    pub fn enqueue_current(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.save_current_draft(cx) {
            return;
        }
        self.message = None;
        let plan = match self.build_plan(PlanValidation::Submission) {
            Ok(plan) => plan,
            Err(error) => {
                self.import.title_required = error.downcast_ref::<TitleRequired>().is_some();
                self.import.source_validation = Some(format!("{error:#}"));
                self.import.validation_attempt = self.import.validation_attempt.wrapping_add(1);
                let subtitle_attention = self.subtitle_attention_required();
                let field = if self.import.source_preview.is_some()
                    && !subtitle_attention
                    && self.value(Field::Title, cx).trim().is_empty()
                {
                    Some(Field::Title)
                } else if self.import.source_preview.is_none() && self.online() {
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
        let parent = self.draft().and_then(|draft| draft.retry_of.clone());
        if let Some((id, created)) =
            self.transact(course2md::tr!("任务尚未加入队列：{}"), |state| {
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
            })
        {
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

    fn request_for(
        &self,
        task: &TaskRecord,
        secrets: &HashMap<String, String>,
    ) -> Result<course2md::execution::Request> {
        let location = self
            .task_library(task)
            .context(course2md::tr!("任务保存位置暂时不可用"))?;
        let refs = ServiceRefs {
            asr: task.plan.asr_service.clone(),
            llm: task.plan.ai_service.clone(),
        };
        let config = self
            .preferences
            .resolve_for_execution_with_secrets(&task.plan.config, &refs, secrets)?
            .into_config();
        let course_id = workspace::course_dir_name(&task.plan.source_id);
        let service_versions = [("asr", refs.asr), ("llm", refs.llm)]
            .into_iter()
            .filter_map(|(purpose, reference)| reference.map(|r| (purpose.to_owned(), r)))
            .collect();
        let source = self
            .workspace
            .as_ref()
            .and_then(|workspace| workspace.state.reader_sources.get(&task.plan.source_id))
            .filter(|path| path.is_file())
            .map(|path| path.to_string_lossy().into_owned())
            .filter(|_| {
                !task.plan.source.online && !std::path::Path::new(&task.plan.source.input).is_file()
            })
            .unwrap_or_else(|| task.plan.source.input.clone());
        Ok(course2md::execution::Request {
            schema: 1,
            operation: task.plan.operation.clone(),
            task_id: task.id.clone(),
            version_id: task.id.clone(),
            course_dir: location.root.join(&course_id),
            course_id,
            source,
            source_id: task.plan.source_id.clone(),
            title: task.plan.title.clone(),
            author: task.plan.source.author.clone(),
            duration: task.plan.source.duration,
            subtitle: if task.plan.options().source_mode != TranscriptSource::Asr {
                task.plan.subtitle.clone()
            } else {
                None
            },
            subtitle_events: None,
            config,
            work_dir: task.work_dir.clone(),
            allow_unauthenticated_asr: task
                .plan
                .asr_service
                .as_deref()
                .and_then(|id| self.preferences.version(id))
                .is_some_and(|version| {
                    version.config.authentication == preferences::Authentication::None
                }),
            control_path: Some(task.work_dir.join("control.json")),
            service_versions,
        })
    }

    /// What a task's control file should say: the engine reads it at every safe boundary.
    fn task_control(&self, task: &TaskRecord) -> Result<Vec<u8>> {
        let stopped: Vec<_> = [&task.plan.asr_service, &task.plan.ai_service]
            .into_iter()
            .flatten()
            .filter(|id| {
                self.preferences
                    .version(id)
                    .is_none_or(|version| self.preferences.is_service_retired(&version.service_id))
            })
            .cloned()
            .collect();
        Ok(serde_json::to_vec(&serde_json::json!({
            "intent": task.intent, "stopped_services": stopped, "resend": task.resend,
        }))?)
    }

    /// Sends a task its current instruction. A failure comes back through
    /// `take_write_error`, which stops the job rather than let it miss the instruction.
    fn send_task_control(&self, task: &TaskRecord) -> Result<()> {
        let workspace = self
            .workspace
            .as_ref()
            .context(course2md::tr!("任务记录不可用"))?;
        workspace.write_control(task, self.task_control(task)?);
        Ok(())
    }

    pub fn refresh_dispatch_controls(&mut self, cx: &mut Context<Self>) {
        if let Some(task) = self
            .work
            .active_task
            .as_deref()
            .and_then(|id| self.workspace.as_ref()?.state.task(id))
            && let Err(error) = self.send_task_control(task)
        {
            self.workspace_error = Some(course2md::tr!(
                "停止新请求的指令尚未保存：{}",
                format!("{error:#}")
            ));
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
            .workspace
            .as_ref()
            .and_then(|w| w.state.next_task())
            .cloned()
        else {
            return;
        };
        // Held until the job runs or the start fails: polls keep coming meanwhile.
        if !self.work.claim_start(&task.id) {
            return;
        }
        let refs = ServiceRefs {
            asr: task.plan.asr_service.clone(),
            llm: task.plan.ai_service.clone(),
        };
        let needed = match self
            .preferences
            .credential_references_for(&task.plan.config, &refs)
        {
            Ok(needed) => needed,
            Err(error) => {
                self.fail_task_before_start(&task.id, format!("{error:#}"));
                cx.notify();
                return;
            }
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
                .recv()
                .await
                .unwrap_or_else(|_| Err(anyhow::anyhow!(course2md::tr!("凭据读取意外中断"))));
            let _ = this.update(cx, |this, cx| {
                match secrets {
                    Ok(secrets) => this.finish_task_start(&task_id, secrets, cx),
                    Err(error) => {
                        this.fail_task_before_start(&task_id, format!("{error:#}"));
                        cx.notify();
                    }
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
        let task = self
            .workspace
            .as_ref()
            .and_then(|w| w.state.task(task_id))
            .cloned();
        // The queue may have changed while credentials were being resolved off-thread.
        let Some(task) = task.filter(|task| {
            self.work.is_starting(&task.id)
                && task.state == TaskState::Queued
                && task.intent == Intent::Run
        }) else {
            self.work.end_start(task_id);
            return;
        };
        let prepared = self.request_for(&task, &secrets).and_then(|request| {
            let control = self.task_control(&task)?;
            let location = self
                .task_library(&task)
                .cloned()
                .context(course2md::tr!("任务保存位置尚未恢复"))?;
            let writes = self
                .workspace
                .as_ref()
                .context(course2md::tr!("任务记录不可用"))?
                .library_writes();
            Ok((request, control, location, writes))
        });
        let (request, control, location, writes) = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                self.fail_task_before_start(&task.id, format!("{error:#}"));
                cx.notify();
                return;
            }
        };
        let read_task = task.clone();
        // All of this touches the library's disk, which may be slow.
        let read = crate::spawn_blocking_io(move || {
            let mut request = request;
            workspace::check_library(&location)?;
            if read_task.plan.options().source_mode != TranscriptSource::Asr
                && read_task.plan.subtitle.is_none()
            {
                request.subtitle_events = workspace::task_subtitle_events(&read_task)?;
            }
            workspace::store_task_subtitles(&read_task)?;
            writes.write_control_now(&read_task.work_dir.join("control.json"), &control)?;
            request.validate()?;
            Ok::<_, anyhow::Error>(request)
        });
        let desktop = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            let request = match read.recv().await {
                Ok(Ok(request)) => request,
                Ok(Err(error)) => {
                    let _ = desktop.update(cx, |this, cx| {
                        this.fail_task_before_start(&task.id, format!("{error:#}"));
                        cx.notify();
                    });
                    return;
                }
                Err(error) => {
                    let _ = desktop.update(cx, |this, cx| {
                        this.fail_task_before_start(&task.id, error.to_string());
                        cx.notify();
                    });
                    return;
                }
            };
            let _ = desktop.update(cx, |this, cx| {
                this.start_task_request(task, request, cx);
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
            .workspace
            .as_ref()
            .and_then(|workspace| workspace.state.task(&task.id))
            .is_some_and(|record| {
                record.state == TaskState::Queued && record.intent == Intent::Run
            });
        if !self.work.is_starting(&task.id)
            || self.work.job.is_some()
            || self.work.closing
            || !current
        {
            self.work.end_start(&task.id);
            return;
        }
        let running = self
            .workspace
            .as_mut()
            .context(course2md::tr!("任务记录不可用"))
            .and_then(|workspace| {
                workspace.transaction(|state| {
                    let record = state
                        .task_mut(&task.id)
                        .context(course2md::tr!("任务不存在"))?;
                    record.state = TaskState::Running;
                    record.events.begin_run();
                    workspace::forget_task_subtitles(record);
                    record.updated = workspace::now();
                    Ok(())
                })
            });
        if let Err(error) = running {
            self.fail_task_before_start(&task.id, format!("{error:#}"));
            cx.notify();
            return;
        }
        // Starting the child and writing the full request to stdin can block on a pipe;
        // keep it off the UI thread.
        let wake = self.event_wake_tx.clone();
        let started = crate::spawn_blocking_io(move || Job::start_task(&request, wake));
        let desktop = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            let job = started.recv().await;
            let _ = desktop.update(cx, |this, cx| match job {
                Ok(Ok(job)) => {
                    this.work.install(&task.id, job);
                    this.work.kind = Kind::Convert;
                    this.work.cancelling = false;
                    this.work.job_started = Some(std::time::Instant::now());
                    this.work.task_status = course2md::tr!("正在生成《{}》", task.plan.title);
                    cx.notify();
                }
                Ok(Err(error)) => {
                    this.fail_task_before_start(&task.id, format!("{error:#}"));
                    cx.notify();
                }
                Err(_) => {
                    this.fail_task_before_start(&task.id, crate::interrupted().to_owned());
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn fail_task_before_start(&mut self, task_id: &str, message: String) {
        self.work.end_start(task_id);
        if let Some(workspace) = &mut self.workspace {
            let compensated = workspace.transaction(|state| {
                let record = state
                    .task_mut(task_id)
                    .context(course2md::tr!("任务不存在"))?;
                record.state = TaskState::NeedsAttention;
                record.events.error = Some(message.clone());
                record.unread = true;
                Ok(())
            });
            if let Err(error) = compensated {
                // Even if recording the compensation fails, the task must not stay Running and be ignored by the
                // scheduler: mark it NeedsAttention in memory too; recover() reconciles with the disk on restart.
                if let Some(record) = workspace.state.task_mut(task_id) {
                    record.state = TaskState::NeedsAttention;
                    record.events.error = Some(message);
                    record.unread = true;
                }
                self.workspace_error =
                    Some(course2md::tr!("任务状态尚未保存：{}", format!("{error:#}")));
            }
        }
    }

    pub fn set_task_intent(&mut self, id: String, intent: Intent, cx: &mut Context<Self>) {
        let active = self.is_running_task(&id);
        if active && intent == Intent::Run {
            self.message = Some(course2md::tr!("当前任务还在处理，请等候当前操作结束。").into());
            cx.notify();
            return;
        }
        let changed = self.transact(course2md::tr!("任务状态尚未保存：{}"), |state| {
            state.set_intent(&id, intent)?;
            if active && intent != Intent::Run {
                state
                    .task_mut(&id)
                    .context(course2md::tr!("任务记录不存在"))?
                    .state = TaskState::Pausing;
            }
            Ok(())
        });
        if changed.is_some() {
            if intent == Intent::Run {
                self.follow_resumed_input_task(&id, cx);
            } else if intent == Intent::Cancel {
                self.import.following_conversion = None;
            }
            if let Some(task) = self
                .workspace
                .as_ref()
                .and_then(|w| w.state.task(&id))
                .cloned()
                && let Err(error) = self.send_task_control(&task)
            {
                self.workspace_error =
                    Some(course2md::tr!("任务指令尚未保存：{}", format!("{error:#}")));
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
            .workspace
            .as_ref()
            .and_then(|workspace| workspace.state.selected_task.clone());
        if previous.as_deref() != Some(id)
            && let Some(previous) = previous
        {
            self.close_task_details(&previous);
        }
        self.transact(course2md::tr!("任务选择尚未保存：{}"), |state| {
            state.selected_task = Some(id.to_owned());
            if let Some(task) = state.task_mut(id) {
                task.unread = false;
            }
            Ok(())
        });
        // Going to a task opens its details when there is something to read
        // or decide; new and running work stays a compact row.
        let waiting = self
            .workspace
            .as_ref()
            .and_then(|workspace| workspace.state.task(id))
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
            self.import.following_conversion = Some(desktop_core::import::ConversionFollow::Task {
                id: id.to_owned(),
                source_revision: self.import.preview_generation,
            });
        } else if self.page == Page::Result && !self.opened.reading {
            self.import.following_conversion = self
                .workspace
                .as_ref()
                .and_then(|workspace| workspace.state.task(id))
                .zip(self.opened.preview.as_ref())
                .and_then(|(task, preview)| {
                    desktop_core::import::ConversionFollow::reader_reprocess(
                        task,
                        &preview.course.dir,
                        self.opened.read_generation,
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
        let Some(task) = self
            .workspace
            .as_ref()
            .and_then(|w| w.state.task(&id))
            .cloned()
        else {
            return;
        };
        if let Some(followup) = &task.handled_by {
            self.select_task(followup, cx);
            return;
        }
        let requests: Vec<_> = task
            .events
            .blocked
            .iter()
            .filter(|b| b.is_uncertain())
            .filter_map(|b| b.request_id.clone())
            .collect();
        if requests.is_empty() {
            return;
        }
        if task.artifact.is_some() {
            let components = task
                .events
                .blocked
                .iter()
                .filter(|b| b.is_uncertain())
                .filter_map(|b| b.purpose.as_deref())
                .map(|purpose| ai_component(purpose).unwrap_or(Component::Proofreading))
                .collect();
            self.reprocess_task(id, components, requests, cx);
            return;
        }
        if self.workspace.is_some()
            && self
                .transact(
                    course2md::tr!("重新发送的选择尚未保存：{}"),
                    |state| state.authorize_uncertain(&id, &requests),
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
        if let Some(id) = self.transact(course2md::tr!("补做任务尚未建立：{}"), |state| {
            state.reprocess(&id, components, resend)
        }) {
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
        if let Some(task) = self
            .workspace
            .as_ref()
            .and_then(|workspace| workspace.state.task(&id))
        {
            if let Some(next) = task.handled_by.clone() {
                self.select_task(&next, cx);
                self.navigate(Page::Task, cx);
                return;
            }
            if task.has_uncertain() {
                self.select_task(&id, cx);
                self.navigate(Page::Task, cx);
                return;
            }
        }
        let components = self
            .workspace
            .as_ref()
            .and_then(|workspace| workspace.state.task(&id))
            .and_then(|task| task.artifact.as_ref().map(|_| task))
            .map(|task| {
                task_component_failures(task)
                    .into_iter()
                    .filter_map(|(component, _, _)| Component::parse(&component))
                    .filter(|component| ai_component(component.as_str()).is_some())
                    .collect::<Vec<_>>()
            })
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
                .workspace
                .as_ref()
                .and_then(|workspace| workspace.state.task(&id))
                .context(course2md::tr!("原任务记录暂时不可用"))?;
            let mut base = task.plan.config.clone();
            base.defaults.transcript_source = Some(course2md::config::TranscriptSource::Subtitle);
            base.llm.enabled = components.contains(&Component::Proofreading);
            base.llm.summarize = components.contains(&Component::Summary);
            // Translation follows the proofread paragraphs it is redone with.
            if !base.llm.enabled {
                base.llm.translate = None;
            }
            let config = self.preferences.config_for_refs(
                &base,
                &ServiceRefs {
                    asr: None,
                    llm: Some(ai_service.clone()),
                },
            )?;
            self.workspace
                .as_mut()
                .context(course2md::tr!("任务记录暂时不可用"))?
                .transaction(|state| {
                    state.reprocess_with_service(
                        &id,
                        components,
                        Vec::new(),
                        Some((ai_service, config)),
                    )
                })
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
                self.workspace_error = Some(course2md::tr!(
                    "服务已保存，补做任务尚未建立：{}",
                    format!("{error:#}")
                ));
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
        let Some(task) = self
            .workspace
            .as_ref()
            .and_then(|workspace| workspace.state.task(&id))
            .cloned()
        else {
            return;
        };
        let events =
            crate::spawn_blocking_io(move || workspace::task_subtitle_events(&task).ok().flatten());
        cx.spawn_in(window, async move |this, cx| {
            let events = events.recv().await.ok().flatten();
            let _ = this.update_in(cx, |this, window, cx| {
                let adjusted = this.transact(
                    course2md::tr!("尚未打开任务调整选项：{}"),
                    |state| state.adjust_task(&id, events),
                );
                if adjusted.is_some() {
                    this.import.following_conversion = None;
                    this.invalidate_source();
                    this.restore_draft(window, cx);
                    this.page = Page::New;
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
            .workspace
            .as_mut()
            .and_then(|workspace| workspace.state.task_mut(id))
            .and_then(|task| task.events.done.take());
        let manifest = done.as_ref().and_then(|d| {
            course2md::artifact::read_manifest(&d.out_dir.join("manifest.json")).ok()
        });
        let Some(workspace) = &mut self.workspace else {
            return;
        };
        let visible =
            self.page == Page::Task && workspace.state.selected_task.as_deref() == Some(id);
        let result = workspace.transaction(|state| {
            let task = state
                .task_mut(id)
                .context(course2md::tr!("任务记录不存在"))?;
            task.updated = workspace::now();
            task.unread = !visible;
            task.events.outcomes = done
                .as_ref()
                .and_then(|done| done.outcomes.clone())
                .or_else(|| manifest.as_ref().map(|manifest| manifest.outcomes.clone()));
            workspace::reconcile_receipts(task)?;
            if let (Some(done), Some(manifest)) = (&done, &manifest) {
                task.artifact = Some(done.out_dir.clone());
                let partial = done.partial.unwrap_or(manifest.partial);
                task.state = if task.has_uncertain() {
                    TaskState::Uncertain
                } else if partial {
                    TaskState::Partial
                } else {
                    TaskState::Complete
                };
                if !partial && task.state != TaskState::Uncertain {
                    task.events.error = None;
                }
            } else {
                task.state = match task.intent {
                    Intent::Cancel => TaskState::Cancelled,
                    Intent::Pause | Intent::Quit => TaskState::Paused,
                    Intent::Run if task.has_uncertain() => TaskState::Uncertain,
                    Intent::Run => TaskState::NeedsAttention,
                };
                if task.events.error.is_none() && !cancelled && task.intent == Intent::Run {
                    task.events.error = Some(
                        if success {
                            course2md::tr!("未生成可读笔记")
                        } else {
                            course2md::tr!("生成中断")
                        }
                        .into(),
                    );
                }
            }
            let record = task.clone();
            if record.state == TaskState::Complete && state.selected_task.as_deref() == Some(id) {
                state.selected_task = None;
            }
            Ok(record)
        });
        match result {
            Ok(task) => {
                if self.reader_viewer_open() {
                    desktop_core::import::ConversionFollow::interrupt_for_reader_viewer(
                        &mut self.import.following_conversion,
                    );
                }
                let follow_input_completion = self
                    .import
                    .following_conversion
                    .as_ref()
                    .zip(self.workspace.as_ref())
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
                let follow_reader_completion = !self.opened.reading
                    && self
                        .import
                        .following_conversion
                        .as_ref()
                        .is_some_and(|follow| {
                            follow.completed_reader_task(
                                &task,
                                self.page,
                                self.opened.read_generation,
                                self.opened
                                    .preview
                                    .as_ref()
                                    .map(|preview| preview.course.dir.as_path()),
                            )
                        });
                if let Some(done) = done {
                    self.work.completed = Some(done.clone());
                    let root = self.task_library(&task).map(|l| l.root.clone());
                    let open = !task.exports_only()
                        && (follow_input_completion || follow_reader_completion);
                    let source = task.plan.source.clone();
                    let folder = task.plan.folder;
                    let out_dir = done.out_dir.clone();
                    let storage_dir = Course::from_completed(&done).storage_dir();
                    let frames = manifest
                        .as_ref()
                        .map(|manifest| manifest.frames.clone())
                        .unwrap_or_default();
                    // Cover copy, folder assignment and thumbnail lookup touch the
                    // library disk; the record above is already committed.
                    let published = crate::spawn_blocking_io(move || {
                        let mut issues = Vec::new();
                        if let Some(root) = &root {
                            if let Err(error) = source::save_cover(&source, &out_dir) {
                                issues
                                    .push(course2md::tr!("封面暂未保存：{}", format!("{error:#}")));
                            }
                            if let Err(error) = organize::Library::edit(root, |library| {
                                library.assign(root, &storage_dir, folder)
                            }) {
                                issues.push(course2md::tr!(
                                    "文件夹归属暂未更新：{}",
                                    format!("{error:#}")
                                ));
                            }
                        }
                        let cover = out_dir.join("cover.jpg");
                        let thumbnail = cover.is_file().then_some(cover).or_else(|| {
                            frames.iter().find_map(|frame| {
                                course2md::artifact::safe_asset_path(&out_dir, &frame.image).ok()
                            })
                        });
                        (issues, thumbnail)
                    });
                    cx.spawn(async move |this, cx| {
                        let Ok((issues, thumbnail)) = published.recv().await else {
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
                                update_published_course(
                                    &mut this.library.courses,
                                    &done,
                                    manifest,
                                    thumbnail,
                                );
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
            Err(error) => {
                self.workspace_error = Some(course2md::tr!(
                    "完成状态尚未保存：{}。已发布的笔记仍保留。",
                    format!("{error:#}")
                ))
            }
        }
        cx.notify();
    }

    pub(crate) fn open_task_export_location(&mut self, id: String, cx: &mut Context<Self>) {
        let destination = (|| {
            let state = &self
                .workspace
                .as_ref()
                .context(course2md::tr!("任务记录暂时不可用"))?
                .state;
            let task = state.task(&id).context(course2md::tr!("任务记录不存在"))?;
            let location = state
                .library(&task.plan.library_id)
                .context(course2md::tr!("任务保存位置暂时不可用"))?;
            workspace::available_task_export(task, location)
        })();
        match destination {
            Ok(path) => cx.reveal_path(&path),
            Err(error) => {
                self.message = Some(course2md::tr!("无法打开导出位置：{}", format!("{error:#}")))
            }
        }
        cx.notify();
    }
}
