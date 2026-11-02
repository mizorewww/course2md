//! What a note is made of for reading: its time and outline, screenshots, exports and
//! the offline video, and the positions that outlive a repair.
use crate::{
    notes::{self, Course, PreviewBlock},
    page::Page,
    reader_navigation as nav, workspace,
};
use std::{
    collections::{BTreeMap, HashMap},
    ops::Range,
    path::PathBuf,
    rc::Rc,
};

pub fn block_anchor(block: &PreviewBlock, index: usize) -> String {
    match block {
        PreviewBlock::Heading { anchor, .. }
        | PreviewBlock::Paragraph { anchor, .. }
        | PreviewBlock::Translation { anchor, .. } => anchor.clone(),
        PreviewBlock::Image(path) => format!(
            "image:{index}:{}",
            path.file_name().unwrap_or_default().to_string_lossy()
        ),
    }
}

/// How far a card excerpt moves its cut to reach a word boundary.
pub const EXCERPT_WORD_SLACK: usize = 24;

/// What a card shows of a screenshot's text without a match to centre on.
pub const EXCERPT_GRAPHEMES: usize = 160;

pub const ZOOM_MIN: f32 = 0.1;

pub const ZOOM_MAX: f32 = 4.;

#[derive(Clone)]
pub struct FrameImage {
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone)]
pub struct Frame {
    pub anchor: String,
    /// `None` when the file is missing or cannot be decoded.
    pub image: Option<FrameImage>,
    pub seconds: f64,
    pub transcript: String,
    /// Lowercased transcript and time that gallery search matches against.
    pub search_text: String,
    /// The section heading this screenshot belongs to.
    pub body_index: Option<usize>,
}

impl Frame {
    pub fn new(
        anchor: String,
        path: Option<PathBuf>,
        seconds: f64,
        transcript: String,
        body_index: Option<usize>,
    ) -> Self {
        let image = path.and_then(|path| {
            let (width, height) = image::image_dimensions(&path)
                .ok()
                .filter(|(width, height)| *width > 0 && *height > 0)?;
            Some(FrameImage {
                path,
                width,
                height,
            })
        });
        let search_text = format!(
            "{}\n{}",
            course2md::render::fmt_ts(seconds),
            transcript.to_lowercase()
        );
        Self {
            anchor,
            image,
            seconds,
            transcript,
            search_text,
            body_index,
        }
    }
}

#[derive(Clone)]
pub struct Version {
    pub course: Course,
    pub label: String,
}

#[derive(Default)]
pub struct ReaderData {
    pub frames: Vec<Frame>,
    pub frame_by_path: HashMap<PathBuf, usize>,
    pub versions: Vec<Version>,
    pub issues: Vec<String>,
    pub export_folder: Option<PathBuf>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum OfflineVideo {
    #[default]
    NotRequested,
    Missing,
    Available(PathBuf),
}

#[derive(Clone)]
pub struct OfflineVideoRequest {
    pub library_root: PathBuf,
    pub task_id: String,
    pub work_dir: PathBuf,
    pub version_dir: PathBuf,
}

impl OfflineVideoRequest {
    pub fn for_version(
        course: &Course,
        task: &workspace::TaskRecord,
        library: &workspace::LibraryLocation,
    ) -> Option<Self> {
        let manifest = course.manifest.as_ref()?;
        if !task.plan.source.online
            || !task.plan.options().keep_video
            || task.id != manifest.task_id
            || task.plan.source_id != manifest.source_id
            || task.artifact.as_ref() != Some(&course.dir)
            || task.plan.library_id != library.id
        {
            return None;
        }
        Some(Self {
            library_root: library.root.clone(),
            task_id: task.id.clone(),
            work_dir: task.work_dir.clone(),
            version_dir: course.dir.clone(),
        })
    }

    /// Called only by reader workers, including the final check before opening.
    pub fn inspect(&self) -> OfflineVideo {
        self.checked_path()
            .map(OfflineVideo::Available)
            .unwrap_or(OfflineVideo::Missing)
    }

    pub fn checked_path(&self) -> Option<PathBuf> {
        let mut components = std::path::Path::new(&self.task_id).components();
        if !matches!(components.next(), Some(std::path::Component::Normal(_)))
            || components.next().is_some()
        {
            return None;
        }
        let root = self.library_root.canonicalize().ok()?;
        let work = self.work_dir.canonicalize().ok()?;
        if work != root.join(".course2md/work").join(&self.task_id)
            || !self.version_dir.canonicalize().ok()?.starts_with(&root)
        {
            return None;
        }
        let media = work.join("media.mp4").canonicalize().ok()?;
        let metadata = media.metadata().ok()?;
        // The system player is started through the shell, which rejects `\\?\` paths.
        (media.parent() == Some(work.as_path()) && metadata.is_file() && metadata.len() > 0)
            .then(|| course2md::config::simplify_verbatim(media))
    }
}

#[derive(Clone)]
pub struct ExportFeedback {
    pub title: String,
    pub format: course2md::config::OutputFormat,
    pub result: Result<PathBuf, String>,
}

#[derive(Default)]
pub struct ExportState {
    pub generation: u64,
    pub pending: Option<(u64, PathBuf)>,
    pub feedback: BTreeMap<PathBuf, ExportFeedback>,
}

impl ExportState {
    pub fn begin(&mut self, version: PathBuf) -> Option<u64> {
        if self.pending.is_some() {
            return None;
        }
        self.generation = self.generation.wrapping_add(1);
        self.pending = Some((self.generation, version));
        Some(self.generation)
    }

