//! Controlled library migration and explicit cleanup of verified old-location backups.
use crate::{
    Desktop,
    storage::{self, Journal, Phase, Progress},
    symbols,
    ui::{self, AsyncStatus},
    workspace,
};
use anyhow::{Context as _, Result, ensure};
use gpui::{prelude::*, *};
use gpui_base::{h_flex, v_flex};
use gpui_kit::component::menu::{ContextMenuExt, PopupMenu, PopupMenuItem};
use gpui_kit_material::{
    icons::{Icon, SymbolIcon},
    prelude::*,
};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

/// The storage operation the dialog and status panel describe.
#[derive(Clone, Default, PartialEq)]
enum StorageOp {
    #[default]
    Move,
    /// Pointing a library id at the folder it now lives in: moved outside the
    /// app, or the same folder after losing its identity.
    Link(String),
    Cleanup,
}

/// Progress reports reach the view at most this often.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);
/// How long a move waits for the running task to pause.
const TASK_STOP_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Default)]
pub struct State {
    pub busy: bool,
    /// The operation dialog is showing; it stays in the tree to play its exit.
    dialog_open: bool,
    op: StorageOp,
    cancel: Option<Arc<AtomicBool>>,
    generation: u64,
    progress: Progress,
    error: Option<String>,
    /// A folder with no identity to prove it: linking waits for a person's answer.
    link_question: Option<(storage::LibraryLink, String)>,
    pending: Vec<(PathBuf, Journal)>,
    /// Move records that could not be read; their files are left in place.
    corrupt_records: Vec<PathBuf>,
    records_generation: u64,
    resume_task: Option<String>,
    /// Closed or signalled when the job a move waits for exits.
    job_exit: Option<smol::channel::Sender<()>>,
    location_checks: LocationChecks,
}

#[derive(Clone, Debug)]
pub(super) struct LocationCheck {
    pub available: bool,
    pub needs_reassociation: bool,
    pub problem: Option<String>,
}

#[derive(Default)]
struct LocationChecks {
    generation: u64,
    expected: BTreeMap<String, PathBuf>,
    results: BTreeMap<String, (PathBuf, LocationCheck)>,
}

fn backup_list_stale() -> &'static str {
    course2md::tr!("备份已删除，但列表尚未更新，请重启应用后检查")
}
fn registry_unavailable() -> &'static str {
    course2md::tr!("笔记库列表暂时无法读取，请重启应用后重试")
}

fn location_paths(locations: &[workspace::LibraryLocation]) -> BTreeMap<String, PathBuf> {
    locations
        .iter()
        .map(|location| (location.id.clone(), location.root.clone()))
        .collect()
}

impl LocationChecks {
    fn begin(&mut self, generation: u64, locations: &[workspace::LibraryLocation]) {
        self.generation = generation;
        self.expected = location_paths(locations);
        // A refresh can keep useful facts about the same path. A new path must
        // remain unknown until its own check returns.
        self.results
            .retain(|id, (root, _)| self.expected.get(id) == Some(root));
    }

    /// None while any location has not been checked yet: it is shown as
    /// checking, never as an offline or empty library.
    fn access(&self, locations: &[workspace::LibraryLocation]) -> Option<storage::LibraryAccess> {
        let mut access = storage::LibraryAccess {
            available: Vec::new(),
            unavailable: Vec::new(),
        };
        for location in locations {
            let check = self.get(location)?;
            if access.available.contains(&location.root)
                || access.unavailable.contains(&location.root)
            {
                continue;
            }
            if check.available {
                access.available.push(location.root.clone());
            } else {
                access.unavailable.push(location.root.clone());
            }
        }
        Some(access)
    }

    fn get(&self, location: &workspace::LibraryLocation) -> Option<&LocationCheck> {
        self.results
            .get(&location.id)
            .filter(|(root, _)| root == &location.root)
            .map(|(_, check)| check)
    }

    fn finish(
        &mut self,
        generation: u64,
        current: &[workspace::LibraryLocation],
        checks: Vec<(workspace::LibraryLocation, LocationCheck)>,
    ) -> bool {
        if generation != self.generation
            || location_paths(current) != self.expected
            || checks.len() != self.expected.len()
            || checks
                .iter()
                .any(|(location, _)| self.expected.get(&location.id) != Some(&location.root))
        {
            return false;
        }
        self.results = checks
            .into_iter()
            .map(|(location, check)| (location.id, (location.root, check)))
            .collect();
        true
    }
}

fn inspect_location(location: &workspace::LibraryLocation) -> LocationCheck {
    let available = std::fs::read_dir(&location.root).is_ok();
    let mut check = LocationCheck {
        available,
        needs_reassociation: false,
        problem: None,
    };
    if !available || location.id.is_empty() {
        return check;
    }
    match std::fs::read_to_string(location.root.join(storage::LIBRARY_ID_FILE)) {
        Ok(identity) if identity.trim() == location.id => {}
        Ok(_) => {
            check.problem = Some(
                course2md::tr!("这个文件夹属于另一个笔记库，请用「重新定位…」选择正确的文件夹")
                    .into(),
            )
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            check.needs_reassociation = true
        }
        Err(error) => {
            check.problem = Some(course2md::tr!(
                "无法确认这个文件夹是否属于本笔记库：{}",
                error
            ))
        }
    }
    check
}

/// What a person needs to see before giving a folder a library's identity.
fn link_confirmation(link: &storage::LibraryLink, found: &storage::NotesFound) -> String {
    let mut detail = course2md::tr!("保存位置：{}\n\n", link.folder.display());
    if found.names.is_empty() {
        detail.push_str(course2md::tr!("未读到笔记。\n"));
    } else {
        detail.push_str(&course2md::tr!(
            "目前可读取 {} 份笔记：\n{}",
            found.names.len(),
            found
                .names
                .iter()
                .take(5)
                .map(|name| format!("• {name}"))
                .collect::<Vec<_>>()
                .join("\n")
        ));
        if found.names.len() > 5 {
            detail.push_str(&course2md::tr!(
                "\n以及另外 {} 份笔记。",
                found.names.len() - 5
            ));
        }
        detail.push('\n');
    }
    if found.unreadable > 0 {
        detail.push_str(&course2md::tr!(
            "另有 {} 份笔记无法读取。\n",
            found.unreadable
        ));
    }
    detail.trim_end().to_owned()
}

