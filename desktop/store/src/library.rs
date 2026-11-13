//! The note library as the app knows it: which folder is in view, where each note lives,
//! and what could not be read.
use crate::{
    locations::LocationCheck,
    notes::{self, Course},
    order, organize, workspace,
};
use std::{collections::BTreeMap, path::PathBuf, rc::Rc};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FolderFilter {
    #[default]
    All,
    Unfiled,
    Folder(u64),
}

impl FolderFilter {
    pub fn is_all(self) -> bool {
        self == Self::All
    }
    pub fn folder_id(self) -> Option<u64> {
        match self {
            Self::Folder(id) => Some(id),
            _ => None,
        }
    }
    /// The place whose sort this view uses.
    pub fn place(self) -> order::Place {
        match self {
            Self::All => order::Place::All,
            Self::Unfiled => order::Place::Unfiled,
            Self::Folder(id) => order::Place::Folder(id),
        }
    }
}

#[derive(Clone)]
pub struct CourseLocation {
    pub id: String,
    pub root: PathBuf,
    pub relative: PathBuf,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum LibraryIssueKind {
    Scan,
    Folders,
    Titles,
}

/// A read failure in one registered library; recovery cards claim theirs by (root, kind).
#[derive(Clone, PartialEq, Eq)]
pub struct LibraryIssue {
    pub root: PathBuf,
    pub kind: LibraryIssueKind,
    pub message: String,
}

/// Facts about the video a note came from, indexed by task id after each refresh.
#[derive(Clone)]
pub struct NoteSource {
    pub author: String,
    pub duration: Option<f64>,
    /// When the video was published, as `YYYY-MM-DD`.
    pub published: Option<String>,
    pub local: bool,
    /// The video page an online note came from.
    pub link: Option<String>,
}

/// Filesystem facts are collected by the library worker and read by the UI.
#[derive(Default)]
pub struct LibraryViewCache {
    pub locations: BTreeMap<PathBuf, CourseLocation>,
    /// Task id → its video, for the notes in the library.
    pub sources: std::collections::HashMap<String, NoteSource>,
    /// Source identity → indices into `LibraryState::courses` of its notes.
    pub notes_by_source: std::collections::HashMap<String, Vec<usize>>,
    pub recovery: BTreeMap<PathBuf, organize::Recovery>,
    pub title_recovery: BTreeMap<PathBuf, organize::Recovery>,
    pub aliases: BTreeMap<PathBuf, BTreeMap<PathBuf, String>>,
    pub alias_issues: Vec<LibraryIssue>,
    /// Each library's sorts and hand-made orders, by its root.
    pub orders: BTreeMap<PathBuf, order::Order>,
    /// Each note's key in its library's order record, by its folder.
    pub note_keys: std::collections::HashMap<PathBuf, String>,
}

impl LibraryViewCache {
    pub fn inspect(
        location: &workspace::LibraryLocation,
        scan: Option<&notes::LibraryScan>,
    ) -> Self {
        let mut cache = Self::default();
        if let Ok(Some(recovery)) = organize::recovery(&location.root) {
            cache.recovery.insert(location.root.clone(), recovery);
        }
        if let Ok(Some(recovery)) = organize::title_recovery(&location.root) {
            cache.title_recovery.insert(location.root.clone(), recovery);
        }
        match organize::title_aliases(&location.root) {
            Ok(names) => {
                cache.aliases.insert(location.root.clone(), names);
            }
            Err(error) => cache.alias_issues.push(LibraryIssue {
                root: location.root.clone(),
                kind: LibraryIssueKind::Titles,
                message: c2m_i18n::tr!(
                    "{}的显示名称尚未读取：{}",
                    location.label(),
                    format!("{error:#}")
                ),
            }),
        }
        if let Some(scan) = scan {
            // Each note's key in the record: its course id, or a copy's own key.
            let named: Vec<(&Course, order::Seen)> = scan
                .courses
                .iter()
                .filter_map(|course| {
                    let manifest = course.manifest.as_ref()?;
                    let folder = organize::relative_key(&location.root, &course.dir).ok()?;
                    Some((
                        course,
                        order::Seen {
                            folder,
                            id: manifest.course_id.clone(),
                            created: manifest.created_at_ms,
                        },
                    ))
                })
                .collect();
            let seen: Vec<order::Seen> = named.iter().map(|(_, seen)| seen.clone()).collect();
            // Notes waiting on the person: being replaced (and so not listed), or with
            // copies kept beside them. They are never forgotten, and a scan that could
            // not list one does not count notes as gone.
            let waiting: Vec<PathBuf> = scan
                .attention
                .iter()
                .filter_map(|attention| match attention {
                    c2m_artifact::artifact::NoteAttention::Blocked { note, .. }
                    | c2m_artifact::artifact::NoteAttention::Kept { note, .. }
                    | c2m_artifact::artifact::NoteAttention::Failed { note, .. } => {
                        organize::relative_key(&location.root, note)
                            .ok()
                            .or_else(|| note.strip_prefix(&location.root).ok().map(Into::into))
                    }
                })
                .collect();
            let listed: std::collections::HashSet<&std::path::Path> =
                seen.iter().map(|seen| seen.folder.as_path()).collect();
            let complete = scan.issues.is_empty()
                && scan.attention.len() == waiting.len()
                && waiting
                    .iter()
                    .all(|folder| listed.contains(folder.as_path()));
            let (record, keys) = order::Order::observe(&location.root, &seen, complete, &waiting);
            for ((course, _), key) in named.iter().zip(keys) {
                cache.note_keys.insert(course.dir.clone(), key);
            }
            cache.orders.insert(location.root.clone(), record);
            for course in &scan.courses {
                let storage = &course.dir;
                let Ok(relative) = organize::relative_key(&location.root, storage) else {
                    continue;
                };
                let membership = CourseLocation {
                    id: location.id.clone(),
                    root: location.root.clone(),
                    relative: relative.clone(),
                };
                // A published note can use a canonical path while a registered
                // root uses a symlink or /tmp alias. Resolve once in this worker.
                let mut paths = vec![storage.clone(), location.root.join(relative)];
                paths.extend(storage.canonicalize().ok());
                for path in paths {
                    cache.locations.insert(path, membership.clone());
                }
            }
        }
        cache
    }

