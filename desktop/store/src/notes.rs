//! Read the library's notes and isolate damaged entries. A note is a folder with a
//! manifest; a course an older app saved as versions is flattened by the startup
//! migration (`crate::migrate`) before it is listed.
use anyhow::{Context, Result};
use c2m_artifact::artifact;
use c2m_protocol::progress::Done;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::SystemTime,
};

fn body_unreadable() -> &'static str {
    c2m_i18n::tr!("笔记正文无法读取，原文件已保留")
}

/// The note was written by a newer app: this app must not show or replace what it
/// cannot read.
#[derive(Debug)]
struct NewerSchema;

impl std::fmt::Display for NewerSchema {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(c2m_i18n::tr!("这份笔记由更新版本的应用生成，请更新应用"))
    }
}

impl std::error::Error for NewerSchema {}

/// Why a conversion is refused before it starts: its note was written by a newer app.
pub fn newer_note_refusal() -> &'static str {
    c2m_i18n::tr!("这份笔记由更新版本的应用生成，请更新应用")
}

/// Whether the note in `dir` was written by a newer app; a missing or damaged one is
/// not. Blocking: it reads the note's manifest.
pub fn note_is_newer(dir: &Path) -> bool {
    artifact::refuse_newer(dir).is_err_and(|error| error.is::<artifact::NewerSchema>())
}

/// The library's own wording for the engine's typed refusal.
fn newer(error: anyhow::Error) -> anyhow::Error {
    if error.is::<artifact::NewerSchema>() {
        NewerSchema.into()
    } else {
        error
    }
}

#[derive(Clone)]
pub struct Course {
    pub dir: PathBuf,
    pub title: String,
    pub modified: SystemTime,
    pub slides: usize,
    pub thumbnail: Option<PathBuf>,
    pub manifest: Option<artifact::Manifest>,
    pub warning: Option<String>,
}
impl Course {
    /// A just-published note. `read_preview` reads its manifest on open and
    /// reports a failure there instead of opening an empty note.
    pub fn from_completed(done: &Done) -> Self {
        Self {
            dir: done.out_dir.clone(),
            title: done.title.clone(),
            modified: SystemTime::now(),
            slides: done.slides,
            thumbnail: None,
            manifest: None,
            warning: None,
        }
    }
    pub fn content_description(&self) -> String {
        if self.slides == 1 {
            // English counts its noun; the catalog keeps the singular apart.
            c2m_i18n::tr!("含 1 张截图").to_owned()
        } else if self.slides > 0 {
            c2m_i18n::tr!("含 {} 张截图", self.slides)
        } else {
            c2m_i18n::tr!("文字笔记").to_owned()
        }
    }
    pub fn incomplete(&self) -> bool {
        self.manifest.as_ref().is_some_and(has_incomplete_content)
    }
}
/// Optional file exports describe a task's output, not the completeness of its note.
pub fn has_incomplete_content(manifest: &artifact::Manifest) -> bool {
    !processing_issues(manifest).is_empty()
}
pub struct LibraryScan {
    pub courses: Vec<Course>,
    pub issues: Vec<String>,
    /// Folders of notes a newer app wrote, which this one must not publish into.
    pub newer: Vec<PathBuf>,
}

fn note_course(dir: &Path) -> Result<Course> {
    let manifest = artifact::read_manifest(&dir.join("manifest.json")).map_err(newer)?;
    anyhow::ensure!(
        artifact::safe_asset_path(dir, &manifest.document).is_ok_and(|path| path.is_file()),
        body_unreadable()
    );
    let images = manifest
        .frames
        .iter()
        .filter_map(|frame| artifact::safe_asset_path(dir, &frame.image).ok())
        .collect::<Vec<_>>();
    Ok(Course {
        dir: dir.to_path_buf(),
        title: manifest.title.clone(),
        modified: SystemTime::UNIX_EPOCH + std::time::Duration::from_millis(manifest.created_at_ms),
        slides: images.len(),
        thumbnail: images.into_iter().next(),
        manifest: Some(manifest),
        warning: None,
    })
}

