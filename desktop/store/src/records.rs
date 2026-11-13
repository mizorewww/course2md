//! The records the desktop's pages share: the workspace record of drafts, tasks and
//! reading positions, and the libraries' notes as last scanned. The app holds one in an
//! entity that every page reads.
use crate::{
    library::LibraryState,
    notes::Course,
    workspace::{Draft, LibraryLocation, TaskRecord, Workspace},
};
use std::{
    path::{Path, PathBuf},
    rc::Rc,
};

/// Where a note is filed: for moves and the reader's crumb.
#[derive(Clone)]
pub struct FolderContext {
    pub root: PathBuf,
    pub folder: Option<u64>,
    pub load_error: Option<String>,
    pub label: String,
    /// Every folder in tree order, labelled with its path.
    pub folders: Vec<(u64, String)>,
}

/// What a folder context is asked for.
pub enum FolderPlace<'a> {
    /// The note folder at this path.
    Note(&'a Path),
}

/// Why the folder picker and moves are unavailable.
pub fn folders_unavailable() -> &'static str {
    c2m_i18n::tr!("文件夹暂不可用，请重新扫描笔记库。")
}

pub struct Records {
    /// The workspace record, when it could be read.
    pub workspace: Option<Workspace>,
    pub library: LibraryState,
}

impl Records {
    pub fn new(workspace: Option<Workspace>, library: LibraryState) -> Self {
        Self { workspace, library }
    }

    /// The current input form.
    pub fn draft(&self) -> Option<&Draft> {
        self.workspace.as_ref()?.state.draft()
    }

    /// The task with this id, if the record is readable.
    pub fn task(&self, id: &str) -> Option<&TaskRecord> {
        self.workspace.as_ref()?.state.task(id)
    }

    /// The library a task's plan saves to; the record may name a removed library.
    pub fn task_library(&self, task: &TaskRecord) -> Option<&LibraryLocation> {
        self.workspace
            .as_ref()?
            .state
            .library(&task.plan.library_id)
    }

    /// The registered library with this id.
    pub fn library_location(&self, id: &str) -> Option<&LibraryLocation> {
        self.workspace.as_ref()?.state.library(id)
    }

    /// The registered libraries; without a record, the library in view under no id.
    pub fn locations(&self) -> Vec<LibraryLocation> {
        self.workspace
            .as_ref()
            .map(|workspace| workspace.state.libraries.clone())
            .unwrap_or_else(|| {
                // Without a workspace record the root has no registered id; an
                // empty id skips the marker identity check for this placeholder.
                vec![LibraryLocation {
                    id: String::new(),
                    name: c2m_i18n::tr!("笔记库").into(),
                    root: self.library.root.clone(),
                    previous_roots: Vec::new(),
                }]
            })
    }

    /// Where new notes go: the default library, or the library in view.
    pub fn output(&self) -> PathBuf {
        self.workspace
            .as_ref()
            .and_then(|w| w.state.library(&w.state.default_library))
            .map(|l| l.root.clone())
            .unwrap_or_else(|| self.library.root.clone())
    }

    /// The registered library a note is in.
    pub fn course_location(&self, course: &Course) -> Option<&LibraryLocation> {
        self.library
            .course_location(&self.workspace.as_ref()?.state.libraries, course)
    }

    /// The folder a note is filed in.
    pub fn course_folder(&self, course: &Course) -> Option<u64> {
        self.library
            .course_folder(&self.workspace.as_ref()?.state.libraries, course)
    }

    /// The note at `dir`, as the library lists it.
    pub fn note_at(&self, dir: &std::path::Path) -> Option<Rc<Course>> {
        self.library
            .courses
            .iter()
            .find(|course| course.dir == dir)
            .cloned()
    }

    /// Apply the names already read by the library worker; no I/O.
    pub fn apply_course_title_aliases(&mut self) {
        let tasks = self
            .workspace
            .as_ref()
            .map(|workspace| workspace.state.tasks.as_slice())
            .unwrap_or_default();
        let library = &mut self.library;
        library.index_note_sources(tasks);
        library.merge_alias_issues();
        for course in &mut library.courses {
            if let Some(location) = library.view_cache.locations.get(&course.dir)
                && let Some(name) = library
                    .view_cache
                    .aliases
                    .get(&location.root)
                    .and_then(|names| names.get(&location.relative))
                && course.title != *name
            {
                Rc::make_mut(course).title = name.clone();
            }
        }
    }

    /// Where `place` is filed, as the folder picker and the reader's crumb show it.
    pub fn folder_context(&self, place: FolderPlace) -> FolderContext {
        let library = &self.library;
        let FolderPlace::Note(path) = place;
        let storage = library
            .courses
            .iter()
            .find(|course| course.dir == *path)
            .map(|course| course.dir.clone())
            .unwrap_or_else(|| path.to_path_buf());
        let root = library
            .view_cache
            .locations
            .get(&storage)
            .map(|location| location.root.clone())
            .unwrap_or_else(|| library.root.clone());
        let loaded = library.indexes.get(&root);
        let checking = loaded.is_none() && library.loading;
        let load_error = (loaded.is_none() && !checking).then(|| folders_unavailable().to_owned());
        let folder = library
            .view_cache
            .locations
            .get(&storage)
            .filter(|location| location.root == root)
            .and_then(|location| loaded?.folder_key(&location.relative));
        let label = if checking {
            c2m_i18n::tr!("正在读取文件夹…").to_owned()
        } else if load_error.is_some() {
            c2m_i18n::tr!("文件夹暂不可用").to_owned()
        } else if let Some(id) = folder {
            loaded
                .filter(|organization| organization.folders.contains_key(&id))
                .map(|organization| organization.path(id).join(" / "))
                .unwrap_or_else(|| c2m_i18n::tr!("文件夹已删除，请重新选择").into())
        } else {
            c2m_i18n::tr!("未分类").into()
        };
        FolderContext {
            root,
            folder,
            load_error,
            label,
            folders: loaded
                .map(|organization| {
                    organization
                        .tree()
                        .into_iter()
                        .map(|(id, _)| (id, organization.path(id).join(" / ")))
                        .collect()
                })
                .unwrap_or_default(),
        }
    }
}
