//! The note library as the app knows it: which folder is in view, where each note lives,
//! and what could not be read.
use crate::{
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

#[cfg(test)]
mod tests {
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