    pub fn is_pending(&self, generation: u64, version: &std::path::Path) -> bool {
        self.pending
            .as_ref()
            .is_some_and(|(current, source)| *current == generation && source == version)
    }

    pub fn cancel(&mut self, generation: u64, version: &std::path::Path) -> bool {
        if !self.is_pending(generation, version) {
            return false;
        }
        self.pending = None;
        true
    }

    pub fn finish(
        &mut self,
        generation: u64,
        version: &std::path::Path,
        feedback: ExportFeedback,
    ) -> bool {
        if !self.cancel(generation, version) {
            return false;
        }
        self.feedback.insert(version.to_path_buf(), feedback);
        true
    }
}

pub fn exported_file_label(path: &std::path::Path) -> String {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| course2md::tr!("导出文件").into());
    course2md::tr!("已导出 · {}", name)
}

/// Position-store key for a manifest course at one tab (`:0` is the note
/// tab). The library's "继续阅读" probe and the reader's save/restore build
/// identical keys through this function.
pub fn manifest_reading_key(course_id: &str, version_id: &str, tab: usize) -> String {
    format!("{course_id}:{version_id}:{tab}")
}

/// A saved position away from the document top names the block it was read
/// at; the top itself is stored as the default position.
pub fn has_reading_position(position: &workspace::ReadingPosition) -> bool {
    position.paragraph.is_some() || position.seconds.is_some_and(|seconds| seconds > 0.)
}

pub fn block_time(blocks: &[PreviewBlock], index: usize) -> Option<f64> {
    for block in blocks.iter().take(index + 1).rev() {
        // An explicitly untimed heading ends the previous time's scope.
        if let PreviewBlock::Heading { seconds, .. } = block {
            return *seconds;
        }
    }
    None
}

pub fn heading_times(blocks: &[PreviewBlock]) -> impl Iterator<Item = (usize, Option<f64>)> + '_ {
    blocks.iter().enumerate().map(|(index, block)| {
        (
            index,
            match block {
                PreviewBlock::Heading { seconds, .. } => *seconds,
                _ => None,
            },
        )
    })
}

pub fn note_position_index(
    blocks: &[PreviewBlock],
    position: &workspace::ReadingPosition,
) -> Option<usize> {
    position
        .paragraph
        .as_ref()
        .and_then(|anchor| {
            blocks
                .iter()
                .enumerate()
                .position(|(index, block)| block_anchor(block, index) == *anchor)
        })
        .or_else(|| {
            position.seconds.and_then(|seconds| {
                nav::nearest_time(heading_times(blocks), seconds).map(|(index, _)| index)
            })
        })
}

/// The contents entry that holds `block`: the last heading at or before it.
pub fn outline_current(outline: &[(usize, String, Option<f64>)], block: usize) -> Option<usize> {
    outline
        .iter()
        .rev()
        .find(|(index, _, _)| *index <= block)
        .map(|(index, _, _)| *index)
}

/// One item of the virtualized screenshot gallery.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GalleryItem {
    /// The note's opening and the gallery's loading, empty or no-match state.
    Header,
    /// One grid row: a range of the matching screenshots.
    Row(Range<usize>),
}

/// The gallery as list items: the header, then rows of `columns` screenshots.
pub fn gallery_items(matches: usize, columns: usize) -> Vec<GalleryItem> {
    let columns = columns.max(1);
    std::iter::once(GalleryItem::Header)
        .chain(
            (0..matches)
                .step_by(columns)
                .map(|start| GalleryItem::Row(start..(start + columns).min(matches))),
        )
        .collect()
}

/// The row now holding screenshot `frame`.
pub fn gallery_row_of(items: &[GalleryItem], matches: &[usize], frame: usize) -> Option<usize> {
    let at = matches.iter().position(|index| *index == frame)?;
    items
        .iter()
        .position(|item| matches!(item, GalleryItem::Row(row) if row.contains(&at)))
}

pub fn frame_label(frame: &Frame) -> String {
    course2md::tr!("{} 的截图", course2md::render::fmt_ts(frame.seconds))
}

pub fn matching_frames(frames: &[Frame], query: &str) -> Vec<usize> {
    let query = query.trim().to_lowercase();
    frames
        .iter()
        .enumerate()
        .filter(|(_, frame)| frame.search_text.contains(&query))
        .map(|(index, _)| index)
        .collect()
}