/// All filesystem probes and the caller's library scan run on the blocking
/// pool, even when this future is polled by the UI executor.
pub(super) async fn scan_locations<T, F>(
    locations: Vec<workspace::LibraryLocation>,
    scan: F,
) -> Vec<(workspace::LibraryLocation, LocationCheck, T)>
where
    T: Send + 'static,
    F: Fn(&workspace::LibraryLocation) -> T + Send + 'static,
{
    smol::unblock(move || {
        locations
            .into_iter()
            .map(|location| {
                let check = inspect_location(&location);
                let result = scan(&location);
                (location, check, result)
            })
            .collect()
    })
    .await
}

impl State {
    /// Only ask the event loop to poll when the resume request can be consumed;
    /// while a move is busy, the storage callbacks wake the loop instead.
    pub fn resume_ready(&self) -> bool {
        self.resume_task.is_some() && !self.busy
    }

    pub(super) fn begin_location_checks(
        &mut self,
        generation: u64,
        locations: &[workspace::LibraryLocation],
    ) {
        self.location_checks.begin(generation, locations);
    }

    pub(super) fn finish_location_checks(
        &mut self,
        generation: u64,
        current: &[workspace::LibraryLocation],
        checks: Vec<(workspace::LibraryLocation, LocationCheck)>,
    ) -> bool {
        self.location_checks.finish(generation, current, checks)
    }

    fn begin(&mut self, op: StorageOp) {
        self.op = op;
        self.error = None;
        self.progress = Progress::default();
        self.link_question = None;
    }

    fn title(&self) -> &'static str {
        match self.op {
            StorageOp::Move => course2md::tr!("移动笔记库"),
            StorageOp::Link(_) => course2md::tr!("重新定位笔记库"),
            StorageOp::Cleanup => course2md::tr!("清理旧位置备份"),
        }
    }

    fn cancel_label(&self) -> &'static str {
        match self.op {
            StorageOp::Move => course2md::tr!("取消移动"),
            StorageOp::Link(_) => course2md::tr!("取消重新定位"),
            StorageOp::Cleanup => course2md::tr!("取消清理"),
        }
    }

    fn request_cancel(&mut self) {
        if let Some(cancel) = &self.cancel {
            cancel.store(true, Ordering::Relaxed);
        }
        // Wakes a move that is still waiting for the task to pause.
        self.job_exit = None;
    }

    /// Called when the running job exits.
    pub(super) fn job_exited(&mut self) {
        if let Some(exit) = self.job_exit.take() {
            let _ = exit.try_send(());
        }
    }
}

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
                    root: self.library_root.clone(),
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
        let checks = &self.storage_ui.location_checks;
        match &self.workspace {
            Some(workspace) => checks.access(&workspace.state.libraries),
            None => checks.access(&self.registered_storage_locations()),
        }
    }
}

fn relocate_preview(preview: &mut crate::notes::Preview, old: &Path, new: &Path) {
    workspace::relocate_path(&mut preview.course.dir, old, new);
    if let Some(path) = &mut preview.course.thumbnail {
        workspace::relocate_path(path, old, new);
    }
    for block in &mut preview.blocks {
        if let crate::notes::PreviewBlock::Image(path) = block {
            workspace::relocate_path(path, old, new);
        }
    }
}