/// Completes the replacements a crash interrupted in the library at `root`: a note
/// being replaced is put back, or the one it replaced deleted. A note another
/// writer holds is left to it.
pub fn recover_library(root: &Path) -> Vec<String> {
    let mut issues = Vec::new();
    let Ok(entries) = std::fs::read_dir(root.join(".course2md").join("swap")) else {
        return issues;
    };
    for entry in entries.flatten() {
        let note = root.join(entry.file_name());
        if let Err(error) = artifact::recover(&note) {
            issues.push(c2m_i18n::tr!(
                "{}：{}",
                note.display(),
                format!("{error:#}")
            ));
        }
    }
    issues
}

pub fn scan_library(root: &Path) -> Result<LibraryScan> {
    let mut scan = LibraryScan {
        courses: Vec::new(),
        issues: Vec::new(),
        newer: Vec::new(),
    };
    if !root.exists() {
        return Ok(scan);
    }
    scan.issues.extend(recover_library(root));
    // Hidden folders are never notes; nor is anything deeper than eight folders.
    let mut walk = walkdir::WalkDir::new(root)
        .max_depth(8)
        .into_iter()
        .filter_entry(|entry| {
            entry.depth() == 0 || !entry.file_name().to_string_lossy().starts_with('.')
        });
    while let Some(entry) = walk.next() {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                if let Some(io) = error.io_error() {
                    let path = error.path().unwrap_or(root).display();
                    scan.issues.push(c2m_i18n::tr!("{}：{}", path, io));
                }
                continue;
            }
        };
        let dir = entry.path();
        if !entry.file_type().is_dir() || entry.depth() == 0 {
            continue;
        }
        if dir.join("manifest.json").is_file() {
            match note_course(dir) {
                Ok(course) => scan.courses.push(course),
                Err(error) => {
                    if error.is::<NewerSchema>() {
                        scan.newer.push(dir.to_path_buf());
                    }
                    scan.issues
                        .push(c2m_i18n::tr!("{}：{}", dir.display(), format!("{error:#}")))
                }
            }
            walk.skip_current_dir();
        } else if artifact::is_legacy(dir) {
            // Saved as versions by an older app and not flattened yet: the startup
            // migration does it, or waits for a task still writing it.
            if note_is_newer(dir) {
                scan.newer.push(dir.to_path_buf());
            }
            scan.issues.push(c2m_i18n::tr!(
                "{}：{}",
                dir.display(),
                c2m_i18n::tr!("这份笔记仍按旧方式保存，正在等待整理")
            ));
            walk.skip_current_dir();
        }
    }
    scan.courses.sort_by_key(|c| std::cmp::Reverse(c.modified));
    Ok(scan)
}

pub fn default_output() -> PathBuf {
    c2m_config::config::home_dir()
        .unwrap_or_else(|| ".".into())
        .join("Documents")
        .join("course2md")
}

#[derive(Clone)]
pub enum PreviewBlock {
    Image(PathBuf),
    Heading {
        text: Arc<str>,
        anchor: String,
        seconds: Option<f64>,
    },
    Paragraph {
        text: Arc<str>,
        anchor: String,
    },
    /// A bilingual note's translation of the paragraph before it.
    Translation {
        text: Arc<str>,
        anchor: String,
    },
}
impl PreviewBlock {
    /// Section headings are the document's own frame timestamps.
    pub fn is_timestamp_heading(&self) -> bool {
        match self {
            Self::Heading {
                text,
                seconds: Some(seconds),
                ..
            } => c2m_artifact::render::fmt_ts(*seconds) == text.as_ref(),
            _ => false,
        }
    }
    pub fn section_index(&self) -> Option<usize> {
        let Self::Heading { anchor, .. } = self else {
            return None;
        };
        anchor.strip_prefix("section-")?.parse().ok()
    }
}

