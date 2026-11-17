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

/// Every file below `dir` with its bytes, but the lock an older app's course
/// holds (flattening takes it as that app does, which creates it).
fn snapshot(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    walk(dir)
        .unwrap()
        .into_iter()
        .filter(|(path, kind)| *kind == EntryKind::File && path != LEGACY_PUBLISH_LOCK)
        .map(|(path, _)| {
            let bytes = std::fs::read(dir.join(&path)).unwrap();
            (path, bytes)
        })
        .collect()
}

/// Ordinary I/O errors at each of `points`, once.
fn io_error_at(points: &[FailPoint]) {
    IO_ERROR_AT.with(|at| at.borrow_mut().extend_from_slice(points));
}

fn crash_at(point: FailPoint) {
    CRASH_AT.with(|crash| crash.set(Some(point)));
}

#[test]
fn a_note_is_one_folder_and_in_place_retries_update_it_cleanly() {
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
    // An in-place retry with the same `task_id` replaces the note with the updated content.
    let again = library
        .publish(&library.target("task-1"), "Different", "")
        .unwrap();
    assert_eq!(again.manifest.task_id, "task-1");
    assert!(
        std::fs::read_to_string(library.note.join("course.md"))
            .unwrap()
            .contains("Different")
    );
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
    let after = std::fs::read_to_string(library.note.join("course.md")).unwrap();
    assert!(after.contains("New body"));
    assert!(
        swap_dir(&library.note)
            .unwrap()
            .join("stale-task-2")
            .is_dir()
    );
    // Recovery clears the stale copy and keeps the new note intact.
    assert!(recover(&library.note).unwrap());
    assert_eq!(
        std::fs::read_to_string(library.note.join("course.md")).unwrap(),
        after
    );
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
fn in_place_repairs_always_replace_the_note_cleanly() {
    let library = Library::new();
    library
        .publish(&library.target("task-1"), "Body", "")
        .unwrap();
    library
        .publish(&library.target("task-2"), "Regenerated", "")
        .unwrap();
    let mut repair = library.target("repair");
    repair.replaces = Some("task-1".into());
    library.publish(&repair, "Repaired", "").unwrap();
    assert!(
        std::fs::read_to_string(library.note.join("course.md"))
            .unwrap()
            .contains("Repaired")
    );
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
    // Listed as the note's own, a regenerate makes it again from the new note.
    let next = library
        .publish(&library.target("task-2"), "Body", "")
        .unwrap();
    assert!(next.kept_edits.is_empty(), "{:?}", next.kept_edits);
    assert_ne!(
        std::fs::read(note.join("exports/structured.json")).unwrap(),
        b"{}"
    );
    assert!(
        next.manifest
            .outputs
            .contains(&"exports/structured.json".to_string())
    );
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
    // The version it pointed at is kept, not deleted.
    assert!(
        legacy
            .course()
            .join("damaged-version-v2/manifest.json")
            .is_file()
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
        FailPoint::AfterRenames,
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
            FailPoint::Staged | FailPoint::Locking => assert_eq!(snapshot(course), before),
            FailPoint::BetweenRenames => assert!(!course.exists()),
            FailPoint::AfterRenames | FailPoint::BeforeDeletingOld => {
                assert!(!is_legacy(course));
                let name = if point == FailPoint::AfterRenames {
                    "old-flatten"
                } else {
                    "stale-flatten"
                };
                let old = swap_dir(course).unwrap().join(name);
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
        // A lock the flat note kept from the swap is not carried past the next
        // publication.
        legacy
            .library
            .publish(&legacy.library.target("after"), "After", "")
            .unwrap();
        assert!(!course.join(LEGACY_PUBLISH_LOCK).exists(), "{point:?}");
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

/// The reader of the app that wrote versions read the current version whenever
/// its body was readable; files changed by hand did not make it fall back.
#[test]
fn flattening_keeps_a_current_version_with_hand_edited_files() {
    let legacy = Legacy::new();
    legacy.version("v1", 1, "First");
    let current = legacy.version("v2", 2, "Second");
    legacy.point_at("v2");
    std::fs::write(current.join("frames/a.jpg"), b"my retouched screenshot").unwrap();
    std::fs::write(current.join("course.md"), b"# my notes").unwrap();
    std::fs::write(current.join("added.txt"), b"added").unwrap();
    let flat = flatten_legacy(legacy.course()).unwrap().unwrap();
    let course = legacy.course();
    assert_eq!(flat.task_id, "v2");
    assert_eq!(
        std::fs::read(course.join("frames/a.jpg")).unwrap(),
        b"my retouched screenshot"
    );
    assert_eq!(
        std::fs::read(course.join("course.md")).unwrap(),
        b"# my notes"
    );
    assert_eq!(std::fs::read(course.join("added.txt")).unwrap(), b"added");
    assert!(!course.join("versions").exists());
    // A regenerate keeps both hand edits beside the new note.
    let next = legacy
        .library
        .publish(&legacy.library.target("next"), "Next", "frames/a.jpg")
        .unwrap();
    assert_eq!(
        next.kept_edits,
        vec!["course.edited.md", "frames/a.edited.jpg"]
    );
    // The only version, edited the same way, is read too.
    let single = Legacy::new();
    let only = single.version("v1", 1, "Only");
    single.point_at("v1");
    std::fs::write(only.join("frames/a.jpg"), b"retouched").unwrap();
    assert_eq!(
        flatten_legacy(single.course()).unwrap().unwrap().task_id,
        "v1"
    );
}

/// A course with no readable version is not flattened, and a new note is still
/// published beside its versions, which it keeps.
#[test]
fn a_course_with_nothing_readable_can_still_be_regenerated() {
    let legacy = Legacy::new();
    let only = legacy.version("v1", 1, "Only");
    std::fs::remove_file(only.join("document.json")).unwrap();
    legacy.point_at("v1");
    assert!(
        flatten_legacy(legacy.course())
            .unwrap_err()
            .is::<NothingReadable>()
    );
    prepare_note(legacy.course()).unwrap();
    legacy
        .library
        .publish(&legacy.library.target("fresh"), "Fresh", "")
        .unwrap();
    let course = legacy.course();
    assert_eq!(note_manifest(course).unwrap().unwrap().task_id, "fresh");
    assert!(course.join("versions/v1/manifest.json").is_file());
    assert!(course.join("current.json").is_file());
    assert!(!is_legacy(course));
}

fn outcomes(summary: Outcome, screenshots: Outcome) -> Outcomes {
    Outcomes {
        transcript: Outcome::succeeded(),
        summary,
        screenshots,
        ..Default::default()
    }
}

fn publish_with(library: &Library, task: &str, outcomes: Outcomes) -> Result<Publication> {
    publish(
        &library.target(task),
        &library.work,
        &meta("Title"),
        &sections("Body", ""),
        None,
        &[],
        outcomes,
        &TaskControl::default(),
    )
}

#[test]
fn failed_summary_retry_preserves_existing_summary_without_failing_publication() {
    let library = Library::new();
    let initial_summary = Summary {
        tldr: "核心结论".into(),
        key_points: vec!["要点一".into()],
        outline: Vec::new(),
    };
    publish(
        &library.target("complete"),
        &library.work,
        &meta("Title"),
        &sections("First body", ""),
        Some(&initial_summary),
        &[],
        outcomes(Outcome::succeeded(), Outcome::succeeded()),
        &TaskControl::default(),
    )
    .unwrap();

    // A retry whose summary failed publishes the updated body while keeping the existing summary.
    let updated = publish(
        &library.target("complete"),
        &library.work,
        &meta("Title"),
        &sections("Updated body", ""),
        None,
        &[],
        outcomes(Outcome::failed("service unreachable"), Outcome::succeeded()),
        &TaskControl::default(),
    )
    .unwrap();
    assert!(updated.manifest.partial);
    assert_eq!(updated.manifest.outcomes.summary.status, Status::Failed);
    let doc: Document =
        serde_json::from_slice(&std::fs::read(library.note.join("document.json")).unwrap())
            .unwrap();
    assert_eq!(doc.summary.unwrap().tldr, "核心结论");
    let md = std::fs::read_to_string(library.note.join("course.md")).unwrap();
    assert!(md.contains("Updated body") && md.contains("核心结论"));

    // Explicitly turning summary off (`NotRequested`) omits it as chosen.
    publish_with(
        &library,
        "without-ai",
        outcomes(Outcome::not_requested(), Outcome::succeeded()),
    )
    .unwrap();
    let plain_doc: Document =
        serde_json::from_slice(&std::fs::read(library.note.join("document.json")).unwrap())
            .unwrap();
    assert!(plain_doc.summary.is_none());
}

#[test]
fn every_export_the_note_had_is_made_again_from_the_new_note() {
    use crate::config::OutputFormat;
    let library = Library::new();
    publish(
        &library.target("first"),
        &library.work,
        &meta("Title"),
        &sections("First body", "frames/a.jpg"),
        None,
        &[OutputFormat::Md, OutputFormat::Json],
        Outcomes::default(),
        &TaskControl::default(),
    )
    .unwrap();
    // A summary-only repair asks for no export.
    let next = publish(
        &library.target("second"),
        &library.work,
        &meta("Title"),
        &sections("Second body", ""),
        None,
        &[],
        Outcomes::default(),
        &TaskControl::default(),
    )
    .unwrap();
    assert_eq!(
        next.manifest.outputs,
        vec!["exports/course.zip", "exports/structured.json"]
    );
    let json = std::fs::read_to_string(library.note.join("exports/structured.json")).unwrap();
    assert!(json.contains("Second body") && !json.contains("First body"));
    // Screenshots the new note does not have leave no empty folder behind.
    assert!(!library.note.join("frames").exists());
}

#[cfg(unix)]
#[test]
fn a_note_reached_through_a_link_is_replaced_where_it_lives() {
    let library = Library::new();
    let elsewhere = library
        .library()
        .parent()
        .unwrap()
        .join("elsewhere/real-note");
    std::fs::create_dir_all(elsewhere.parent().unwrap()).unwrap();
    let real = Target {
        course_dir: elsewhere.clone(),
        ..library.target("first")
    };
    library.publish(&real, "First", "").unwrap();
    std::fs::create_dir_all(library.library()).unwrap();
    std::os::unix::fs::symlink(&elsewhere, &library.note).unwrap();
    library
        .publish(&library.target("second"), "Second", "")
        .unwrap();
    assert!(
        std::fs::symlink_metadata(&library.note)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        note_manifest(&elsewhere).unwrap().unwrap().task_id,
        "second"
    );
    assert!(!library.library().join(".course2md/swap").exists());
}

#[test]
fn a_second_task_for_a_note_is_refused_while_the_first_holds_it() {
    let library = Library::new();
    let first = claim_note(&library.note).unwrap();
    assert!(claim_note(&library.note).is_err());
    drop(first);
    claim_note(&library.note).unwrap();
}

#[test]
fn a_full_disk_is_not_reported_as_a_note_in_use() {
    let full = format!(
        "{:#}",
        replace_error(std::io::Error::from(std::io::ErrorKind::StorageFull))
    );
    assert!(
        full.contains("磁盘空间不足") || full.contains("disk space"),
        "{full}"
    );
    let other = format!(
        "{:#}",
        replace_error(std::io::Error::from(std::io::ErrorKind::InvalidInput))
    );
    assert!(
        !other.contains("其他程序") && !other.contains("another program"),
        "{other}"
    );
}

/// Replaced notes that cannot be deleted are made writable and cleaned up; even
/// temporarily undeletable entries never deadlock future tasks.
#[cfg(unix)]
#[test]
fn replaced_notes_in_swap_are_cleaned_up_and_never_block_future_tasks() {
    use std::os::unix::fs::PermissionsExt;
    let library = Library::new();
    library
        .publish(&library.target("task-1"), "Body", "")
        .unwrap();
    let swap = swap_dir(&library.note).unwrap();
    // A read-only folder inside a replaced note is made writable and deleted.
    std::fs::create_dir_all(swap.join("stale-x/locked")).unwrap();
    std::fs::write(swap.join("stale-x/locked/file"), b"x").unwrap();
    std::fs::set_permissions(
        swap.join("stale-x/locked"),
        std::fs::Permissions::from_mode(0o555),
    )
    .unwrap();
    prepare_note(&library.note).unwrap();
    assert!(!swap.exists());
    // Even many leftover copies while swap is read-only do not stop prepare_note.
    for n in 0..5 {
        std::fs::create_dir_all(swap.join(format!("stale-{n}"))).unwrap();
    }
    std::fs::set_permissions(&swap, std::fs::Permissions::from_mode(0o555)).unwrap();
    let result = prepare_note(&library.note);
    std::fs::set_permissions(&swap, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(result.is_ok());
    // Once writable again, the next task clears them all.
    prepare_note(&library.note).unwrap();
    assert!(!swap.exists());
}

#[test]
fn a_folder_of_notes_recovers_each_interrupted_replacement() {
    let library = Library::new();
    library
        .publish(&library.target("task-1"), "Body", "")
        .unwrap();
    let before = snapshot(&library.note);
    crash_at(FailPoint::BetweenRenames);
    assert!(
        library
            .publish(&library.target("task-2"), "New", "")
            .is_err()
    );
    assert!(!library.note.exists());
    assert!(recover_folder(&library.library()).is_empty());
    assert_eq!(snapshot(&library.note), before);
}

/// The reviewer's repro: a file of note A cannot be deleted, so replacing A with
/// B leaves a half-deleted copy of A behind, newer than the copy of B. A crash
/// between the renames of the next replacement must put B back, never A.
#[cfg(target_os = "macos")]
#[test]
fn a_crash_puts_back_the_note_that_was_in_place_not_a_stuck_older_copy() {
    let flags = |flag: &str, path: &Path| {
        assert!(
            std::process::Command::new("chflags")
                .arg(flag)
                .arg(path)
                .status()
                .unwrap()
                .success()
        );
    };
    let library = Library::new();
    library
        .publish(&library.target("note-a"), "Body A", "frames/a.jpg")
        .unwrap();
    let immutable = library.note.join("frames/a.jpg");
    flags("uchg", &immutable);
    library
        .publish(&library.target("note-b"), "Body B", "")
        .unwrap();
    let swap = swap_dir(&library.note).unwrap();
    let stuck = swap.join("stale-note-b");
    assert!(stuck.join("frames/a.jpg").is_file(), "A's copy is stuck");
    std::fs::write(library.note.join("mine.txt"), b"added to B").unwrap();
    let before = snapshot(&library.note);
    // Make the stuck copy the newest thing in the swap folder, as deleting the
    // rest of it does.
    std::thread::sleep(std::time::Duration::from_millis(20));
    std::fs::create_dir(stuck.join("touched")).unwrap();
    std::fs::remove_dir(stuck.join("touched")).unwrap();
    crash_at(FailPoint::BetweenRenames);
    let error = library
        .publish(&library.target("note-c"), "Body C", "")
        .unwrap_err();
    assert!(simulated_crash(&error));
    assert!(!library.note.exists());
    assert!(recover(&library.note).unwrap());
    assert_eq!(
        snapshot(&library.note),
        before,
        "B and the added file are back"
    );
    assert_eq!(
        note_manifest(&library.note).unwrap().unwrap().task_id,
        "note-b"
    );
    // The stuck copy is still a copy, never the note.
    assert!(stuck.join("frames/a.jpg").is_file());
    flags("nouchg", &stuck.join("frames/a.jpg"));
    prepare_note(&library.note).unwrap();
    assert!(!swap.exists());
}

/// Without the immutable flag: a leftover copy is never put back in place of the
/// note, whatever its time, and a crash after the renames keeps the new note.
#[test]
fn only_the_copy_named_live_is_ever_put_back() {
    let library = Library::new();
    library
        .publish(&library.target("note-b"), "Body B", "")
        .unwrap();
    let before = snapshot(&library.note);
    let swap = swap_dir(&library.note).unwrap();
    crash_at(FailPoint::BetweenRenames);
    assert!(
        library
            .publish(&library.target("note-c"), "Body C", "")
            .is_err()
    );
    // A copy an earlier replacement left, newer than the copy of B.
    std::thread::sleep(std::time::Duration::from_millis(20));
    std::fs::create_dir_all(swap.join("old-earlier/frames")).unwrap();
    std::fs::write(swap.join("old-earlier/manifest.json"), b"{}").unwrap();
    assert!(recover(&library.note).unwrap());
    assert_eq!(snapshot(&library.note), before);
    assert!(!swap.exists());
    // Missing note, no record, no copy that holds a note: nothing is put back, swap cleaned.
    std::fs::create_dir_all(swap.join("old-stray")).unwrap();
    std::fs::write(swap.join("old-stray/manifest.json"), b"{}").unwrap();
    let moved = library.library().join("moved-away");
    std::fs::rename(&library.note, &moved).unwrap();
    assert!(recover(&library.note).unwrap());
    assert!(!library.note.exists());
    assert!(!swap.exists());
    std::fs::rename(&moved, &library.note).unwrap();
    // A crash after both renames keeps the new note and clears the record.
    crash_at(FailPoint::AfterRenames);
    assert!(
        library
            .publish(&library.target("note-d"), "Body D", "")
            .is_err()
    );
    assert!(recover(&library.note).unwrap());
    assert_eq!(
        note_manifest(&library.note).unwrap().unwrap().task_id,
        "note-d"
    );
    assert!(!swap.exists());
}

/// A link whose target is moved aside mid-replacement still leads recovery,
/// tasks and publishing to where the note lives; relative links too.
#[cfg(unix)]
#[test]
fn a_linked_note_is_recovered_while_its_target_is_moved_aside() {
    let library = Library::new();
    let real = library.library().join("store/real-note");
    std::fs::create_dir_all(real.parent().unwrap()).unwrap();
    library
        .publish(
            &Target {
                course_dir: real.clone(),
                ..library.target("first")
            },
            "First",
            "",
        )
        .unwrap();
    std::os::unix::fs::symlink("store/real-note", &library.note).unwrap();
    let before = snapshot(&real);
    crash_at(FailPoint::BetweenRenames);
    assert!(
        library
            .publish(&library.target("second"), "Second", "")
            .is_err()
    );
    assert!(!real.exists(), "the link points nowhere now");
    assert!(recover(&library.note).unwrap());
    assert_eq!(snapshot(&real), before);
    // Again, recovered by the next task's preparation instead.
    crash_at(FailPoint::BetweenRenames);
    assert!(
        library
            .publish(&library.target("second"), "Second", "")
            .is_err()
    );
    prepare_note(&library.note).unwrap();
    assert_eq!(snapshot(&real), before);
    library
        .publish(&library.target("second"), "Second", "")
        .unwrap();
    assert_eq!(note_manifest(&real).unwrap().unwrap().task_id, "second");
    assert!(
        std::fs::symlink_metadata(&library.note)
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[test]
fn a_full_disk_while_writing_says_so() {
    let error = disk_full(
        anyhow::Error::new(std::io::Error::from(std::io::ErrorKind::StorageFull))
            .context("writing course.md"),
    );
    let text = format!("{error:#}");
    assert!(
        text.contains("磁盘空间不足") || text.contains("disk space"),
        "{text}"
    );
    let other = disk_full(anyhow::anyhow!("other"));
    assert_eq!(format!("{other:#}"), "other");
}

#[test]
fn a_busy_note_and_a_broken_lock_are_told_apart() {
    let library = Library::new();
    let held = claim_note(&library.note).unwrap();
    let Err(busy) = claim_note(&library.note) else {
        panic!("a held note was claimed again")
    };
    assert!(busy.is::<crate::outcome::NoteBusy>(), "{busy:#}");
    let text = format!("{busy:#}");
    assert!(!text.contains("os error"), "{text}");
    drop(held);
    // The lock file's folder is a file: not another task, an error that says what.
    let locks = library_dir(&library.note).unwrap().join("locks");
    std::fs::remove_dir_all(&locks).unwrap();
    std::fs::write(&locks, b"not a folder").unwrap();
    let Err(broken) = claim_note(&library.note) else {
        panic!("a broken lock was claimed")
    };
    assert!(!broken.is::<crate::outcome::NoteBusy>(), "{broken:#}");
}

/// A crash while the marker itself is written leaves its temporary file in the
/// swap folder; recovery clears it with the unfinished new note.
#[test]
fn a_crash_while_naming_the_live_copy_is_recovered() {
    let library = Library::new();
    library
        .publish(&library.target("task-1"), "Body", "")
        .unwrap();
    let before = snapshot(&library.note);
    let swap = swap_dir(&library.note).unwrap();
    std::fs::create_dir_all(swap.join("new-task-2")).unwrap();
    std::fs::write(swap.join(".tmpAbC123"), b"old-task-2").unwrap();
    assert!(recover(&library.note).unwrap());
    assert!(!swap.exists());
    assert_eq!(snapshot(&library.note), before);
}

/// The note moved aside between the renames, with its record intact.
fn between_renames(library: &Library) -> (PathBuf, BTreeMap<String, Vec<u8>>) {
    library
        .publish(&library.target("task-1"), "Body", "frames/a.jpg")
        .unwrap();
    std::fs::write(library.note.join("mine.txt"), b"added").unwrap();
    let before = snapshot(&library.note);
    crash_at(FailPoint::BetweenRenames);
    assert!(
        library
            .publish(&library.target("task-2"), "New", "")
            .is_err()
    );
    let swap = swap_dir(&library.note).unwrap();
    assert!(!library.note.exists());
    let record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(swap.join(LIVE_MARKER)).unwrap()).unwrap();
    assert_eq!(record["copy"], "old-task-2");
    assert_eq!(record["publication"].as_str().unwrap().len(), 32);
    (swap, before)
}

/// Even when `live` is damaged, unreadable, a folder, a symlink, or names a non-note,
/// `recover` self-heals by restoring the valid `old-*` note and cleaning up `swap`.
#[test]
fn a_damaged_record_self_heals_from_valid_old_copy_without_blocking() {
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("empty", b"".to_vec()),
        ("garbage", b"garbage".to_vec()),
        ("parent", b"../course".to_vec()),
        ("traversal", b"old-task-2/../old-task-2".to_vec()),
        ("upper", b"OLD-task-2".to_vec()),
        ("spaced", b" old-task-2  ".to_vec()),
        ("two lines", b"old-task-2\nold-x".to_vec()),
        ("huge", vec![b'a'; 70_000]),
        ("not utf-8", b"\xff\xfeold-task-2".to_vec()),
    ];
    for (case, bytes) in cases {
        let library = Library::new();
        let (swap, before) = between_renames(&library);
        std::fs::write(swap.join(LIVE_MARKER), &bytes).unwrap();
        assert!(recover(&library.note).unwrap(), "{case}");
        assert!(note_attention(&library.note).is_none(), "{case}");
        assert_eq!(snapshot(&library.note), before, "{case}");
        assert!(!swap.exists(), "{case}");
    }
    // A folder, a link, or a record naming a file: all self-heal cleanly.
    let library = Library::new();
    let (swap, before) = between_renames(&library);
    std::fs::remove_file(swap.join(LIVE_MARKER)).unwrap();
    std::fs::create_dir(swap.join(LIVE_MARKER)).unwrap();
    assert!(recover(&library.note).unwrap());
    assert_eq!(snapshot(&library.note), before);
    assert!(!swap.exists());
    #[cfg(unix)]
    {
        let library = Library::new();
        let (swap, before) = between_renames(&library);
        let outside = library.library().join("outside");
        std::fs::write(&outside, "old-task-2").unwrap();
        std::fs::remove_file(swap.join(LIVE_MARKER)).unwrap();
        std::os::unix::fs::symlink(&outside, swap.join(LIVE_MARKER)).unwrap();
        assert!(recover(&library.note).unwrap());
        assert_eq!(snapshot(&library.note), before);
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "old-task-2");
        assert!(!swap.exists());
    }
    let library = Library::new();
    let (swap, before) = between_renames(&library);
    std::fs::write(swap.join("old-file"), b"hi").unwrap();
    std::fs::write(swap.join(LIVE_MARKER), "old-file").unwrap();
    assert!(recover(&library.note).unwrap());
    assert_eq!(snapshot(&library.note), before);
    assert!(!swap.exists());
}

#[cfg(unix)]
#[test]
fn an_unreadable_record_self_heals_from_valid_old_copy() {
    use std::os::unix::fs::PermissionsExt;
    let library = Library::new();
    let (swap, before) = between_renames(&library);
    std::fs::set_permissions(
        swap.join(LIVE_MARKER),
        std::fs::Permissions::from_mode(0o000),
    )
    .unwrap();
    assert!(recover(&library.note).unwrap());
    assert_eq!(snapshot(&library.note), before);
    assert!(!swap.exists());
}

/// Something recreated the note's folder before recovery ran: an empty folder,
/// or one a sync tool or Finder made. It is not a note: the valid copy goes back
/// and swap is cleaned up.
#[test]
fn a_folder_recreated_at_the_note_is_replaced_by_the_restored_note() {
    for contents in [None, Some(".DS_Store")] {
        let library = Library::new();
        let (swap, before) = between_renames(&library);
        std::fs::create_dir(&library.note).unwrap();
        if let Some(name) = contents {
            std::fs::write(library.note.join(name), b"x").unwrap();
        }
        assert!(recover(&library.note).unwrap());
        assert_eq!(snapshot(&library.note), before, "{contents:?}");
        assert!(!swap.exists(), "{contents:?}");
    }
}

/// A copy without a record is cleaned up while a note is in place, and put back
/// when no note is in place.
#[test]
fn a_copy_without_a_record_heals_missing_note_or_cleans_up_when_intact() {
    let library = Library::new();
    library
        .publish(&library.target("task-1"), "Body", "")
        .unwrap();
    let swap = swap_dir(&library.note).unwrap();
    std::fs::create_dir_all(&swap).unwrap();
    let copy = swap.join("old-NESTED");
    copy_tree(&library.note, &copy);
    let kept = snapshot(&copy);
    // With the note intact, leftover `old-*` is cleaned up.
    assert!(recover(&library.note).unwrap());
    assert!(!swap.exists());
    // With the note gone, a valid `old-*` copy restores the note.
    std::fs::create_dir_all(&swap).unwrap();
    copy_tree(&library.note, &copy);
    std::fs::remove_dir_all(&library.note).unwrap();
    assert!(recover(&library.note).unwrap());
    assert_eq!(snapshot(&library.note), kept);
    assert!(!swap.exists());
}

/// A record left after the new note was in place names a copy no longer there:
/// the record goes, and nothing else changes.
#[test]
fn a_record_after_a_finished_replacement_is_cleared() {
    let library = Library::new();
    library
        .publish(&library.target("task-1"), "Body", "")
        .unwrap();
    let before = snapshot(&library.note);
    let swap = swap_dir(&library.note).unwrap();
    std::fs::create_dir_all(&swap).unwrap();
    // Written by hand with a line ending, as `echo` does.
    std::fs::write(swap.join(LIVE_MARKER), "old-gone\n").unwrap();
    assert!(recover(&library.note).unwrap());
    assert_eq!(snapshot(&library.note), before);
    assert!(!swap.exists());
}

fn copy_tree(from: &Path, to: &Path) {
    for (relative, kind) in walk(from).unwrap() {
        match kind {
            EntryKind::Dir => std::fs::create_dir_all(to.join(&relative)).unwrap(),
            _ => {
                std::fs::create_dir_all(to.join(&relative).parent().unwrap()).unwrap();
                std::fs::copy(from.join(&relative), to.join(&relative)).unwrap();
            }
        }
    }
}

fn another_note(library: &Library, task: &str) -> PathBuf {
    let elsewhere = library.library().join(format!("elsewhere-{task}"));
    library
        .publish(
            &Target {
                course_dir: elsewhere.clone(),
                ..library.target(task)
            },
            "Another body",
            "",
        )
        .unwrap();
    elsewhere
}

/// When an intact note is already in place, leftover swap entries are cleaned up
/// without touching the note in place.
#[test]
fn an_intact_note_in_place_cleans_up_leftover_swap_entries() {
    for how in ["backup", "versions", "prepare"] {
        let library = Library::new();
        let (swap, _) = between_renames(&library);
        let other = another_note(&library, "restored");
        if how == "versions" {
            let version = library.note.join("versions/v1");
            std::fs::create_dir_all(&version).unwrap();
            copy_tree(&other, &version);
            std::fs::write(
                library.note.join("current.json"),
                r#"{"schema":1,"course_id":"course","version_id":"v1","manifest":"versions/v1/manifest.json"}"#,
            )
            .unwrap();
        } else {
            std::fs::rename(&other, &library.note).unwrap();
        }
        let in_place = snapshot(&library.note);
        if how == "prepare" {
            prepare_note(&library.note).unwrap();
        } else {
            assert!(recover(&library.note).unwrap(), "{how}");
        }
        if how != "versions" {
            assert_eq!(snapshot(&library.note), in_place, "{how}");
        }
        assert!(!swap.exists(), "{how}");
        assert!(note_attention(&library.note).is_none());
    }
}

/// A damaged record beside a note in place cleans up the swap folder and keeps the note usable.
#[test]
fn a_damaged_record_beside_a_whole_note_cleans_up_swap_and_keeps_note_usable() {
    let library = Library::new();
    library
        .publish(&library.target("task-1"), "Body", "")
        .unwrap();
    let swap = swap_dir(&library.note).unwrap();
    std::fs::create_dir_all(swap.join("old-x")).unwrap();
    std::fs::write(swap.join(LIVE_MARKER), "garbage").unwrap();
    let before = snapshot(&library.note);
    assert!(recover(&library.note).unwrap());
    prepare_note(&library.note).unwrap();
    assert_eq!(snapshot(&library.note), before);
    assert!(!swap.exists());
    library
        .publish(&library.target("task-2"), "Next", "")
        .unwrap();
    assert!(!swap.exists());
}

/// With no note and no record, the newest valid `old-*` copy is restored and
/// remaining scratch entries are cleaned up.
#[test]
fn record_less_copies_restore_the_newest_valid_note() {
    let library = Library::new();
    library
        .publish(&library.target("note-b"), "Body B", "")
        .unwrap();
    let swap = swap_dir(&library.note).unwrap();
    std::fs::create_dir_all(&swap).unwrap();
    std::fs::rename(&library.note, swap.join("old-Q")).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(10));
    let newer = another_note(&library, "note-a");
    let expected = snapshot(&newer);
    std::fs::rename(&newer, swap.join("old-P")).unwrap();
    assert!(recover(&library.note).unwrap());
    assert_eq!(snapshot(&library.note), expected);
    assert!(!swap.exists());
    assert!(note_attention(&library.note).is_none());
    // One older app's course, moved aside by a flatten that stopped.
    let legacy = Legacy::new();
    legacy.version("v1", 1, "Only");
    legacy.point_at("v1");
    let swap = swap_dir(legacy.course()).unwrap();
    std::fs::create_dir_all(&swap).unwrap();
    std::fs::rename(legacy.course(), swap.join("old-flatten")).unwrap();
    assert!(recover(legacy.course()).unwrap());
    assert!(is_legacy(legacy.course()));
}

/// Recovery that cannot read or write says where and what to check.
#[cfg(unix)]
#[test]
fn recovery_that_cannot_run_says_where_to_look() {
    use std::os::unix::fs::PermissionsExt;
    let library = Library::new();
    let (swap, before) = between_renames(&library);
    std::fs::set_permissions(&swap, std::fs::Permissions::from_mode(0o300)).unwrap();
    let error = recover(&library.note);
    let attention = note_attention(&library.note);
    std::fs::set_permissions(&swap, std::fs::Permissions::from_mode(0o755)).unwrap();
    let text = format!("{:#}", error.unwrap_err());
    assert!(
        text.contains(&swap.display().to_string()) && !text.starts_with("Permission"),
        "{text}"
    );
    assert!(matches!(attention, Some(NoteAttention::Failed { .. })));
    assert!(recover(&library.note).unwrap());
    assert_eq!(snapshot(&library.note), before);
}

/// Flattening holds the lock an older app holds while it publishes a version:
/// a publication already under way finishes first and is the one kept, and
/// one that starts meanwhile cannot take the lock.
#[test]
fn flattening_waits_for_an_older_apps_publication() {
    let legacy = Legacy::new();
    legacy.version("v1", 1, "First");
    legacy.point_at("v1");
    let course = legacy.course().to_path_buf();
    let lock = crate::runtime::lock_file(&course.join(LEGACY_PUBLISH_LOCK)).unwrap();
    let (started, release) = std::sync::mpsc::channel();
    let older_app = std::thread::spawn({
        let legacy_course = course.clone();
        move || {
            started.send(()).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(500));
            // The older app's version lands, and its pointer moves, under its lock.
            std::fs::rename(
                legacy_course.join("versions/v1"),
                legacy_course.join("versions/v2"),
            )
            .unwrap();
            let path = legacy_course.join("versions/v2/manifest.json");
            let mut value: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            value["task_id"] = "v2".into();
            value["version_id"] = "v2".into();
            std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
            std::fs::write(
                legacy_course.join("current.json"),
                r#"{"schema":1,"course_id":"course","version_id":"v2","manifest":"versions/v2/manifest.json"}"#,
            )
            .unwrap();
            drop(lock);
        }
    });
    release.recv().unwrap();
    let flat = flatten_legacy(&course).unwrap().unwrap();
    older_app.join().unwrap();
    assert_eq!(flat.task_id, "v2", "the version published first is kept");
    assert!(!course.join(LEGACY_PUBLISH_LOCK).exists());
    // While a flatten holds it, the older app cannot take it.
    let legacy = Legacy::new();
    legacy.version("v1", 1, "First");
    legacy.point_at("v1");
    let held = crate::runtime::lock_file(&legacy.course().join(LEGACY_PUBLISH_LOCK)).unwrap();
    let busy = std::thread::spawn({
        let course = legacy.course().to_path_buf();
        move || crate::runtime::try_lock_file(&course.join(LEGACY_PUBLISH_LOCK)).unwrap()
    });
    assert!(busy.join().unwrap().is_none());
    drop(held);
}

/// Swap entries of a note another writer holds are untouched until the writer finishes.
#[test]
fn swap_of_a_note_another_writer_holds_is_untouched() {
    let library = Library::new();
    library
        .publish(&library.target("task-1"), "Body", "")
        .unwrap();
    let swap = swap_dir(&library.note).unwrap();
    std::fs::create_dir_all(swap.join("old-x")).unwrap();
    let writer = lock_note(&library.note).unwrap();
    assert!(note_attention(&library.note).is_none());
    assert!(swap.join("old-x").is_dir());
    drop(writer);
    assert!(note_attention(&library.note).is_none());
    assert!(!swap.exists());
}

/// A lock an interrupted flatten or an older app left in a flat note is taken
/// where it is, never removed and made again, and a regenerate never carries one
/// into the new note.
#[test]
fn a_left_lock_is_not_part_of_the_note() {
    let library = Library::new();
    library
        .publish(&library.target("task-1"), "Body", "")
        .unwrap();
    let lock = library.note.join(LEGACY_PUBLISH_LOCK);
    std::fs::write(&lock, b"").unwrap();
    let left_at = std::fs::metadata(&lock).unwrap().modified().unwrap();
    prepare_note(&library.note).unwrap();
    assert_eq!(
        std::fs::metadata(&lock).unwrap().modified().unwrap(),
        left_at
    );
    library
        .publish(&library.target("task-2"), "Body two", "")
        .unwrap();
    assert!(!lock.exists());
    assert!(
        walk(&library.note)
            .unwrap()
            .iter()
            .all(|(path, _)| !path.contains(".publish"))
    );
}

/// A flat note an older app saved a newer version into: that version becomes the
/// note and the person's files come along cleanly.
#[test]
fn a_version_an_older_app_saved_later_becomes_the_note_and_keeps_added_files() {
    let legacy = Legacy::populated();
    let course = legacy.course().to_path_buf();
    flatten_legacy(&course).unwrap().unwrap();
    std::fs::write(course.join("added-after.txt"), b"later").unwrap();
    let resave = || {
        legacy.version("v9", 9, "Ninth");
        legacy.point_at("v9");
        std::fs::write(course.join(LEGACY_PUBLISH_LOCK), b"").unwrap();
        let exports = course.join("exports/v9/t9");
        std::fs::create_dir_all(&exports).unwrap();
        std::fs::write(exports.join("course.html"), b"<p>v9 export</p>").unwrap();
    };
    resave();
    assert!(!is_legacy(&course));
    assert!(needs_flatten(&course));
    let manifest = flatten_legacy(&course).unwrap().unwrap();
    assert_eq!(manifest.task_id, "v9");
    let flat = note_manifest(&course).unwrap().unwrap();
    validate_note(&course, &flat).unwrap();
    assert!(
        std::fs::read_to_string(course.join("course.md"))
            .unwrap()
            .contains("Ninth")
    );
    for gone in [
        "versions",
        "current.json",
        LEGACY_PUBLISH_LOCK,
        "exports/v9",
    ] {
        assert!(!course.join(gone).exists(), "{gone}");
    }
    assert_eq!(
        std::fs::read(course.join("exports/course.t9.html")).unwrap(),
        b"<p>v9 export</p>"
    );
    assert_eq!(std::fs::read(course.join("my-notes.txt")).unwrap(), b"mine");
    assert_eq!(
        std::fs::read(course.join("added-after.txt")).unwrap(),
        b"later"
    );
    assert!(!swap_dir(&course).unwrap().exists());
    assert!(!needs_flatten(&course));
    // A version older than the flat note is not taken, and nothing changes.
    let legacy = Legacy::populated();
    let course = legacy.course().to_path_buf();
    flatten_legacy(&course).unwrap().unwrap();
    legacy.version("v0", 0, "Zeroth");
    legacy.point_at("v0");
    assert!(!needs_flatten(&course));
    let before = snapshot(&course);
    assert!(flatten_legacy(&course).unwrap().is_none());
    assert_eq!(snapshot(&course), before);
    assert!(!swap_dir(&course).unwrap().exists());
}

/// A lock file that cannot be opened is not reported as an older app saving.
#[test]
fn a_lock_that_cannot_be_opened_is_not_called_busy() {
    let legacy = Legacy::new();
    legacy.version("v1", 1, "First");
    legacy.point_at("v1");
    let lock = legacy.course().join(LEGACY_PUBLISH_LOCK);
    std::fs::create_dir(&lock).unwrap();
    let error = format!("{:#}", flatten_legacy(legacy.course()).unwrap_err());
    assert!(
        !error.contains(tr!(
            "旧版应用正在保存这份笔记，请等它完成后重试 / An older version of the app is saving this note; try again when it finishes"
        )),
        "{error}"
    );
    assert!(error.contains(&lock.display().to_string()), "{error}");
    assert!(is_legacy(legacy.course()));
}

/// A flat note an older app saved a newer version into (`Second` is the flat
/// note's text, `Ninth` the older app's).
fn resaved_note() -> (Legacy, PathBuf) {
    let legacy = Legacy::populated();
    let course = legacy.course().to_path_buf();
    flatten_legacy(&course).unwrap().unwrap();
    legacy.version("v9", 9, "Ninth");
    legacy.point_at("v9");
    assert!(needs_flatten(&course));
    (legacy, course)
}

/// A crash at any point of a flatten of a resaved note self-heals on the next run
/// and makes the older app's version the note cleanly.
#[test]
fn a_crash_while_taking_an_older_apps_version_heals_cleanly() {
    for point in [
        FailPoint::Staged,
        FailPoint::BetweenRenames,
        FailPoint::AfterRenames,
        FailPoint::BeforeDeletingOld,
    ] {
        let (_legacy, course) = resaved_note();
        crash_at(point);
        assert!(flatten_legacy(&course).is_err(), "{point:?}");
        flatten_legacy(&course).unwrap();
        prepare_note(&course).unwrap();
        assert!(
            std::fs::read_to_string(course.join("course.md"))
                .unwrap()
                .contains("Ninth"),
            "{point:?}"
        );
        assert!(!needs_flatten(&course), "{point:?}");
        assert!(!swap_dir(&course).unwrap().exists(), "{point:?}");
    }
}

/// A publication into a flat note waits for an older app saving into it, and
/// the version that app saved is flattened before the note is replaced, not
/// swapped away under it.
#[test]
fn a_publication_waits_for_an_older_app_saving_into_the_note() {
    let legacy = Legacy::populated();
    let course = legacy.course().to_path_buf();
    flatten_legacy(&course).unwrap().unwrap();
    let lock = crate::runtime::lock_file(&course.join(LEGACY_PUBLISH_LOCK)).unwrap();
    let (started, ready) = std::sync::mpsc::channel();
    let older_app = std::thread::spawn({
        let legacy_version = legacy.version("v9", 9, "Ninth");
        let course = course.clone();
        // The version is staged beside the note until the older app's save lands.
        let staged = course.parent().unwrap().join("staged-v9");
        std::fs::rename(&legacy_version, &staged).unwrap();
        std::fs::remove_dir(course.join("versions")).unwrap();
        move || {
            started.send(()).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(500));
            std::fs::create_dir_all(course.join("versions")).unwrap();
            std::fs::rename(&staged, course.join("versions/v9")).unwrap();
            std::fs::write(
                course.join("current.json"),
                r#"{"schema":1,"course_id":"course","version_id":"v9","manifest":"versions/v9/manifest.json"}"#,
            )
            .unwrap();
            drop(lock);
        }
    });
    ready.recv().unwrap();
    legacy
        .library
        .publish(&legacy.library.target("next"), "Next", "")
        .unwrap();
    older_app.join().unwrap();
    assert!(published_by(&course, "next").unwrap().is_some());
    for gone in ["versions", "current.json", LEGACY_PUBLISH_LOCK] {
        assert!(!course.join(gone).exists(), "{gone}");
    }
    assert!(!swap_dir(&course).unwrap().exists());
}

/// A read-only lock left in a flat note does not stop a publication after its
/// work: it is locked through a read-only handle, and not carried.
#[cfg(unix)]
#[test]
fn a_read_only_lock_does_not_stop_a_publication() {
    use std::os::unix::fs::PermissionsExt;
    let legacy = Legacy::populated();
    let course = legacy.course().to_path_buf();
    flatten_legacy(&course).unwrap().unwrap();
    let lock = course.join(LEGACY_PUBLISH_LOCK);
    std::fs::write(&lock, b"").unwrap();
    std::fs::set_permissions(&lock, std::fs::Permissions::from_mode(0o444)).unwrap();
    // Held by the older app, it is still waited for.
    let held = crate::runtime::try_lock_file(&lock).unwrap().unwrap();
    assert!(crate::runtime::try_lock_file(&lock).unwrap().is_none());
    drop(held);
    prepare_note(&course).unwrap();
    legacy
        .library
        .publish(&legacy.library.target("next"), "Next", "")
        .unwrap();
    assert!(!lock.exists());
    assert!(published_by(&course, "next").unwrap().is_some());
}

/// A lock that cannot be opened at all stops a task before its work.
#[test]
fn a_lock_that_cannot_be_opened_stops_the_task_before_its_work() {
    let legacy = Legacy::populated();
    let course = legacy.course().to_path_buf();
    flatten_legacy(&course).unwrap().unwrap();
    let lock = course.join(LEGACY_PUBLISH_LOCK);
    std::fs::create_dir(&lock).unwrap();
    let error = format!("{:#}", prepare_note(&course).unwrap_err());
    assert!(error.contains(&lock.display().to_string()), "{error}");
}

/// A lock path that is a link is refused, never followed, before a task's work
/// and by a flatten.
#[cfg(unix)]
#[test]
fn a_lock_that_is_a_link_is_refused_not_followed() {
    let legacy = Legacy::populated();
    let course = legacy.course().to_path_buf();
    flatten_legacy(&course).unwrap().unwrap();
    let target = legacy.library.library().join("elsewhere");
    std::fs::write(&target, b"").unwrap();
    let lock = course.join(LEGACY_PUBLISH_LOCK);
    std::os::unix::fs::symlink(&target, &lock).unwrap();
    let error = format!("{:#}", prepare_note(&course).unwrap_err());
    assert!(error.contains(&lock.display().to_string()), "{error}");
    assert!(
        legacy
            .library
            .publish(&legacy.library.target("next"), "Next", "")
            .is_err()
    );
    assert!(lock.symlink_metadata().unwrap().file_type().is_symlink());
}

#[test]
fn repeated_before_deleting_old_errors_never_deadlock_future_publications() {
    let library = Library::new();
    library
        .publish(&library.target("task-0"), "Initial", "")
        .unwrap();
    for round in 1..=5 {
        io_error_at(&[FailPoint::BeforeDeletingOld]);
        let _ = library.publish(
            &library.target(&format!("task-{round}")),
            &format!("Body {round}"),
            "",
        );
    }
    library
        .publish(&library.target("task-final"), "Final body", "")
        .unwrap();
    assert!(!swap_dir(&library.note).unwrap().exists());
    assert!(
        std::fs::read_to_string(library.note.join("course.md"))
            .unwrap()
            .contains("Final body")
    );
}
