use super::*;
use crate::timeline::TranscriptEvent;
use std::collections::BTreeMap;

fn meta(title: &str) -> VideoMeta {
    VideoMeta {
        title: title.into(),
        uploader: String::new(),
        duration: 1.,
        webpage_url: "https://example.invalid/video".into(),
        extractor: "example".into(),
        id: "source".into(),
    }
}

fn sections(text: &str, image: &str) -> Vec<Section> {
    vec![Section {
        t: 0.,
        end: 1.,
        image: image.into(),
        speech: vec![TranscriptEvent {
            start: 0.,
            end: 1.,
            text: text.into(),
            raw: None,
            translation: None,
        }],
    }]
}

struct Library {
    _root: tempfile::TempDir,
    work: PathBuf,
    note: PathBuf,
}

impl Library {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let work = root.path().join("work");
        std::fs::create_dir_all(work.join("frames")).unwrap();
        for (name, shade) in [("a", 10u8), ("b", 200)] {
            image::RgbImage::from_pixel(2, 2, image::Rgb([shade; 3]))
                .save_with_format(
                    work.join(format!("frames/{name}.jpg")),
                    image::ImageFormat::Jpeg,
                )
                .unwrap();
        }
        let note = root.path().join("library/course");
        Self {
            work,
            note,
            _root: root,
        }
    }

    fn target(&self, task: &str) -> Target {
        Target {
            task_id: task.into(),
            course_id: "course".into(),
            source_id: "source".into(),
            course_dir: self.note.clone(),
            replaces: None,
        }
    }

    fn publish(&self, target: &Target, text: &str, image: &str) -> Result<Publication> {
        publish(
            target,
            &self.work,
            &meta("Title"),
            &sections(text, image),
            None,
            &[crate::config::OutputFormat::Html],
            Outcomes::default(),
            &TaskControl::default(),
        )
    }

    fn library(&self) -> PathBuf {
        self.note.parent().unwrap().to_path_buf()
    }
}

/// Every file below `dir` with its bytes.
fn snapshot(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    walk(dir)
        .unwrap()
        .into_iter()
        .filter(|(_, kind)| *kind == EntryKind::File)
        .map(|(path, _)| {
            let bytes = std::fs::read(dir.join(&path)).unwrap();
            (path, bytes)
        })
        .collect()
}

fn crash_at(point: FailPoint) {
    CRASH_AT.with(|crash| crash.set(Some(point)));
}

#[test]
fn a_note_is_one_folder_and_its_task_publishes_it_once() {
    let library = Library::new();
    let first = library
        .publish(&library.target("task-1"), "Body", "frames/a.jpg")
        .unwrap();
    assert_eq!(first.manifest.schema, MANIFEST_SCHEMA);
    let written = std::fs::read_to_string(library.note.join("manifest.json")).unwrap();
    assert!(!written.contains("version_id") && !written.contains("revision"));
    for file in [
        "document.json",
        "course.md",
        "frames/a.jpg",
        "exports/course.html",
    ] {
        assert!(library.note.join(file).is_file(), "{file}");
    }
    assert!(!library.note.join("versions").exists());
    assert!(!library.note.join("current.json").exists());
    let before = snapshot(&library.note);
    // The same task again, as after a crash before it recorded its result.
    let again = library
        .publish(&library.target("task-1"), "Different", "")
        .unwrap();
    assert_eq!(again.manifest.created_at_ms, first.manifest.created_at_ms);
    assert_eq!(snapshot(&library.note), before);
    assert_eq!(
        published_by(&library.note, "task-1")
            .unwrap()
            .unwrap()
            .task_id,
        "task-1"
    );
    assert!(published_by(&library.note, "task-2").unwrap().is_none());
    // Nothing of the swap is left beside the note.
    assert!(!swap_dir(&library.note).unwrap().exists());
}