    pub(crate) fn merge(&mut self, other: Self) {
        for (path, location) in other.locations {
            let replace = self.locations.get(&path).is_none_or(|previous| {
                location.root.components().count() > previous.root.components().count()
            });
            if replace {
                self.locations.insert(path, location);
            }
        }
        self.recovery.extend(other.recovery);
        self.title_recovery.extend(other.title_recovery);
        self.aliases.extend(other.aliases);
        self.alias_issues.extend(other.alias_issues);
        self.orders.extend(other.orders);
        self.note_keys.extend(other.note_keys);
    }
}

/// Whether an unfinished task is still writing the note with
/// `source_id` into the library `library_id`. Tasks name their library by
/// `LibraryLocation::id`, never by its path.
pub fn note_has_unfinished_task(
    tasks: &[crate::workspace::TaskRecord],
    library_id: &str,
    source_id: Option<&str>,
) -> bool {
    note_unfinished_task(tasks, library_id, source_id).is_some()
}

/// The one unfinished task that will replace the note of `source_id` in `library_id`.
pub fn note_unfinished_task<'a>(
    tasks: &'a [crate::workspace::TaskRecord],
    library_id: &str,
    source_id: Option<&str>,
) -> Option<&'a crate::workspace::TaskRecord> {
    let source = source_id?;
    tasks.iter().find(|task| {
        // An attempt a follow-up replaced stays unfinished, but is no longer worked on.
        !task.state.finished()
            && task.handled_by.is_none()
            && task.plan.source_id == source
            && task.plan.library_id == library_id
    })
}

/// What one library's scan found, as `locations::scan_locations` returns it.
pub(crate) type LibraryScanned = (
    workspace::LibraryLocation,
    LocationCheck,
    (
        anyhow::Result<notes::LibraryScan>,
        anyhow::Result<organize::Library>,
        LibraryViewCache,
    ),
);

/// Every library's notes and folders, as of the last scan.
pub struct LibraryState {
    /// The folder record of the library in view.
    pub current: organize::Library,
    /// The library in view.
    pub root: PathBuf,
    pub error: Option<String>,
    pub issues: Vec<LibraryIssue>,
    pub indexes: BTreeMap<PathBuf, organize::Library>,
    pub view_cache: LibraryViewCache,
    /// The scan whose results still count.
    pub generation: u64,
    pub folder_filter: FolderFilter,
    /// Library notes, shared so render closures hold a pointer, not a copy.
    pub courses: Vec<Rc<Course>>,
    pub cover_by_source: BTreeMap<String, PathBuf>,
    pub loading: bool,
    /// Note folders a newer app wrote, which this one must not replace; see
    /// `notes::LibraryScan::newer`.
    pub newer_notes: std::collections::BTreeSet<PathBuf>,
    /// Notes whose interrupted replacement needs the person, or with copies kept
    /// beside them; see `notes::LibraryScan::attention`.
    pub note_attention: Vec<c2m_artifact::artifact::NoteAttention>,
    /// Changes whenever what the notes are sorted by may have changed: a scan, their
    /// names and sources, or an order record; a view sorting them keys on it.
    pub revision: u64,
}