#[derive(Clone)]
pub enum ProcessingStage {
    Transcript,
    Screenshots,
    Proofreading,
    Translation,
    Summary,
}
impl ProcessingStage {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Transcript => c2m_i18n::tr!("文字转写"),
            Self::Screenshots => c2m_i18n::tr!("截图"),
            Self::Proofreading => c2m_i18n::tr!("AI 校对"),
            Self::Translation => c2m_i18n::tr!("AI 翻译"),
            Self::Summary => c2m_i18n::tr!("摘要"),
        }
    }
}
#[derive(Clone)]
pub struct ProcessingIssue {
    pub stage: ProcessingStage,
    pub outcome: artifact::Outcome,
}
impl ProcessingIssue {
    /// The recorded cause as published (engine messages already use the UI language).
    pub fn detail(&self) -> Option<&str> {
        self.outcome
            .message
            .as_deref()
            .map(str::trim)
            .filter(|detail| !detail.is_empty())
    }
}
fn processing_issues(manifest: &artifact::Manifest) -> Vec<ProcessingIssue> {
    [
        (ProcessingStage::Transcript, &manifest.outcomes.transcript),
        (ProcessingStage::Screenshots, &manifest.outcomes.screenshots),
        (
            ProcessingStage::Proofreading,
            &manifest.outcomes.proofreading,
        ),
        (ProcessingStage::Translation, &manifest.outcomes.translation),
        (ProcessingStage::Summary, &manifest.outcomes.summary),
    ]
    .into_iter()
    .filter(|(_, outcome)| {
        matches!(
            outcome.status,
            artifact::Status::Failed | artifact::Status::Partial
        )
    })
    .map(|(stage, outcome)| ProcessingIssue {
        stage,
        outcome: outcome.clone(),
    })
    .collect()
}
#[derive(Clone)]
pub struct Preview {
    pub course: Course,
    pub blocks: Vec<PreviewBlock>,
    pub outputs: Vec<String>,
    pub plain_text: String,
    pub markdown_text: String,
    pub document: artifact::Document,
    pub issues: Vec<String>,
    /// Incomplete processing is recoverable through its task, never by re-reading files.
    pub processing_issues: Vec<ProcessingIssue>,
}

pub fn read_preview(mut course: Course) -> Result<Preview> {
    let manifest = match course.manifest.take() {
        Some(manifest) => manifest,
        None => {
            artifact::read_manifest(&course.dir.join("manifest.json")).context(body_unreadable())?
        }
    };
    let document: artifact::Document = serde_json::from_slice(&std::fs::read(
        artifact::safe_asset_path(&course.dir, &manifest.document)?,
    )?)?;
    anyhow::ensure!(
        document.schema == 1 && artifact::has_readable_body(&document.sections),
        body_unreadable()
    );
    let markdown = artifact::safe_asset_path(&course.dir, &manifest.markdown)
        .and_then(|path| Ok(std::fs::read_to_string(path)?))
        .unwrap_or_else(|_| {
            c2m_artifact::render::render_markdown(&document.meta, &document.sections)
        });
    let mut plain_text = format!("{}\n\n", manifest.title);
    let mut blocks = Vec::new();
    if let Some(summary) = &document.summary {
        blocks.push(PreviewBlock::Heading {
            text: c2m_i18n::tr!("摘要").into(),
            anchor: "summary".into(),
            seconds: None,
        });
        blocks.push(PreviewBlock::Paragraph {
            text: summary.tldr.clone().into(),
            anchor: "summary-tldr".into(),
        });
        plain_text.push_str(&summary.tldr);
        plain_text.push_str("\n\n");
        for (i, point) in summary.key_points.iter().enumerate() {
            blocks.push(PreviewBlock::Paragraph {
                text: point.clone().into(),
                anchor: format!("key-point-{i}"),
            });
            plain_text.push_str(point);
            plain_text.push('\n');
        }
    }
    let mut issues = course.warning.clone().into_iter().collect::<Vec<_>>();
    if artifact::validate_note(&course.dir, &manifest).is_err() {
        issues.push(c2m_i18n::tr!("部分笔记文件缺失或已改动，先显示能读到的内容。").into());
    }
    for frame in &manifest.frames {
        if artifact::safe_asset_path(&course.dir, &frame.image).is_err() {
            issues.push(c2m_i18n::tr!(
                "{} 的截图无法读取",
                c2m_artifact::render::fmt_ts(frame.t)
            ));
        }
    }
    for (section_index, section) in document.sections.iter().enumerate() {
        blocks.push(PreviewBlock::Heading {
            text: c2m_artifact::render::fmt_ts(section.t).into(),
            anchor: format!("section-{section_index}"),
            seconds: Some(section.t),
        });
        if !section.image.is_empty()
            && manifest.frames.iter().any(|f| f.image == section.image)
            && let Ok(path) = artifact::safe_asset_path(&course.dir, &section.image)
        {
            blocks.push(PreviewBlock::Image(path));
        }
        for (paragraph_index, paragraph) in section.speech.iter().enumerate() {
            if !paragraph.text.trim().is_empty() {
                blocks.push(PreviewBlock::Paragraph {
                    text: paragraph.text.clone().into(),
                    anchor: format!("paragraph-{section_index}-{paragraph_index}"),
                });
                plain_text.push_str(&paragraph.text);
                plain_text.push_str("\n\n");
                if let Some(translation) = paragraph
                    .translation
                    .as_ref()
                    .filter(|text| !text.trim().is_empty())
                {
                    blocks.push(PreviewBlock::Translation {
                        text: translation.clone().into(),
                        anchor: format!("translation-{section_index}-{paragraph_index}"),
                    });
                    plain_text.push_str(translation);
                    plain_text.push_str("\n\n");
                }
            }
        }
    }
    let processing_issues = processing_issues(&manifest);
    course.title = manifest.title.clone();
    course.slides = manifest.frames.len();
    let outputs = manifest.outputs.clone();
    course.manifest = Some(manifest);
    Ok(Preview {
        course,
        blocks,
        outputs,
        plain_text,
        markdown_text: without_image_references(&markdown),
        document,
        issues,
        processing_issues,
    })
}

