//! Read published versions and isolate damaged library entries.
use crate::backend::Done;
use anyhow::{Context, Result};
use course2md::artifact;
use gpui::SharedString;
use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    time::SystemTime,
};

fn version_damaged() -> &'static str {
    course2md::tr!("笔记版本信息已损坏")
}
fn body_unreadable() -> &'static str {
    course2md::tr!("笔记正文无法读取，原文件已保留")
}

/// The note was written by a newer app. Unlike damage, an older intact version is
/// no answer: this app must not show or republish over what it cannot read.
#[derive(Debug)]
struct NewerSchema;

impl std::fmt::Display for NewerSchema {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(course2md::tr!("这份笔记由更新版本的应用生成，请更新应用"))
    }
}

impl std::error::Error for NewerSchema {}

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
    /// A just-published version. `read_preview` reads its manifest on open and
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
            course2md::tr!("含 1 张截图").to_owned()
        } else if self.slides > 0 {
            course2md::tr!("含 {} 张截图", self.slides)
        } else {
            course2md::tr!("文字笔记").to_owned()
        }
    }
    pub fn incomplete(&self) -> bool {
        self.manifest.as_ref().is_some_and(has_incomplete_content)
    }
    /// Published versions live in `<note>/versions/<id>`; the note folder owns them.
    pub fn storage_dir(&self) -> PathBuf {
        self.dir
            .parent()
            .filter(|parent| parent.file_name().is_some_and(|name| name == "versions"))
            .and_then(Path::parent)
            .unwrap_or(&self.dir)
            .to_path_buf()
    }
}
/// Optional file exports describe a task's output, not the completeness of its note.
/// Published manifests stay immutable when a later task successfully repairs exports.
pub(crate) fn has_incomplete_content(manifest: &artifact::Manifest) -> bool {
    !processing_issues(manifest).is_empty()
}
pub struct LibraryScan {
    pub courses: Vec<Course>,
    pub issues: Vec<String>,
}

/// Listing checks that each required file exists at its recorded size; content
/// digests are verified when the note is opened.
fn assets_present(dir: &Path, manifest: &artifact::Manifest) -> bool {
    manifest
        .assets
        .iter()
        .filter(|asset| !asset.path.starts_with("exports/"))
        .all(|asset| {
            artifact::safe_asset_path(dir, &asset.path)
                .and_then(|path| Ok(std::fs::metadata(path)?.len()))
                .is_ok_and(|bytes| bytes == asset.bytes)
        })
}