pub fn frame_excerpt(frame: &Frame, query: &str) -> String {
    use unicode_segmentation::UnicodeSegmentation;
    let text = frame.transcript.as_str();
    let found = nav::text_matches(text, query).into_iter().next();
    if found.is_none() && text.trim().is_empty() {
        return course2md::tr!("这张截图没有对应文字").into();
    }
    let (mut start, end) = match &found {
        Some(found) => {
            // Step back by whole characters, so an emoji sequence or a letter
            // with combining marks is never split.
            let mut start = text[..found.start]
                .grapheme_indices(true)
                .rev()
                .nth(15)
                .map(|(index, _)| index)
                .unwrap_or(0);
            // Start at a word boundary, but within a few characters, so a match
            // after a very long token (a URL) still falls within the excerpt's
            // three lines; such a token is cut, after the leading ellipsis.
            let mut walked = 0;
            while start > 0
                && walked < EXCERPT_WORD_SLACK
                && text.as_bytes()[start].is_ascii_alphanumeric()
                && text.as_bytes()[start - 1].is_ascii_alphanumeric()
            {
                start -= 1;
                walked += 1;
            }
            if walked == EXCERPT_WORD_SLACK {
                start += walked;
            }
            (start, found.end)
        }
        None => (0, 0),
    };
    let wanted = if found.is_some() {
        100
    } else {
        EXCERPT_GRAPHEMES
    };
    let mut end = text[end..]
        .grapheme_indices(true)
        .nth(wanted)
        .map(|(index, _)| end + index)
        .unwrap_or(text.len());
    let mut walked = 0;
    while end < text.len()
        && walked < EXCERPT_WORD_SLACK
        && text.as_bytes()[end - 1].is_ascii_alphanumeric()
        && text.as_bytes()[end].is_ascii_alphanumeric()
    {
        end += 1;
        walked += 1;
    }
    // A formula is shown whole or not at all.
    for (range, piece) in course2md::math::piece_sources(text) {
        if matches!(piece, course2md::math::Piece::Math { .. }) {
            if range.start < start && start < range.end {
                start = range.start;
            }
            if range.start < end && end < range.end {
                end = range.end;
            }
        }
    }
    format!(
        "{}{}{}",
        if start > 0 { "…" } else { "" },
        text[start..end].trim(),
        if end < text.len() { "…" } else { "" },
    )
}

/// Chapter titles from the summary outline, keyed by the section heading where
/// each chapter begins. A heading belongs to the last outline entry that does
/// not start after it; screenshot times rarely coincide with outline times.
pub fn chapter_starts(
    blocks: &[PreviewBlock],
    outline: &[(f64, String)],
) -> HashMap<usize, String> {
    let mut starts = HashMap::new();
    let mut previous: Option<&String> = None;
    for (index, block) in blocks.iter().enumerate() {
        let PreviewBlock::Heading {
            seconds: Some(seconds),
            ..
        } = block
        else {
            continue;
        };
        let Some((_, chapter)) = outline
            .iter()
            .filter(|(time, _)| *time <= seconds + 1.5)
            .max_by(|(a, _), (b, _)| a.total_cmp(b))
        else {
            continue;
        };
        if previous != Some(chapter) {
            starts.insert(index, chapter.clone());
        }
        previous = Some(chapter);
    }
    starts
}

pub fn summary_outline(preview: &notes::Preview) -> Vec<(f64, String)> {
    preview
        .document
        .summary
        .iter()
        .flat_map(|summary| &summary.outline)
        .map(|item| (item.t, item.title.clone()))
        .collect()
}