/// Copy-friendly markdown: strips image spans and the reference definitions only
/// they use, leaving authored prose, inline code, lists, tables and links untouched.
pub fn without_image_references(markdown: &str) -> String {
    use pulldown_cmark::{Event, Tag};
    let mut removed = Vec::new();
    let mut image_ids = Vec::new();
    let mut link_ids = Vec::new();
    let mut events = pulldown_cmark::Parser::new_ext(markdown, pulldown_cmark::Options::all())
        .into_offset_iter();
    for (event, range) in events.by_ref() {
        match event {
            Event::Start(Tag::Image { id, .. }) => {
                removed.push(range);
                image_ids.push(id);
            }
            Event::Start(Tag::Link { id, .. }) => link_ids.push(id),
            _ => {}
        }
    }
    // Labels match case-insensitively, so the parser's own table decides. A
    // definition may have a link using it as well; that link must keep working.
    let definitions = events.reference_definitions();
    let span_of = |id: &pulldown_cmark::CowStr| Some(definitions.get(id)?.span.clone());
    let linked: Vec<_> = link_ids.iter().filter_map(span_of).collect();
    for span in image_ids.iter().filter_map(span_of) {
        if !linked.contains(&span) {
            removed.push(whole_line(markdown, span));
        }
    }
    // An image's description can hold another image; only the outermost is cut.
    removed.sort_by_key(|range| (range.start, std::cmp::Reverse(range.end)));
    let mut result = String::with_capacity(markdown.len());
    let mut copied = 0;
    for range in removed {
        if range.start >= copied {
            result.push_str(&markdown[copied..range.start]);
        }
        copied = copied.max(range.end);
    }
    result.push_str(&markdown[copied..]);
    result
}

/// `span` with the rest of its last line, when only blank space follows it.
fn whole_line(text: &str, span: std::ops::Range<usize>) -> std::ops::Range<usize> {
    let rest = &text[span.end..];
    let line = rest.find('\n').map_or(rest.len(), |newline| newline + 1);
    if !text[span.clone()].ends_with('\n') && rest[..line].trim().is_empty() {
        span.start..span.end + line
    } else {
        span
    }
}

/// Published notes for tests: one frame and one paragraph per time.
#[cfg(any(test, feature = "test-support"))]
pub mod fixtures {
    use super::Course;
    use c2m_artifact::artifact;
    use c2m_core::timeline::{Section, TranscriptEvent};
    use c2m_source::fetch::VideoMeta;
    use std::path::Path;

    pub fn published_note(
        root: &Path,
        revision: u64,
        times: &[f64],
        outcomes: artifact::Outcomes,
    ) -> Course {
        published_note_translated(root, revision, times, outcomes, None)
    }

