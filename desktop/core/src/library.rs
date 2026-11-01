//! The note library as the app knows it: which folder is in view, where each note lives,
//! and what could not be read.
use crate::{
    locations::LocationCheck,
    notes::{self, Course},
    organize, workspace,
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
    /// Source identity → indices into `Desktop::courses` of its notes.
    pub notes_by_source: std::collections::HashMap<String, Vec<usize>>,
    /// Course identity → index into `Desktop::courses` of its newest version.
    pub newest_by_course: std::collections::HashMap<String, usize>,
    pub recovery: BTreeMap<PathBuf, organize::Recovery>,
    pub title_recovery: BTreeMap<PathBuf, organize::Recovery>,
    pub aliases: BTreeMap<PathBuf, BTreeMap<PathBuf, String>>,
    pub alias_issues: Vec<LibraryIssue>,
}

impl LibraryViewCache {
    /// A newer version of the note `manifest` belongs to, when the library has one.
    pub fn newer_version(
        &self,
        courses: &[Rc<Course>],
        manifest: &course2md::artifact::Manifest,
    ) -> Option<Rc<Course>> {
        self.newest_by_course
            .get(&manifest.course_id)
            .and_then(|index| courses.get(*index))
            .filter(|course| {
                course
                    .manifest
                    .as_ref()
                    // Indices are rebuilt after each scan; until then an inserted
                    // note can shift them, so the identity is checked too.
                    .is_some_and(|newest| {
                        newest.course_id == manifest.course_id
                            && newest.revision > manifest.revision
                    })
            })
            .cloned()
    }

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
                message: course2md::tr!(
                    "{}的显示名称尚未读取：{}",
                    location.name,
                    format!("{error:#}")
                ),
            }),
        }
        if let Some(scan) = scan {
            for course in &scan.courses {
                let storage = course.storage_dir();
                let Ok(relative) = organize::relative_key(&location.root, &storage) else {
                    continue;
                };
                let membership = CourseLocation {
                    id: location.id.clone(),
                    root: location.root.clone(),
                    relative: relative.clone(),
                };
                // Imported versions can use canonical paths while a registered
                // root uses a symlink or /tmp alias. Resolve once in this worker.
                let mut paths = vec![
                    course.dir.clone(),
                    storage.clone(),
                    location.root.join(relative),
                ];
                paths.extend(storage.canonicalize().ok());
                paths.extend(course.dir.canonicalize().ok());
                for path in paths {
                    cache.locations.insert(path, membership.clone());
                }
            }
        }
        cache
    }

    pub fn merge(&mut self, other: Self) {
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
    }
}

/// Whether an unfinished task is still writing a version of the note with
/// `source_id` into the library `library_id`. Tasks name their library by
/// `LibraryLocation::id`, never by its path.
pub fn note_has_unfinished_task(
    tasks: &[crate::workspace::TaskRecord],
    library_id: &str,
    source_id: Option<&str>,
) -> bool {
    source_id.is_some_and(|source| {
        tasks.iter().any(|task| {
            !task.state.finished()
                && task.plan.source_id == source
                && task.plan.library_id == library_id
        })
    })
}

/// What one library's scan found, as `locations::scan_locations` returns it.
pub type LibraryScanned = (
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
            loading: false,
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
        self.courses.clear();
        self.cover_by_source.clear();
        self.issues.clear();
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
                    message: course2md::tr!("{}读取失败：{}", location.name, format!("{error:#}")),
                }),
            }
            match organization {
                Ok(library) => {
                    self.indexes.insert(location.root, library);
                }
                Err(error) => self.issues.push(LibraryIssue {
                    root: location.root.clone(),
                    kind: LibraryIssueKind::Folders,
                    message: course2md::tr!(
                        "{}的文件夹信息已损坏：{}",
                        location.name,
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
            self.error = Some(course2md::tr!("文件夹信息读取失败").into());
        }
    }

    /// The registered library a note lives in.
    pub fn course_location<'a>(
        &self,
        registered: &'a [workspace::LibraryLocation],
        course: &Course,
    ) -> Option<&'a workspace::LibraryLocation> {
        let cached = self.view_cache.locations.get(&course.storage_dir())?;
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
        let cached = self.view_cache.locations.get(&course.storage_dir())?;
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
                    && issue.root == PathBuf::from("/b"))
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
                config: Default::default(),
                asr_service: None,
                ai_service: None,
            },
            state,
            intent: Intent::Run,
            created: 0,
            updated: 0,
            parent: None,
            handled_by: None,
            work_dir: "/tmp/work".into(),
            events: crate::activity::JobEvents {
                error: None,
                ..Default::default()
            },
            artifact: None,
            unread: false,
            resend: vec![],
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
    }
}