#[test]
fn a_regenerate_keeps_added_and_edited_files_and_drops_the_old_notes_own() {
    let library = Library::new();
    library
        .publish(&library.target("task-1"), "Old body", "frames/a.jpg")
        .unwrap();
    std::fs::write(library.note.join("course.md"), "# My corrections\n").unwrap();
    std::fs::create_dir_all(library.note.join("mine/deep")).unwrap();
    std::fs::write(library.note.join("mine/deep/notes.txt"), b"added").unwrap();
    std::fs::write(library.note.join("exports/handout.pdf"), b"pdf").unwrap();
    std::fs::write(library.note.join("exports/course.html"), b"<p>edited</p>").unwrap();
    let replaced = library
        .publish(&library.target("task-2"), "New body", "frames/b.jpg")
        .unwrap();
    let note = &library.note;
    assert_eq!(replaced.manifest.task_id, "task-2");
    assert_eq!(
        replaced.kept_edits,
        vec!["course.edited.md", "exports/course.edited.html"]
    );
    assert_eq!(
        std::fs::read_to_string(note.join("course.edited.md")).unwrap(),
        "# My corrections\n"
    );
    assert_eq!(
        std::fs::read(note.join("exports/course.edited.html")).unwrap(),
        b"<p>edited</p>"
    );
    assert!(
        std::fs::read_to_string(note.join("course.md"))
            .unwrap()
            .contains("New body")
    );
    assert_eq!(
        std::fs::read(note.join("mine/deep/notes.txt")).unwrap(),
        b"added"
    );
    assert_eq!(
        std::fs::read(note.join("exports/handout.pdf")).unwrap(),
        b"pdf"
    );
    // The old note's own screenshot is gone; the new one is in place.
    assert!(!note.join("frames/a.jpg").exists());
    assert!(note.join("frames/b.jpg").is_file());
    validate_note(note, &replaced.manifest).unwrap();
    // A second edit of the regenerated note is kept under the next free name.
    std::fs::write(note.join("course.md"), "# Second edit\n").unwrap();
    let third = library
        .publish(&library.target("task-3"), "Third body", "")
        .unwrap();
    assert_eq!(third.kept_edits, vec!["course.edited-2.md"]);
    assert_eq!(
        std::fs::read_to_string(note.join("course.edited.md")).unwrap(),
        "# My corrections\n"
    );
}

#[test]
fn an_edited_markdown_file_leaves_the_note_intact() {
    let library = Library::new();
    let published = library
        .publish(&library.target("task-1"), "Body", "")
        .unwrap();
    std::fs::write(library.note.join("course.md"), "edited").unwrap();
    validate_note(&library.note, &published.manifest).unwrap();
    assert!(published_by(&library.note, "task-1").unwrap().is_some());
    std::fs::write(library.note.join("document.json"), "{}").unwrap();
    assert!(validate_note(&library.note, &published.manifest).is_err());
    assert!(published_by(&library.note, "task-1").unwrap().is_none());
}

#[test]
fn a_crash_while_building_leaves_the_note_as_it_was() {
    let library = Library::new();
    library
        .publish(&library.target("task-1"), "Old body", "frames/a.jpg")
        .unwrap();
    let before = snapshot(&library.note);
    crash_at(FailPoint::Staged);
    let error = library
        .publish(&library.target("task-2"), "New body", "")
        .unwrap_err();
    assert!(simulated_crash(&error));
    assert_eq!(snapshot(&library.note), before);
    assert!(swap_dir(&library.note).unwrap().join("new-task-2").is_dir());
    assert!(recover(&library.note).unwrap());
    assert!(!swap_dir(&library.note).unwrap().exists());
    assert_eq!(snapshot(&library.note), before);
    let published = library
        .publish(&library.target("task-2"), "New body", "")
        .unwrap();
    assert_eq!(published.manifest.task_id, "task-2");
}

#[test]
fn a_crash_between_the_renames_puts_the_old_note_back() {
    let library = Library::new();
    library
        .publish(&library.target("task-1"), "Old body", "frames/a.jpg")
        .unwrap();
    std::fs::write(library.note.join("mine.txt"), b"added").unwrap();
    let before = snapshot(&library.note);
    crash_at(FailPoint::BetweenRenames);
    assert!(
        library
            .publish(&library.target("task-2"), "New body", "")
            .is_err()
    );
    assert!(!library.note.exists(), "the crash came between the renames");
    // The next publication recovers before it starts, as does a library scan.
    assert!(recover(&library.note).unwrap());
    assert_eq!(snapshot(&library.note), before);
    assert!(!swap_dir(&library.note).unwrap().exists());
}

#[test]
fn a_crash_before_deleting_the_old_note_keeps_the_new_one() {
    let library = Library::new();
    library
        .publish(&library.target("task-1"), "Old body", "frames/a.jpg")
        .unwrap();
    crash_at(FailPoint::BeforeDeletingOld);
    let error = library
        .publish(&library.target("task-2"), "New body", "")
        .unwrap_err();
    assert!(simulated_crash(&error));
    let after = snapshot(&library.note);
    assert!(swap_dir(&library.note).unwrap().join("old-task-2").is_dir());
    // A restart of the task finds its note in place and leaves it.
    let restarted = library
        .publish(&library.target("task-2"), "New body", "")
        .unwrap();
    assert_eq!(restarted.manifest.task_id, "task-2");
    assert_eq!(snapshot(&library.note), after);
    assert!(!swap_dir(&library.note).unwrap().exists());
}

