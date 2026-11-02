//! Controlled library migration and explicit cleanup of verified old-location backups.
use crate::{
    Desktop,
    course_library::CommandsButton,
    folder_tree::{Command, command_context_menu},
    storage::{self, Phase, Progress},
    symbols,
    ui::{self, AsyncStatus, Resource},
    workspace,
};
use anyhow::{Context as _, Result, ensure};
use desktop_store::locations::*;
use gpui::{prelude::*, *};
use gpui_base::{h_flex, v_flex};
use gpui_kit_material::prelude::*;
use std::{
    path::{Path, PathBuf},
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

/// Progress reports reach the view at most this often.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);
/// How long a move waits for the running task to pause.
const TASK_STOP_TIMEOUT: Duration = Duration::from_secs(30);

impl Desktop {
    pub(super) fn registered_storage_locations(&self) -> Vec<workspace::LibraryLocation> {
        self.workspace
            .as_ref()
            .map(|workspace| workspace.state.libraries.clone())
            .unwrap_or_else(|| {
                // Without a workspace record the root has no registered id; an
                // empty id skips the marker identity check for this placeholder.
                vec![workspace::LibraryLocation {
                    id: String::new(),
                    name: course2md::tr!("笔记库").into(),
                    root: self.library.root.clone(),
                    previous_roots: Vec::new(),
                }]
            })
    }

    pub(super) fn cached_location_check(
        &self,
        location: &workspace::LibraryLocation,
    ) -> Option<&LocationCheck> {
        self.storage_ui.location_checks.get(location)
    }

    pub(super) fn cached_library_access(&self) -> Option<storage::LibraryAccess> {
        self.storage_ui
            .location_checks
            .access(&self.registered_storage_locations())
    }
}

impl Desktop {
    /// An operation that could not start: its dialog, saying why.
    fn storage_refused(&mut self, op: StorageOp, message: String, cx: &mut Context<Self>) {
        self.storage_ui.begin(op);
        self.storage_ui.error = Some(message);
        self.open_storage_dialog(cx);
    }

    /// An operation starting: nothing else may run, and a new generation of its results.
    fn storage_busy(&mut self) -> (u64, Arc<AtomicBool>) {
        let cancel = Arc::new(AtomicBool::new(false));
        self.storage_ui.busy = true;
        self.storage_ui.generation = self.storage_ui.generation.wrapping_add(1);
        self.storage_ui.cancel = Some(cancel.clone());
        (self.storage_ui.generation, cancel)
    }

    /// Asks for a folder off the shell's prompt. A failed prompt reports itself
    /// in the operation dialog; a dismissed one does nothing.
    fn pick_folder(
        &mut self,
        op: StorageOp,
        prompt: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
        then: impl FnOnce(&mut Self, PathBuf, &mut Window, &mut Context<Self>) + 'static,
    ) {
        let answer = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some(prompt.into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let answer = answer.await;
            let _ = this.update_in(cx, |this, window, cx| {
                match answer {
                    Ok(Ok(paths)) => {
                        if let Some(folder) = paths.into_iter().flatten().next() {
                            then(this, folder, window, cx);
                        }
                    }
                    Ok(Err(error)) => this.storage_refused(
                        op,
                        course2md::tr!("无法选择文件夹：{}", format!("{error:#}")),
                        cx,
                    ),
                    Err(error) => this.storage_refused(
                        op,
                        course2md::tr!("无法打开文件选择器：{}", error),
                        cx,
                    ),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Asks which folder the library now lives in, then links it.
    pub fn begin_library_relocation(
        &mut self,
        library_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.storage_ui.busy {
            return;
        }
        self.pick_folder(
            StorageOp::Link(library_id.clone()),
            course2md::tr!("选择这个笔记库移动后的文件夹"),
            window,
            cx,
            move |this, folder, window, cx| this.link_library(library_id, folder, window, cx),
        );
    }

    /// The same folder, after it lost its identity.
    pub fn begin_library_reassociation(
        &mut self,
        library_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(root) = self
            .workspace
            .as_ref()
            .and_then(|workspace| workspace.state.library(&library_id))
            .map(|location| location.root.clone())
        else {
            return;
        };
        self.link_library(library_id, root, window, cx);
    }

    /// Points a library at `folder`, asking first if the folder has no identity to prove it.
    fn link_library(
        &mut self,
        library_id: String,
        folder: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.storage_ui.busy {
            return;
        }
        let op = StorageOp::Link(library_id.clone());
        if self.work.job.is_some() || self.import.preview_workers > 0 {
            let message = course2md::tr!("正在处理内容，结束或停止后再重新定位");
            return self.storage_refused(op, message.into(), cx);
        }
        self.storage_ui.begin(op);
        if !self.save_current_draft(cx) {
            return;
        }
        let Some(state) = self.workspace.as_ref().map(|w| w.state.clone()) else {
            return;
        };
        let (generation, cancel) = self.storage_busy();
        self.storage_ui.progress.message = course2md::tr!("正在核对所选文件夹…").into();
        self.open_storage_dialog(cx);
        cx.spawn_in(window, async move |this, cx| {
            let worker_cancel = cancel.clone();
            let inspected = crate::spawn_blocking_io(move || {
                storage::inspect_link(&state, &library_id, &folder, &worker_cancel)
            })
            .recv()
            .await
            .unwrap_or_else(|_| Err(anyhow::anyhow!(crate::interrupted())));
            let _ = this.update_in(cx, |this, window, cx| match inspected {
                Err(error) => this.finish_link(generation, Err(error), cx),
                Ok(link) => match &link.unmarked {
                    // The dialog stays open and busy, and asks.
                    Some(found) => {
                        let detail = link_confirmation(&link, found);
                        this.storage_ui.cancel = None;
                        this.storage_ui.link_question = Some((link, detail));
                        cx.notify();
                    }
                    None => this.publish_library_link(link, generation, cancel, window, cx),
                },
            });
        })
        .detach();
        cx.notify();
    }

    /// The disk half off the UI thread, then the record, then what follows the library.
    fn publish_library_link(
        &mut self,
        link: storage::LibraryLink,
        generation: u64,
        cancel: Arc<AtomicBool>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.spawn_in(window, async move |this, cx| {
            let (link, finished) = crate::spawn_blocking_io(move || {
                let tasks = storage::finish_link(&link);
                (link, tasks)
            })
            .recv()
            .await
            .unwrap_or_else(|_| unreachable!("the blocking pool outlives this task"));
            let _ = this.update_in(cx, |this, window, cx| {
                let result = finished.and_then(|tasks| {
                    ensure!(
                        !this.work.closing && !cancel.load(Ordering::Relaxed),
                        course2md::tr!("已取消重新定位")
                    );
                    ensure!(
                        this.work.job.is_none() && this.import.preview_workers == 0,
                        course2md::tr!("内容仍在处理中，请停止后重新定位")
                    );
                    let workspace = this.workspace.as_mut().context(registry_unavailable())?;
                    workspace.transaction(|state| storage::publish_link(state, &link, tasks))?;
                    Ok(link)
                });
                let linked = result.is_ok();
                if let Ok(link) = &result {
                    this.apply_link(link, window, cx);
                }
                this.finish_link(generation, result.map(|_| ()), cx);
                if linked && this.storage_ui.error.is_none() {
                    this.storage_ui.dialog_open = false;
                }
            });
        })
        .detach();
    }

    /// What follows a library to its new folder outside the record.
    fn apply_link(
        &mut self,
        link: &storage::LibraryLink,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.library.root == link.previous_root {
            self.library.root = link.folder.clone();
        }
        self.opened.stop_reading();
        if let Some(preview) = self.opened.preview.as_mut() {
            let was_inside = preview.course.dir.starts_with(&link.previous_root);
            relocate_preview(Rc::make_mut(preview), &link.previous_root, &link.folder);
            if was_inside {
                self.restore_reading_position(cx);
            }
        }
        let settings = self.relocate_settings_paths(&link.previous_root, &link.folder, cx);
        self.storage_ui.error = settings.err().map(|error| {
            course2md::tr!(
                "笔记库已重新定位，模型位置尚未保存。请在生成笔记设置中重试保存：{}",
                format!("{error:#}")
            )
        });
        self.workspace_error = None;
        self.restore_draft(window, cx);
        self.message = Some(course2md::tr!("已重新定位到 {}", link.folder.display()));
    }

    /// Back to idle after an operation: others may run again.
    fn storage_idle(&mut self) {
        self.storage_ui.busy = false;
        self.wake_poll();
        self.storage_ui.cancel = None;
    }

    fn finish_link(&mut self, generation: u64, result: Result<()>, cx: &mut Context<Self>) {
        if self.storage_ui.generation != generation {
            return;
        }
        self.storage_idle();
        self.storage_ui.progress = Progress::default();
        if let Err(error) = result {
            self.storage_ui.error = Some(course2md::tr!("尚未重新定位：{}", format!("{error:#}")));
        }
        self.refresh_library(cx);
        self.start_next_task(cx);
        cx.notify();
    }

    /// Reads the move records on a blocking thread and applies what they say.
    pub fn restore_storage_state(&mut self, cx: &mut Context<Self>) {
        let directory = self.preferences.root().join("storage");
        let (libraries, backups) = self
            .workspace
            .as_ref()
            .map(|workspace| {
                (
                    workspace
                        .state
                        .libraries
                        .iter()
                        .map(|library| (library.id.clone(), library.root.clone()))
                        .collect::<Vec<_>>(),
                    workspace.state.storage_backups.clone(),
                )
            })
            .unwrap_or_default();
        self.storage_ui.records_generation = self.storage_ui.records_generation.wrapping_add(1);
        let generation = self.storage_ui.records_generation;
        let task = crate::spawn_blocking_io(move || {
            storage::read_records(&directory, &libraries, &backups)
        });
        cx.spawn(async move |this, cx| {
            let Ok(records) = task.recv().await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                if this.storage_ui.records_generation == generation {
                    this.apply_storage_records(records, cx);
                }
            });
        })
        .detach();
    }

    fn apply_storage_records(&mut self, records: storage::Records, cx: &mut Context<Self>) {
        self.storage_ui.pending = records.pending.clone();
        self.storage_ui.corrupt_records = records.corrupt.clone();
        // Model paths follow a moved library. Repeating this after a crash is harmless,
        // and once they follow there is nothing left to do.
        let moved: Vec<_> = self
            .workspace
            .iter()
            .flat_map(|workspace| workspace.state.storage_backups.iter())
            .filter(|backup| self.preferences.references_storage_path(&backup.path))
            .map(|backup| (backup.path.clone(), backup.current_root.clone()))
            .collect();
        for (old, new) in moved {
            if let Err(error) = self.relocate_settings_paths(&old, &new, cx) {
                self.storage_ui.error = Some(course2md::tr!(
                    "笔记库已移动，模型位置尚未保存。旧位置备份已保留：{}",
                    format!("{error:#}")
                ))
            }
        }
        if !records.cleaned.is_empty()
            && let Some(workspace) = &mut self.workspace
            && workspace
                .transaction(|state| {
                    state
                        .storage_backups
                        .retain(|backup| !records.cleaned.contains(&backup.id));
                    Ok(())
                })
                .is_err()
        {
            self.storage_ui.error = Some(backup_list_stale().into());
        }
        crate::spawn_blocking_io(move || storage::settle_records(&records));
        cx.notify();
    }

    fn abandon_library_move(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let task = crate::spawn_blocking_io(move || storage::abandon_move(&path));
        cx.spawn(async move |this, cx| {
            let result = task
                .recv()
                .await
                .unwrap_or_else(|_| Err(anyhow::anyhow!(crate::interrupted())));
            let _ = this.update(cx, |this, cx| {
                if let Err(error) = result {
                    this.storage_ui.error =
                        Some(course2md::tr!("尚未放弃这次移动：{}", format!("{error:#}")));
                }
                this.restore_storage_state(cx);
            });
        })
        .detach();
    }

    fn set_default_library(&mut self, id: String, cx: &mut Context<Self>) {
        let Some(root) = self
            .workspace
            .as_ref()
            .and_then(|workspace| workspace.state.library(&id))
            .map(|library| library.root.clone())
        else {
            self.workspace_error = Some(course2md::tr!("这个笔记库已被移除").into());
            cx.notify();
            return;
        };
        let probe =
            crate::spawn_blocking_io(move || tempfile::NamedTempFile::new_in(&root).map(drop));
        cx.spawn(async move |this, cx| {
            let writable = matches!(probe.recv().await, Ok(Ok(())));
            let _ = this.update(cx, |this, cx| {
                let Some(workspace) = &mut this.workspace else {
                    return;
                };
                let result = if writable {
                    workspace.transaction(|state| {
                        state.default_library = id.clone();
                        Ok(())
                    })
                } else {
                    Err(anyhow::anyhow!(course2md::tr!(
                        "这个文件夹暂时不能写入，请重新连接磁盘或检查访问权限"
                    )))
                };
                match result {
                    Ok(()) => this.message = Some(course2md::tr!("已设为默认保存位置").into()),
                    Err(error) => {
                        this.workspace_error =
                            Some(course2md::tr!("默认位置尚未更改：{}", format!("{error:#}")))
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub fn poll_storage(&mut self, cx: &mut Context<Self>) {
        if !self.storage_ui.busy
            && self.work.job.is_none()
            && let Some(id) = self.storage_ui.resume_task.take()
        {
            let can_resume = self
                .workspace
                .as_ref()
                .and_then(|workspace| workspace.state.task(&id))
                .is_some_and(|task| {
                    task.state == workspace::TaskState::Paused
                        && task.intent == workspace::Intent::Pause
                });
            if can_resume {
                self.set_task_intent(id, workspace::Intent::Run, cx);
            }
        }
    }

    pub fn cancel_storage_for_close(&mut self) {
        self.storage_ui.request_cancel();
    }

    pub fn begin_library_move(
        &mut self,
        library_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.storage_ui.busy {
            return;
        }
        let Some(location) = self
            .workspace
            .as_ref()
            .and_then(|workspace| workspace.state.library(&library_id))
            .cloned()
        else {
            return;
        };
        self.pick_folder(
            StorageOp::Move,
            course2md::tr!("选择空文件夹作为笔记库的新位置"),
            window,
            cx,
            move |_, destination, window, cx| {
                let root = location.root.clone();
                // canonicalize and read_dir can stall on a network or removable disk
                let checked = crate::spawn_blocking_io(move || {
                    storage::validate_destination(&root, &destination)
                });
                cx.spawn_in(window, async move |this, cx| {
                    let checked = checked
                        .recv()
                        .await
                        .unwrap_or_else(|_| Err(anyhow::anyhow!(crate::interrupted())));
                    let _ = this.update_in(cx, |this, window, cx| {
                        match checked {
                            Ok((source, destination)) => this.start_library_move(
                                location.id.clone(),
                                source,
                                destination,
                                None,
                                window,
                                cx,
                            ),
                            Err(error) => {
                                this.storage_refused(StorageOp::Move, format!("{error:#}"), cx)
                            }
                        }
                        cx.notify();
                    });
                })
                .detach();
            },
        );
    }

    fn start_library_move(
        &mut self,
        library_id: String,
        source: PathBuf,
        destination: PathBuf,
        resume: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.storage_ui.busy || !self.save_current_draft(cx) {
            return;
        }
        let op = StorageOp::Move;
        let Some(state) = self
            .workspace
            .as_ref()
            .map(|workspace| workspace.state.clone())
        else {
            return self.storage_refused(op, registry_unavailable().into(), cx);
        };
        // Only a conversion task can pause and resume around a move.
        if self.work.job.is_some() && self.work.active_task.is_none() {
            let message = course2md::tr!("模型下载完成或停止后再移动笔记库");
            return self.storage_refused(op, message.into(), cx);
        }
        self.storage_ui.begin(op);
        let (generation, cancel) = self.storage_busy();
        let job_exit = self.work.job.is_some().then(|| {
            let (exit, exited) = smol::channel::bounded(1);
            self.storage_ui.job_exit = Some(exit);
            self.storage_ui.progress.message = course2md::tr!("正在暂停当前任务…").into();
            exited
        });
        if let Some(id) = self.work.active_task.clone() {
            let running = state
                .task(&id)
                .is_some_and(|task| task.intent == workspace::Intent::Run);
            if running {
                self.storage_ui.resume_task = Some(id.clone());
                self.set_task_intent(id, workspace::Intent::Pause, cx);
            }
        }
        self.open_storage_dialog(cx);
        let journal_dir = self.preferences.root().join("storage");
        cx.spawn_in(window, async move |this, cx| {
            if let Some(exited) = job_exit {
                let timeout = cx.background_executor().timer(TASK_STOP_TIMEOUT);
                let stopped = smol::future::or(async { exited.recv().await.is_ok() }, async {
                    timeout.await;
                    false
                })
                .await;
                if !stopped {
                    let message = if cancel.load(Ordering::Relaxed) {
                        course2md::tr!("已取消移动")
                    } else {
                        course2md::tr!("当前任务仍在保存进度，停止后再移动笔记库")
                    };
                    let _ =
                        this.update(cx, |this, cx| this.finish_storage_error(message.into(), cx));
                    return;
                }
            }
            let (report, reports) = smol::channel::unbounded();
            let worker_cancel = cancel.clone();
            // Moving is a large amount of blocking file I/O; see crate::spawn_blocking_io.
            let worker = crate::spawn_blocking_io(move || {
                let sent = std::cell::Cell::new(None::<Instant>);
                let report = |value: Progress| {
                    let now = Instant::now();
                    let due = sent
                        .get()
                        .is_none_or(|at| now.duration_since(at) >= PROGRESS_INTERVAL);
                    if due || value.completed == value.total {
                        sent.set(Some(now));
                        let _ = report.try_send(value);
                    }
                };
                storage::check_overlap(&state, &library_id, &destination)?;
                match resume {
                    Some(path) => storage::resume_move(&path, &worker_cancel, &report),
                    None => storage::prepare_move(
                        library_id,
                        &source,
                        &destination,
                        &journal_dir,
                        &worker_cancel,
                        &report,
                    ),
                }
            });
            // The report channel closes when the worker returns.
            while let Ok(snapshot) = reports.recv().await {
                let _ = this.update(cx, |this, cx| {
                    if this.storage_ui.generation == generation
                        && this.storage_ui.progress != snapshot
                    {
                        this.storage_ui.progress = snapshot;
                        cx.notify();
                    }
                });
            }
            let result = worker
                .recv()
                .await
                .unwrap_or_else(|_| Err(anyhow::anyhow!(crate::interrupted())));
            let _ = this.update_in(cx, |this, window, cx| {
                if this.storage_ui.generation != generation {
                    return;
                }
                match result {
                    Ok(mut prepared) => {
                        if this.work.closing || cancel.load(Ordering::Relaxed) {
                            this.finish_storage_error(
                                course2md::tr!("副本已核验，保存位置尚未切换").into(),
                                cx,
                            );
                            return;
                        }
                        let committed = this
                            .workspace
                            .as_mut()
                            .context(registry_unavailable())
                            .and_then(|workspace| {
                                workspace
                                    .transaction(|state| storage::publish_move(state, &prepared))
                            });
                        if let Err(error) = committed {
                            this.finish_storage_error(
                                course2md::tr!(
                                    "副本已核验，保存位置尚未切换：{}",
                                    format!("{error:#}")
                                ),
                                cx,
                            );
                            return;
                        }
                        if this.library.root == prepared.journal.source {
                            this.library.root = prepared.journal.destination.clone();
                        }
                        prepared.journal.phase = Phase::Committed;
                        this.storage_ui.error = None;
                        if let Err(error) = this.relocate_settings_paths(
                            &prepared.journal.source,
                            &prepared.journal.destination,
                            cx,
                        ) {
                            this.storage_ui.error = Some(course2md::tr!(
                                "笔记库已移动，模型位置尚未保存。旧位置备份已保留：{}",
                                format!("{error:#}")
                            ))
                        }
                        // A record left behind is marked committed at the next start.
                        let _ = storage::save_journal(&prepared.path, &prepared.journal);
                        this.storage_idle();
                        this.storage_ui.progress = Progress {
                            message: course2md::tr!(
                                "笔记库已移动到 {}。旧位置保留为已核验备份。",
                                prepared.journal.destination.display()
                            ),
                            completed: 1,
                            total: 1,
                        };
                        this.restore_storage_state(cx);
                        this.restore_draft(window, cx);
                        this.refresh_library(cx);
                        this.poll_storage(cx);
                        this.start_next_task(cx);
                    }
                    Err(error) => this.finish_storage_error(
                        course2md::tr!(
                            "{}。保存位置尚未切换，已复制的内容保留在目标位置。",
                            format!("{error:#}")
                        ),
                        cx,
                    ),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn finish_storage_error(&mut self, message: String, cx: &mut Context<Self>) {
        self.storage_idle();
        self.storage_ui.error = Some(message);
        self.restore_storage_state(cx);
        self.refresh_library(cx);
        self.poll_storage(cx);
        self.start_next_task(cx);
        cx.notify();
    }

    pub(crate) fn open_storage_dialog(&mut self, cx: &mut Context<Self>) {
        self.storage_ui.dialog_open = true;
        cx.notify();
    }

    /// Capture builds: one shot shows one dialog.
    #[cfg(feature = "capture")]
    pub(crate) fn close_storage_dialog(&mut self) {
        self.storage_ui.dialog_open = false;
    }

    /// Capture builds: a real operation on the first library, or the dialog at rest.
    #[cfg(feature = "capture")]
    pub(crate) fn capture_storage_operation(
        &mut self,
        operation: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(library) = self
            .workspace
            .as_ref()
            .and_then(|workspace| workspace.state.libraries.first().cloned())
        else {
            return self.open_storage_dialog(cx);
        };
        match operation.split_once(':') {
            Some(("move", folder)) => {
                match storage::validate_destination(&library.root, Path::new(folder)) {
                    Ok((source, destination)) => {
                        self.start_library_move(library.id, source, destination, None, window, cx)
                    }
                    Err(error) => self.storage_refused(StorageOp::Move, format!("{error:#}"), cx),
                }
            }
            _ if operation == "link" => self.begin_library_reassociation(library.id, window, cx),
            _ => self.open_storage_dialog(cx),
        }
    }

    /// The operation dialog: its work, a question, or how it ended. Rendered by the shell.
    pub(crate) fn storage_dialog(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let storage = &self.storage_ui;
        let cancelling = storage
            .cancel
            .as_ref()
            .is_some_and(|cancel| cancel.load(Ordering::Relaxed));
        let dialog = MaterialDialog::new("storage-dialog")
            .container(ColorRole::SURFACE)
            .headline(storage.title())
            .width(if is_compact(window) { 320. } else { 560. })
            .dismiss_on_scrim(false)
            .dismiss_on_escape(false)
            .modal(storage.dialog_open);
        if let Some((_, detail)) = &storage.link_question {
            return dialog
                .headline(course2md::tr!("重新关联此保存位置？"))
                .supporting_text(detail.clone())
                .action(
                    material_button("decline-library-link", ButtonVariant::Text)
                        .child(course2md::tr!("取消"))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.storage_ui.link_question = None;
                            this.storage_ui.dialog_open = false;
                            this.storage_ui.progress = Progress::default();
                            this.storage_idle();
                            this.start_next_task(cx);
                            cx.notify();
                        })),
                )
                .action(
                    material_button("accept-library-link", ButtonVariant::Filled)
                        .child(course2md::tr!("重新关联"))
                        .on_click(cx.listener(|this, _, window, cx| {
                            if let Some((link, _)) = this.storage_ui.link_question.take() {
                                let generation = this.storage_ui.generation;
                                let cancel = Arc::new(AtomicBool::new(false));
                                this.publish_library_link(link, generation, cancel, window, cx);
                                cx.notify();
                            }
                        })),
                )
                .into_any_element();
        }
        let message = (!storage.progress.message.is_empty())
            .then(|| SharedString::from(storage.progress.message.clone()));
        let status = if storage.busy {
            AsyncStatus::Busy(if cancelling {
                course2md::tr!("正在结束操作…").into()
            } else {
                message.unwrap_or_else(|| course2md::tr!("正在处理文件…").into())
            })
        } else if let Some(error) = &storage.error {
            AsyncStatus::Failed(error.clone().into())
        } else {
            AsyncStatus::Success(message.unwrap_or_else(|| course2md::tr!("操作已完成").into()))
        };
        let fraction = (storage.busy && storage.progress.total > 0)
            .then(|| storage.progress.completed as f32 / storage.progress.total as f32);
        let dialog = dialog.content(
            v_flex()
                .w_full()
                .min_w_0()
                .px(rem(24.))
                .gap(rem(16.))
                .children(ui::async_status("storage-operation", &status, None, cx))
                .when_some(fraction, |view, fraction| {
                    view.child(progress_with_label(
                        "storage-progress",
                        storage.title(),
                        fraction,
                        window,
                        cx,
                    ))
                }),
        );
        if storage.busy {
            return dialog
                .action(
                    material_button("cancel-library-move", ButtonVariant::Text)
                        .disabled(cancelling)
                        .child(if cancelling {
                            course2md::tr!("正在结束…")
                        } else {
                            storage.cancel_label()
                        })
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.storage_ui.request_cancel();
                            cx.notify();
                        })),
                )
                .into_any_element();
        }
        let retry = match &storage.op {
            StorageOp::Link(id) if storage.error.is_some() && storage.progress.completed == 0 => {
                Some(id.clone())
            }
            _ => None,
        };
        dialog
            .action(
                material_button("close-storage-operation", ButtonVariant::Text)
                    .child(course2md::tr!("关闭"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.storage_ui.dialog_open = false;
                        cx.notify();
                    })),
            )
            .when_some(retry, |dialog, id| {
                dialog.action(
                    material_button("retry-library-relocation", ButtonVariant::Filled)
                        .child(course2md::tr!("重新选择文件夹…"))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.storage_ui.dialog_open = false;
                            this.begin_library_relocation(id.clone(), window, cx);
                        })),
                )
            })
            .into_any_element()
    }

    /// What needs attention above the locations; healthy storage shows nothing.
    fn storage_status_panel(&self, cx: &mut Context<Self>) -> Option<Div> {
        let storage = &self.storage_ui;
        let mut lines: Vec<(&str, AsyncStatus)> = Vec::new();
        if matches!(storage.op, StorageOp::Link(_))
            && !storage.busy
            && storage.error.is_none()
            && storage.progress.completed > 0
            && storage.progress.completed == storage.progress.total
        {
            lines.push((
                "storage-relocation-success",
                AsyncStatus::Success(storage.progress.message.clone().into()),
            ));
        }
        match self.cached_library_access() {
            None => lines.push((
                "storage-locations-checking",
                AsyncStatus::Busy(course2md::tr!("正在检查保存位置…").into()),
            )),
            Some(access) if access.coverage() == storage::LibraryCoverage::Unavailable => lines
                .push((
                    "storage-access-coverage",
                    AsyncStatus::Warning(course2md::tr!("所有保存位置都暂时无法访问").into()),
                )),
            Some(_) => {}
        }
        if let Some(error) = &storage.error {
            lines.push((
                "storage-status-error",
                AsyncStatus::Failed(error.clone().into()),
            ));
        }
        if !storage.corrupt_records.is_empty() {
            lines.push((
                "storage-corrupt-records",
                AsyncStatus::Failed(
                    course2md::tr!(
                        "有 {} 次未完成的移动记录已损坏，相关文件均已保留",
                        storage.corrupt_records.len()
                    )
                    .into(),
                ),
            ));
        }
        let corrupt = (!storage.corrupt_records.is_empty()).then(|| {
            let paths = storage
                .corrupt_records
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join("\n");
            TileScale::Regular
                .support("storage-corrupt-paths", paths)
                .pl(ICON_SLOT + ICON_GAP)
        });
        let pending = (!storage.pending.is_empty()).then(|| {
            storage.pending.iter().enumerate().fold(
                TileGroup::new("storage-pending", course2md::tr!("未完成的移动")),
                |group, (index, (path, journal))| {
                    let (resume, abandon, journal) = (path.clone(), path.clone(), journal.clone());
                    let description = format!(
                        "{} → {}",
                        journal.source.display(),
                        journal.destination.display()
                    );
                    let destination = journal.destination.clone();
                    group.child(storage_item(
                        ("storage-pending-description", index),
                        description,
                        [
                            material_button(("resume-library-move", index), ButtonVariant::Tonal)
                                .icon(symbols::play_arrow())
                                .child(course2md::tr!("继续移动"))
                                .disabled(storage.busy)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.start_library_move(
                                        journal.library_id.clone(),
                                        journal.source.clone(),
                                        journal.destination.clone(),
                                        Some(resume.clone()),
                                        window,
                                        cx,
                                    )
                                })),
                            material_button(("open-move-copy", index), ButtonVariant::Text)
                                .icon(symbols::folder_open())
                                .child(course2md::tr!("打开目标副本"))
                                .on_click(move |_, _, cx| cx.open_with_system(&destination)),
                            material_button(("abandon-library-move", index), ButtonVariant::Text)
                                .icon(symbols::close())
                                .child(course2md::tr!("放弃移动，保留副本"))
                                .disabled(storage.busy)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.abandon_library_move(abandon.clone(), cx)
                                })),
                        ],
                    ))
                },
            )
        });
        let backups = self
            .workspace
            .as_ref()
            .map(|workspace| workspace.state.storage_backups.as_slice())
            .filter(|backups| !backups.is_empty())
            .map(|backups| {
                backups.iter().enumerate().fold(
                    TileGroup::new("storage-backups", course2md::tr!("旧位置备份")),
                    |group, (index, backup)| {
                        let (id, path) = (backup.id.clone(), backup.path.clone());
                        group.child(storage_item(
                            ("storage-backup-description", index),
                            backup.path.display().to_string(),
                            [
                                material_button(
                                    ("open-library-backup", index),
                                    ButtonVariant::Text,
                                )
                                .icon(symbols::folder_open())
                                .child(course2md::tr!("打开备份位置"))
                                .on_click(move |_, _, cx| cx.open_with_system(&path)),
                                material_button(
                                    ("cleanup-library-backup", index),
                                    ButtonVariant::Text,
                                )
                                .icon(symbols::delete())
                                .child(course2md::tr!("清理旧位置备份…"))
                                .disabled(storage.busy)
                                .on_click(cx.listener(
                                    move |this, _, window, cx| {
                                        this.begin_backup_cleanup(id.clone(), window, cx)
                                    },
                                )),
                            ],
                        ))
                    },
                )
            });
        (!lines.is_empty() || pending.is_some() || backups.is_some()).then(|| {
            v_flex()
                .w_full()
                .min_w_0()
                .flex_shrink_0()
                .gap(rem(16.))
                .children(
                    lines
                        .into_iter()
                        .filter_map(|(id, status)| ui::async_status(id, &status, None, cx)),
                )
                .children(corrupt)
                .children(pending)
                .children(backups)
        })
    }

    /// A location's commands, for its menu and its right-click.
    fn location_actions(
        &self,
        library: &workspace::LibraryLocation,
        repair: bool,
        offline: bool,
        default: bool,
        cx: &mut Context<Self>,
    ) -> Vec<Command> {
        let busy = self.storage_ui.busy;
        let desktop = cx.weak_entity();
        let root = library.root.clone();
        let (default_id, move_id) = (library.id.clone(), library.id.clone());
        vec![
            Command::global(
                course2md::tr!("打开位置"),
                symbols::folder_open(),
                move |_, cx| open_location(&root, cx),
            )
            .disabled(offline)
            .ends_group(),
            Command::new(
                if default {
                    course2md::tr!("已是默认保存位置")
                } else {
                    course2md::tr!("设为默认保存位置")
                },
                symbols::check_circle(),
                &desktop,
                move |this, _, cx| this.set_default_library(default_id.clone(), cx),
            )
            .disabled(default || repair || busy),
            Command::new(
                course2md::tr!("移动笔记库…"),
                symbols::database(),
                &desktop,
                move |this, window, cx| this.begin_library_move(move_id.clone(), window, cx),
            )
            .disabled(repair || busy),
        ]
    }

    /// One location: its name and state, its path, what is wrong and how to
    /// repair it, and its actions.
    fn location_row(
        &self,
        index: usize,
        library: &workspace::LibraryLocation,
        several: bool,
        default: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let busy = self.storage_ui.busy;
        let check = self.cached_location_check(library);
        let offline = check.is_some_and(|check| !check.available);
        let relocate = check.is_some_and(|check| !check.available || check.problem.is_some());
        let reassociate = check.is_some_and(|check| check.needs_reassociation);
        let repair = reassociate || relocate;
        let path = course2md::config::simplify_verbatim(library.root.clone());
        let actions = self.location_actions(library, repair, offline, default, cx);
        let more = CommandsButton {
            id: ("storage-location-menu", index).into(),
            label: course2md::tr!("笔记库操作").into(),
            subject: Some(course2md::tr!("{} 操作", library.name).into()),
            commands: actions.clone(),
            tonal: false,
        };
        let (id, reassociate_id) = (library.id.clone(), library.id.clone());
        let repair_button = |name: &'static str, variant, icon, label: &'static str| {
            material_button((name, index), variant)
                .when(variant == ButtonVariant::Outlined, |button| {
                    button.tone(ButtonTone::Accent)
                })
                .icon(icon)
                .child(label)
                .disabled(busy)
        };
        let repairs = repair.then(|| {
            h_flex()
                .min_w_0()
                .pt(rem(4.))
                .gap(rem(8.))
                .flex_wrap()
                .when(reassociate, |row| {
                    row.child(
                        repair_button(
                            "reassociate-storage-location",
                            ButtonVariant::Tonal,
                            symbols::database(),
                            course2md::tr!("重新关联…"),
                        )
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                this.begin_library_reassociation(reassociate_id.clone(), window, cx)
                            },
                        )),
                    )
                })
                .when(relocate, |row| {
                    row.child(
                        repair_button(
                            "relocate-storage-location",
                            if reassociate {
                                ButtonVariant::Outlined
                            } else {
                                ButtonVariant::Tonal
                            },
                            symbols::folder_open(),
                            course2md::tr!("重新定位…"),
                        )
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                this.begin_library_relocation(id.clone(), window, cx)
                            },
                        )),
                    )
                })
        });
        let problem = check
            .and_then(|check| check.problem.clone())
            .map(|problem| {
                (
                    "storage-location-problem",
                    AsyncStatus::Failed(problem.into()),
                )
            });
        let unconfirmed = reassociate.then(|| {
            let message = course2md::tr!("无法确认这是原来的笔记库，请检查后重新关联");
            (
                "storage-association-needed",
                AsyncStatus::Warning(message.into()),
            )
        });
        let notes = problem
            .into_iter()
            .chain(unconfirmed)
            .filter_map(|(id, status)| ui::async_status((id, index), &status, None, cx));
        let open_root = library.root.clone();
        let open = material_button(("open-storage-location", index), ButtonVariant::Tonal)
            .icon(symbols::folder_open())
            .child(course2md::tr!("打开位置"))
            .disabled(offline)
            .on_click(move |_, _, cx| open_location(&open_root, cx));
        let status = [
            (default && several).then_some((StatusKind::Tonal, course2md::tr!("默认保存位置"))),
            offline.then_some((StatusKind::Warning, course2md::tr!("暂时不可访问"))),
        ];
        let below = notes
            .map(IntoElement::into_any_element)
            .chain(repairs.map(IntoElement::into_any_element))
            .chain((default && several).then(|| {
                tile_info(
                    "storage-policy",
                    course2md::tr!("更改默认位置只影响后续生成的笔记"),
                )
                .into_any_element()
            }))
            .collect();
        let row = Resource {
            id: format!("storage-location-row-{index}").into(),
            icon: symbols::folder().into(),
            tint: Tint::Amber,
            mono: true,
            title: library.name.clone().into(),
            status: status.into_iter().flatten().collect(),
            busy: false,
            lines: vec![(path.display().to_string(), false)],
            below,
            actions: vec![open.into_any_element(), more.into_any_element()],
        }
        .row(cx);
        div()
            .id(("storage-location", index))
            .relative()
            .w_full()
            .min_w_0()
            .px(ITEM_INSET)
            .py(rem(16.))
            .child(command_context_menu(
                ("storage-location-context", index),
                actions,
            ))
            .child(row)
            .into_any_element()
    }

    /// Storage locations listed in the settings page; per-location operations
    /// live in this module alongside the migration flow.
    pub(super) fn storage_settings_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut locations =
            TileGroup::new("storage-locations-heading", course2md::tr!("笔记库位置"));
        if let Some(workspace) = &self.workspace {
            // The default is only a choice once there is more than one location.
            let several = workspace.state.libraries.len() > 1;
            for (index, library) in workspace.state.libraries.iter().enumerate() {
                let default = workspace.state.default_library == library.id;
                locations =
                    locations.child(self.location_row(index, library, several, default, cx));
            }
        }
        let add = cx.listener(|this, _, window, cx| this.register_storage_location(window, cx));
        let locations = locations.tile(move |outer| {
            ui::add_row(
                "register-storage-location",
                course2md::tr!("添加笔记库位置"),
                None,
                false,
                outer,
                add,
            )
        });
        v_flex()
            .w_full()
            .min_w_0()
            .flex_shrink_0()
            .gap(rem(28.))
            .children(self.storage_status_panel(cx))
            .child(locations)
            .into_any_element()
    }

    fn begin_backup_cleanup(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.storage_ui.busy {
            return;
        }
        let Some(backup) = self
            .workspace
            .as_ref()
            .and_then(|workspace| {
                workspace
                    .state
                    .storage_backups
                    .iter()
                    .find(|backup| backup.id == id)
            })
            .cloned()
        else {
            return;
        };
        if self.preferences.references_storage_path(&backup.path) {
            let message =
                course2md::tr!("模型仍在使用旧位置，请先在「生成笔记」中保存模型位置，再清理备份");
            return self.storage_refused(StorageOp::Cleanup, message.into(), cx);
        }
        let consequence = course2md::tr!(
            "将删除 {} 中的旧位置备份。当前笔记库仍保留在 {}。",
            backup.path.display(),
            backup.current_root.display()
        );
        ui::Confirm::new(
            course2md::tr!("清理旧位置备份？"),
            course2md::tr!("保留备份"),
            course2md::tr!("清理备份"),
        )
        .danger()
        .text(consequence)
        .ask(
            cx.listener(move |this, _, window, cx| this.cleanup_backup(backup.clone(), window, cx)),
            window,
            cx,
        );
    }

    fn cleanup_backup(
        &mut self,
        backup: storage::BackupRecord,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.storage_ui.busy {
            return;
        }
        self.storage_ui.begin(StorageOp::Cleanup);
        let (_, cancel) = self.storage_busy();
        let worker_cancel = cancel.clone();
        self.storage_ui.progress.message = course2md::tr!("正在清理备份…").into();
        self.open_storage_dialog(cx);
        let roots = self
            .workspace
            .iter()
            .flat_map(|workspace| &workspace.state.libraries)
            .map(|library| library.root.clone())
            .collect::<Vec<_>>();
        let id = backup.id.clone();
        // Cleaning up deletes files synchronously; see crate::spawn_blocking_io.
        let worker = crate::spawn_blocking_io(move || {
            storage::cleanup_backup(&backup, &roots, &worker_cancel)
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = worker
                .recv()
                .await
                .unwrap_or_else(|_| Err(anyhow::anyhow!(crate::interrupted())));
            let _ = this.update(cx, |this, cx| {
                this.storage_idle();
                this.storage_ui.progress = Progress::default();
                match result {
                    Ok(()) => {
                        if let Some(workspace) = &mut this.workspace {
                            match workspace.transaction(|state| {
                                state.storage_backups.retain(|backup| backup.id != id);
                                Ok(())
                            }) {
                                Ok(()) => {
                                    this.storage_ui.progress.message =
                                        course2md::tr!("旧位置备份已清理").into();
                                }
                                Err(_) => this.storage_ui.error = Some(backup_list_stale().into()),
                            }
                        }
                    }
                    Err(_) if cancel.load(Ordering::Relaxed) => {
                        this.storage_ui.error = Some(course2md::tr!("已取消清理").into())
                    }
                    Err(error) => this.storage_ui.error = Some(format!("{error:#}")),
                }
                this.refresh_library(cx);
                this.start_next_task(cx);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}

fn open_location(root: &Path, cx: &mut App) {
    cx.open_with_system(&course2md::config::simplify_verbatim(root.to_owned()));
}

/// A resource row of the status panel: what it is, then what can be done with it.
fn storage_item<const N: usize>(
    id: impl Into<ElementId>,
    description: impl Into<SharedString>,
    actions: [MaterialButton; N],
) -> Div {
    v_flex()
        .w_full()
        .min_w_0()
        .gap(rem(8.))
        .px(ITEM_INSET)
        .py(rem(16.))
        .child(TileScale::Regular.support(id, description))
        .child(
            h_flex()
                .min_w_0()
                .gap(rem(8.))
                .flex_wrap()
                .children(actions),
        )
}