impl LibraryState {
    pub fn new(root: PathBuf) -> Self {
        Self {
            current: Default::default(),
            root,
            error: None,
            issues: Vec::new(),
            indexes: BTreeMap::new(),
            view_cache: Default::default(),
            generation: 0,
            folder_filter: FolderFilter::All,
            courses: Vec::new(),
            cover_by_source: BTreeMap::new(),
            newer_notes: Default::default(),
            note_attention: Vec::new(),
            loading: false,
            revision: 0,
        }
    }

    /// Starts a scan: results of an earlier one no longer count.
    pub fn begin_scan(&mut self) -> u64 {
        self.generation = self.generation.wrapping_add(1);
        self.loading = true;
        self.generation
    }

    /// Replaces what was known with what a scan found, newest notes first.
    pub fn apply_scan(&mut self, results: Vec<LibraryScanned>) {
        self.loading = false;
        self.touch();
        self.courses.clear();
        self.cover_by_source.clear();
        self.issues.clear();
        self.newer_notes.clear();
        self.note_attention.clear();
        self.indexes.clear();
        self.view_cache = Default::default();
        for (location, _, (scan, organization, cache)) in results {
            self.view_cache.merge(cache);
            match scan {
                Ok(scan) => {
                    for course in &scan.courses {
                        if let Some(cover) = &course.thumbnail
                            && let Some(source_id) = course
                                .manifest
                                .as_ref()
                                .map(|manifest| manifest.source_id.clone())
                        {
                            self.cover_by_source.insert(source_id, cover.clone());
                        }
                    }
                    self.courses.extend(scan.courses.into_iter().map(Rc::new));
                    self.newer_notes.extend(scan.newer);
                    self.note_attention.extend(scan.attention);
                    self.issues
                        .extend(scan.issues.into_iter().map(|message| LibraryIssue {
                            root: location.root.clone(),
                            kind: LibraryIssueKind::Scan,
                            message,
                        }));
                }
                Err(error) => self.issues.push(LibraryIssue {
                    root: location.root.clone(),
                    kind: LibraryIssueKind::Scan,
                    message: c2m_i18n::tr!(
                        "{}读取失败：{}",
                        location.label(),
                        format!("{error:#}")
                    ),
                }),
            }
            match organization {
                Ok(library) => {
                    self.indexes.insert(location.root, library);
                }
                Err(error) => self.issues.push(LibraryIssue {
                    root: location.root.clone(),
                    kind: LibraryIssueKind::Folders,
                    message: c2m_i18n::tr!(
                        "{}的文件夹信息已损坏：{}",
                        location.label(),
                        format!("{error:#}")
                    ),
                }),
            }
        }
        self.courses.sort_by_key(|c| std::cmp::Reverse(c.modified));
    }

    /// Settles which library is in view once a scan's folder records are in: the chosen one,
    /// or the first registered one that reads when the chosen one is not registered.
    pub fn settle_root(&mut self, registered: &[workspace::LibraryLocation]) {
        if let Some(library) = self.indexes.get(&self.root) {
            self.current = library.clone();
            self.error = None;
        } else if let Some((root, library)) = registered.first().and_then(|location| {
            self.indexes
                .get(&location.root)
                .map(|library| (location.root.clone(), library.clone()))
        }) {
            self.root = root;
            self.current = library;
            self.error = None;
        } else {
            self.error = Some(c2m_i18n::tr!("文件夹信息读取失败").into());
        }
    }