#[test]
fn a_newer_apps_note_is_refused_before_any_work_and_left_alone() {
    let library = Library::new();
    assert!(refuse_newer(&library.note).is_ok(), "no note yet");
    library
        .publish(&library.target("task-1"), "Body", "")
        .unwrap();
    assert!(refuse_newer(&library.note).is_ok(), "this app's note");
    let path = library.note.join("manifest.json");
    let mut value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    value["schema"] = 3.into();
    std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    let before = snapshot(&library.note);
    let early = refuse_newer(&library.note).unwrap_err();
    assert!(early.is::<NewerSchema>(), "{early:#}");
    assert!(prepare_note(&library.note).unwrap_err().is::<NewerSchema>());
    let error = library
        .publish(&library.target("task-2"), "Body", "")
        .unwrap_err();
    assert!(error.is::<NewerSchema>(), "{error:#}");
    assert!(published_by(&library.note, "task-1").is_err());
    assert_eq!(snapshot(&library.note), before);
}

#[test]
fn a_repair_replaces_only_the_note_it_was_planned_from() {
    let library = Library::new();
    library
        .publish(&library.target("task-1"), "Body", "")
        .unwrap();
    library
        .publish(&library.target("task-2"), "Regenerated", "")
        .unwrap();
    let before = snapshot(&library.note);
    let mut repair = library.target("repair");
    repair.replaces = Some("task-1".into());
    let error = library.publish(&repair, "Repaired", "").unwrap_err();
    assert!(error.is::<NoteChanged>(), "{error:#}");
    assert_eq!(snapshot(&library.note), before);
    repair.replaces = Some("task-2".into());
    library.publish(&repair, "Repaired", "").unwrap();
}

#[test]
fn an_export_is_written_into_the_note_and_listed_as_its_own() {
    use crate::config::OutputFormat;
    let library = Library::new();
    library
        .publish(&library.target("task-1"), "Body", "")
        .unwrap();
    let note = &library.note;
    // The note's own export is replaced; so is a file with the same bytes.
    let written = write_export(note, OutputFormat::Html, b"<p>new</p>").unwrap();
    assert_eq!(written.path, note.join("exports/course.html"));
    assert!(written.kept_edit.is_none());
    let again = write_export(note, OutputFormat::Html, b"<p>new</p>").unwrap();
    assert!(again.kept_edit.is_none());
    // A hand edit is kept beside it.
    std::fs::write(note.join("exports/course.html"), b"<p>mine</p>").unwrap();
    let edited = write_export(note, OutputFormat::Html, b"<p>newer</p>").unwrap();
    assert_eq!(
        edited.kept_edit.as_deref(),
        Some("exports/course.edited.html")
    );
    let json = write_export(note, OutputFormat::Json, b"{}").unwrap();
    assert!(json.kept_edit.is_none());
    let manifest = note_manifest(note).unwrap().unwrap();
    assert!(
        manifest
            .outputs
            .contains(&"exports/structured.json".to_string())
    );
    assert!(
        manifest
            .assets
            .iter()
            .any(|asset| asset.path == "exports/course.html"
                && asset.sha256 == crate::digest::digest(b"<p>newer</p>"))
    );
    validate_note(note, &manifest).unwrap();
    // Listed as the note's own, a regenerate replaces it rather than keep it.
    let next = library
        .publish(&library.target("task-2"), "Body", "")
        .unwrap();
    assert!(next.kept_edits.is_empty(), "{:?}", next.kept_edits);
    assert!(!note.join("exports/structured.json").exists());
    assert_eq!(
        std::fs::read(note.join("exports/course.edited.html")).unwrap(),
        b"<p>mine</p>"
    );
}

#[test]
fn edited_names_keep_the_extension_and_folder() {
    let none = |_: &str| false;
    assert_eq!(edited_name("course.md", none), "course.edited.md");
    assert_eq!(
        edited_name("exports/a.b.html", none),
        "exports/a.b.edited.html"
    );
    assert_eq!(edited_name("README", none), "README.edited");
    assert_eq!(edited_name(".hidden", none), ".hidden.edited");
    assert_eq!(
        edited_name("course.md", |name| name == "course.edited.md"),
        "course.edited-2.md"
    );
    assert_eq!(
        labelled_name("exports/course.html", "task", none),
        "exports/course.task.html"
    );
}