pub fn reader_outline(preview: &notes::Preview) -> Vec<(usize, String, Option<f64>)> {
    let headings = preview
        .blocks
        .iter()
        .enumerate()
        .filter_map(|(index, block)| {
            if let PreviewBlock::Heading { text, seconds, .. } = block {
                Some((
                    index,
                    text.to_string(),
                    *seconds,
                    block.is_timestamp_heading(),
                ))
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    nav::content_outline(&headings, &summary_outline(preview))
}

pub fn load_reader_data(preview: &notes::Preview) -> ReaderData {
    let mut data = ReaderData::default();
    let dir = &preview.course.dir;
    data.export_folder = existing_export_folder(dir, &preview.outputs);
    let Some(manifest) = &preview.course.manifest else {
        return data;
    };
    let headings: HashMap<usize, usize> = preview
        .blocks
        .iter()
        .enumerate()
        .filter_map(|(index, block)| block.section_index().map(|section| (section, index)))
        .collect();
    for (index, frame) in manifest.frames.iter().enumerate() {
        let section = preview
            .document
            .sections
            .iter()
            .enumerate()
            .find(|(_, section)| section.image == frame.image);
        let transcript = section
            .map(|(_, section)| {
                section
                    .speech
                    .iter()
                    .map(|speech| speech.text.as_str())
                    .collect::<Vec<_>>()
                    .join("\n\n")
            })
            .unwrap_or_default();
        let body_index = section
            .and_then(|(section, _)| headings.get(&section))
            .copied()
            .or_else(|| nav::nearest_time(heading_times(&preview.blocks), frame.t).map(|(i, _)| i));
        let frame = Frame::new(
            format!("frame:{index}:{}", frame.image),
            course2md::artifact::safe_asset_path(dir, &frame.image).ok(),
            frame.t,
            transcript,
            body_index,
        );
        if let Some(image) = &frame.image {
            data.frame_by_path
                .entry(image.path.clone())
                .or_insert(index);
        }
        data.frames.push(frame);
    }
    let parent = dir
        .parent()
        .filter(|parent| parent.file_name().is_some_and(|name| name == "versions"));
    let Some(parent) = parent else {
        return data;
    };
    match std::fs::read_dir(parent) {
        Ok(entries) => {
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_dir() || !path.join("manifest.json").is_file() {
                    continue;
                }
                match course2md::artifact::read_manifest(&path.join("manifest.json")) {
                    Ok(other) if other.course_id == manifest.course_id => {
                        let mut course = preview.course.clone();
                        course.dir = path;
                        course.title = other.title.clone();
                        course.slides = other.frames.len();
                        let label = course2md::tr!(
                            "第 {} 版 · {}{}",
                            other.revision,
                            nav::timestamp_local(other.created_at_ms),
                            if notes::has_incomplete_content(&other) {
                                course2md::tr!(" · 部分完成")
                            } else {
                                ""
                            }
                        );
                        course.manifest = Some(other);
                        data.versions.push(Version { course, label });
                    }
                    Err(_) => data
                        .issues
                        .push(course2md::tr!("有一个旧版本无法读取").into()),
                    _ => {}
                }
            }
            data.versions.sort_by_key(|version| {
                std::cmp::Reverse(
                    version
                        .course
                        .manifest
                        .as_ref()
                        .map(|manifest| (manifest.revision, manifest.created_at_ms))
                        .unwrap_or_default(),
                )
            });
        }
        Err(error) => data
            .issues
            .push(course2md::tr!("其他版本暂时无法读取：{}", error)),
    }
    data
}

pub fn existing_export_folder(dir: &std::path::Path, outputs: &[String]) -> Option<PathBuf> {
    let folder = course2md::artifact::safe_asset_path(dir, "exports").ok()?;
    let has_file = outputs.iter().any(|output| {
        std::path::Path::new(output).starts_with("exports")
            && course2md::artifact::safe_asset_path(dir, output).is_ok_and(|path| path.is_file())
    });
    has_file.then_some(folder)
}

pub fn processing_notice(issues: &[notes::ProcessingIssue]) -> Option<String> {
    if issues.is_empty() {
        return None;
    }
    Some(course2md::tr!(
        "{}未完成",
        issues
            .iter()
            .map(|issue| issue.stage.label())
            .collect::<Vec<_>>()
            .join(course2md::tr!("、"))
    ))
}

pub fn files_need_reload(preview: &notes::Preview, issues: &[String], frames: &[Frame]) -> bool {
    !preview.issues.is_empty()
        || !issues.is_empty()
        || frames.iter().any(|frame| frame.image.is_none())
}

/// The text's scroll offset (zero or negative) after moving `delta` pixels
/// further down, kept within its `max` scroll distance.
pub fn image_text_offset(offset: f32, max: f32, delta: f32) -> f32 {
    (offset - delta).clamp(-max.max(0.), 0.)
}

/// Preserve the image point under the viewport center while either axis grows
/// from a centered preview into scrollable content, or returns to a fitted image.
pub fn image_zoom_offset(viewport: f32, old_extent: f32, new_extent: f32, offset: f32) -> f32 {
    if viewport <= 0. || old_extent <= 0. {
        return 0.;
    }
    let old_margin = (viewport - old_extent).max(0.) * 0.5;
    let image_position = ((viewport * 0.5 - offset - old_margin) / old_extent).clamp(0., 1.);
    let new_margin = (viewport - new_extent).max(0.) * 0.5;
    (viewport * 0.5 - new_margin - image_position * new_extent)
        .clamp(-(new_extent - viewport).max(0.), 0.)
}

pub fn zoomed(scale: f32, factor: f32) -> f32 {
    (scale * factor).clamp(ZOOM_MIN, ZOOM_MAX)
}

/// What a selection of a note copies: its source, without the figures and the blank
/// lines they leave.
pub fn copied_text(selected: &str) -> String {
    let mut text = notes::without_image_references(selected);
    while text.contains("\n\n\n") {
        text = text.replace("\n\n\n", "\n\n");
    }
    text.trim().to_owned()
}

pub fn carry_repaired_reading_positions(
    positions: &mut BTreeMap<String, workspace::ReadingPosition>,
    previous: (&str, &str),
    next: (&str, &str),
) {
    if previous.0 != next.0 || previous.1 == next.1 {
        return;
    }
    for tab in 0..=1 {
        let previous_key = manifest_reading_key(previous.0, previous.1, tab);
        let next_key = manifest_reading_key(next.0, next.1, tab);
        if let Some(position) = positions.get(&previous_key).cloned() {
            positions.entry(next_key).or_insert(position);
        }
    }
}

/// A completed read clears only its own earlier failure notice; newer,
/// unrelated feedback stays visible.
pub fn settle_reader_notice(current: &mut Option<String>, failure: &mut Option<String>) {
    if failure
        .take()
        .is_some_and(|failed| current.as_deref() == Some(failed.as_str()))
    {
        *current = None;
    }
}

/// The note being read, or on its way. A read that finishes after a newer one started no
/// longer counts.
#[derive(Default)]
pub struct OpenedNote {
    pub preview: Option<Rc<notes::Preview>>,
    pub read_generation: u64,
    pub reading: bool,
    pub opening_course: Option<PathBuf>,
    pub reader_course_error: Option<(PathBuf, String)>,
    pub reader_failure_notice: Option<String>,
    /// The page reading returns to.
    pub result_origin: Page,
    pub result_tab: usize,
}

impl OpenedNote {
    /// Starts reading a note from `page`: returning from it goes to where the person came
    /// from, also when one note opens another. Returns the read's generation.
    pub fn begin_read(&mut self, path: PathBuf, page: Page) -> u64 {
        self.reading = true;
        self.opening_course = Some(path);
        self.reader_course_error = None;
        self.read_generation = self.read_generation.wrapping_add(1);
        self.result_origin = if page != Page::Result {
            page
        } else if self.result_origin == Page::Result {
            Page::Library
        } else {
            self.result_origin
        };
        self.read_generation
    }

    /// Late results of a running read no longer count.
    pub fn stop_reading(&mut self) {
        self.read_generation = self.read_generation.wrapping_add(1);
        self.reading = false;
    }
}

/// Why a note could not be read, in the person's terms.
pub fn note_read_failure(error: &anyhow::Error) -> &'static str {
    let kind = error.chain().find_map(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .map(std::io::Error::kind)
    });
    match kind {
        Some(std::io::ErrorKind::NotFound) => {
            course2md::tr!("笔记文件不存在，请重新连接保存位置后重试")
        }
        Some(std::io::ErrorKind::PermissionDenied) => {
            course2md::tr!("无权读取这份笔记，请检查保存位置的访问权限")
        }
        _ => course2md::tr!("这份笔记无法读取，请检查保存位置中的文件"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{notes::Course, source};

    #[test]
    fn returning_from_a_note_goes_where_the_person_came_from() {
        let mut opened = OpenedNote::default();
        assert_eq!(opened.begin_read("a".into(), Page::Task), 1);
        assert!(opened.reading);
        assert!(opened.result_origin == Page::Task);
        // One note opening another keeps the way back.
        opened.begin_read("b".into(), Page::Result);
        assert!(opened.result_origin == Page::Task);
        opened.result_origin = Page::Result;
        opened.begin_read("c".into(), Page::Result);
        assert!(opened.result_origin == Page::Library);
        opened.stop_reading();
        assert!(!opened.reading);
        assert_eq!(opened.read_generation, 4);
    }

    fn fixture_version(
        root: &std::path::Path,
        revision: u64,
        times: &[f64],
    ) -> crate::notes::Course {
        crate::notes::fixtures::published_note(root, revision, times, Default::default())
    }

    fn offline_task_fixture(
        root: &std::path::Path,
        course: &Course,
    ) -> (workspace::TaskRecord, workspace::LibraryLocation) {
        let manifest = course.manifest.as_ref().unwrap();
        let library = workspace::LibraryLocation {
            id: "offline-library".into(),
            name: "Offline library".into(),
            root: root.to_owned(),
            previous_roots: Vec::new(),
        };
        let task = workspace::TaskRecord {
            id: manifest.task_id.clone(),
            plan: workspace::TaskPlan {
                operation: Default::default(),
                source: source::Source {
                    input: "https://www.bilibili.com/video/BVfixture?p=2".into(),
                    identity: manifest.source_id.clone(),
                    online: true,
                    ..Default::default()
                },
                source_id: manifest.source_id.clone(),
                title: course.title.clone(),
                library_id: library.id.clone(),
                folder: None,
                subtitle: None,
                config: course2md::settings::ConfigFile {
                    defaults: course2md::settings::Defaults {
                        keep_video: Some(true),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                asr_service: None,
                ai_service: None,
            },
            state: workspace::TaskState::Complete,
            intent: workspace::Intent::Run,
            created: 0,
            updated: 0,
            parent: None,
            handled_by: None,
            held: false,
            work_dir: root.join(".course2md/work").join(&manifest.task_id),
            events: crate::activity::JobEvents {
                error: None,
                ..Default::default()
            },
            artifact: Some(course.dir.clone()),
            unread: false,
            resend: Vec::new(),
        };
        (task, library)
    }

    #[test]
    fn a_copy_leaves_figures_behind() {
        assert_eq!(
            super::copied_text("一段\n\n![](figure:5)\n\n### 01:00\n\n![](figure:9)"),
            "一段\n\n### 01:00"
        );
    }

    #[test]
    fn keys_scroll_the_screenshot_text_within_its_range() {
        // Down moves the content up (a more negative offset), up returns it.
        assert_eq!(image_text_offset(0., 500., 90.), -90.);
        assert_eq!(image_text_offset(-90., 500., -90.), 0.);
        // Home and End go to either end; nothing scrolls past them.
        assert_eq!(image_text_offset(-200., 500., f32::INFINITY), -500.);
        assert_eq!(image_text_offset(-200., 500., f32::NEG_INFINITY), 0.);
        assert_eq!(image_text_offset(0., 0., 90.), 0.);
    }

    #[test]
    fn saved_export_folder_requires_a_listed_file_inside_the_version() {
        let temp = tempfile::tempdir().unwrap();
        let exports = temp.path().join("exports");
        let outputs = vec!["exports/course.html".into()];
        assert!(existing_export_folder(temp.path(), &outputs).is_none());
        std::fs::create_dir(&exports).unwrap();
        assert!(existing_export_folder(temp.path(), &outputs).is_none());
        std::fs::write(exports.join("course.html"), "<p>笔记</p>").unwrap();
        assert_eq!(existing_export_folder(temp.path(), &outputs), Some(exports));
        assert!(existing_export_folder(temp.path(), &[]).is_none());
        assert!(existing_export_folder(temp.path(), &["exports/../course.html".into()]).is_none());
    }

    #[test]
    fn export_feedback_stays_with_its_version_and_ignores_cancelled_callbacks() {
        let old = std::path::PathBuf::from("/library/old-note/versions/one");
        let other = std::path::PathBuf::from("/library/other-note/versions/two");
        let path = std::path::PathBuf::from("/exports/old-note.html");
        let success = || ExportFeedback {
            title: "旧笔记".into(),
            format: course2md::config::OutputFormat::Html,
            result: Ok(path.clone()),
        };
        let mut state = ExportState::default();
        let cancelled = state.begin(old.clone()).unwrap();
        assert!(state.cancel(cancelled, &old));
        assert!(state.feedback.is_empty());
        let next = state.begin(other.clone()).unwrap();
        assert!(!state.finish(cancelled, &old, success()));
        assert!(state.is_pending(next, &other));
        assert!(state.finish(
            next,
            &other,
            ExportFeedback {
                title: "另一份笔记".into(),
                format: course2md::config::OutputFormat::Html,
                result: Err("保存位置不可写".into()),
            }
        ));
        let cancelled_retry = state.begin(other.clone()).unwrap();
        assert!(state.cancel(cancelled_retry, &other));
        assert!(state.feedback[&other].result.is_err());
        let completed = state.begin(old.clone()).unwrap();
        assert!(state.finish(completed, &old, success()));
        assert_eq!(state.feedback[&old].result.as_ref().unwrap(), &path);
        assert!(state.feedback[&other].result.is_err());
    }

    #[test]
    fn gallery_search_matches_its_own_text_and_preserves_frame_identity() {
        let frame = |index: usize, transcript: &str| {
            Frame::new(
                format!("frame-{index}"),
                None,
                index as f64 * 10.,
                transcript.into(),
                Some(index),
            )
        };
        let frames = vec![
            frame(0, "结构与分组"),
            frame(1, "OpenAI 与模型"),
            frame(2, "结语"),
        ];
        assert_eq!(matching_frames(&frames, "  OPENAI  "), vec![1]);
        assert_eq!(matching_frames(&frames, "00:20"), vec![2]);
        assert!(matching_frames(&frames, "不存在的内容").is_empty());
        assert_eq!(matching_frames(&frames, ""), vec![0, 1, 2]);
        let phrase = frame(
            3,
            "Some introductory words. OpenAI said that Buckmaster offered terms.",
        );
        assert!(frame_excerpt(&phrase, "Buckmaster").starts_with("…OpenAI said that Buckmaster"));
        // An excerpt around a match never cuts a formula in half.
        let formula = frame(
            4,
            "前文前文前文前文 $a + b + c + d + e + f + g + h$ 后文 match",
        );
        let excerpt = frame_excerpt(&formula, "match");
        assert!(
            excerpt.contains("$a + b + c + d + e + f + g + h$"),
            "{excerpt}"
        );
        // A match after a very long token starts close to the match.
        let url = format!("链接 https://example.com/{}/end?q=x 结尾", "a".repeat(300));
        let excerpt = frame_excerpt(&frame(5, &url), "end");
        let before = excerpt.find("/end").unwrap();
        assert!(excerpt.starts_with('…') && before < 60, "{excerpt}");
        // The step back counts whole characters, so an emoji sequence stays whole.
        let family = "👨\u{200d}👩\u{200d}👧\u{200d}👦";
        let emoji = frame(
            6,
            &format!("前文{family}一二三四五六七八九十一二三四 zebra"),
        );
        let excerpt = frame_excerpt(&emoji, "zebra");
        assert!(excerpt.starts_with(&format!("…{family}")), "{excerpt}");
    }

    #[test]
    fn image_zoom_keeps_the_visible_center_and_all_edges_reachable() {
        // A centered small image grows around its midpoint, then fits again.
        assert_eq!(image_zoom_offset(1000., 640., 1280., 0.), -140.);
        assert_eq!(image_zoom_offset(1000., 1280., 640., -140.), 0.);
        // A panned image preserves the same content at the viewport center.
        assert_eq!(image_zoom_offset(1000., 2000., 4000., -400.), -1300.);
        // Shrinking near an edge clamps to reachable bounds without hiding it.
        assert_eq!(image_zoom_offset(1000., 2000., 1200., -1000.), -200.);
        assert_eq!(image_zoom_offset(1000., 2000., 1200., 0.), 0.);
    }

    #[test]
    fn an_untimed_heading_ends_the_preceding_time_scope() {
        let blocks = vec![
            PreviewBlock::Heading {
                text: "0:10".into(),
                anchor: "a".into(),
                seconds: Some(10.),
            },
            PreviewBlock::Paragraph {
                text: "第一段".into(),
                anchor: "b".into(),
            },
            PreviewBlock::Heading {
                text: "补充主题".into(),
                anchor: "c".into(),
                seconds: None,
            },
            PreviewBlock::Paragraph {
                text: "第二段".into(),
                anchor: "d".into(),
            },
        ];
        assert_eq!(block_time(&blocks, 1), Some(10.));
        assert_eq!(block_time(&blocks, 3), None);
    }

    #[test]
    fn offline_video_uses_its_version_task_and_rechecks_removed_media() {
        let root = tempfile::tempdir().unwrap();
        let first = fixture_version(root.path(), 1, &[10.]);
        let second = fixture_version(root.path(), 2, &[20.]);
        let (task, library) = offline_task_fixture(root.path(), &first);
        std::fs::create_dir_all(&task.work_dir).unwrap();
        let media = task.work_dir.join("media.mp4");
        std::fs::write(&media, "retained first video").unwrap();
        let request = OfflineVideoRequest::for_version(&first, &task, &library).unwrap();
        assert_eq!(
            request.inspect(),
            OfflineVideo::Available(course2md::config::canonicalize(&media).unwrap())
        );
        assert!(OfflineVideoRequest::for_version(&second, &task, &library).is_none());
        let mut changed = task.clone();
        changed.artifact = Some(second.dir.clone());
        assert!(OfflineVideoRequest::for_version(&first, &changed, &library).is_none());
        changed = task.clone();
        changed.plan.source_id = "another-video".into();
        assert!(OfflineVideoRequest::for_version(&first, &changed, &library).is_none());
        changed = task.clone();
        changed.plan.config.defaults.keep_video = Some(false);
        assert!(OfflineVideoRequest::for_version(&first, &changed, &library).is_none());
        std::fs::remove_file(media).unwrap();
        assert_eq!(request.inspect(), OfflineVideo::Missing);
    }

    #[test]
    fn offline_video_refuses_other_task_paths_and_files_outside_its_library() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let course = fixture_version(root.path(), 1, &[10.]);
        let (mut task, library) = offline_task_fixture(root.path(), &course);
        let expected_work = task.work_dir.clone();
        std::fs::write(outside.path().join("media.mp4"), "unrelated video").unwrap();
        task.work_dir = outside.path().to_owned();
        let request = OfflineVideoRequest::for_version(&course, &task, &library).unwrap();
        assert_eq!(request.inspect(), OfflineVideo::Missing);
        task.work_dir = expected_work;
        std::fs::create_dir_all(&task.work_dir).unwrap();
        let mut request = OfflineVideoRequest::for_version(&course, &task, &library).unwrap();
        std::fs::write(task.work_dir.join("media.mp4"), "retained video").unwrap();
        request.version_dir = outside.path().to_owned();
        assert_eq!(request.inspect(), OfflineVideo::Missing);

        #[cfg(unix)]
        {
            std::fs::remove_file(task.work_dir.join("media.mp4")).unwrap();
            std::os::unix::fs::symlink(
                outside.path().join("media.mp4"),
                task.work_dir.join("media.mp4"),
            )
            .unwrap();
            let request = OfflineVideoRequest::for_version(&course, &task, &library).unwrap();
            assert_eq!(request.inspect(), OfflineVideo::Missing);
        }
    }

    #[test]
    fn summary_failure_keeps_body_readable_and_does_not_offer_file_reload() {
        let root = tempfile::tempdir().unwrap();
        let detail = "摘要尚未完成，已保留正文 / Summary incomplete; body retained";
        let course = crate::notes::fixtures::published_note(
            root.path(),
            1,
            &[10.],
            course2md::artifact::Outcomes {
                transcript: course2md::artifact::Outcome::succeeded(),
                screenshots: course2md::artifact::Outcome::succeeded(),
                summary: course2md::artifact::Outcome::failed(detail),
                ..Default::default()
            },
        );
        let preview = crate::notes::read_preview(course.clone()).unwrap();
        let data = load_reader_data(&preview);
        assert!(preview.plain_text.contains("10 秒的正文"));
        assert!(preview.issues.is_empty());
        assert_eq!(
            processing_notice(&preview.processing_issues).as_deref(),
            Some("摘要未完成")
        );
        assert_eq!(
            preview.processing_issues[0].outcome.message.as_deref(),
            Some(detail)
        );
        assert_eq!(preview.course.manifest.as_ref().unwrap().task_id, "task-1");
        assert!(!files_need_reload(&preview, &data.issues, &data.frames));

        std::fs::remove_file(course.dir.join("frames/frame-0.png")).unwrap();
        let damaged = crate::notes::read_preview(course).unwrap();
        let data = load_reader_data(&damaged);
        assert!(files_need_reload(&damaged, &data.issues, &data.frames));
        assert_eq!(
            processing_notice(&damaged.processing_issues).as_deref(),
            Some("摘要未完成")
        );
    }

    #[test]
    fn a_missing_earlier_frame_does_not_shift_later_times_or_mix_versions() {
        let root = tempfile::tempdir().unwrap();
        let old = fixture_version(root.path(), 1, &[10., 20., 30.]);
        let new = fixture_version(root.path(), 2, &[40.]);
        std::fs::remove_file(old.dir.join("frames/frame-0.png")).unwrap();
        let old_preview = crate::notes::read_preview(old.clone()).unwrap();
        assert!(
            old_preview
                .issues
                .iter()
                .any(|issue| issue.starts_with("00:10"))
        );
        let old_data = load_reader_data(&old_preview);
        assert_eq!(old_data.frames.len(), 3);
        assert!(old_data.frames[0].image.is_none());
        assert_eq!(old_data.frames[1].seconds, 20.);
        assert!(old_data.frames[1].transcript.contains("20 秒"));
        let body = old_data.frames[1].body_index.unwrap();
        assert!(matches!(
            &old_preview.blocks[body],
            PreviewBlock::Heading { anchor, .. } if anchor == "section-1"
        ));
        assert_eq!(old_data.frame_by_path.len(), 2);
        assert_eq!(old_data.versions.len(), 2);
        let new_data = load_reader_data(&crate::notes::read_preview(new.clone()).unwrap());
        assert_eq!(new_data.frames.len(), 1);
        assert_eq!(new_data.frames[0].seconds, 40.);
        assert!(
            new_data.frames[0]
                .image
                .as_ref()
                .unwrap()
                .path
                .starts_with(&new.dir)
        );
    }

    #[test]
    fn the_contents_follow_the_last_heading_at_or_before_the_top() {
        let outline = vec![(2, "一".to_owned(), None), (6, "二".to_owned(), None)];
        assert_eq!(outline_current(&outline, 0), None);
        assert_eq!(outline_current(&outline, 2), Some(2));
        assert_eq!(outline_current(&outline, 5), Some(2));
        assert_eq!(outline_current(&outline, 9), Some(6));
    }

    #[test]
    fn repair_position_handoff_uses_the_latest_old_position_and_keeps_existing_versions() {
        use crate::workspace::ReadingPosition;
        use std::collections::BTreeMap;
        let start = ReadingPosition {
            seconds: Some(12.),
            offset: -120.,
            ..Default::default()
        };
        let latest = ReadingPosition {
            paragraph: Some("section-80".into()),
            seconds: Some(80.),
            offset: -840.,
            fraction: Some(0.4),
            ..Default::default()
        };
        let gallery = ReadingPosition {
            seconds: Some(36.),
            offset: -300.,
            ..Default::default()
        };
        let mut positions = BTreeMap::from([
            ("course:old:0".into(), start.clone()),
            ("course:old:1".into(), gallery.clone()),
        ]);
        // The user keeps scrolling while the new version is read. Completion
        // saves this latest old-note position before performing the handoff.
        positions.insert("course:old:0".into(), latest.clone());
        carry_repaired_reading_positions(&mut positions, ("course", "old"), ("course", "new"));
        assert_eq!(positions["course:new:0"], latest);
        assert_eq!(positions["course:new:1"], gallery);
        assert_eq!(positions["course:old:0"], latest);
        assert_eq!(positions["course:old:1"], gallery);

        positions.insert("course:new:0".into(), start.clone());
        carry_repaired_reading_positions(&mut positions, ("course", "old"), ("course", "new"));
        assert_eq!(positions["course:new:0"], start);
        let snapshot = positions.clone();
        carry_repaired_reading_positions(&mut positions, ("course", "old"), ("different", "new"));
        carry_repaired_reading_positions(&mut positions, ("course", "old"), ("course", "old"));
        assert_eq!(positions, snapshot);
    }

    #[test]
    fn a_successful_read_clears_only_its_own_failure_notice() {
        let failed = "笔记文件暂时无法访问";
        for (current, failure, expected) in [
            (Some(failed), Some(failed), None),
            (Some("分类已恢复"), Some(failed), Some("分类已恢复")),
            (None, Some(failed), None),
            (Some(failed), None, Some(failed)),
        ] {
            let mut message = current.map(String::from);
            let mut failure = failure.map(String::from);
            settle_reader_notice(&mut message, &mut failure);
            assert_eq!(message.as_deref(), expected);
            assert!(failure.is_none());
        }
    }
}