    /// The registered library a note lives in.
    pub fn course_location<'a>(
        &self,
        registered: &'a [workspace::LibraryLocation],
        course: &Course,
    ) -> Option<&'a workspace::LibraryLocation> {
        let cached = self.view_cache.locations.get(&course.dir)?;
        registered
            .iter()
            .find(|lib| lib.id == cached.id && lib.root == cached.root)
    }

    /// The folder a note is filed in.
    pub fn course_folder(
        &self,
        registered: &[workspace::LibraryLocation],
        course: &Course,
    ) -> Option<u64> {
        let root = &self.course_location(registered, course)?.root;
        let cached = self.view_cache.locations.get(&course.dir)?;
        self.indexes.get(root)?.folder_key(&cached.relative)
    }

    /// The notes made from one video.
    pub fn notes_for_source(&self, identity: &str) -> impl Iterator<Item = &Course> {
        self.view_cache
            .notes_by_source
            .get(identity)
            .into_iter()
            .flatten()
            .filter_map(|index| self.courses.get(*index).map(|course| &**course))
    }

    pub fn issue_messages(&self, root: &std::path::Path, kind: LibraryIssueKind) -> Vec<String> {
        self.issues
            .iter()
            .filter(|issue| issue.kind == kind && issue.root == root)
            .map(|issue| issue.message.clone())
            .collect()
    }

    /// Records that what the notes are sorted by may have changed.
    pub fn touch(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }

    /// The note's key in its library's order record.
    pub fn note_key(&self, course: &Course) -> Option<&str> {
        self.view_cache
            .note_keys
            .get(&course.dir)
            .map(String::as_str)
    }

    pub fn index_note_sources(&mut self, tasks: &[workspace::TaskRecord]) {
        self.touch();
        let mut notes_by_source: std::collections::HashMap<String, Vec<usize>> = Default::default();
        for (index, course) in self.courses.iter().enumerate() {
            if let Some(manifest) = &course.manifest {
                notes_by_source
                    .entry(manifest.source_id.clone())
                    .or_default()
                    .push(index);
            }
        }
        self.view_cache.notes_by_source = notes_by_source;
        let wanted: std::collections::HashSet<&str> = self
            .courses
            .iter()
            .filter_map(|course| Some(course.manifest.as_ref()?.task_id.as_str()))
            .collect();
        self.view_cache.sources = tasks
            .iter()
            .filter(|task| wanted.contains(task.id.as_str()))
            .map(|task| {
                let source = &task.plan.source;
                (
                    task.id.clone(),
                    NoteSource {
                        author: source.author.clone(),
                        duration: (source.duration.is_finite() && source.duration > 0.)
                            .then_some(source.duration),
                        published: source.published.clone(),
                        local: !source.online,
                        link: source.online.then(|| source.input.clone()),
                    },
                )
            })
            .collect();
    }

    pub fn note_source(&self, course: &Course) -> Option<&NoteSource> {
        self.view_cache
            .sources
            .get(&course.manifest.as_ref()?.task_id)
    }

    pub fn course_display_title(&self, course: &Course) -> String {
        self.courses
            .iter()
            .find(|item| item.dir == course.dir)
            .map(|item| item.title.clone())
            .unwrap_or_else(|| course.title.clone())
    }

    /// Reports a note's unreadable titles once, however often the names are applied.
    pub fn merge_alias_issues(&mut self) {
        for issue in self.view_cache.alias_issues.clone() {
            if !self.issues.contains(&issue) {
                self.issues.push(issue);
            }
        }
    }

    /// The note made from `identity` to show first: one saved where the input would save.
    pub fn preferred_note(
        &self,
        identity: &str,
        preferred: Option<&std::path::Path>,
    ) -> Option<&Course> {
        self.notes_for_source(identity)
            .min_by_key(|course| !preferred.is_some_and(|root| course.dir.starts_with(root)))
    }
}

#[cfg(test)]
mod tests {
    use super::{LibraryIssueKind, LibraryScanned, LibraryState};
    use crate::{locations::LocationCheck, notes, organize, workspace};
    use std::path::PathBuf;

    fn location(root: &str) -> workspace::LibraryLocation {
        workspace::LibraryLocation {
            id: root.into(),
            name: root.into(),
            root: root.into(),
            previous_roots: Vec::new(),
        }
    }

    fn scanned(root: &str, folders: bool) -> LibraryScanned {
        (
            location(root),
            LocationCheck {
                available: true,
                needs_reassociation: false,
                problem: None,
            },
            (
                Ok(notes::LibraryScan {
                    courses: Vec::new(),
                    issues: vec!["一份笔记无法读取".into()],
                    attention: Vec::new(),
                    newer: Vec::new(),
                }),
                if folders {
                    Ok(organize::Library::default())
                } else {
                    Err(anyhow::anyhow!("坏了"))
                },
                Default::default(),
            ),
        )
    }