/// A course as the app wrote it before notes were flat.
struct Legacy {
    library: Library,
}

impl Legacy {
    fn new() -> Self {
        Self {
            library: Library::new(),
        }
    }

    fn course(&self) -> &Path {
        &self.library.note
    }

    /// Publishes a flat note elsewhere and turns it into `versions/<id>`.
    fn version(&self, id: &str, revision: u64, text: &str) -> PathBuf {
        let scratch = self.library.library().join(format!("scratch-{id}"));
        let target = Target {
            course_dir: scratch.clone(),
            ..self.library.target(id)
        };
        publish(
            &target,
            &self.library.work,
            &meta("Title"),
            &sections(text, "frames/a.jpg"),
            None,
            &[crate::config::OutputFormat::Html],
            Outcomes::default(),
            &TaskControl::default(),
        )
        .unwrap();
        let dir = self.course().join("versions").join(id);
        std::fs::create_dir_all(dir.parent().unwrap()).unwrap();
        std::fs::rename(&scratch, &dir).unwrap();
        let path = dir.join("manifest.json");
        let mut value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        value["schema"] = 1.into();
        value["version_id"] = id.into();
        value["revision"] = revision.into();
        value["created_at_ms"] = (1000 + revision).into();
        std::fs::write(&path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
        dir
    }

    fn point_at(&self, id: &str) {
        std::fs::write(
            self.course().join("current.json"),
            serde_json::to_vec(&serde_json::json!({
                "schema": 1, "course_id": "course", "version_id": id,
                "manifest": format!("versions/{id}/manifest.json"),
            }))
            .unwrap(),
        )
        .unwrap();
    }

    /// Three versions, the pointer at the middle one, exports of two of them, a
    /// file added to the course folder and an older app's task progress.
    fn populated() -> Self {
        let legacy = Self::new();
        legacy.version("v1", 1, "First");
        legacy.version("v2", 2, "Second");
        legacy.version("v3", 3, "Third");
        legacy.point_at("v2");
        let course = legacy.course();
        for (version, task, bytes) in [
            ("v2", "export-task", &b"<p>v2 export</p>"[..]),
            ("v1", "old-export", b"<p>v1 export</p>"),
        ] {
            let dir = course.join("exports").join(version).join(task);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("course.html"), bytes).unwrap();
            std::fs::write(dir.join("structured.json"), bytes).unwrap();
        }
        std::fs::write(course.join("my-notes.txt"), b"mine").unwrap();
        std::fs::create_dir_all(course.join(".work/cli-task")).unwrap();
        std::fs::write(course.join(".work/cli-task/receipt.json"), b"paid").unwrap();
        legacy
    }
}

#[test]
fn flattening_keeps_the_current_version_and_deletes_the_others() {
    let legacy = Legacy::populated();
    let course = legacy.course();
    assert!(is_legacy(course));
    let manifest = flatten_legacy(course).unwrap().unwrap();
    assert_eq!(manifest.task_id, "v2");
    assert_eq!(manifest.schema, MANIFEST_SCHEMA);
    assert!(!is_legacy(course));
    assert!(!course.join("versions").exists());
    assert!(!course.join("current.json").exists());
    let flat = note_manifest(course).unwrap().unwrap();
    validate_note(course, &flat).unwrap();
    assert!(
        std::fs::read_to_string(course.join("course.md"))
            .unwrap()
            .contains("Second")
    );
    // The current version's later export joins the note's exports; its listed
    // export keeps its name.
    assert_eq!(
        std::fs::read(course.join("exports/course.export-task.html")).unwrap(),
        b"<p>v2 export</p>"
    );
    assert_eq!(
        std::fs::read(course.join("exports/structured.json")).unwrap(),
        b"<p>v2 export</p>"
    );
    assert!(!course.join("exports/v1").exists());
    assert!(!course.join("exports/v2").exists());
    assert_eq!(std::fs::read(course.join("my-notes.txt")).unwrap(), b"mine");
    assert_eq!(
        std::fs::read(
            legacy
                .library
                .library()
                .join(".course2md/work/cli-task/receipt.json")
        )
        .unwrap(),
        b"paid"
    );
    assert!(!course.join(".work").exists());
    assert!(!swap_dir(course).unwrap().exists());
    // Already flat: nothing more to do.
    assert!(flatten_legacy(course).unwrap().is_none());
    // Flat, it is replaced in place like any note.
    legacy
        .library
        .publish(&legacy.library.target("next"), "Next", "")
        .unwrap();
    assert!(published_by(course, "next").unwrap().is_some());
}