    /// A published note whose paragraphs carry `translation`.
    pub(crate) fn published_note_translated(
        root: &Path,
        revision: u64,
        times: &[f64],
        outcomes: artifact::Outcomes,
        translation: Option<&str>,
    ) -> Course {
        let target = artifact::Target {
            task_id: format!("task-{revision}"),
            course_id: "fixture-course".into(),
            source_id: "fixture-source".into(),
            course_dir: root.join("course"),
            replaces: None,
        };
        let manifest = publish(&target, revision, times, outcomes, translation);
        Course {
            dir: target.course_dir.clone(),
            title: manifest.title.clone(),
            modified: std::time::SystemTime::now(),
            slides: times.len(),
            thumbnail: None,
            manifest: Some(manifest),
            warning: None,
        }
    }

    /// Publishes `target`'s note: one frame and one paragraph per time, its text
    /// naming `revision`.
    pub fn publish(
        target: &artifact::Target,
        revision: u64,
        times: &[f64],
        outcomes: artifact::Outcomes,
        translation: Option<&str>,
    ) -> artifact::Manifest {
        let work = target
            .course_dir
            .parent()
            .unwrap()
            .join(format!("work-{}-{revision}", target.task_id));
        std::fs::create_dir_all(work.join("frames")).unwrap();
        let sections = times
            .iter()
            .enumerate()
            .map(|(index, time)| {
                let relative = format!("frames/frame-{index}.png");
                image::RgbImage::new(4 + revision as u32, 3)
                    .save(work.join(&relative))
                    .unwrap();
                Section {
                    t: *time,
                    end: *time + 10.,
                    image: relative,
                    speech: vec![TranscriptEvent {
                        start: *time,
                        end: *time + 10.,
                        text: format!("第 {revision} 版，{time} 秒的正文"),
                        raw: None,
                        translation: translation.map(str::to_owned),
                    }],
                }
            })
            .collect::<Vec<_>>();
        let meta = VideoMeta {
            title: "测试课程".into(),
            uploader: "作者".into(),
            duration: 90.,
            webpage_url: "https://www.bilibili.com/video/BVfixture?p=2".into(),
            extractor: "bilibili".into(),
            id: "fixture".into(),
        };
        let manifest =
            artifact::publish_blocking(target, &work, &meta, &sections, None, &[], outcomes)
                .unwrap();
        std::fs::remove_dir_all(work).unwrap();
        manifest
    }