    /// A note being replaced is missing from the scan's notes and waits on the
    /// person: however often the library is scanned, it keeps when it was added.
    #[test]
    fn a_note_waiting_on_the_person_keeps_its_place() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_path_buf();
        let seen = crate::order::Seen {
            folder: "course-a".into(),
            id: "course-a".into(),
            created: 7,
        };
        crate::order::Order::observe(&root, &[seen], true, &[]);
        let mut location = location("library");
        location.root = root.clone();
        let scan = |attention: Vec<c2m_artifact::artifact::NoteAttention>| notes::LibraryScan {
            courses: Vec::new(),
            issues: Vec::new(),
            attention,
            newer: Vec::new(),
        };
        let blocked = c2m_artifact::artifact::NoteAttention::Blocked {
            note: root.join("course-a"),
            swap: root.join(".course2md/swap/course-a"),
        };
        for _ in 0..crate::order::FORGET_AFTER + 1 {
            let cache =
                super::LibraryViewCache::inspect(&location, Some(&scan(vec![blocked.clone()])));
            assert_eq!(cache.orders[&root].added("course-a"), Some(7));
        }
        // Once nothing waits and it is still gone, it is forgotten in time.
        for _ in 0..crate::order::FORGET_AFTER {
            super::LibraryViewCache::inspect(&location, Some(&scan(Vec::new())));
        }
        assert_eq!(crate::order::Order::load(&root).added("course-a"), None);
    }

    #[test]
    fn a_scan_replaces_what_was_known_and_reports_each_library_apart() {
        let mut state = LibraryState::new(PathBuf::from("/a"));
        assert_eq!(state.begin_scan(), 1);
        assert!(state.loading);
        state.apply_scan(vec![scanned("/a", true), scanned("/b", false)]);
        assert!(!state.loading);
        assert_eq!(state.indexes.len(), 1);
        assert_eq!(state.issues.len(), 3);
        assert!(
            state
                .issues
                .iter()
                .any(|issue| issue.kind == LibraryIssueKind::Folders
                    && issue.root == std::path::Path::new("/b"))
        );
        state.apply_scan(vec![scanned("/a", true)]);
        assert_eq!(state.issues.len(), 1);
    }

    #[test]
    fn an_unregistered_root_follows_the_first_library_that_reads() {
        let mut state = LibraryState::new(PathBuf::from("/gone"));
        state.apply_scan(vec![scanned("/a", true)]);
        state.settle_root(&[location("/a")]);
        assert_eq!(state.root, PathBuf::from("/a"));
        assert_eq!(state.error, None);
        let mut lost = LibraryState::new(PathBuf::from("/gone"));
        lost.apply_scan(Vec::new());
        lost.settle_root(&[location("/a")]);
        assert!(lost.error.is_some());
        assert_eq!(lost.root, PathBuf::from("/gone"));
    }

    #[test]
    fn a_note_with_a_running_task_is_not_trashed() {
        use crate::workspace::{Intent, TaskPlan, TaskRecord, TaskState};
        let task = |state: TaskState| TaskRecord {
            id: "task-1".into(),
            plan: TaskPlan {
                operation: Default::default(),
                source: Default::default(),
                source_id: "video-a".into(),
                title: "Course A".into(),
                library_id: "library-1".into(),
                folder: None,
                subtitle: None,
                text_source: None,
                repair: None,
            },
            state,
            intent: Intent::Run,
            created: 0,
            updated: 0,
            parent: None,
            handled_by: None,
            held: false,
            work_dir: "/tmp/work".into(),
            events: crate::activity::JobEvents {
                error: None,
                ..Default::default()
            },
            artifact: None,
            unread: false,
            resend: vec![],
            replaced_by: None,
            ran_with: None,
        };
        let busy = super::note_has_unfinished_task;
        let running = [task(TaskState::Running)];
        assert!(busy(&running, "library-1", Some("video-a")));
        // The old guard compared the id with the library's root path.
        assert!(!busy(&running, "/Users/someone/Notes", Some("video-a")));
        assert!(!busy(&running, "library-2", Some("video-a")));
        assert!(!busy(&running, "library-1", Some("video-b")));
        assert!(!busy(&running, "library-1", None));
        assert!(!busy(
            &[task(TaskState::Complete)],
            "library-1",
            Some("video-a")
        ));
        // An uncertain attempt whose resend ran in a follow-up that has finished.
        let replaced = TaskRecord {
            handled_by: Some("task-2".into()),
            ..task(TaskState::Uncertain)
        };
        let followup = TaskRecord {
            id: "task-2".into(),
            parent: Some("task-1".into()),
            ..task(TaskState::Complete)
        };
        assert!(!busy(&[replaced, followup], "library-1", Some("video-a")));
    }
}
