//! The records the desktop's pages share: the workspace record of drafts, tasks and
//! reading positions, and the libraries' notes as last scanned. The app holds one in an
//! entity that every page reads.
use crate::{
    library::LibraryState,
    notes::Course,
    workspace::{Draft, LibraryLocation, TaskRecord, Workspace},
};
use std::{path::PathBuf, rc::Rc};

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

    /// The note version at `dir`, as the library lists it.
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
            if let Some(location) = library.view_cache.locations.get(&course.storage_dir())
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
}
