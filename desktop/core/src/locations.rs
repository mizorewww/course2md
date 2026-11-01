//! The storage page's state: which operation is running, and what each library location
//! was last found to be.
use crate::{
    storage::{self, Journal, Progress},
    workspace,
};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

/// The storage operation the dialog and status panel describe.
#[derive(Clone, Default, PartialEq)]
pub enum StorageOp {
    #[default]
    Move,
    /// Pointing a library id at the folder it now lives in: moved outside the
    /// app, or the same folder after losing its identity.
    Link(String),
    Cleanup,
}

#[derive(Default)]
pub struct State {
    pub busy: bool,
    /// The operation dialog is showing; it stays in the tree to play its exit.
    pub dialog_open: bool,
    pub op: StorageOp,
    pub cancel: Option<Arc<AtomicBool>>,
    pub generation: u64,
    pub progress: Progress,
    pub error: Option<String>,
    /// A folder with no identity to prove it: linking waits for a person's answer.
    pub link_question: Option<(storage::LibraryLink, String)>,
    pub pending: Vec<(PathBuf, Journal)>,
    /// Move records that could not be read; their files are left in place.
    pub corrupt_records: Vec<PathBuf>,
    pub records_generation: u64,
    pub resume_task: Option<String>,
    /// Closed or signalled when the job a move waits for exits.
    pub job_exit: Option<smol::channel::Sender<()>>,
    pub location_checks: LocationChecks,
}

impl State {
    /// Only ask the event loop to poll when the resume request can be consumed;
    /// while a move is busy, the storage callbacks wake the loop instead.
    pub fn resume_ready(&self) -> bool {
        self.resume_task.is_some() && !self.busy
    }

    pub fn begin(&mut self, op: StorageOp) {
        self.op = op;
        self.error = None;
        self.progress = Progress::default();
        self.link_question = None;
    }

    pub fn title(&self) -> &'static str {
        match self.op {
            StorageOp::Move => course2md::tr!("移动笔记库"),
            StorageOp::Link(_) => course2md::tr!("重新定位笔记库"),
            StorageOp::Cleanup => course2md::tr!("清理旧位置备份"),
        }
    }

    pub fn cancel_label(&self) -> &'static str {
        match self.op {
            StorageOp::Move => course2md::tr!("取消移动"),
            StorageOp::Link(_) => course2md::tr!("取消重新定位"),
            StorageOp::Cleanup => course2md::tr!("取消清理"),
        }
    }

    pub fn request_cancel(&mut self) {
        if let Some(cancel) = &self.cancel {
            cancel.store(true, Ordering::Relaxed);
        }
        // Wakes a move that is still waiting for the task to pause.
        self.job_exit = None;
    }

    /// Called when the running job exits.
    pub fn job_exited(&mut self) {
        if let Some(exit) = self.job_exit.take() {
            let _ = exit.try_send(());
        }
    }
}

#[derive(Clone, Debug)]
pub struct LocationCheck {
    pub available: bool,
    pub needs_reassociation: bool,
    pub problem: Option<String>,
}

#[derive(Default)]
pub struct LocationChecks {
    pub generation: u64,
    pub expected: BTreeMap<String, PathBuf>,
    pub results: BTreeMap<String, (PathBuf, LocationCheck)>,
}

impl LocationChecks {
    pub fn begin(&mut self, generation: u64, locations: &[workspace::LibraryLocation]) {
        self.generation = generation;
        self.expected = location_paths(locations);
        // A refresh can keep useful facts about the same path. A new path must
        // remain unknown until its own check returns.
        self.results
            .retain(|id, (root, _)| self.expected.get(id) == Some(root));
    }

    /// None while any location has not been checked yet: it is shown as
    /// checking, never as an offline or empty library.
    pub fn access(
        &self,
        locations: &[workspace::LibraryLocation],
    ) -> Option<storage::LibraryAccess> {
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

    pub fn get(&self, location: &workspace::LibraryLocation) -> Option<&LocationCheck> {
        self.results
            .get(&location.id)
            .filter(|(root, _)| root == &location.root)
            .map(|(_, check)| check)
    }

    pub fn finish(
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

pub fn backup_list_stale() -> &'static str {
    course2md::tr!("备份已删除，但列表尚未更新，请重启应用后检查")
}

pub fn registry_unavailable() -> &'static str {
    course2md::tr!("笔记库列表暂时无法读取，请重启应用后重试")
}

pub fn location_paths(locations: &[workspace::LibraryLocation]) -> BTreeMap<String, PathBuf> {
    locations
        .iter()
        .map(|location| (location.id.clone(), location.root.clone()))
        .collect()
}

pub fn inspect_location(location: &workspace::LibraryLocation) -> LocationCheck {
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
pub fn link_confirmation(link: &storage::LibraryLink, found: &storage::NotesFound) -> String {
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
pub async fn scan_locations<T, F>(
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

pub fn relocate_preview(preview: &mut crate::notes::Preview, old: &Path, new: &Path) {
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

#[cfg(test)]
mod tests {
    use super::{LocationChecks, inspect_location, relocate_preview, scan_locations};
    use crate::{conversion::ConversionOptions, source, storage, workspace};
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