    /// Adds version `task_id` to the course `target.course_dir` as an older app
    /// saved it: `versions/<task_id>/` with a schema 1 manifest. Its version id is its
    /// task id, as the desktop named them.
    pub fn legacy_version(
        target: &artifact::Target,
        revision: u64,
        times: &[f64],
        outcomes: artifact::Outcomes,
    ) -> std::path::PathBuf {
        let course = &target.course_dir;
        let scratch = course
            .parent()
            .unwrap()
            .join(format!("scratch-{}", target.task_id));
        publish(
            &artifact::Target {
                course_dir: scratch.clone(),
                ..target.clone()
            },
            revision,
            times,
            outcomes,
            None,
        );
        let dir = course.join("versions").join(&target.task_id);
        std::fs::create_dir_all(dir.parent().unwrap()).unwrap();
        std::fs::rename(&scratch, &dir).unwrap();
        let path = dir.join("manifest.json");
        let mut value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        value["schema"] = 1.into();
        value["version_id"] = target.task_id.clone().into();
        value["revision"] = revision.into();
        value["created_at_ms"] = (1000 + revision).into();
        std::fs::write(&path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
        dir
    }

    /// Points the course `course` at its version `version`, as `current.json` did.
    pub fn point_at(course: &Path, course_id: &str, version: &str) {
        std::fs::write(
            course.join("current.json"),
            serde_json::to_vec(&serde_json::json!({
                "schema": 1, "course_id": course_id, "version_id": version,
                "manifest": format!("versions/{version}/manifest.json"),
            }))
            .unwrap(),
        )
        .unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::{fixtures::published_note, *};

    #[test]
    fn translations_follow_their_paragraphs_in_the_reader() {
        let root = tempfile::tempdir().unwrap();
        let course = super::fixtures::published_note_translated(
            root.path(),
            1,
            &[10.],
            Default::default(),
            Some("Body of version 1"),
        );
        let preview = read_preview(course).unwrap();
        let position = preview
            .blocks
            .iter()
            .position(|block| matches!(block, PreviewBlock::Paragraph { .. }))
            .unwrap();
        assert!(matches!(
            &preview.blocks[position + 1],
            PreviewBlock::Translation { text, .. } if text.as_ref() == "Body of version 1"
        ));
        assert!(preview.plain_text.contains("Body of version 1"));
        assert!(preview.markdown_text.contains("> Body of version 1"));
    }

    #[test]
    fn optional_export_failure_does_not_describe_readable_content_as_incomplete() {
        let root = tempfile::tempdir().unwrap();
        published_note(root.path(), 1, &[10.], Default::default());
        let mut course = scan_library(root.path()).unwrap().courses.remove(0);
        let manifest = course.manifest.as_mut().unwrap();
        manifest.partial = true;
        manifest.outcomes.exports.insert(
            "html".into(),
            artifact::Outcome::failed("destination unavailable"),
        );
        assert!(!course.incomplete());
        course.manifest.as_mut().unwrap().outcomes.summary =
            artifact::Outcome::failed("service unavailable");
        assert!(course.incomplete());
    }

    #[test]
    fn library_discovers_published_notes_and_ignores_task_leftovers() {
        let root = tempfile::tempdir().unwrap();
        let note = published_note(root.path(), 1, &[10.], Default::default());
        let leftover = root.path().join("failed");
        std::fs::create_dir_all(&leftover).unwrap();
        std::fs::write(leftover.join("run.json"), r#"{"success":false}"#).unwrap();
        let scan = scan_library(root.path()).unwrap();
        assert_eq!(scan.courses.len(), 1);
        assert!(scan.issues.is_empty());
        assert_eq!(scan.courses[0].dir, note.dir);
        let preview = read_preview(scan.courses[0].clone()).unwrap();
        assert!(preview.plain_text.contains("10 秒的正文"));
        assert!(preview.issues.is_empty());
        assert!(
            preview
                .blocks
                .iter()
                .any(|b| matches!(b,PreviewBlock::Heading{seconds:Some(t),..} if *t==10.))
        );
    }

    #[test]
    fn a_regenerated_note_is_listed_once_and_a_damaged_one_is_reported() {
        let root = tempfile::tempdir().unwrap();
        published_note(root.path(), 1, &[10.], Default::default());
        let note = published_note(root.path(), 2, &[20.], Default::default());
        let scan = scan_library(root.path()).unwrap();
        assert_eq!(scan.courses.len(), 1);
        assert_eq!(scan.courses[0].manifest.as_ref().unwrap().task_id, "task-2");
        // A completed task's course reads its manifest on open.
        let opened = read_preview(Course::from_completed(&Done {
            out_dir: note.dir.clone(),
            ..Default::default()
        }))
        .unwrap();
        assert_eq!(opened.course.dir, note.dir);
        assert!(opened.plain_text.contains("第 2 版"));
        assert!(
            read_preview(Course::from_completed(&Done {
                out_dir: root.path().join("missing"),
                ..Default::default()
            }))
            .is_err()
        );
        // No older note stands in for a damaged one.
        std::fs::remove_file(note.dir.join("document.json")).unwrap();
        let scan = scan_library(root.path()).unwrap();
        assert!(scan.courses.is_empty());
        assert_eq!(scan.issues.len(), 1, "{:?}", scan.issues);
    }

    #[test]
    fn a_note_from_a_newer_app_asks_for_an_update() {
        let root = tempfile::tempdir().unwrap();
        let note = published_note(root.path(), 1, &[10.], Default::default());
        let manifest = note.dir.join("manifest.json");
        let mut value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
        value["schema"] = (artifact::MANIFEST_SCHEMA + 1).into();
        std::fs::write(&manifest, serde_json::to_vec(&value).unwrap()).unwrap();
        let scan = scan_library(root.path()).unwrap();
        assert!(scan.courses.is_empty());
        assert_eq!(scan.newer, vec![note.dir.clone()]);
        assert!(
            scan.issues[0].contains("更新版本的应用"),
            "{:?}",
            scan.issues
        );
        assert!(note_is_newer(&note.dir));
        assert!(!note_is_newer(&root.path().join("missing")));
    }

    /// A course an older app saved as versions waits for the startup migration; a
    /// replacement a crash interrupted is completed by the scan.
    #[test]
    fn a_scan_finishes_an_interrupted_replacement_and_waits_for_versions_to_be_flattened() {
        let root = tempfile::tempdir().unwrap();
        let note = published_note(root.path(), 1, &[10.], Default::default());
        // A crash between the two renames: the note is in the swap folder.
        let swap = root.path().join(".course2md/swap/course");
        std::fs::create_dir_all(&swap).unwrap();
        std::fs::rename(&note.dir, swap.join("old-task-2")).unwrap();
        let scan = scan_library(root.path()).unwrap();
        assert_eq!(scan.courses.len(), 1, "{:?}", scan.issues);
        assert!(!swap.exists());

        let target = artifact::Target {
            task_id: "old-task".into(),
            course_id: "course-legacy".into(),
            source_id: "legacy-source".into(),
            course_dir: root.path().join("legacy"),
            replaces: None,
        };
        fixtures::legacy_version(&target, 1, &[5.], Default::default());
        fixtures::point_at(&target.course_dir, "course-legacy", "old-task");
        let scan = scan_library(root.path()).unwrap();
        assert_eq!(scan.courses.len(), 1);
        assert_eq!(scan.issues.len(), 1, "{:?}", scan.issues);
        assert!(scan.issues[0].contains("旧方式保存"), "{:?}", scan.issues);
    }

    #[test]
    fn opening_reports_changed_files_the_listing_did_not_hash() {
        let root = tempfile::tempdir().unwrap();
        let note = published_note(root.path(), 1, &[10.], Default::default());
        let frame = note.dir.join("frames/frame-0.png");
        let mut bytes = std::fs::read(&frame).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 1;
        std::fs::write(&frame, bytes).unwrap();
        let course = scan_library(root.path()).unwrap().courses.remove(0);
        assert!(course.warning.is_none());
        assert_eq!(read_preview(course).unwrap().issues.len(), 1);
    }

    #[test]
    fn processing_details_show_the_published_message() {
        let issue = ProcessingIssue {
            stage: ProcessingStage::Summary,
            outcome: artifact::Outcome::failed("摘要尚未完成，已保留正文"),
        };
        assert_eq!(issue.detail(), Some("摘要尚未完成，已保留正文"));
    }

    #[test]
    fn copy_markdown_removes_real_images_but_keeps_inline_code_and_surrounding_words() {
        let value = "before ![inline](frames/a.png) after\n\n![ref][pic]\n\n[pic]: frames/a.png\n\n`![example](literal)`";
        let copied = without_image_references(value);
        assert!(copied.contains("before  after"));
        assert!(!copied.contains("frames/a.png"));
        assert!(copied.contains("`![example](literal)`"));
    }

    #[test]
    fn copy_markdown_handles_nested_images() {
        assert_eq!(without_image_references("a ![![b](c)](e) f"), "a  f");
        assert_eq!(
            without_image_references("![![b][c]](e) ![g](h)\n\n[c]: d\n"),
            " \n\n"
        );
    }

    #[test]
    fn copy_markdown_matches_definitions_as_the_parser_does() {
        assert_eq!(
            without_image_references("![x][PIC]\n\n[pic]: frames/a.png\nnext\n"),
            "\n\nnext\n"
        );
        // Text that only looks like a definition, and one a link still uses, stay.
        let code = "![x][pic]\n\n[pic]: a.png\n\n```\n[pic]: kept\n```\n";
        assert!(without_image_references(code).contains("[pic]: kept"));
        let shared = "![x][a] and [y][a]\n\n[a]: https://example.com\n";
        assert_eq!(
            without_image_references(shared),
            " and [y][a]\n\n[a]: https://example.com\n"
        );
    }

    #[test]
    fn copy_markdown_keeps_line_endings() {
        assert_eq!(without_image_references("text\n"), "text\n");
        assert_eq!(without_image_references("a\r\n\r\nb\r\n"), "a\r\n\r\nb\r\n");
        assert_eq!(without_image_references("no newline"), "no newline");
    }
}