impl Desktop {
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
                    Ok(Err(error)) => {
                        this.storage_ui.begin(op);
                        this.storage_ui.error =
                            Some(course2md::tr!("无法选择文件夹：{}", format!("{error:#}")));
                        this.open_storage_dialog(cx);
                    }
                    Err(error) => {
                        this.storage_ui.begin(op);
                        this.storage_ui.error =
                            Some(course2md::tr!("无法打开文件选择器：{}", error));
                        this.open_storage_dialog(cx);
                    }
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
        self.storage_ui.begin(StorageOp::Link(library_id.clone()));
        if self.job.is_some() || self.preview_workers > 0 {
            self.storage_ui.error =
                Some(course2md::tr!("正在处理内容，结束或停止后再重新定位").into());
            self.open_storage_dialog(cx);
            return;
        }
        if !self.save_current_draft(cx) {
            return;
        }
        let Some(state) = self.workspace.as_ref().map(|w| w.state.clone()) else {
            return;
        };
        self.storage_ui.busy = true;
        self.storage_ui.generation = self.storage_ui.generation.wrapping_add(1);
        let generation = self.storage_ui.generation;
        let cancel = Arc::new(AtomicBool::new(false));
        self.storage_ui.cancel = Some(cancel.clone());
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
                        !this.closing && !cancel.load(Ordering::Relaxed),
                        course2md::tr!("已取消重新定位")
                    );
                    ensure!(
                        this.job.is_none() && this.preview_workers == 0,
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
        if self.library_root == link.previous_root {
            self.library_root = link.folder.clone();
        }
        self.read_generation = self.read_generation.wrapping_add(1);
        self.reading = false;
        if let Some(preview) = self.preview.as_mut() {
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
            && self.job.is_none()
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
                                this.storage_ui.begin(StorageOp::Move);
                                this.storage_ui.error = Some(format!("{error:#}"));
                                this.open_storage_dialog(cx);
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
        self.storage_ui.begin(StorageOp::Move);
        let Some(state) = self
            .workspace
            .as_ref()
            .map(|workspace| workspace.state.clone())
        else {
            self.storage_ui.error = Some(registry_unavailable().into());
            self.open_storage_dialog(cx);
            return;
        };
        // Only a conversion task can pause and resume around a move.
        if self.job.is_some() && self.active_task.is_none() {
            self.storage_ui.error = Some(course2md::tr!("模型下载完成或停止后再移动笔记库").into());
            self.open_storage_dialog(cx);
            return;
        }
        self.storage_ui.busy = true;
        self.storage_ui.generation = self.storage_ui.generation.wrapping_add(1);
        let generation = self.storage_ui.generation;
        let cancel = Arc::new(AtomicBool::new(false));
        self.storage_ui.cancel = Some(cancel.clone());
        let job_exit = self.job.is_some().then(|| {
            let (exit, exited) = smol::channel::bounded(1);
            self.storage_ui.job_exit = Some(exit);
            self.storage_ui.progress.message = course2md::tr!("正在暂停当前任务…").into();
            exited
        });
        if let Some(id) = self.active_task.clone() {
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
                        if this.closing || cancel.load(Ordering::Relaxed) {
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
                        if this.library_root == prepared.journal.source {
                            this.library_root = prepared.journal.destination.clone();
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
                    Err(error) => {
                        self.storage_ui.begin(StorageOp::Move);
                        self.storage_ui.error = Some(format!("{error:#}"));
                        self.open_storage_dialog(cx);
                    }
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
            .iter()
            .flat_map(|workspace| &workspace.state.storage_backups)
            .enumerate()
            .fold(None, |group: Option<TileGroup>, (index, backup)| {
                let (id, path) = (backup.id.clone(), backup.path.clone());
                let group = group.unwrap_or_else(|| {
                    TileGroup::new("storage-backups", course2md::tr!("旧位置备份"))
                });
                Some(
                    group.child(storage_item(
                        ("storage-backup-description", index),
                        backup.path.display().to_string(),
                        [
                            material_button(("open-library-backup", index), ButtonVariant::Text)
                                .icon(symbols::folder_open())
                                .child(course2md::tr!("打开备份位置"))
                                .on_click(move |_, _, cx| cx.open_with_system(&path)),
                            material_button(("cleanup-library-backup", index), ButtonVariant::Text)
                                .icon(symbols::delete())
                                .child(course2md::tr!("清理旧位置备份…"))
                                .disabled(storage.busy)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.begin_backup_cleanup(id.clone(), window, cx)
                                })),
                        ],
                    )),
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

    /// One action of a location, for its menu and its right-click.
    fn location_actions(
        &self,
        library: &workspace::LibraryLocation,
        repair: bool,
        offline: bool,
        default: bool,
    ) -> Vec<LocationAction> {
        let busy = self.storage_ui.busy;
        let root = library.root.clone();
        let (default_id, move_id) = (library.id.clone(), library.id.clone());
        vec![
            LocationAction {
                label: course2md::tr!("打开位置"),
                icon: symbols::folder_open(),
                enabled: !offline,
                run: Rc::new(move |_, _, cx| open_location(&root, cx)),
            },
            LocationAction {
                label: if default {
                    course2md::tr!("已是默认保存位置")
                } else {
                    course2md::tr!("设为默认保存位置")
                },
                icon: symbols::check_circle(),
                enabled: !default && !repair && !busy,
                run: Rc::new(move |this, _, cx| this.set_default_library(default_id.clone(), cx)),
            },
            LocationAction {
                label: course2md::tr!("移动笔记库…"),
                icon: symbols::storage(),
                enabled: !repair && !busy,
                run: Rc::new(move |this, window, cx| {
                    this.begin_library_move(move_id.clone(), window, cx)
                }),
            },
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
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let scale = TileScale::Regular;
        let busy = self.storage_ui.busy;
        let check = self.cached_location_check(library);
        let offline = check.is_some_and(|check| !check.available);
        let relocate = check.is_some_and(|check| !check.available || check.problem.is_some());
        let reassociate = check.is_some_and(|check| check.needs_reassociation);
        let repair = reassociate || relocate;
        let path = course2md::config::simplify_verbatim(library.root.clone());
        let actions = self.location_actions(library, repair, offline, default);
        let entity = cx.weak_entity();
        let items = actions.iter().map(|action| {
            let (run, entity) = (action.run.clone(), entity.clone());
            MaterialMenuItem::new(action.label)
                .leading(Icon::from(action.icon.clone()))
                .disabled(!action.enabled)
                .on_click(move |_, window, cx| {
                    let _ = entity.update(cx, |this, cx| run(this, window, cx));
                })
        });
        let more = ui::menu(
            ("storage-location-menu", index),
            MaterialTooltip::plain(
                ("storage-location-actions-tip", index),
                course2md::tr!("笔记库操作"),
            )
            .trigger(IconButton::new(
                ("storage-location-actions", index),
                symbols::more_vert(),
                course2md::tr!("{} 操作", library.name),
            )),
            MaterialMenu::new(("storage-location-items", index)).items(items),
            window,
            cx,
        )
        .anchor(Anchor::TopRight);
        let (id, reassociate_id) = (library.id.clone(), library.id.clone());
        let repairs = repair.then(|| {
            h_flex()
                .min_w_0()
                .pt(rem(4.))
                .gap(rem(8.))
                .flex_wrap()
                .when(reassociate, |row| {
                    row.child(
                        material_button(
                            ("reassociate-storage-location", index),
                            ButtonVariant::Tonal,
                        )
                        .icon(symbols::storage())
                        .child(course2md::tr!("重新关联…"))
                        .disabled(busy)
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                this.begin_library_reassociation(reassociate_id.clone(), window, cx)
                            },
                        )),
                    )
                })
                .when(relocate, |row| {
                    row.child(
                        material_button(
                            ("relocate-storage-location", index),
                            if reassociate {
                                ButtonVariant::Outlined
                            } else {
                                ButtonVariant::Tonal
                            },
                        )
                        .icon(symbols::folder_open())
                        .child(course2md::tr!("重新定位…"))
                        .disabled(busy)
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                this.begin_library_relocation(id.clone(), window, cx)
                            },
                        )),
                    )
                })
        });
        let status = |id, status: AsyncStatus, cx: &mut Context<Self>| {
            ui::async_status((id, index), &status, None, cx)
        };
        let problem = check
            .and_then(|check| check.problem.clone())
            .and_then(|problem| {
                status(
                    "storage-location-problem",
                    AsyncStatus::Failed(problem.into()),
                    cx,
                )
            });
        let unconfirmed = reassociate
            .then(|| {
                status(
                    "storage-association-needed",
                    AsyncStatus::Warning(
                        course2md::tr!("无法确认这是原来的笔记库，请检查后重新关联").into(),
                    ),
                    cx,
                )
            })
            .flatten();
        let open_root = library.root.clone();
        let row = h_flex()
            .w_full()
            .min_w_0()
            .items_start()
            .gap(scale.gap())
            .px(ITEM_INSET)
            .py(rem(16.))
            .child(Icon::from(symbols::folder()).color(color(ColorRole::ON_SURFACE_VARIANT)))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(rem(4.))
                    .child(
                        h_flex()
                            .min_w_0()
                            .gap(rem(8.))
                            .items_center()
                            .flex_wrap()
                            .child(
                                scale.label(("storage-location-name", index), library.name.clone()),
                            )
                            .when(default && several, |row| {
                                row.child(
                                    status_chip(StatusKind::Tonal)
                                        .child(course2md::tr!("默认保存位置")),
                                )
                            })
                            .when(offline, |row| {
                                row.child(
                                    status_chip(StatusKind::Warning)
                                        .child(course2md::tr!("暂时不可访问")),
                                )
                            }),
                    )
                    .child(
                        scale.support(("storage-location-path", index), path.display().to_string()),
                    )
                    .children(problem)
                    .children(unconfirmed)
                    .children(repairs)
                    .when(default && several, |text| {
                        text.child(tile_info(
                            "storage-policy",
                            course2md::tr!("更改默认位置只影响后续生成的笔记"),
                        ))
                    }),
            )
            .child(
                h_flex()
                    .flex_shrink_0()
                    .items_center()
                    .child(
                        MaterialTooltip::plain(
                            ("open-storage-location-tip", index),
                            course2md::tr!("打开位置"),
                        )
                        .trigger(
                            IconButton::new(
                                ("open-storage-location", index),
                                symbols::folder_open(),
                                course2md::tr!("打开位置"),
                            )
                            .disabled(offline)
                            .on_click(move |_, _, cx| open_location(&open_root, cx)),
                        ),
                    )
                    .child(more),
            );
        div()
            .id(("storage-location", index))
            .w_full()
            .min_w_0()
            .context_menu(move |menu, _, _| {
                actions
                    .iter()
                    .enumerate()
                    .fold(menu, |menu, (index, action)| {
                        let (run, entity) = (action.run.clone(), entity.clone());
                        menu.when(index == 1, PopupMenu::separator).item(
                            PopupMenuItem::new(action.label)
                                .icon(action.icon.clone().component())
                                .disabled(!action.enabled)
                                .on_click(move |_, window, cx| {
                                    let _ = entity.update(cx, |this, cx| run(this, window, cx));
                                }),
                        )
                    })
            })
            .child(row)
            .into_any_element()
    }

    /// Storage locations listed in the settings page; per-location operations
    /// live in this module alongside the migration flow.
    pub(super) fn storage_settings_page(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut locations =
            TileGroup::new("storage-locations-heading", course2md::tr!("笔记库位置"));
        if let Some(workspace) = &self.workspace {
            // The default is only a choice once there is more than one location.
            let several = workspace.state.libraries.len() > 1;
            for (index, library) in workspace.state.libraries.iter().enumerate() {
                let default = workspace.state.default_library == library.id;
                locations = locations
                    .child(self.location_row(index, library, several, default, window, cx));
            }
        }
        let add = cx.listener(|this, _, window, cx| this.register_storage_location(window, cx));
        let locations = locations.tile(move |outer| {
            let scale = TileScale::Regular;
            scale
                .button("register-storage-location", outer)
                .accessibility_label(course2md::tr!("添加笔记库位置"))
                .child(
                    scale
                        .row()
                        .child(
                            Icon::from(symbols::add()).color(color(ColorRole::ON_SURFACE_VARIANT)),
                        )
                        .child(scale.label(
                            "register-storage-location-label",
                            course2md::tr!("添加笔记库位置"),
                        )),
                )
                .on_click(add)
                .into_any_element()
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
            self.storage_ui.begin(StorageOp::Cleanup);
            self.storage_ui.error = Some(
                course2md::tr!("模型仍在使用旧位置，请先在「生成笔记」中保存模型位置，再清理备份")
                    .into(),
            );
            self.open_storage_dialog(cx);
            return;
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
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        self.storage_ui.begin(StorageOp::Cleanup);
        self.storage_ui.busy = true;
        self.storage_ui.cancel = Some(cancel.clone());
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

type LocationRun = Rc<dyn Fn(&mut Desktop, &mut Window, &mut Context<Desktop>)>;

struct LocationAction {
    label: &'static str,
    icon: SymbolIcon,
    enabled: bool,
    run: LocationRun,
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

#[cfg(test)]
mod tests {
    use super::{LocationChecks, inspect_location, relocate_preview, scan_locations};
    use crate::{ConversionOptions, source, storage, workspace};
    use std::{path::PathBuf, sync::atomic::AtomicBool};

    fn inspect_relocation(
        state: &workspace::State,
        id: &str,
        folder: &std::path::Path,
    ) -> anyhow::Result<storage::LibraryLink> {
        storage::inspect_link(state, id, folder, &AtomicBool::new(false))
    }

    /// The disk half and the record half of a link, in one step.
    fn publish_relocation(
        state: &mut workspace::State,
        link: &storage::LibraryLink,
    ) -> anyhow::Result<()> {
        let tasks = storage::finish_link(link)?;
        storage::publish_link(state, link, tasks)
    }

    fn test_location(root: PathBuf) -> workspace::LibraryLocation {
        workspace::LibraryLocation {
            id: "test-library".into(),
            name: "笔记库".into(),
            root,
            previous_roots: Vec::new(),
        }
    }

    #[test]
    fn locating_a_moved_library_reopens_notes_and_preserves_task_intent() {
        let directory = tempfile::tempdir().unwrap();
        let old = directory.path().join("old");
        let new = directory.path().join("moved");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("video.mp4"), "original video").unwrap();
        let work = old.join("note-work");
        std::fs::create_dir_all(&work).unwrap();
        let target = course2md::artifact::Target {
            task_id: "task-relocate".into(),
            course_id: "course-relocate".into(),
            source_id: "local:original-video".into(),
            version_id: "v1".into(),
            course_dir: old.join("note"),
        };
        let sections = vec![course2md::timeline::Section {
            t: 0.,
            end: 1.,
            image: String::new(),
            speech: vec![course2md::timeline::TranscriptEvent {
                start: 0.,
                end: 1.,
                text: "Relocation must preserve this body.".into(),
                raw: None,
                translation: None,
            }],
        }];
        let meta = course2md::fetch::VideoMeta {
            title: "Original title".into(),
            uploader: String::new(),
            duration: 0.,
            webpage_url: String::new(),
            extractor: "local".into(),
            id: "original-video".into(),
        };
        course2md::artifact::publish_blocking(
            &target,
            &work,
            &meta,
            &sections,
            None,
            &[],
            Default::default(),
        )
        .unwrap();
        std::fs::remove_dir_all(&work).unwrap();
        let old = old.canonicalize().unwrap();
        let record = directory.path().join("workspace.json");
        let mut workspace = workspace::Workspace::open_at_inline(
            record.clone(),
            old.clone(),
            ConversionOptions::default(),
        )
        .unwrap();
        let id = workspace.state.default_library.clone();
        let source = source::Source {
            input: old.join("video.mp4").display().to_string(),
            identity: "local:original-video".into(),
            title: "Original title".into(),
            online: false,
            ..Default::default()
        };
        let draft = workspace.state.draft_mut().unwrap();
        draft.input = source.input.clone();
        draft.online = false;
        draft.source = Some(source.clone());
        draft.title = "Keep my custom title".into();
        draft.custom_title = true;
        draft.folder = Some(7);
        let draft_id = draft.id.clone();
        let plan = workspace::TaskPlan {
            operation: course2md::execution::Operation::Generate,
            source: source.clone(),
            source_id: source.identity.clone(),
            title: "Frozen task".into(),
            library_id: id.clone(),
            folder: Some(7),
            subtitle: None,
            config: course2md::settings::ConfigFile::default(),
            asr_service: Some("fixed-service-version".into()),
            ai_service: None,
        };
        let (task_id, _) = workspace.state.enqueue(plan, None).unwrap();
        let task = workspace.state.task_mut(&task_id).unwrap();
        task.state = workspace::TaskState::Uncertain;
        task.intent = workspace::Intent::Pause;
        let work = task.work_dir.clone();
        workspace
            .state
            .reader_sources
            .insert("inside".into(), old.join("video.mp4"));
        let external = directory.path().join("external.mp4");
        workspace
            .state
            .reader_sources
            .insert("outside".into(), external.clone());
        let mut preview =
            crate::notes::read_preview(crate::notes::scan_library(&old).unwrap().courses.remove(0))
                .unwrap();
        let original_text = preview.plain_text.clone();
        workspace.state.task_mut(&task_id).unwrap().artifact = Some(preview.course.dir.clone());
        workspace.transaction(|_| Ok(())).unwrap();
        let binding = br#"{"task_id":"kept","source_id":"kept"}"#;
        std::fs::create_dir_all(&work).unwrap();
        std::fs::write(work.join("task-identity.json"), binding).unwrap();
        std::fs::write(
            work.join("control.json"),
            br#"{"intent":"pause","resend":[]}"#,
        )
        .unwrap();
        std::fs::rename(&old, &new).unwrap();
        let new = new.canonicalize().unwrap();
        assert!(!old.exists());

        let located = inspect_relocation(&workspace.state, &id, &new).unwrap();
        workspace
            .transaction(|state| publish_relocation(state, &located))
            .unwrap();
        let state = &workspace.state;
        assert_eq!(state.library(&id).unwrap().root, new);
        assert_eq!(state.default_library, id);
        assert_eq!(state.current_draft, draft_id);
        assert_eq!(state.draft().unwrap().title, "Keep my custom title");
        assert_eq!(state.draft().unwrap().folder, Some(7));
        assert_eq!(
            state.draft().unwrap().input,
            new.join("video.mp4").display().to_string()
        );
        let task = state.task(&task_id).unwrap();
        assert_eq!(task.state, workspace::TaskState::Uncertain);
        assert_eq!(task.intent, workspace::Intent::Pause);
        assert_eq!(
            task.plan.asr_service.as_deref(),
            Some("fixed-service-version")
        );
        assert_eq!(state.reader_sources["inside"], new.join("video.mp4"));
        assert_eq!(state.reader_sources["outside"], external);
        assert!(
            state.storage_backups.is_empty(),
            "locating must not invent a verified backup"
        );
        assert_eq!(
            std::fs::read(task.work_dir.join("task-identity.json")).unwrap(),
            binding
        );
        assert_eq!(
            std::fs::read(task.work_dir.join("control.json")).unwrap(),
            br#"{"intent":"pause","resend":[]}"#
        );
        assert_eq!(
            std::fs::read_to_string(new.join(".course2md-library-id")).unwrap(),
            id
        );
        relocate_preview(&mut preview, &old, &new);
        assert_eq!(preview.plain_text, original_text);
        assert!(preview.course.dir.starts_with(&new));
        let reread = crate::notes::read_preview(preview.course).unwrap();
        assert_eq!(reread.plain_text, original_text);
        let reopened =
            workspace::Workspace::open_at_inline(record, old.clone(), ConversionOptions::default())
                .unwrap();
        assert_eq!(reopened.state.library(&id).unwrap().root, new);
        assert!(
            reopened
                .state
                .library(&id)
                .unwrap()
                .previous_roots
                .contains(&old)
        );
        assert!(
            !old.exists(),
            "recovery must not recreate the missing original directory"
        );
    }

    #[test]
    fn locating_rejects_missing_wrong_and_overlapping_library_identity_without_writes() {
        let directory = tempfile::tempdir().unwrap();
        let old = directory.path().join("old");
        let target = directory.path().join("target");
        std::fs::create_dir(&old).unwrap();
        std::fs::create_dir(&target).unwrap();
        let workspace = workspace::Workspace::open_at_inline(
            directory.path().join("workspace.json"),
            old,
            ConversionOptions::default(),
        )
        .unwrap();
        workspace.save().unwrap();
        let id = workspace.state.default_library.clone();
        let before = std::fs::read(workspace.storage_path()).unwrap();
        // A folder with no identity needs a person's confirmation, and is not touched yet.
        assert!(
            inspect_relocation(&workspace.state, &id, &target)
                .unwrap()
                .unmarked
                .is_some_and(|found| found.names.is_empty())
        );
        assert!(!target.join(".course2md-library-id").exists());
        std::fs::write(target.join(".course2md-library-id"), "another-library").unwrap();
        assert!(inspect_relocation(&workspace.state, &id, &target).is_err());
        assert_eq!(
            std::fs::read_to_string(target.join(".course2md-library-id")).unwrap(),
            "another-library"
        );
        std::fs::write(target.join(".course2md-library-id"), &id).unwrap();
        let mut state = workspace.state.clone();
        state.libraries.push(workspace::LibraryLocation {
            id: "other".into(),
            name: "Other".into(),
            root: target.clone(),
            previous_roots: Vec::new(),
        });
        assert!(inspect_relocation(&state, &id, &target).is_err());
        assert_eq!(std::fs::read(workspace.storage_path()).unwrap(), before);
    }

    #[test]
    fn locating_rechecks_registry_and_selected_directory_before_publication() {
        let directory = tempfile::tempdir().unwrap();
        let old = directory.path().join("old");
        let target = directory.path().join("target");
        std::fs::create_dir(&old).unwrap();
        std::fs::create_dir(&target).unwrap();
        let mut workspace = workspace::Workspace::open_at_inline(
            directory.path().join("workspace.json"),
            old,
            ConversionOptions::default(),
        )
        .unwrap();
        let id = workspace.state.default_library.clone();
        std::fs::write(target.join(".course2md-library-id"), &id).unwrap();
        let located = inspect_relocation(&workspace.state, &id, &target).unwrap();
        let mut changed = workspace.state.clone();
        changed.libraries[0].root = directory.path().join("another-location");
        let before = changed.clone();
        assert!(publish_relocation(&mut changed, &located).is_err());
        assert!(changed == before);
        std::fs::rename(&target, directory.path().join("original-selected")).unwrap();
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join(".course2md-library-id"), &id).unwrap();
        let before = workspace.state.clone();
        assert!(
            workspace
                .transaction(|state| publish_relocation(state, &located))
                .is_err()
        );
        assert!(workspace.state == before);
        let located = inspect_relocation(&workspace.state, &id, &target).unwrap();
        std::fs::write(target.join(".course2md-library-id"), "different-library").unwrap();
        assert!(
            workspace
                .transaction(|state| publish_relocation(state, &located))
                .is_err()
        );
        assert!(workspace.state == before);
    }

    #[test]
    fn locating_keeps_the_original_registry_when_publication_cannot_be_saved() {
        let directory = tempfile::tempdir().unwrap();
        let old = directory.path().join("old");
        let target = directory.path().join("target");
        std::fs::create_dir(&old).unwrap();
        let record = directory.path().join("workspace.json");
        let mut workspace = workspace::Workspace::open_at_inline(
            record.clone(),
            old.clone(),
            ConversionOptions::default(),
        )
        .unwrap();
        workspace.save().unwrap();
        let id = workspace.state.default_library.clone();
        let before = workspace.state.clone();
        let record_bytes = std::fs::read(&record).unwrap();
        std::fs::rename(&old, &target).unwrap();
        let located = inspect_relocation(&workspace.state, &id, &target).unwrap();
        // A directory at the output file is a portable publication failure, not
        // a chmod assertion that would silently pass under a privileged runner.
        std::fs::rename(&record, directory.path().join("original-record.json")).unwrap();
        std::fs::create_dir(&record).unwrap();
        assert!(
            workspace
                .transaction(|state| publish_relocation(state, &located))
                .is_err()
        );
        assert!(workspace.state == before);
        assert_eq!(
            std::fs::read(directory.path().join("original-record.json")).unwrap(),
            record_bytes
        );
        assert_eq!(
            std::fs::read_to_string(target.join(".course2md-library-id")).unwrap(),
            id
        );
        assert!(!old.exists());
    }

    #[test]
    fn slow_library_scan_runs_outside_the_calling_thread() {
        let directory = tempfile::tempdir().unwrap();
        let location = test_location(directory.path().to_owned());
        std::fs::write(directory.path().join(".course2md-library-id"), &location.id).unwrap();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let caller = std::thread::current().id();
        smol::block_on(async {
            let mut scan = Box::pin(scan_locations(vec![location.clone()], move |_| {
                started_tx.send(std::thread::current().id()).unwrap();
                release_rx
                    .recv_timeout(std::time::Duration::from_secs(2))
                    .unwrap();
            }));
            // Polling the real scan boundary returns while the worker remains
            // blocked. This checks thread isolation, not a native hang fixture.
            assert!(smol::future::poll_once(&mut scan).await.is_none());
            let worker = started_rx
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap();
            assert_ne!(worker, caller);
            let mut cache = LocationChecks::default();
            cache.begin(1, std::slice::from_ref(&location));
            assert!(cache.get(&location).is_none());
            release_tx.send(()).unwrap();
            let result = scan.await;
            assert!(result[0].1.available);
            assert!(!result[0].1.needs_reassociation);
        });
    }

    #[test]
    fn disconnected_libraries_keep_search_coverage_unknown_until_reconnected() {
        let directory = tempfile::tempdir().unwrap();
        let mut first = test_location(directory.path().join("first"));
        first.id = "first".into();
        let mut second = test_location(directory.path().join("second"));
        second.id = "second".into();
        let locations = [first.clone(), second.clone()];
        for location in &locations {
            std::fs::create_dir(&location.root).unwrap();
        }
        let mut generation = 0;
        let mut coverage = || {
            generation += 1;
            let mut checks = LocationChecks::default();
            checks.begin(generation, &locations);
            assert!(
                checks.finish(
                    generation,
                    &locations,
                    locations
                        .iter()
                        .map(|location| (location.clone(), inspect_location(location)))
                        .collect()
                )
            );
            checks.access(&locations).unwrap().coverage()
        };
        assert_eq!(coverage(), storage::LibraryCoverage::Complete);
        std::fs::rename(&first.root, directory.path().join("first-away")).unwrap();
        assert_eq!(coverage(), storage::LibraryCoverage::Partial);
        std::fs::rename(&second.root, directory.path().join("second-away")).unwrap();
        assert_eq!(coverage(), storage::LibraryCoverage::Unavailable);
        std::fs::rename(directory.path().join("first-away"), &first.root).unwrap();
        assert_eq!(coverage(), storage::LibraryCoverage::Partial);
        let unchecked = LocationChecks::default();
        assert!(unchecked.access(&locations).is_none());
    }

    #[test]
    fn location_checks_reject_old_generations_and_changed_paths() {
        let directory = tempfile::tempdir().unwrap();
        let old = test_location(directory.path().join("old"));
        let new = test_location(directory.path().join("new"));
        std::fs::create_dir(&old.root).unwrap();
        std::fs::create_dir(&new.root).unwrap();
        std::fs::write(old.root.join(".course2md-library-id"), &old.id).unwrap();
        let mut cache = LocationChecks::default();
        cache.begin(1, std::slice::from_ref(&old));
        assert!(cache.finish(
            1,
            std::slice::from_ref(&old),
            vec![(old.clone(), inspect_location(&old))]
        ));
        assert!(cache.get(&old).unwrap().available);
        cache.begin(2, std::slice::from_ref(&old));
        assert!(
            cache.get(&old).is_some(),
            "same-path refresh keeps the last checked result"
        );
        assert!(!cache.finish(
            1,
            std::slice::from_ref(&old),
            vec![(old.clone(), inspect_location(&old))]
        ));
        assert!(!cache.finish(
            2,
            std::slice::from_ref(&new),
            vec![(old.clone(), inspect_location(&old))]
        ));
        assert!(
            cache.get(&new).is_none(),
            "a new path is pending, not offline"
        );
        cache.begin(3, std::slice::from_ref(&new));
        assert!(cache.get(&old).is_none());
        assert!(!cache.finish(
            2,
            std::slice::from_ref(&new),
            vec![(old.clone(), inspect_location(&old))]
        ));
        assert!(cache.finish(
            3,
            std::slice::from_ref(&new),
            vec![(new.clone(), inspect_location(&new))]
        ));
        assert!(cache.get(&new).unwrap().needs_reassociation);
    }

    #[test]
    fn registry_commit_preserves_ids_and_moves_only_library_owned_paths() {
        let directory = tempfile::tempdir().unwrap();
        let old = directory.path().join("old");
        let new = directory.path().join("new");
        std::fs::create_dir_all(old.join("media")).unwrap();
        std::fs::create_dir_all(&new).unwrap();
        std::fs::write(old.join("media/video.mp4"), "fixture video content").unwrap();
        let old = std::fs::canonicalize(old).unwrap();
        let new = std::fs::canonicalize(new).unwrap();
        let record_path = directory.path().join("workspace.json");
        let mut workspace = workspace::Workspace::open_at_inline(
            record_path.clone(),
            old.clone(),
            ConversionOptions::default(),
        )
        .unwrap();
        let library_id = workspace.state.default_library.clone();
        let mut source = source::Source {
            input: old.join("media/video.mp4").display().to_string(),
            title: "My confirmed title".into(),
            identity: "local:content-stays-the-same".into(),
            online: false,
            cover: Some(old.join("media/cover.png")),
            ..Default::default()
        };
        let draft = workspace.state.draft_mut().unwrap();
        draft.online = false;
        draft.input = source.input.clone();
        draft.source = Some(source.clone());
        draft.title = "My custom title".into();
        draft.custom_title = true;
        draft.folder = Some(17);
        let original_draft_id = draft.id.clone();
        let mut config = course2md::settings::ConfigFile::default();
        config.defaults.model_dir = Some(old.join("models"));
        let plan = workspace::TaskPlan {
            operation: course2md::execution::Operation::Reprocess {
                base_version_dir: old.join("note/versions/old-version"),
                components: vec![course2md::execution::Component::Summary],
                prior_work_dir: Some(old.join(".course2md/work/old-task")),
            },
            source: source.clone(),
            source_id: source.identity.clone(),
            title: "Frozen title".into(),
            library_id: library_id.clone(),
            folder: Some(17),
            subtitle: Some(old.join("subtitles/selected.srt")),
            config,
            asr_service: Some("fixed-asr-version".into()),
            ai_service: Some("fixed-ai-version".into()),
        };
        let (task_id, _) = workspace.state.enqueue(plan, None).unwrap();
        let old_task_work = workspace.state.task(&task_id).unwrap().work_dir.clone();
        source.input = directory.path().join("outside.mp4").display().to_string();
        let outside_input = source.input.clone();
        let mut outside_draft =
            workspace::Draft::new(false, library_id.clone(), ConversionOptions::default());
        outside_draft.input = outside_input.clone();
        outside_draft.source = Some(source);
        workspace
            .state
            .reader_sources
            .insert("local:relocated-video".into(), old.join("media/video.mp4"));
        workspace
            .state
            .reader_sources
            .insert("local:outside-video".into(), PathBuf::from(&outside_input));
        workspace.transaction(|_| Ok(())).unwrap();
        let prepared = storage::prepare_move(
            library_id.clone(),
            &old,
            &new,
            &directory.path().join("journals"),
            &AtomicBool::new(false),
            &|_| {},
        )
        .unwrap();
        assert_eq!(workspace.state.library(&library_id).unwrap().root, old);
        // Exercise an external current input separately: the workbench now has
        // one form, and reopening must not revive a second managed draft.
        let mut external_input_state = workspace.state.clone();
        external_input_state.current_draft = outside_draft.id.clone();
        external_input_state.drafts = vec![outside_draft];
        storage::publish_move(&mut external_input_state, &prepared).unwrap();
        assert_eq!(external_input_state.draft().unwrap().input, outside_input);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let permissions = std::fs::metadata(directory.path()).unwrap().permissions();
            std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o555))
                .unwrap();
            let failed = workspace.transaction(|state| storage::publish_move(state, &prepared));
            std::fs::set_permissions(directory.path(), permissions).unwrap();
            assert!(failed.is_err());
            assert_eq!(workspace.state.library(&library_id).unwrap().root, old);
        }
        let before_commit = workspace::Workspace::open_at_inline(
            record_path.clone(),
            old.clone(),
            ConversionOptions::default(),
        )
        .unwrap();
        assert_eq!(before_commit.state.library(&library_id).unwrap().root, old);
        assert_eq!(
            before_commit.state.task(&task_id).unwrap().work_dir,
            old_task_work
        );
        workspace
            .transaction(|state| storage::publish_move(state, &prepared))
            .unwrap();
        let mut reopened = workspace::Workspace::open_at_inline(
            record_path,
            old.clone(),
            ConversionOptions::default(),
        )
        .unwrap();
        let state = &reopened.state;
        let lagging: storage::Journal =
            serde_json::from_slice(&std::fs::read(&prepared.path).unwrap()).unwrap();
        assert_eq!(lagging.phase, storage::Phase::Verified);
        assert_eq!(state.library(&library_id).unwrap().root, new);
        assert_eq!(
            state.reader_sources["local:relocated-video"],
            new.join("media/video.mp4")
        );
        assert_eq!(
            state.reader_sources["local:outside-video"],
            PathBuf::from(&outside_input)
        );
        let draft = state
            .drafts
            .iter()
            .find(|draft| draft.id == original_draft_id)
            .unwrap();
        assert_eq!(draft.library_id, library_id);
        assert_eq!(draft.folder, Some(17));
        assert_eq!(draft.title, "My custom title");
        assert_eq!(
            draft.input,
            new.join("media/video.mp4").display().to_string()
        );
        assert_eq!(
            draft.source.as_ref().unwrap().identity,
            "local:content-stays-the-same"
        );
        assert_eq!(state.drafts.len(), 1);
        let task = state.task(&task_id).unwrap();
        assert_eq!(task.plan.title, "Frozen title");
        assert_eq!(task.plan.asr_service.as_deref(), Some("fixed-asr-version"));
        assert_eq!(
            task.plan.config.defaults.model_dir,
            Some(new.join("models"))
        );
        assert_eq!(
            task.work_dir,
            new.join(old_task_work.strip_prefix(&old).unwrap())
        );
        assert!(
            matches!(&task.plan.operation, course2md::execution::Operation::Reprocess {base_version_dir, prior_work_dir:Some(work),..}
            if base_version_dir.starts_with(&new) && work.starts_with(&new))
        );
        assert_eq!(state.storage_backups[0].path, old);
        assert!(
            state
                .library(&library_id)
                .unwrap()
                .previous_roots
                .contains(&old)
        );
        assert!(old.join("media/video.mp4").is_file());
        let final_root = directory.path().join("third");
        std::fs::create_dir(&final_root).unwrap();
        let second_move = storage::prepare_move(
            library_id.clone(),
            &new,
            &final_root,
            &directory.path().join("journals"),
            &AtomicBool::new(false),
            &|_| {},
        )
        .unwrap();
        reopened
            .transaction(|state| storage::publish_move(state, &second_move))
            .unwrap();
        let location = reopened.state.library(&library_id).unwrap();
        assert!(location.previous_roots.contains(&old) && location.previous_roots.contains(&new));
        assert_eq!(
            reopened.state.reader_sources["local:relocated-video"],
            location.root.join("media/video.mp4")
        );
        assert!(
            reopened
                .state
                .storage_backups
                .iter()
                .all(|backup| backup.current_root == location.root)
        );
    }

    #[test]
    fn another_registered_library_is_rejected_even_when_empty() {
        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("first");
        let second = directory.path().join("second");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        let mut workspace = workspace::Workspace::open_at_inline(
            directory.path().join("workspace.json"),
            first,
            ConversionOptions::default(),
        )
        .unwrap();
        workspace.state.libraries.push(workspace::LibraryLocation {
            id: "other".into(),
            name: "Other library".into(),
            root: second.clone(),
            previous_roots: Vec::new(),
        });
        assert!(
            storage::check_overlap(&workspace.state, &workspace.state.default_library, &second)
                .is_err()
        );
        assert!(std::fs::read_dir(second).unwrap().next().is_none());
    }
}