#[test]
fn flattening_without_a_usable_pointer_takes_the_newest_intact_version() {
    let legacy = Legacy::new();
    legacy.version("v1", 1, "First");
    legacy.version("v2", 2, "Second");
    let damaged = legacy.version("v3", 3, "Third");
    std::fs::write(damaged.join("document.json"), b"{}").unwrap();
    std::fs::write(legacy.course().join("current.json"), b"{not json").unwrap();
    assert_eq!(
        flatten_legacy(legacy.course()).unwrap().unwrap().task_id,
        "v2"
    );
    // A pointer at a damaged version is passed over the same way.
    let legacy = Legacy::new();
    legacy.version("v1", 1, "First");
    let damaged = legacy.version("v2", 2, "Second");
    std::fs::remove_file(damaged.join("document.json")).unwrap();
    legacy.point_at("v2");
    assert_eq!(
        flatten_legacy(legacy.course()).unwrap().unwrap().task_id,
        "v1"
    );
    // With nothing intact, the course is left as it was.
    let legacy = Legacy::new();
    let only = legacy.version("v1", 1, "First");
    std::fs::remove_file(only.join("document.json")).unwrap();
    legacy.point_at("v1");
    let before = snapshot(legacy.course());
    assert!(flatten_legacy(legacy.course()).is_err());
    assert_eq!(snapshot(legacy.course()), before);
}

#[test]
fn a_newer_apps_pointer_is_not_flattened() {
    let legacy = Legacy::new();
    legacy.version("v1", 1, "First");
    std::fs::write(
        legacy.course().join("current.json"),
        br#"{"schema":2,"course_id":"course","version_id":"v1","manifest":"versions/v1/manifest.json"}"#,
    )
    .unwrap();
    let before = snapshot(legacy.course());
    assert!(
        refuse_newer(legacy.course())
            .unwrap_err()
            .is::<NewerSchema>()
    );
    assert!(
        flatten_legacy(legacy.course())
            .unwrap_err()
            .is::<NewerSchema>()
    );
    assert_eq!(snapshot(legacy.course()), before);
}

#[test]
fn an_interrupted_flatten_is_finished_by_running_it_again() {
    for point in [
        FailPoint::Staged,
        FailPoint::BetweenRenames,
        FailPoint::BeforeDeletingOld,
    ] {
        let legacy = Legacy::populated();
        let course = legacy.course();
        let before = snapshot(course);
        crash_at(point);
        let error = flatten_legacy(course).unwrap_err();
        assert!(simulated_crash(&error), "{point:?}: {error:#}");
        match point {
            // The older versions are still there until the flat note is in place.
            FailPoint::Staged => assert_eq!(snapshot(course), before),
            FailPoint::BetweenRenames => assert!(!course.exists()),
            FailPoint::BeforeDeletingOld => {
                assert!(!is_legacy(course));
                let old = swap_dir(course).unwrap().join("old-flatten");
                assert!(old.join("versions/v1").is_dir() && old.join("versions/v3").is_dir());
            }
        }
        if point == FailPoint::BetweenRenames {
            assert!(recover(course).unwrap());
            assert_eq!(snapshot(course), before, "{point:?}");
        }
        flatten_legacy(course).unwrap();
        assert!(!is_legacy(course), "{point:?}");
        assert_eq!(note_manifest(course).unwrap().unwrap().task_id, "v2");
        assert!(!swap_dir(course).unwrap().exists(), "{point:?}");
        assert_eq!(
            std::fs::read(
                legacy
                    .library
                    .library()
                    .join(".course2md/work/cli-task/receipt.json")
            )
            .unwrap(),
            b"paid",
            "{point:?}"
        );
    }
}

#[test]
fn preparing_a_note_flattens_it_and_moves_task_progress_out() {
    let legacy = Legacy::populated();
    prepare_note(legacy.course()).unwrap();
    assert!(!is_legacy(legacy.course()));
    // A course folder an older app made for a task that never published.
    let library = Library::new();
    std::fs::create_dir_all(library.note.join(".work/task")).unwrap();
    std::fs::write(library.note.join(".work/task/media.mp4"), b"video").unwrap();
    prepare_note(&library.note).unwrap();
    assert!(!library.note.exists());
    assert_eq!(
        std::fs::read(work_dir(&library.note, "task").unwrap().join("media.mp4")).unwrap(),
        b"video"
    );
}