fn version_course(dir: &Path) -> Result<Course> {
    let manifest = artifact::read_manifest(&dir.join("manifest.json"))?;
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

fn current_course(dir: &Path) -> Result<Course> {
    let current = || -> Result<Course> {
        let pointer: artifact::CurrentVersion =
            serde_json::from_slice(&std::fs::read(dir.join("current.json"))?)?;
        if pointer.schema > 1 {
            return Err(NewerSchema.into());
        }
        anyhow::ensure!(pointer.schema == 1, version_damaged());
        let manifest_path = artifact::safe_asset_path(dir, &pointer.manifest)?;
        let course = version_course(manifest_path.parent().context(version_damaged())?)?;
        anyhow::ensure!(
            course.manifest.as_ref().is_some_and(|manifest| {
                manifest.course_id == pointer.course_id && manifest.version_id == pointer.version_id
            }),
            version_damaged()
        );
        Ok(course)
    };
    current().or_else(|error| {
        if error.is::<NewerSchema>() {
            return Err(error);
        }
        let mut intact = std::fs::read_dir(dir.join("versions"))
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .filter_map(|entry| version_course(&entry.path()).ok())
            .filter(|course| {
                course
                    .manifest
                    .as_ref()
                    .is_some_and(|manifest| assets_present(&course.dir, manifest))
            })
            .collect::<Vec<_>>();
        intact.sort_by_key(|course| course.manifest.as_ref().map(|m| m.revision));
        let mut course = intact.pop().ok_or(error)?;
        course.warning =
            Some(course2md::tr!("最新版本无法读取，正在显示上一个完好的版本。").into());
        Ok(course)
    })
}

pub fn scan_library(root: &Path) -> Result<LibraryScan> {
    let mut scan = LibraryScan {
        courses: Vec::new(),
        issues: Vec::new(),
    };
    if !root.exists() {
        return Ok(scan);
    }
    let mut queue = VecDeque::from([(root.to_path_buf(), 0)]);
    while let Some((dir, depth)) = queue.pop_front() {
        if dir.join("current.json").is_file() {
            match current_course(&dir) {
                Ok(course) => scan.courses.push(course),
                Err(error) => scan.issues.push(course2md::tr!(
                    "{}：{}",
                    dir.display(),
                    format!("{error:#}")
                )),
            }
            continue;
        }
        if depth < 8 {
            match std::fs::read_dir(&dir) {
                Ok(entries) => {
                    for entry in entries {
                        match entry {
                            Ok(entry) => {
                                if entry.file_name().to_string_lossy().starts_with('.')
                                    || entry.file_name() == "versions"
                                {
                                    continue;
                                }
                                match entry.file_type() {
                                    Ok(kind) if kind.is_dir() => {
                                        queue.push_back((entry.path(), depth + 1))
                                    }
                                    Ok(_) => {}
                                    Err(error) => scan.issues.push(course2md::tr!(
                                        "{}：{}",
                                        entry.path().display(),
                                        error
                                    )),
                                }
                            }
                            Err(error) => {
                                scan.issues
                                    .push(course2md::tr!("{}：{}", dir.display(), error))
                            }
                        }
                    }
                }
                Err(error) => scan
                    .issues
                    .push(course2md::tr!("{}：{}", dir.display(), error)),
            }
        }
    }
    scan.courses.sort_by_key(|c| std::cmp::Reverse(c.modified));
    Ok(scan)
}

pub fn default_output() -> PathBuf {
    course2md::config::home_dir()
        .unwrap_or_else(|| ".".into())
        .join("Documents")
        .join("course2md")
}

#[derive(Clone)]
pub enum PreviewBlock {
    Image(PathBuf),
    Heading {
        text: SharedString,
        anchor: String,
        seconds: Option<f64>,
    },
    Paragraph {
        text: SharedString,
        anchor: String,
    },
    /// A bilingual note's translation of the paragraph before it.
    Translation {
        text: SharedString,
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
            } => course2md::render::fmt_ts(*seconds) == text.as_ref(),
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
            Self::Transcript => course2md::tr!("文字转写"),
            Self::Screenshots => course2md::tr!("截图"),
            Self::Proofreading => course2md::tr!("AI 校对"),
            Self::Translation => course2md::tr!("AI 翻译"),
            Self::Summary => course2md::tr!("摘要"),
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
        .unwrap_or_else(|_| course2md::render::render_markdown(&document.meta, &document.sections));
    let mut plain_text = format!("{}\n\n", manifest.title);
    let mut blocks = Vec::new();
    if let Some(summary) = &document.summary {
        blocks.push(PreviewBlock::Heading {
            text: course2md::tr!("摘要").into(),
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
    if artifact::validate_version(&course.dir, &manifest).is_err() {
        issues.push(course2md::tr!("部分笔记文件缺失或已改动，先显示能读到的内容。").into());
    }
    for frame in &manifest.frames {
        if artifact::safe_asset_path(&course.dir, &frame.image).is_err() {
            issues.push(course2md::tr!(
                "{} 的截图无法读取",
                course2md::render::fmt_ts(frame.t)
            ));
        }
    }
    for (section_index, section) in document.sections.iter().enumerate() {
        blocks.push(PreviewBlock::Heading {
            text: course2md::render::fmt_ts(section.t).into(),
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
#[cfg(test)]
pub(crate) mod fixtures {
    use super::Course;
    use course2md::{
        artifact,
        fetch::VideoMeta,
        timeline::{Section, TranscriptEvent},
    };
    use std::path::Path;

    pub(crate) fn published_note(
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
        let work = root.join(format!("work-{revision}"));
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
        let target = artifact::Target {
            task_id: format!("task-{revision}"),
            course_id: "fixture-course".into(),
            source_id: "fixture-source".into(),
            version_id: format!("v{revision}"),
            course_dir: root.join("course"),
        };
        let meta = VideoMeta {
            title: "测试课程".into(),
            uploader: "作者".into(),
            duration: 90.,
            webpage_url: "https://www.bilibili.com/video/BVfixture?p=2".into(),
            extractor: "bilibili".into(),
            id: "fixture".into(),
        };
        let manifest =
            artifact::publish_blocking(&target, &work, &meta, &sections, None, &[], outcomes)
                .unwrap();
        Course {
            dir: target.version_dir(),
            title: meta.title,
            modified: std::time::SystemTime::now(),
            slides: times.len(),
            thumbnail: None,
            manifest: Some(manifest),
            warning: None,
        }
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
        assert_eq!(
            scan.courses[0].storage_dir().canonicalize().unwrap(),
            root.path().join("course").canonicalize().unwrap()
        );
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
    fn a_damaged_latest_version_falls_back_to_the_last_intact_one() {
        let root = tempfile::tempdir().unwrap();
        let old = published_note(root.path(), 1, &[10.], Default::default());
        let new = published_note(root.path(), 2, &[20.], Default::default());
        std::fs::remove_file(new.dir.join("document.json")).unwrap();
        let course = scan_library(root.path()).unwrap().courses.remove(0);
        assert_eq!(course.dir, old.dir);
        assert!(course.warning.is_some());
        // A completed task's course reads its manifest on open.
        let opened = read_preview(Course::from_completed(&Done {
            out_dir: old.dir.clone(),
            ..Default::default()
        }))
        .unwrap();
        assert_eq!(opened.course.storage_dir(), old.storage_dir());
        assert!(
            read_preview(Course::from_completed(&Done {
                out_dir: root.path().join("missing"),
                ..Default::default()
            }))
            .is_err()
        );
    }

    #[test]
    fn a_note_from_a_newer_app_asks_for_an_update_instead_of_falling_back() {
        let root = tempfile::tempdir().unwrap();
        published_note(root.path(), 1, &[10.], Default::default());
        let pointer = root.path().join("course/current.json");
        let mut value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&pointer).unwrap()).unwrap();
        value["schema"] = 2.into();
        std::fs::write(&pointer, serde_json::to_vec(&value).unwrap()).unwrap();
        let scan = scan_library(root.path()).unwrap();
        assert!(scan.courses.is_empty());
        assert!(
            scan.issues[0].contains("更新版本的应用"),
            "{:?}",
            scan.issues
        );
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
