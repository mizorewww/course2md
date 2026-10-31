//! Reading one immutable note version, independently of its exports. The note is one
//! Markdown document in a scrollable `TextView` (see `note_text`), which owns selection,
//! highlights and virtualization; the screenshots are a list of rows beside it.
use crate::{
    Course, Desktop, Page, note_text,
    notes::{self, PreviewBlock},
    reader_images::{Loaded, ReaderImages, Size},
    reader_navigation as nav, source, symbols, ui, workspace,
};
use gpui::{prelude::*, *};
use gpui_base::{
    TextSelection, h_flex,
    input::Copy as CopyText,
    text::{
        MarkdownExtensions, RangeHighlight, RenderedText, SelectionFormat, TextView, TextViewState,
    },
    v_flex,
};
use gpui_kit::component::{
    input::{InputEvent, InputState},
    scroll::Scrollbar,
};
use gpui_kit_material::{
    color::ColorRole::*,
    components::title_bar_control,
    icons::{Icon, SymbolIcon},
    motion,
    prelude::*,
};
use std::{
    cell::Cell,
    collections::{BTreeMap, HashMap},
    ops::Range,
    path::PathBuf,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    time::Duration,
};

/// How long scrolling must rest before the reading position is recorded.
const READING_POSITION_SETTLE: Duration = Duration::from_secs(1);

actions!(
    course2md_reader,
    [
        NextMatch,
        PreviousMatch,
        CloseFind,
        PreviousImage,
        NextImage,
        ZoomIn,
        ZoomOut,
        FitImage,
        ReaderPageUp,
        ReaderPageDown,
        ReaderStart,
        ReaderEnd,
        ImageTextUp,
        ImageTextDown
    ]
);

#[derive(Clone)]
struct FrameImage {
    path: PathBuf,
    width: u32,
    height: u32,
}
#[derive(Clone)]
struct Frame {
    anchor: String,
    /// `None` when the file is missing or cannot be decoded.
    image: Option<FrameImage>,
    seconds: f64,
    transcript: String,
    /// Lowercased transcript and time that gallery search matches against.
    search_text: String,
    /// The section heading this screenshot belongs to.
    body_index: Option<usize>,
}
impl Frame {
    fn new(
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
struct Version {
    course: Course,
    label: String,
}
#[derive(Default)]
struct ReaderData {
    frames: Vec<Frame>,
    frame_by_path: HashMap<PathBuf, usize>,
    versions: Vec<Version>,
    issues: Vec<String>,
    export_folder: Option<PathBuf>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum OfflineVideo {
    #[default]
    NotRequested,
    Missing,
    Available(PathBuf),
}

#[derive(Clone)]
struct OfflineVideoRequest {
    library_root: PathBuf,
    task_id: String,
    work_dir: PathBuf,
    version_dir: PathBuf,
}

impl OfflineVideoRequest {
    fn for_version(
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
    fn inspect(&self) -> OfflineVideo {
        self.checked_path()
            .map(OfflineVideo::Available)
            .unwrap_or(OfflineVideo::Missing)
    }

    fn checked_path(&self) -> Option<PathBuf> {
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

/// The screenshot viewer. It outlives closing, so the dialog can play its exit.
struct ImageViewer {
    open: bool,
    /// Its content has taken focus from the dialog.
    focused: bool,
    text_scroll: ScrollHandle,
    viewport_scroll: ScrollHandle,
    viewport_size: gpui::Size<Pixels>,
    frames: Vec<Frame>,
    filtered: bool,
    index: usize,
    zoom: Option<f32>,
    /// A drag panning the enlarged image: where it started and the scroll offset then.
    drag: Option<(Point<Pixels>, Point<Pixels>)>,
    text_open: Option<bool>,
    focus: FocusHandle,
}

const ZOOM_STEP_IN: f32 = 1.25;
const ZOOM_STEP_OUT: f32 = 0.8;
const ZOOM_MIN: f32 = 0.1;
const ZOOM_MAX: f32 = 4.;

fn zoomed(scale: f32, factor: f32) -> f32 {
    (scale * factor).clamp(ZOOM_MIN, ZOOM_MAX)
}

#[derive(Clone)]
struct ExportFeedback {
    title: String,
    format: course2md::config::OutputFormat,
    result: Result<PathBuf, String>,
}

#[derive(Default)]
struct ExportState {
    generation: u64,
    pending: Option<(u64, PathBuf)>,
    feedback: BTreeMap<PathBuf, ExportFeedback>,
}

impl ExportState {
    fn begin(&mut self, version: PathBuf) -> Option<u64> {
        if self.pending.is_some() {
            return None;
        }
        self.generation = self.generation.wrapping_add(1);
        self.pending = Some((self.generation, version));
        Some(self.generation)
    }

    fn is_pending(&self, generation: u64, version: &std::path::Path) -> bool {
        self.pending
            .as_ref()
            .is_some_and(|(current, source)| *current == generation && source == version)
    }

    fn cancel(&mut self, generation: u64, version: &std::path::Path) -> bool {
        if !self.is_pending(generation, version) {
            return false;
        }
        self.pending = None;
        true
    }

    fn finish(
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

fn exported_file_label(path: &std::path::Path) -> String {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| course2md::tr!("导出文件").into());
    course2md::tr!("已导出 · {}", name)
}

/// A tooltip naming an icon button.
fn tool(
    button: IconButton,
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
) -> impl IntoElement {
    MaterialTooltip::plain(id, label).trigger(button)
}

/// One of the viewer's icons: `tip` also names its key.
fn viewer_tool(
    id: &'static str,
    icon: SymbolIcon,
    label: &'static str,
    tip: &'static str,
    disabled: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    tool(
        IconButton::new(id, icon, label)
            .disabled(disabled)
            .on_click(on_click),
        SharedString::from(format!("{id}-tip")),
        tip,
    )
}

pub(crate) struct State {
    pub(crate) images: ReaderImages,
    note: NoteState,
    find: FindState,
    gallery: GalleryState,
    restore: RestoreState,
    focus: FocusHandle,
    details_open: bool,
    processing_details_open: bool,
    /// The contents over the article, when they cannot sit beside it.
    toc_open: bool,
    toc_scroll: ScrollHandle,
    /// The contents entry last scrolled into view, so the rail follows the reading
    /// position without fighting a reader who scrolls it.
    toc_revealed: Option<usize>,
    loaded: Option<PathBuf>,
    generation: u64,
    data_loading: bool,
    frames: Rc<Vec<Frame>>,
    frame_by_path: Rc<HashMap<PathBuf, usize>>,
    /// The note's contents and the chapter titles its headings open.
    outline: Rc<Vec<(usize, String, Option<f64>)>>,
    versions: Vec<Version>,
    issues: Vec<String>,
    viewer: Option<ImageViewer>,
    viewer_text: MarkdownExtensions,
    viewer_line: Arc<AtomicU32>,
    exports: BTreeMap<PathBuf, PathBuf>,
    export_folder: Option<PathBuf>,
    export_state: ExportState,
    source_loading: bool,
    /// Cancels the source relocation probe in flight; set when the note changes, a new probe starts or it finishes.
    source_probe_cancel: Option<Arc<AtomicBool>>,
    source: Option<nav::SourceTarget>,
    source_available: bool,
    offline_video: OfflineVideo,
    offline_opening: bool,
    window: AnyWindowHandle,
    _subscriptions: Vec<Subscription>,
}

/// The note as one document.
struct NoteState {
    view: Entity<TextViewState>,
    /// The view's own list: its items are the document's top-level blocks.
    list: ListState,
    extensions: MarkdownExtensions,
    line_width: Arc<AtomicU32>,
    document: NoteDocument,
    /// The decode width of the article's figures.
    figure_size: Size,
}

/// In-note find: its input, the matches and the current one.
struct FindState {
    input: Entity<InputState>,
    open: bool,
    return_focus: Option<FocusHandle>,
    /// What the matches were found in: changes when a new parse lands.
    text: Option<RenderedText>,
    prepared: Option<nav::PreparedText>,
    matches: Rc<Vec<Range<usize>>>,
    current: usize,
    /// The color the matches were painted in: a new theme paints them again.
    painted: Option<Rgba>,
}

/// The screenshot tab: its search, matching frames and virtualized rows.
struct GalleryState {
    find: Entity<InputState>,
    search_position: Option<workspace::ReadingPosition>,
    /// Screenshots matching the gallery search, in note order.
    matches: Rc<Vec<usize>>,
    /// Each matching screenshot's excerpt, as Markdown with its matches marked.
    excerpts: Rc<HashMap<usize, String>>,
    list: ListState,
    items: Rc<Vec<GalleryItem>>,
    columns: usize,
    extensions: MarkdownExtensions,
    line_width: Arc<AtomicU32>,
}

/// Returning to a saved reading position once its view has what it needs, and recording
/// the position while reading.
#[derive(Default)]
struct RestoreState {
    pending: Option<workspace::ReadingPosition>,
    active: bool,
    generation: u64,
    last_position: Option<(String, workspace::ReadingPosition)>,
    /// The pending record of a settling scroll; replacing it restarts the wait.
    save: Option<Task<()>>,
}

impl State {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Desktop>) -> Self {
        let find =
            cx.new(|cx| InputState::new(window, cx).placeholder(course2md::tr!("查找笔记内容")));
        let gallery_find = cx
            .new(|cx| InputState::new(window, cx).placeholder(course2md::tr!("搜索截图对应文字")));
        let subscription = cx.subscribe(&find, |this, _, event, cx| match event {
            InputEvent::Change => {
                let query = this.reader_ui.find.input.read(cx).value();
                this.reader_ui.find.open |= !query.trim().is_empty();
                this.update_reader_matches(cx);
                if this.result_tab == 0 {
                    this.move_reader_match(0, cx);
                }
            }
            InputEvent::PressEnter { shift, .. } => {
                this.move_reader_match(if *shift { -1 } else { 1 }, cx)
            }
            _ => {}
        });
        let gallery_subscription = cx.subscribe(&gallery_find, |this, _, event, cx| {
            if matches!(event, InputEvent::Change) {
                this.gallery_search_changed(cx);
            }
        });
        let find_keys = [
            crate::secondary_binding("secondary-g", NextMatch, Some("CourseReader"), cx),
            crate::secondary_binding("secondary-shift-g", PreviousMatch, Some("CourseReader"), cx),
        ];
        cx.bind_keys(find_keys);
        cx.bind_keys([
            KeyBinding::new("escape", CloseFind, Some("CourseReader")),
            KeyBinding::new("left", PreviousImage, Some("ReaderImage")),
            KeyBinding::new("right", NextImage, Some("ReaderImage")),
            KeyBinding::new("=", ZoomIn, Some("ReaderImage")),
            KeyBinding::new("+", ZoomIn, Some("ReaderImage")),
            KeyBinding::new("-", ZoomOut, Some("ReaderImage")),
            KeyBinding::new("0", FitImage, Some("ReaderImage")),
            // The screenshot's text scrolls from the keyboard as well.
            KeyBinding::new("up", ImageTextUp, Some("ReaderImage")),
            KeyBinding::new("down", ImageTextDown, Some("ReaderImage")),
            KeyBinding::new("pageup", ReaderPageUp, Some("ReaderImage")),
            KeyBinding::new("pagedown", ReaderPageDown, Some("ReaderImage")),
            KeyBinding::new("home", ReaderStart, Some("ReaderImage")),
            KeyBinding::new("end", ReaderEnd, Some("ReaderImage")),
            KeyBinding::new("pageup", ReaderPageUp, Some("CourseReader && !Input")),
            KeyBinding::new("pagedown", ReaderPageDown, Some("CourseReader && !Input")),
            KeyBinding::new("home", ReaderStart, Some("CourseReader && !Input")),
            KeyBinding::new("end", ReaderEnd, Some("CourseReader && !Input")),
        ]);
        let note = cx.new(|cx| TextViewState::markdown("", cx));
        let note_list = note.read(cx).list_state().clone();
        // A new parse brings new text to search and lets a pending position apply.
        let parsed = cx.observe(&note, |this, note, cx| {
            let text = note.read(cx).rendered_text();
            if this.reader_ui.find.text.as_ref() != Some(&text) {
                this.reader_ui.find.text = Some(text);
                this.reader_ui.find.prepared = None;
                if this.reader_ui.find.open {
                    this.update_reader_matches(cx);
                }
                cx.notify();
            }
        });
        let desktop = cx.weak_entity();
        let moved = move |list: &ListState| {
            let desktop = desktop.clone();
            let list = list.clone();
            let last = Rc::new(Cell::new(None));
            move |_: &ListScrollEvent, _: &mut Window, cx: &mut App| {
                let (desktop, list, last) = (desktop.clone(), list.clone(), last.clone());
                // The list is still borrowed while it calls this.
                cx.defer(move |cx| {
                    let top = list.logical_scroll_top().item_ix;
                    let changed = last.replace(Some(top)) != Some(top);
                    let _ = desktop.update(cx, |this, cx| {
                        if changed {
                            cx.notify();
                        }
                        this.reading_position_moved(cx);
                    });
                });
            }
        };
        note_list.set_scroll_handler(moved(&note_list));
        let gallery_list = ListState::new(1, ListAlignment::Top, px(1000.));
        gallery_list.set_scroll_handler(moved(&gallery_list));
        let blocks = cx.weak_entity();
        let render_block: note_text::RenderBlock = Arc::new(move |address, window, cx| {
            blocks
                .update(cx, |this, cx| this.note_block(address, window, cx))
                .unwrap_or_else(|_| div().into_any_element())
        });
        let line_width = Arc::new(AtomicU32::new(0));
        let viewer_line = Arc::new(AtomicU32::new(0));
        let gallery_line = Arc::new(AtomicU32::new(0));
        Self {
            images: Default::default(),
            note: NoteState {
                view: note,
                list: note_list,
                extensions: note_text::note_extensions(line_width.clone(), render_block),
                line_width,
                document: NoteDocument::default(),
                figure_size: Size::thumbnail(1.),
            },
            find: FindState {
                input: find,
                open: false,
                return_focus: None,
                text: None,
                prepared: None,
                matches: Rc::default(),
                current: 0,
                painted: None,
            },
            gallery: GalleryState {
                find: gallery_find,
                search_position: None,
                matches: Rc::default(),
                excerpts: Rc::default(),
                list: gallery_list,
                items: Rc::new(vec![GalleryItem::Header]),
                columns: 1,
                extensions: note_text::extensions(gallery_line.clone()),
                line_width: gallery_line,
            },
            restore: RestoreState::default(),
            focus: cx.focus_handle(),
            details_open: false,
            processing_details_open: false,
            toc_open: false,
            toc_scroll: ScrollHandle::new(),
            toc_revealed: None,
            loaded: None,
            generation: 0,
            data_loading: false,
            frames: Rc::default(),
            frame_by_path: Rc::default(),
            outline: Rc::default(),
            versions: Vec::new(),
            issues: Vec::new(),
            viewer: None,
            viewer_text: note_text::extensions(viewer_line.clone()),
            viewer_line,
            exports: BTreeMap::new(),
            export_folder: None,
            export_state: ExportState::default(),
            source_loading: false,
            source_probe_cancel: None,
            source: None,
            source_available: false,
            offline_video: OfflineVideo::NotRequested,
            offline_opening: false,
            window: window.window_handle(),
            _subscriptions: vec![subscription, gallery_subscription, parsed],
        }
    }
}

fn block_anchor(block: &PreviewBlock, index: usize) -> String {
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
/// Position-store key for a manifest course at one tab (`:0` is the note
/// tab). The library's "继续阅读" probe and the reader's save/restore build
/// identical keys through this function.
pub(crate) fn manifest_reading_key(course_id: &str, version_id: &str, tab: usize) -> String {
    format!("{course_id}:{version_id}:{tab}")
}
/// A saved position away from the document top names the block it was read
/// at; the top itself is stored as the default position.
pub(crate) fn has_reading_position(position: &workspace::ReadingPosition) -> bool {
    position.paragraph.is_some() || position.seconds.is_some_and(|seconds| seconds > 0.)
}
fn block_time(blocks: &[PreviewBlock], index: usize) -> Option<f64> {
    for block in blocks.iter().take(index + 1).rev() {
        // An explicitly untimed heading ends the previous time's scope.
        if let PreviewBlock::Heading { seconds, .. } = block {
            return *seconds;
        }
    }
    None
}
fn heading_times(blocks: &[PreviewBlock]) -> impl Iterator<Item = (usize, Option<f64>)> + '_ {
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
fn note_position_index(
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

/// The address of the note's opening: its facts, notices and details.
const HEAD_BLOCK: &str = "reader:head";
const FIGURE_BLOCK: &str = "figure:";

/// A note as Markdown, with what each top-level block of it shows.
#[derive(Default)]
struct NoteDocument {
    markdown: String,
    /// The note block each top-level block draws, when it draws one.
    blocks: Rc<Vec<Option<usize>>>,
}

impl NoteDocument {
    fn new(
        title: &str,
        blocks: &[PreviewBlock],
        chapters: &HashMap<usize, String>,
        seek: impl Fn(f64) -> Option<String>,
    ) -> Self {
        let mut parts: Vec<(String, Option<usize>)> = vec![
            (
                format!("# {}", note_text::plain_inline_markdown(title)),
                None,
            ),
            (format!("![]({HEAD_BLOCK})"), None),
        ];
        // A section's screenshot follows its paragraphs.
        let mut figures: Vec<(String, Option<usize>)> = Vec::new();
        let mut index = 0;
        while let Some(block) = blocks.get(index) {
            let paragraph_blocks = |text: &str, index: usize| {
                note_text::blocks(text)
                    .into_iter()
                    .map(move |block| (block, Some(index)))
            };
            match block {
                PreviewBlock::Heading { text, seconds, .. } => {
                    parts.append(&mut figures);
                    if let Some(chapter) = chapters.get(&index) {
                        let chapter = note_text::plain_inline_markdown(chapter);
                        parts.push((format!("## {chapter}"), Some(index)));
                    }
                    let level = if block.is_timestamp_heading() {
                        "###"
                    } else {
                        "##"
                    };
                    let link = seconds
                        .and_then(&seek)
                        .map(|url| format!(" [{}]({url})", note_text::SOURCE_LINK_LABEL))
                        .unwrap_or_default();
                    let heading = note_text::plain_inline_markdown(text);
                    parts.push((format!("{level} {heading}{link}"), Some(index)));
                }
                PreviewBlock::Paragraph { anchor, .. } if anchor.starts_with("key-point-") => {
                    // The key points are one list.
                    let start = index;
                    let mut list = Vec::new();
                    while let Some(PreviewBlock::Paragraph { text, anchor }) = blocks.get(index)
                        && anchor.starts_with("key-point-")
                    {
                        list.push(format!("- {}", note_text::inline_markdown(text)));
                        index += 1;
                    }
                    parts.push((list.join("\n"), Some(start)));
                    continue;
                }
                PreviewBlock::Paragraph { text, .. } => parts.extend(paragraph_blocks(text, index)),
                // A translation reads as a quieter companion to the paragraph above it.
                PreviewBlock::Translation { text, .. } => parts.push((
                    note_text::blocks(text)
                        .join("\n\n")
                        .lines()
                        .map(|line| format!("> {line}"))
                        .collect::<Vec<_>>()
                        .join("\n"),
                    Some(index),
                )),
                PreviewBlock::Image(_) => {
                    figures.push((format!("![]({FIGURE_BLOCK}{index})"), Some(index)))
                }
            }
            index += 1;
        }
        parts.append(&mut figures);
        let (parts, blocks): (Vec<_>, Vec<_>) = parts.into_iter().unzip();
        Self {
            markdown: parts.join("\n\n"),
            blocks: Rc::new(blocks),
        }
    }

    /// The top-level block drawing note block `block`, or else the last before it.
    fn item_of(&self, block: usize) -> Option<usize> {
        self.blocks
            .iter()
            .position(|shown| *shown == Some(block))
            .or_else(|| {
                self.blocks
                    .iter()
                    .rposition(|shown| shown.is_some_and(|shown| shown < block))
            })
    }

    /// The note block at or after top-level block `item`.
    fn block_at(&self, item: usize) -> Option<usize> {
        self.blocks.get(item..)?.iter().flatten().next().copied()
    }
}

/// The contents entry that holds `block`: the last heading at or before it.
fn outline_current(outline: &[(usize, String, Option<f64>)], block: usize) -> Option<usize> {
    outline
        .iter()
        .rev()
        .find(|(index, _, _)| *index <= block)
        .map(|(index, _, _)| *index)
}

/// One item of the virtualized screenshot gallery.
#[derive(Clone, Debug, PartialEq, Eq)]
enum GalleryItem {
    /// The note's opening and the gallery's loading, empty or no-match state.
    Header,
    /// One grid row: a range of the matching screenshots.
    Row(Range<usize>),
}

/// The gallery as list items: the header, then rows of `columns` screenshots.
fn gallery_items(matches: usize, columns: usize) -> Vec<GalleryItem> {
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
fn gallery_row_of(items: &[GalleryItem], matches: &[usize], frame: usize) -> Option<usize> {
    let at = matches.iter().position(|index| *index == frame)?;
    items
        .iter()
        .position(|item| matches!(item, GalleryItem::Row(row) if row.contains(&at)))
}

/// The first screenshot of the gallery's top row.
fn gallery_top_frame(items: &[GalleryItem], matches: &[usize], list: &ListState) -> Option<usize> {
    let GalleryItem::Row(row) = items.get(list.logical_scroll_top().item_ix)? else {
        return None;
    };
    matches.get(row.start).copied()
}

fn frame_label(frame: &Frame) -> String {
    course2md::tr!("{} 的截图", course2md::render::fmt_ts(frame.seconds))
}

fn matching_frames(frames: &[Frame], query: &str) -> Vec<usize> {
    let query = query.trim().to_lowercase();
    frames
        .iter()
        .enumerate()
        .filter(|(_, frame)| frame.search_text.contains(&query))
        .map(|(index, _)| index)
        .collect()
}

/// How far a card excerpt moves its cut to reach a word boundary.
const EXCERPT_WORD_SLACK: usize = 24;
/// What a card shows of a screenshot's text without a match to centre on.
const EXCERPT_GRAPHEMES: usize = 160;

fn frame_excerpt(frame: &Frame, query: &str) -> String {
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
    for (range, piece) in note_text::piece_sources(text) {
        if matches!(piece, crate::math::Piece::Math { .. }) {
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
fn chapter_starts(blocks: &[PreviewBlock], outline: &[(f64, String)]) -> HashMap<usize, String> {
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

fn summary_outline(preview: &notes::Preview) -> Vec<(f64, String)> {
    preview
        .document
        .summary
        .iter()
        .flat_map(|summary| &summary.outline)
        .map(|item| (item.t, item.title.clone()))
        .collect()
}

fn reader_outline(preview: &notes::Preview) -> Vec<(usize, String, Option<f64>)> {
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

/// A screenshot in its 16:9 slot: the image, its placeholder, or that it cannot be read.
fn screenshot_slot(id: impl Into<ElementId>, loaded: Option<Loaded>) -> Div {
    let id = id.into();
    let slot = div()
        .relative()
        .w_full()
        .aspect_ratio(16. / 9.)
        .rounded(RADIUS_MEDIUM)
        .overflow_hidden()
        .bg(color(SURFACE_CONTAINER_HIGHEST));
    match loaded {
        Some(Loaded::Ready(image, fresh)) => {
            let picture = img(image)
                .absolute()
                .inset_0()
                .size_full()
                .object_fit(ObjectFit::Contain);
            // A screenshot that just arrived fades in over its placeholder; one
            // scrolled back into view is already there.
            slot.child(if fresh {
                motion::enter(id, picture)
            } else {
                picture.into_any_element()
            })
        }
        Some(Loaded::Failed) | None => slot.child(unreadable_screenshot(id).absolute().inset_0()),
        Some(Loaded::Pending) => slot.child(
            div()
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .child(Icon::from(symbols::image()).color(color(FAINT))),
        ),
    }
}

/// A screenshot whose file cannot be read, in the space its image takes.
fn unreadable_screenshot(id: impl Into<ElementId>) -> Stateful<Div> {
    let id = id.into();
    v_flex()
        .id(id.clone())
        .items_center()
        .justify_center()
        .gap(rem(8.))
        .p(rem(16.))
        .bg(color(WARNING_BG))
        .text_color(color(WARNING))
        .child(Icon::from(symbols::image()))
        .child(
            accessible_text(
                ElementId::NamedChild(Arc::new(id), "text".into()),
                course2md::tr!("截图无法读取"),
            )
            .text_size(TEXT_AUX)
            .text_center(),
        )
}

/// The space and layout of the page at the current window and text scale.
#[derive(Clone, Copy)]
struct ReaderMetrics {
    content_width: f32,
    /// The contents sit beside the article.
    toc_beside: bool,
    /// So short that the views give their row to the text.
    short: bool,
    /// The article's text column and its inset in the card.
    line_width: f32,
    inset: Rems,
    scale_factor: f32,
}

const TOC_GAP: Rems = rem(24.);
const TOC_WIDTH: Rems = rem(260.);
/// The article card's widest measure, and the narrowest it gets beside the contents.
const ARTICLE_MAX: Rems = rem(820.);
const ARTICLE_MIN_BESIDE: Rems = rem(560.);
const GALLERY_GAP: Rems = rem(12.);
/// The screenshot grid's narrowest column.
const GALLERY_MIN_COLUMN: Rems = rem(260.);
/// A window with less height than this for the page shows no view tabs.
const SHORT_MAX_HEIGHT: Rems = rem(336.);
const TITLE_BAR_HEIGHT: Rems = rem(56.);
/// The widest the search field grows.
const SEARCH_MAX_WIDTH: Rems = rem(560.);

impl ReaderMetrics {
    fn new(window: &Window, has_outline: bool) -> Self {
        let rem_size = window.rem_size();
        let pixels = |value: Rems| f32::from(value.to_pixels(rem_size));
        let content_width = crate::views::shell_content_width(Page::Result, window);
        let toc_beside = has_outline
            && content_width >= pixels(TOC_WIDTH) + pixels(TOC_GAP) + pixels(ARTICLE_MIN_BESIDE);
        let article = if toc_beside {
            content_width - pixels(TOC_WIDTH) - pixels(TOC_GAP)
        } else {
            content_width
        }
        .min(pixels(ARTICLE_MAX));
        // Narrow cards keep more of their width for text.
        let inset = if article < pixels(rem(560.)) {
            rem(20.)
        } else {
            rem(48.)
        };
        let available = window.bounds().size.height - TITLE_BAR_HEIGHT.to_pixels(rem_size);
        Self {
            content_width,
            toc_beside,
            short: available < SHORT_MAX_HEIGHT.to_pixels(rem_size),
            line_width: (article - pixels(inset) * 2.).max(0.),
            inset,
            scale_factor: window.scale_factor(),
        }
    }
}

impl Desktop {
    fn reader_exporting(&self) -> bool {
        self.reader_ui.export_state.pending.is_some()
    }

    pub(super) fn reader_viewer_open(&self) -> bool {
        self.reader_ui
            .viewer
            .as_ref()
            .is_some_and(|viewer| viewer.open)
    }

    /// The list of the current view.
    fn reader_list(&self) -> &ListState {
        if self.result_tab == 0 {
            &self.reader_ui.note.list
        } else {
            &self.reader_ui.gallery.list
        }
    }

    fn scroll_reader_page(&mut self, direction: f32, cx: &mut Context<Self>) {
        let list = self.reader_list();
        list.scroll_by(-list.viewport_bounds().size.height * 0.85 * direction);
        self.reading_position_moved(cx);
        cx.notify();
    }
    fn reading_key(&self) -> Option<String> {
        let preview = self.preview.as_ref()?;
        Some(match &preview.course.manifest {
            Some(manifest) => {
                manifest_reading_key(&manifest.course_id, &manifest.version_id, self.result_tab)
            }
            None => {
                let identity = self
                    .course_location(&preview.course)
                    .and_then(|library| {
                        preview
                            .course
                            .dir
                            .strip_prefix(&library.root)
                            .ok()
                            .map(|relative| format!("{}:{}", library.id, relative.display()))
                    })
                    .unwrap_or_else(|| preview.course.dir.display().to_string());
                format!("{identity}:{}", self.result_tab)
            }
        })
    }
    /// The current view's top block and how far into it the view starts.
    fn capture_reading_position(&self) -> Option<workspace::ReadingPosition> {
        let preview = self.preview.as_ref()?;
        let top = self.reader_list().logical_scroll_top();
        let (paragraph, seconds) = if self.result_tab == 0 {
            let index = self.reader_ui.note.document.block_at(top.item_ix)?;
            // Above the first block the note draws, the note reads from its top.
            if self
                .reader_ui
                .note
                .document
                .blocks
                .get(top.item_ix)?
                .is_none()
            {
                return Some(Default::default());
            }
            (
                preview
                    .blocks
                    .get(index)
                    .map(|block| block_anchor(block, index)),
                block_time(&preview.blocks, index),
            )
        } else {
            let gallery = &self.reader_ui.gallery;
            let Some(frame) = gallery_top_frame(&gallery.items, &gallery.matches, &gallery.list)
            else {
                return Some(Default::default());
            };
            let frame = self.reader_ui.frames.get(frame)?;
            (Some(frame.anchor.clone()), Some(frame.seconds))
        };
        Some(workspace::ReadingPosition {
            paragraph,
            seconds,
            within: -f32::from(top.offset_in_item),
            ..Default::default()
        })
    }
    /// Scrolling records the position once it has settled, so continuous
    /// scrolling does not clone and rewrite the workspace for every step.
    /// Navigation and exit call `save_reading_position` to record at once.
    fn reading_position_moved(&mut self, cx: &mut Context<Self>) {
        self.reader_ui.restore.save = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(READING_POSITION_SETTLE)
                .await;
            let _ = this.update(cx, |this, cx| this.save_reading_position(cx));
        }));
    }
    pub fn save_reading_position(&mut self, cx: &mut Context<Self>) {
        if self.page != Page::Result
            || self.reading
            || (self.result_tab == 1 && self.reader_ui.gallery.search_position.is_some())
            || self.reader_ui.restore.active
            || self.reader_ui.restore.pending.is_some()
        {
            return;
        }
        let (Some(key), Some(position)) = (self.reading_key(), self.capture_reading_position())
        else {
            return;
        };
        if self.reader_ui.restore.last_position.as_ref() == Some(&(key.clone(), position.clone())) {
            return;
        }
        if let Some(workspace) = &mut self.workspace
            && let Err(error) = workspace.transaction(|state| {
                state.positions.insert(key.clone(), position.clone());
                Ok(())
            })
        {
            self.workspace_error =
                Some(course2md::tr!("阅读位置尚未保存：{}", format!("{error:#}")));
            cx.notify();
            return;
        }
        self.reader_ui.restore.last_position = Some((key, position));
    }
    pub fn restore_reading_position(&mut self, cx: &mut Context<Self>) {
        self.ensure_reader_data(cx);
        let saved = self
            .reading_key()
            .and_then(|key| self.workspace.as_ref()?.state.positions.get(&key))
            .cloned()
            .unwrap_or_default();
        self.return_reader_to(saved, cx);
        self.reader_ui.restore.last_position = None;
    }
    /// Asks the current view for `position` once it can show it.
    fn return_reader_to(&mut self, position: workspace::ReadingPosition, cx: &mut Context<Self>) {
        self.reader_ui.restore.pending = Some(position);
        self.reader_ui.restore.generation += 1;
        self.reader_ui.restore.active = false;
        cx.notify();
    }
    /// The screenshot `position` names: by anchor, or the nearest by time.
    fn frame_of_position(&self, position: &workspace::ReadingPosition) -> Option<usize> {
        let frames = &self.reader_ui.frames;
        position
            .paragraph
            .as_ref()
            .and_then(|anchor| frames.iter().position(|frame| frame.anchor == *anchor))
            .or_else(|| {
                nav::nearest_time(
                    frames
                        .iter()
                        .enumerate()
                        .map(|(i, frame)| (i, Some(frame.seconds))),
                    position.seconds?,
                )
                .map(|(index, _)| index)
            })
    }
    /// Whether the note's document has reached its view and its list.
    fn note_is_current(&self, cx: &App) -> bool {
        let note = &self.reader_ui.note;
        note.list.item_count() == note.document.blocks.len()
            && note.view.read(cx).rendered_text().source() == note.document.markdown
    }
    fn schedule_reader_restore(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.reader_ui.restore.pending.is_none()
            || (self.result_tab == 1 && self.reader_ui.data_loading)
            || (self.result_tab == 0 && !self.note_is_current(cx))
        {
            return;
        }
        let Some(position) = self.reader_ui.restore.pending.take() else {
            return;
        };
        self.reader_ui.restore.active = true;
        let generation = self.reader_ui.restore.generation;
        let key = self.reading_key();
        // After this frame has laid out what it just received.
        cx.on_next_frame(window, move |this, _, cx| {
            if this.reader_ui.restore.generation != generation || this.reading_key() != key {
                return;
            }
            let target = this.reader_list_target(&position);
            this.reader_list().scroll_to(target);
            this.reader_ui.restore.active = false;
            this.reader_ui.restore.last_position = None;
            cx.notify();
        });
    }
    /// Where the current view's list shows `position`.
    fn reader_list_target(&self, position: &workspace::ReadingPosition) -> ListOffset {
        let item = if self.result_tab == 0 {
            self.preview.as_ref().and_then(|preview| {
                let block = note_position_index(&preview.blocks, position)?;
                self.reader_ui.note.document.item_of(block)
            })
        } else {
            let gallery = &self.reader_ui.gallery;
            self.frame_of_position(position)
                .and_then(|frame| gallery_row_of(&gallery.items, &gallery.matches, frame))
        };
        match item {
            Some(item_ix) => ListOffset {
                item_ix,
                offset_in_item: px(-position.within).max(px(0.)),
            },
            None => ListOffset::default(),
        }
    }
    pub(crate) fn ensure_reader_data(&mut self, cx: &mut Context<Self>) {
        let Some(preview) = self.preview.clone() else {
            return;
        };
        if self.reader_ui.loaded.as_ref() == Some(&preview.course.dir) {
            return;
        }
        self.reader_ui.loaded = Some(preview.course.dir.clone());
        self.reader_ui.frames = Rc::default();
        self.release_reader_images(cx);
        self.reader_ui.toc_revealed = None;
        self.reader_ui.toc_scroll.set_offset(point(px(0.), px(0.)));
        self.reader_ui.frame_by_path = Rc::default();
        self.reader_ui.gallery.matches = Rc::default();
        self.reader_ui.gallery.excerpts = Rc::default();
        self.set_gallery_items(self.reader_ui.gallery.columns);
        self.reader_ui.versions.clear();
        self.reader_ui.issues.clear();
        self.reader_ui.source = None;
        // A different note: stop the relocation probe in flight.
        if let Some(cancel) = self.reader_ui.source_probe_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        self.reader_ui.source_loading = false;
        self.reader_ui.source_available = false;
        self.reader_ui.export_folder = None;
        self.reader_ui.offline_video = OfflineVideo::NotRequested;
        self.reader_ui.offline_opening = false;
        self.reader_ui.gallery.search_position = None;
        self.reader_ui.find.open = false;
        self.reader_ui.find.matches = Rc::default();
        self.reader_ui.details_open = false;
        self.reader_ui.processing_details_open = false;
        self.rebuild_note_document(&preview, cx);
        // Clearing an input needs the window, which this entity is being
        // updated inside; clear both searches once the update ends.
        let inputs = [
            self.reader_ui.find.input.clone(),
            self.reader_ui.gallery.find.clone(),
        ];
        let window = self.reader_ui.window;
        cx.defer(move |cx| {
            let _ = window.update(cx, |_, window, cx| {
                for input in &inputs {
                    input.update(cx, |input, cx| input.set_value("", window, cx));
                }
            });
        });
        self.refresh_reader_data(cx);
    }

    /// The note's document and contents follow the loaded note.
    fn rebuild_note_document(&mut self, preview: &notes::Preview, cx: &mut Context<Self>) {
        let source = self.unmapped_reader_source();
        let chapters = chapter_starts(&preview.blocks, &summary_outline(preview));
        let document = NoteDocument::new(&preview.course.title, &preview.blocks, &chapters, |at| {
            nav::seek_url(source.as_ref()?, at)
        });
        self.reader_ui.outline = Rc::new(reader_outline(preview));
        self.reader_ui.note.view.update(cx, |view, cx| {
            view.clear_range_highlights(cx);
            view.set_text(&document.markdown, cx);
        });
        self.reader_ui.note.document = document;
    }

    /// Search the screenshots once per query change: the matching frames and their
    /// excerpts.
    fn update_gallery_matches(&mut self, cx: &App) {
        let query = self.reader_ui.gallery.find.read(cx).value();
        let matches = matching_frames(&self.reader_ui.frames, &query);
        self.reader_ui.gallery.excerpts = Rc::new(
            matches
                .iter()
                .map(|index| {
                    let text = frame_excerpt(&self.reader_ui.frames[*index], &query);
                    let marks = nav::text_matches(&text, &query);
                    (
                        *index,
                        note_text::marked_markdown(&text, &marks, color(FIND_HIGHLIGHT)),
                    )
                })
                .collect(),
        );
        self.reader_ui.gallery.matches = Rc::new(matches);
        self.set_gallery_items(self.reader_ui.gallery.columns);
    }

    /// A search keeps the position it left, to return to when it is cleared.
    fn gallery_search_changed(&mut self, cx: &mut Context<Self>) {
        self.update_gallery_matches(cx);
        if self.result_tab != 1 {
            return;
        }
        let searching = !self
            .reader_ui
            .gallery
            .find
            .read(cx)
            .value()
            .trim()
            .is_empty();
        if searching {
            if self.reader_ui.gallery.search_position.is_none() {
                self.save_reading_position(cx);
                self.reader_ui.gallery.search_position = self.capture_reading_position();
            }
            self.reader_ui.gallery.list.scroll_to(ListOffset::default());
        } else if let Some(position) = self.reader_ui.gallery.search_position.take() {
            self.return_reader_to(position, cx);
        }
        cx.notify();
    }

    /// Cut the matching screenshots into rows of `columns`, keeping the top screenshot
    /// in view.
    fn set_gallery_items(&mut self, columns: usize) {
        let gallery = &mut self.reader_ui.gallery;
        let items = gallery_items(gallery.matches.len(), columns);
        gallery.columns = columns;
        if *gallery.items == items {
            return;
        }
        let top = gallery.list.logical_scroll_top();
        let frame = gallery_top_frame(&gallery.items, &gallery.matches, &gallery.list);
        let item_ix = frame
            .and_then(|frame| gallery_row_of(&items, &gallery.matches, frame))
            .unwrap_or(top.item_ix.min(items.len() - 1));
        gallery.list.reset(items.len());
        gallery.list.scroll_to(ListOffset {
            item_ix,
            offset_in_item: top.offset_in_item,
        });
        gallery.items = Rc::new(items);
    }
    fn refresh_reader_data(&mut self, cx: &mut Context<Self>) {
        // The blocking closure below needs an owned, Send preview; data loads
        // are per open/reload, not per frame, so this clone is not the hot path.
        let Some(preview) = self
            .preview
            .as_ref()
            .map(|preview| preview.as_ref().clone())
        else {
            return;
        };
        let path = preview.course.dir.clone();
        let source = self.unmapped_reader_source();
        let offline_request = self.reader_offline_video_request();
        let locations = self
            .workspace
            .as_ref()
            .map(|workspace| {
                workspace
                    .state
                    .libraries
                    .iter()
                    .map(|library| (library.root.clone(), library.previous_roots.clone()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        self.reader_ui.generation += 1;
        let generation = self.reader_ui.generation;
        self.reader_ui.data_loading = true;
        self.reader_ui.offline_opening = false;
        cx.spawn(async move |this, cx| {
            // Reading the note's files and the offline check are blocking; see crate::spawn_blocking_io.
            let (data, source, source_available, offline_video) =
                crate::spawn_blocking_io(move || {
                    let source = source.map(|source| match source {
                        nav::SourceTarget::Local(path) => {
                            nav::SourceTarget::Local(nav::relocated_source(&path, &locations))
                        }
                        other => other,
                    });
                    let available = source.as_ref().is_some_and(|source| match source {
                        nav::SourceTarget::Web(_) => true,
                        nav::SourceTarget::Local(path) => path.is_file(),
                    });
                    let offline_video = offline_request
                        .map(|request| request.inspect())
                        .unwrap_or_default();
                    (load_reader_data(&preview), source, available, offline_video)
                })
                .recv()
                .await
                .unwrap_or_default();
            let _ = this.update(cx, |this, cx| {
                if this.reader_ui.generation != generation
                    || this
                        .preview
                        .as_ref()
                        .is_none_or(|preview| preview.course.dir != path)
                {
                    return;
                }
                let top = (this.result_tab == 1
                    && this.reader_ui.restore.pending.is_none()
                    && !this.reader_ui.restore.active)
                    .then(|| this.capture_reading_position())
                    .flatten();
                this.reader_ui.frames = Rc::new(data.frames);
                this.reader_ui.frame_by_path = Rc::new(data.frame_by_path);
                this.update_gallery_matches(cx);
                this.reader_ui.versions = data.versions;
                this.reader_ui.issues = data.issues;
                this.reader_ui.export_folder = data.export_folder;
                this.reader_ui.source = source;
                this.reader_ui.source_available = source_available;
                this.reader_ui.offline_video = offline_video;
                this.reader_ui.data_loading = false;
                if let Some(top) = top {
                    this.return_reader_to(top, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }
    fn reader_source_key(&self) -> Option<String> {
        let preview = self.preview.as_ref()?;
        Some(
            preview
                .course
                .manifest
                .as_ref()
                .map(|m| {
                    if m.source_id.is_empty() {
                        m.course_id.clone()
                    } else {
                        m.source_id.clone()
                    }
                })
                .unwrap_or_else(|| preview.course.storage_dir().display().to_string()),
        )
    }
    fn reader_offline_video_request(&self) -> Option<OfflineVideoRequest> {
        let course = &self.preview.as_ref()?.course;
        let manifest = course.manifest.as_ref()?;
        let state = &self.workspace.as_ref()?.state;
        let task = state.task(&manifest.task_id)?;
        let library = state.library(&task.plan.library_id)?;
        OfflineVideoRequest::for_version(course, task, library)
    }
    fn play_reader_offline_video(&mut self, cx: &mut Context<Self>) {
        if self.reader_ui.offline_opening {
            return;
        }
        let Some(request) = self.reader_offline_video_request() else {
            return;
        };
        let version = request.version_dir.clone();
        let generation = self.reader_ui.generation;
        self.reader_ui.offline_opening = true;
        cx.spawn(async move |this, cx| {
            let status = crate::spawn_blocking_io(move || request.inspect())
                .recv()
                .await
                .unwrap_or_default();
            let _ = this.update(cx, |this, cx| {
                if this.reader_ui.generation != generation
                    || this
                        .preview
                        .as_ref()
                        .is_none_or(|preview| preview.course.dir != version)
                {
                    return;
                }
                this.reader_ui.offline_opening = false;
                if this.page == Page::Result {
                    if let OfflineVideo::Available(path) = &status {
                        cx.open_with_system(path);
                    } else {
                        this.message = Some(course2md::tr!("保留的视频已不在").into());
                    }
                }
                this.reader_ui.offline_video = status;
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    fn unmapped_reader_source(&self) -> Option<nav::SourceTarget> {
        if let Some(path) = self
            .reader_source_key()
            .and_then(|key| self.workspace.as_ref()?.state.reader_sources.get(&key))
        {
            return Some(nav::SourceTarget::Local(path.clone()));
        }
        let meta = &self.preview.as_ref()?.document.meta;
        nav::source_target(&meta.webpage_url, meta.is_local())
    }
    fn open_reader_source(&mut self, cx: &mut Context<Self>) {
        match self.reader_ui.source.clone() {
            Some(nav::SourceTarget::Web(url)) => cx.open_url(&url),
            Some(nav::SourceTarget::Local(path)) if path.is_file() => cx.open_with_system(&path),
            Some(nav::SourceTarget::Local(path)) => {
                self.message = Some(course2md::tr!(
                    "原视频不在 {}，请用「找到原视频…」指定新位置",
                    path.display()
                ));
                cx.notify();
            }
            None => {}
        }
    }
    fn relocate_reader_source(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.reader_ui.source_loading {
            return;
        }
        let Some(key) = self.reader_source_key() else {
            return;
        };
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(course2md::tr!("选择原视频").into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let path = match receiver.await {
                Ok(Ok(Some(paths))) => paths.into_iter().next(),
                _ => None,
            };
            let Some(path) = path else {
                return;
            };
            let cancel = Arc::new(AtomicBool::new(false));
            let probing = cancel.clone();
            let _ = this.update(cx, |this, cx| {
                this.reader_ui.source_probe_cancel = Some(cancel);
                this.reader_ui.source_loading = true;
                cx.notify();
            });
            let input = path.to_string_lossy().into_owned();
            let identity = key.clone();
            let checked = crate::spawn_blocking_io(move || {
                let source::SourceProbe::Single(source) = source::probe(input, false, probing)?
                else {
                    anyhow::bail!(course2md::tr!("请选择可读取的视频文件"));
                };
                anyhow::ensure!(
                    !crate::source::is_local_identity(&identity) || identity == source.identity,
                    course2md::tr!("这个文件与生成笔记时的视频内容不同，请选择同一个原视频")
                );
                Ok::<_, anyhow::Error>(())
            });
            let checked = checked.recv().await.unwrap_or_else(|_| {
                Err(anyhow::anyhow!(course2md::tr!(
                    "检查视频时意外中断，请重试"
                )))
            });
            let _ = this.update(cx, |this, cx| {
                this.reader_ui.source_loading = false;
                this.reader_ui.source_probe_cancel = None;
                let result = checked.and_then(|_| {
                    this.workspace
                        .as_mut()
                        .ok_or_else(|| {
                            anyhow::anyhow!(course2md::tr!("暂时无法保存原视频位置，请重试"))
                        })?
                        .transaction(|state| {
                            state.reader_sources.insert(key, path.clone());
                            Ok(())
                        })
                });
                this.message = Some(match result {
                    Ok(()) => {
                        this.refresh_reader_data(cx);
                        course2md::tr!("已定位原视频：{}", path.display())
                    }
                    Err(error) => course2md::tr!("原视频位置没有更改：{}", error.to_string()),
                });
                cx.notify();
            });
        })
        .detach();
    }
    /// The search input of the current view.
    fn reader_find_input(&self) -> Entity<InputState> {
        if self.result_tab == 0 {
            self.reader_ui.find.input.clone()
        } else {
            self.reader_ui.gallery.find.clone()
        }
    }
    pub fn open_reader_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.preview.is_none() {
            return;
        }
        if !self.reader_ui.find.open {
            self.reader_ui.find.return_focus = window.focused(cx);
        }
        self.reader_ui.find.open |= self.result_tab == 0;
        self.reader_find_input()
            .update(cx, |input, cx| input.focus(window, cx));
        if self.result_tab == 0 {
            self.update_reader_matches(cx);
        }
        cx.notify();
    }
    fn close_reader_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.reader_ui.find.open = false;
        self.reader_ui.find.matches = Rc::default();
        self.reader_ui
            .note
            .view
            .update(cx, |view, cx| view.clear_range_highlights(cx));
        self.reader_find_input()
            .update(cx, |input, cx| input.set_value("", window, cx));
        match self.reader_ui.find.return_focus.take() {
            Some(focus) => focus.focus(window, cx),
            None => self.reader_ui.focus.focus(window, cx),
        }
        cx.notify();
    }
    fn update_reader_matches(&mut self, cx: &mut Context<Self>) {
        let query = self.reader_ui.find.input.read(cx).value().to_string();
        let find = &mut self.reader_ui.find;
        let matches = match &find.text {
            Some(text) if !query.trim().is_empty() => {
                let prepared = find
                    .prepared
                    .get_or_insert_with(|| nav::prepare_text(text.as_str()));
                nav::text_matches_prepared(prepared, &query)
            }
            _ => Vec::new(),
        };
        find.matches = Rc::new(matches);
        find.current = 0;
        self.paint_reader_matches(cx);
        cx.notify();
    }
    /// Find highlights over the text; the current match reads stronger.
    fn paint_reader_matches(&mut self, cx: &mut Context<Self>) {
        self.reader_ui.find.painted = Some(color(FIND_CURRENT));
        let find = &self.reader_ui.find;
        let highlights = find.matches.iter().enumerate().map(|(index, range)| {
            RangeHighlight::new(
                range.clone(),
                color(if index == find.current {
                    FIND_CURRENT
                } else {
                    FIND_HIGHLIGHT
                }),
            )
        });
        let highlights: Vec<_> = highlights.collect();
        self.reader_ui.note.view.update(cx, |view, cx| {
            // Ranges of older text wait for the new parse, which searches again.
            let _ = view.set_range_highlights(highlights, cx);
        });
    }
    fn move_reader_match(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.result_tab != 0 {
            return;
        }
        let count = self.reader_ui.find.matches.len();
        if count == 0 {
            return;
        }
        let find = &mut self.reader_ui.find;
        find.current = (find.current as isize + delta).rem_euclid(count as isize) as usize;
        let found = find.matches[find.current].clone();
        self.paint_reader_matches(cx);
        self.reader_ui.note.view.update(cx, |view, cx| {
            let _ = view.reveal_range(found, cx);
        });
        self.reading_position_moved(cx);
        cx.notify();
    }
    /// Shows note block `index` at the top of the note tab.
    fn jump_reader_block(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.result_tab != 0 {
            self.save_reading_position(cx);
            self.result_tab = 0;
        }
        let Some(preview) = &self.preview else {
            return;
        };
        let Some(block) = preview.blocks.get(index) else {
            return;
        };
        let position = workspace::ReadingPosition {
            paragraph: Some(block_anchor(block, index)),
            seconds: block_time(&preview.blocks, index),
            ..Default::default()
        };
        self.return_reader_to(position, cx);
    }
    fn open_reader_version(&mut self, course: impl Into<Rc<Course>>, cx: &mut Context<Self>) {
        self.load_reader_version(Rc::unwrap_or_clone(course.into()), false, cx);
    }
    fn load_reader_version(&mut self, course: Course, refresh: bool, cx: &mut Context<Self>) {
        if self.reading
            || (!refresh
                && self
                    .preview
                    .as_ref()
                    .is_some_and(|preview| preview.course.dir == course.dir))
        {
            return;
        }
        self.save_reading_position(cx);
        let position = self.capture_reading_position();
        let tab = self.result_tab;
        self.read_generation += 1;
        let generation = self.read_generation;
        self.reading = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = crate::spawn_blocking_io(move || notes::read_preview(course))
                .recv()
                .await
                .unwrap_or_else(|_| Err(anyhow::anyhow!(course2md::tr!("打开时意外中断，请重试"))));
            let _ = this.update(cx, |this, cx| {
                if this.read_generation != generation {
                    return;
                }
                this.reading = false;
                match result {
                    Ok(mut preview) => {
                        preview.course.title = this.course_display_title(&preview.course);
                        this.preview = Some(Rc::new(preview));
                        this.result_tab = tab;
                        if refresh {
                            this.reader_ui.loaded = None;
                        }
                        // The event path loads the data; rendering no longer does.
                        this.ensure_reader_data(cx);
                        let has_saved_position = this.reading_key().is_some_and(|key| {
                            this.workspace.as_ref().is_some_and(|workspace| {
                                workspace.state.positions.contains_key(&key)
                            })
                        });
                        this.restore_reading_position(cx);
                        if refresh {
                            this.reader_ui.restore.pending = position.clone();
                        }
                        if !refresh
                            && !has_saved_position
                            && let Some(seconds) = position.and_then(|position| position.seconds)
                        {
                            let blocks = &this.preview.as_ref().unwrap().blocks;
                            let nearest = nav::nearest_time(heading_times(blocks), seconds);
                            if let Some((index, exact)) = nearest {
                                this.reader_ui.restore.pending = Some(workspace::ReadingPosition {
                                    seconds: Some(seconds),
                                    ..Default::default()
                                });
                                if !exact {
                                    this.message = Some(course2md::tr!(
                                        "这个版本没有 {} 处的内容，已跳到最近的 {}。",
                                        course2md::render::fmt_ts(seconds),
                                        course2md::render::fmt_ts(
                                            block_time(blocks, index).unwrap()
                                        )
                                    ));
                                }
                            } else {
                                this.reader_ui.restore.pending = Some(Default::default());
                            }
                        }
                    }
                    Err(error) => {
                        this.message = Some(course2md::tr!(
                            "这个版本暂时无法打开：{}",
                            error.to_string()
                        ))
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
    /// Capture harness: the screenshot tab, a screenshot in the viewer and
    /// the end of the current view, as `capture.rs` names them.
    #[cfg(feature = "capture")]
    pub(crate) fn capture_reader_view(
        &mut self,
        shots: bool,
        shot: Option<usize>,
        end: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if shots || shot.is_some() {
            self.select_reader_view(1, window, cx);
        }
        if end {
            // After the tab's saved position has been restored.
            cx.spawn(async move |this, cx| {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(800))
                    .await;
                let _ = this.update(cx, |this, cx| {
                    this.reader_list().scroll_to_end();
                    cx.notify();
                });
            })
            .detach();
        }
        if let Some(index) = shot {
            self.open_reader_image(index, window, cx);
        }
        cx.notify();
    }

    /// Debug validation: open the requested note after the first scan.
    pub(crate) fn open_validation_note(&mut self, cx: &mut Context<Self>) {
        let Some(wanted) = self.validation.as_mut().and_then(|v| v.note.take()) else {
            return;
        };
        let found = self
            .courses
            .iter()
            .find(|course| {
                course.title.contains(&wanted) || course.dir.to_string_lossy().contains(&wanted)
            })
            .cloned();
        if let Some(course) = found {
            self.open_course(course, cx);
        }
    }

    /// Debug validation: show the requested reader panels once the note loaded.
    pub(crate) fn show_validation_panels(&mut self) {
        // Only after the requested note was opened.
        if self.validation.as_ref().is_none_or(|v| v.note.is_some()) {
            return;
        }
        let Some(validation) = self.validation.take() else {
            return;
        };
        for panel in validation.panels.split(',').map(str::trim) {
            match panel {
                "find" => self.reader_ui.find.open = true,
                "info" => self.reader_ui.details_open = true,
                "toc" => self.reader_ui.toc_open = true,
                "images" => self.result_tab = 1,
                _ => {}
            }
        }
    }

    /// The note version at `dir`: the library's shared copy, or the open note
    /// when the library has not listed it yet. Copies only when it must.
    pub(crate) fn note_at(&self, dir: &std::path::Path) -> Option<Rc<Course>> {
        self.courses
            .iter()
            .find(|course| course.dir == dir)
            .cloned()
            .or_else(|| {
                self.preview
                    .as_ref()
                    .filter(|preview| preview.course.dir == dir)
                    .map(|preview| Rc::new(preview.course.clone()))
            })
    }

    pub fn export_course(
        &mut self,
        course: Rc<Course>,
        format: course2md::config::OutputFormat,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let version = course.dir.clone();
        let Some(generation) = self.reader_ui.export_state.begin(version.clone()) else {
            return;
        };
        let title = self.course_display_title(&course);
        // The engine's file-name rules: also control characters, trailing dots,
        // device names such as `CON` and over-long titles.
        let stem = course2md::config::sanitize_filename_with_fallback(&title, "course");
        let extension = match format {
            course2md::config::OutputFormat::Md => "zip",
            course2md::config::OutputFormat::Html => "html",
            course2md::config::OutputFormat::Json => "json",
        };
        let suggested = format!("{stem}.{extension}");
        let receiver = cx.prompt_for_new_path(&self.output(), Some(&suggested));
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let path = match receiver.await {
                Ok(Ok(Some(path))) => path,
                Ok(Ok(None)) => {
                    let _ = this.update(cx, |this, cx| {
                        if this.reader_ui.export_state.cancel(generation, &version) {
                            cx.notify();
                        }
                    });
                    return;
                }
                _ => {
                    let _ = this.update(cx, |this, cx| {
                        if this.reader_ui.export_state.finish(
                            generation,
                            &version,
                            ExportFeedback {
                                title,
                                format,
                                result: Err(course2md::tr!("无法打开文件选择器，请重试。").into()),
                            },
                        ) {
                            cx.notify();
                        }
                    });
                    return;
                }
            };
            if !this
                .update(cx, |this, _| {
                    this.reader_ui.export_state.is_pending(generation, &version)
                })
                .unwrap_or(false)
            {
                return;
            }
            let target = path.clone();
            let source = version.clone();
            let exported = crate::spawn_blocking_io(move || {
                let protected = source.canonicalize()?;
                let parent = target
                    .parent()
                    .ok_or_else(|| anyhow::anyhow!(course2md::tr!("导出位置无效")))?
                    .canonicalize()?;
                anyhow::ensure!(
                    !parent.starts_with(&protected)
                        && !parent.ancestors().any(|ancestor| ancestor
                            .join("manifest.json")
                            .is_file()
                            && ancestor.parent().is_some_and(|parent| parent
                                .file_name()
                                .is_some_and(|name| name == "versions"))),
                    course2md::tr!("不能保存在笔记自己的文件夹里，请换个位置")
                );
                // The native save panel owns the explicit overwrite decision; the old file
                // remains intact until a complete export is ready beside it.
                if target.exists() {
                    let temp = parent.join(format!(
                        ".course2md-export-{}.{}",
                        workspace::new_id("file"),
                        extension
                    ));
                    let result = course2md::portable::export(&source, format, &temp)
                        .and_then(|_| std::fs::rename(&temp, &target).map_err(anyhow::Error::from));
                    if result.is_err() {
                        let _ = std::fs::remove_file(&temp);
                    }
                    result
                } else {
                    course2md::portable::export(&source, format, &target).map(|_| ())
                }
            })
            .recv()
            .await
            .unwrap_or_else(|_| Err(anyhow::anyhow!(course2md::tr!("导出时意外中断，请重试"))));
            let _ = this.update(cx, |this, cx| {
                let result = exported
                    .map(|()| path.clone())
                    .map_err(|error| format!("{error:#}"));
                let success = result.is_ok();
                if !this.reader_ui.export_state.finish(
                    generation,
                    &version,
                    ExportFeedback {
                        title,
                        format,
                        result,
                    },
                ) {
                    return;
                }
                if success {
                    this.reader_ui.exports.insert(version, path);
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn course_export_feedback(
        &self,
        course: &Course,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        let feedback = self
            .reader_ui
            .export_state
            .feedback
            .get(&course.dir)?
            .clone();
        let id = format!("export-feedback:{}", course.dir.display());
        let child = |name: &str| SharedString::from(format!("{id}:{name}"));
        let source = course.dir.clone();
        let dismiss_label = course2md::tr!("关闭这份笔记的导出提示");
        let dismiss = tool(
            IconButton::new(child("dismiss"), symbols::close(), dismiss_label).on_click(
                cx.listener(move |this, _, _, cx| {
                    this.reader_ui.export_state.feedback.remove(&source);
                    cx.notify();
                }),
            ),
            child("dismiss-tip"),
            dismiss_label,
        );
        Some(match feedback.result {
            Ok(path) => {
                let label = exported_file_label(&path);
                h_flex()
                    .w_full()
                    .min_w_0()
                    .gap(rem(8.))
                    .items_center()
                    .child(Icon::from(symbols::check()).color(color(SUCCESS)))
                    .child(
                        accessible_text(child("text"), label.clone())
                            .aria_label(course2md::tr!("《{}》{}", feedback.title, label))
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(TEXT_AUX)
                            .text_color(color(ON_SURFACE_VARIANT)),
                    )
                    .child(
                        material_button(child("reveal"), ButtonVariant::Text)
                            .icon(symbols::folder_open())
                            .child(course2md::tr!("打开导出文件夹"))
                            .on_click(move |_, _, cx| cx.reveal_path(&path)),
                    )
                    .child(dismiss)
            }
            Err(error) => {
                let retry = course.dir.clone();
                v_flex()
                    .w_full()
                    .min_w_0()
                    .gap(rem(4.))
                    .child(
                        error_text(child("text"), course2md::tr!("导出未完成")).text_size(TEXT_AUX),
                    )
                    .child(
                        accessible_text(child("detail"), error)
                            .text_size(TEXT_AUX)
                            .text_color(color(ON_SURFACE_VARIANT))
                            .whitespace_normal(),
                    )
                    .child(
                        h_flex()
                            .gap(rem(8.))
                            .items_center()
                            .child(
                                material_button(child("retry"), ButtonVariant::Outlined)
                                    .icon(symbols::refresh())
                                    .child(course2md::tr!("重试导出"))
                                    .disabled(self.reader_exporting())
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        if let Some(course) = this.note_at(&retry) {
                                            this.export_course(course, feedback.format, window, cx);
                                        }
                                    })),
                            )
                            .child(dismiss),
                    )
            }
        })
    }

    fn select_reader_view(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.result_tab != index {
            self.save_reading_position(cx);
            self.result_tab = index;
            let searching = !self
                .reader_ui
                .gallery
                .find
                .read(cx)
                .value()
                .trim()
                .is_empty();
            if index == 1 && searching {
                self.reader_ui.restore.pending = None;
                self.reader_ui.gallery.list.scroll_to(ListOffset::default());
            } else {
                self.restore_reading_position(cx);
            }
        }
        self.reader_ui.focus.focus(window, cx);
        cx.notify();
    }

    /// The page and the reading tools it puts in the title bar.
    pub fn reader_page(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (AnyElement, Option<AnyElement>) {
        let Some(preview) = self.preview.clone() else {
            return (self.reader_opening(cx), None);
        };
        self.reader_ui.images.next_frame();
        self.schedule_reader_restore(window, cx);
        if !self.reader_ui.find.matches.is_empty()
            && self.reader_ui.find.painted != Some(color(FIND_CURRENT))
        {
            self.paint_reader_matches(cx);
        }
        let metrics = ReaderMetrics::new(window, !self.reader_ui.outline.is_empty());
        let tools = self.reader_tools(&preview, metrics, window, cx);
        let strip = self.reader_strip(&preview, cx);
        let tabs = (!metrics.short).then(|| self.reader_tabs(&preview, metrics, cx));
        let find = self.reader_find_bar(cx);
        let view = if self.result_tab == 0 {
            self.reader_article(&preview, metrics, cx)
        } else {
            self.reader_gallery(&preview, metrics, window, cx)
        };
        let viewer = self.reader_viewer(window, cx);
        (
            motion::state_enter(
                SharedString::from(format!("reader-open:{}", preview.course.dir.display())),
                v_flex()
                    .id("reader-page")
                    .key_context("CourseReader")
                    .track_focus(&self.reader_ui.focus)
                    .on_action(
                        cx.listener(|this, _: &NextMatch, _, cx| this.move_reader_match(1, cx)),
                    )
                    .on_action(
                        cx.listener(|this, _: &PreviousMatch, _, cx| {
                            this.move_reader_match(-1, cx)
                        }),
                    )
                    .on_action(cx.listener(|this, _: &CloseFind, window, cx| {
                        let searching = !this.reader_find_input().read(cx).value().is_empty();
                        if this.reader_ui.toc_open {
                            this.reader_ui.toc_open = false;
                            cx.notify();
                        } else if this.reader_ui.find.open || searching {
                            this.close_reader_find(window, cx);
                        } else {
                            cx.propagate();
                        }
                    }))
                    .on_action(
                        cx.listener(|this, _: &ReaderPageUp, _, cx| {
                            this.scroll_reader_page(1., cx)
                        }),
                    )
                    .on_action(cx.listener(|this, _: &ReaderPageDown, _, cx| {
                        this.scroll_reader_page(-1., cx)
                    }))
                    .on_action(cx.listener(|this, _: &ReaderStart, _, cx| {
                        this.reader_list().scroll_to(ListOffset::default());
                        this.reading_position_moved(cx);
                        cx.notify();
                    }))
                    .on_action(cx.listener(|this, _: &ReaderEnd, _, cx| {
                        this.reader_list().scroll_to_end();
                        this.reading_position_moved(cx);
                        cx.notify();
                    }))
                    // Dragging a scrollbar moves a list without its scroll handler; the
                    // release records the settled position (unchanged ones are skipped).
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| this.reading_position_moved(cx)),
                    )
                    .relative()
                    .h_full()
                    .min_h_0()
                    .w_full()
                    // A note alone keeps its views at its own measure.
                    .when(self.result_tab == 0 && !metrics.toc_beside, |page| {
                        page.max_w(ARTICLE_MAX).mx_auto()
                    })
                    .pb(rem(16.))
                    .gap(rem(12.))
                    .children(strip)
                    .children(tabs)
                    .children(find)
                    .child(view)
                    .children(viewer),
            ),
            Some(tools),
        )
    }

    /// Opening a note: the return stays available beside a quiet progress card.
    fn reader_opening(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let origin = self.reader_origin_label();
        let label = course2md::tr!("返回「{}」", origin);
        let opening = course2md::tr!("正在打开笔记…");
        motion::enter(
            "reader-opening",
            v_flex()
                .w_full()
                .max_w(ARTICLE_MAX)
                .mx_auto()
                .gap(rem(20.))
                .pt(rem(8.))
                .child(tool(
                    IconButton::new("reader-loading-back", symbols::arrow_back(), label.clone())
                        .variant(IconButtonVariant::Tonal)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.navigate(this.result_origin, cx);
                        })),
                    "reader-loading-back-tip",
                    label,
                ))
                .child(
                    h_flex()
                        .w_full()
                        .items_center()
                        .gap(rem(12.))
                        .px(rem(48.))
                        .py(rem(40.))
                        .rounded(RADIUS_HERO)
                        .bg(color(SURFACE))
                        .child(spinner("reader-opening-spinner", opening, cx))
                        .child(
                            accessible_text("opening-note", opening)
                                .role(Role::Status)
                                .text_color(color(ON_SURFACE_VARIANT)),
                        ),
                ),
        )
    }

    /// What passes over the note while reading it, kept in view: a finished export and a
    /// version on its way.
    fn reader_strip(
        &mut self,
        preview: &Rc<notes::Preview>,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        let feedback = self.course_export_feedback(&preview.course, cx);
        let loading = course2md::tr!("正在切换版本…");
        let switching = self.reading.then(|| {
            motion::enter(
                "reader-version-loading",
                h_flex()
                    .items_center()
                    .gap(rem(8.))
                    .child(spinner("reader-version-spinner", loading, cx))
                    .child(
                        accessible_text("reader-version-loading-label", loading)
                            .role(Role::Status)
                            .text_color(color(ON_SURFACE_VARIANT)),
                    ),
            )
        });
        (feedback.is_some() || switching.is_some()).then(|| {
            v_flex()
                .w_full()
                .min_w_0()
                .flex_shrink_0()
                .gap(rem(8.))
                .children(feedback)
                .children(switching)
        })
    }

    /// The peer views, and the contents button when they cannot sit beside the article.
    fn reader_tabs(
        &mut self,
        preview: &Rc<notes::Preview>,
        metrics: ReaderMetrics,
        cx: &mut Context<Self>,
    ) -> Div {
        let shots = preview.course.slides;
        h_flex()
            .w_full()
            .min_w_0()
            .flex_shrink_0()
            .gap(rem(12.))
            .items_center()
            .child(
                div().flex_1().min_w_0().child(
                    MaterialTabs::new("reader-tabs", course2md::tr!("阅读视图"))
                        .variant(TabsVariant::Secondary)
                        .items([
                            TabItem::new("note", course2md::tr!("笔记")).icon(symbols::article()),
                            TabItem::new(
                                "images",
                                if shots > 0 {
                                    course2md::tr!("截图 · {}", shots)
                                } else {
                                    course2md::tr!("截图").to_owned()
                                },
                            )
                            .icon(symbols::image()),
                        ])
                        .selected(if self.result_tab == 0 {
                            "note"
                        } else {
                            "images"
                        })
                        .on_change(cx.listener(|this, selected: &SharedString, window, cx| {
                            this.select_reader_view(
                                usize::from(selected.as_ref() == "images"),
                                window,
                                cx,
                            );
                        })),
                ),
            )
            .children(self.reader_toc_toggle(metrics, cx))
    }

    /// The contents over the article, for a note whose contents cannot sit beside it.
    fn reader_toc_toggle(
        &self,
        metrics: ReaderMetrics,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if self.result_tab != 0 || self.reader_ui.outline.is_empty() || metrics.toc_beside {
            return None;
        }
        let label = course2md::tr!("阅读目录");
        Some(
            tool(
                IconButton::new("note-contents", symbols::format_list_bulleted(), label)
                    .toggle(true)
                    .selected(self.reader_ui.toc_open)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.reader_ui.toc_open = !this.reader_ui.toc_open;
                        cx.notify();
                    })),
                "note-contents-tip",
                label,
            )
            .into_any_element(),
        )
    }

    /// The note: the document, and its contents beside it or over it.
    fn reader_article(
        &mut self,
        preview: &Rc<notes::Preview>,
        metrics: ReaderMetrics,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let current_document = self.note_is_current(cx);
        let note = &mut self.reader_ui.note;
        note.line_width
            .store(metrics.line_width as u32, Ordering::Relaxed);
        note.figure_size = Size::thumbnail(metrics.line_width * metrics.scale_factor);
        let article =
            div()
                .id("note-reader")
                .role(Role::Document)
                .aria_label(preview.course.title.clone())
                .relative()
                .flex_1()
                .min_h_0()
                .min_w_0()
                .w_full()
                .h_full()
                .max_w(ARTICLE_MAX)
                .mx_auto()
                .pt(rem(24.))
                .rounded(RADIUS_HERO)
                .bg(color(SURFACE))
                // Copying from the note leaves its figures behind.
                .capture_action(|_: &CopyText, window, cx| {
                    let text = TextSelection::selected_text(window, cx);
                    let text = note_text::copied_text(&text);
                    if !text.is_empty() {
                        cx.write_to_clipboard(ClipboardItem::new_string(text));
                        cx.stop_propagation();
                    }
                })
                // A long note parses off the UI thread; it shows what it is doing until then.
                .when(!current_document, |article| {
                    article.child(div().absolute().top(rem(24.)).left(metrics.inset).child(
                        spinner("reader-note-loading", course2md::tr!("正在打开笔记…"), cx),
                    ))
                })
                .child(
                    TextView::new(&note.view)
                        .scrollable(true)
                        .selection_format(SelectionFormat::Source)
                        .markdown_extensions(note.extensions.clone())
                        .style(note_text::note_style(cx))
                        .text_size(TEXT_READER)
                        .line_height(relative(1.8))
                        .size_full()
                        .px(metrics.inset),
                );
        let current = {
            let top = note.list.logical_scroll_top().item_ix;
            note.document
                .block_at(top)
                .and_then(|block| outline_current(&self.reader_ui.outline, block))
        };
        if self.reader_ui.outline.is_empty() {
            return article.into_any_element();
        }
        self.reveal_toc_entry(current);
        if metrics.toc_beside {
            h_flex()
                .flex_1()
                .min_h_0()
                .w_full()
                .gap(TOC_GAP)
                .items_stretch()
                .child(self.reader_toc(current, true, cx))
                .child(article)
                .into_any_element()
        } else {
            div()
                .relative()
                .flex()
                .flex_1()
                .min_h_0()
                .w_full()
                .child(article)
                .when(self.reader_ui.toc_open, |body| {
                    body.child(
                        div()
                            .absolute()
                            .inset_0()
                            .p(rem(12.))
                            .rounded(RADIUS_HERO)
                            .bg(color(SURFACE))
                            .child(self.reader_toc(current, false, cx)),
                    )
                })
                .into_any_element()
        }
    }

    /// The rail follows the reading position: reading on reveals the entry after the
    /// current one too, so the current entry never rests on the rail's edge.
    fn reveal_toc_entry(&mut self, current: Option<usize>) {
        if current == self.reader_ui.toc_revealed {
            return;
        }
        let outline = &self.reader_ui.outline;
        let position_of =
            |block: Option<usize>| outline.iter().position(|(index, ..)| Some(*index) == block);
        let previous = position_of(self.reader_ui.toc_revealed);
        if let Some(position) = position_of(current) {
            let ahead = previous.is_none_or(|previous| position > previous);
            self.reader_ui
                .toc_scroll
                .scroll_to_item(position + if ahead { 2 } else { 1 });
        }
        self.reader_ui.toc_revealed = current;
    }

    /// Where the note goes: one selected item per heading. Its first child is its title,
    /// so an entry's child index is one past its place.
    fn reader_toc(
        &self,
        current: Option<usize>,
        beside: bool,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        v_flex()
            .id("note-toc")
            .role(Role::Navigation)
            .aria_label(course2md::tr!("阅读目录"))
            .when(beside, |toc| toc.w(TOC_WIDTH).flex_shrink_0())
            .when(!beside, |toc| toc.w_full())
            .h_full()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.reader_ui.toc_scroll)
            .child(
                h_flex()
                    .items_center()
                    .flex_shrink_0()
                    .px(rem(16.))
                    .pb(rem(8.))
                    .child(section_title("toc-title", course2md::tr!("阅读目录")).flex_1())
                    .when(!beside, |row| {
                        row.child(
                            material_button("close-note-toc", ButtonVariant::Text)
                                .icon(symbols::close())
                                .child(course2md::tr!("关闭"))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.reader_ui.toc_open = false;
                                    cx.notify();
                                })),
                        )
                    }),
            )
            .gap(TILE_GAP)
            .children(self.reader_ui.outline.iter().enumerate().map(
                |(place, (index, label, seconds))| {
                    let index = *index;
                    let time = seconds
                        .filter(|seconds| *label != course2md::render::fmt_ts(*seconds))
                        .map(course2md::render::fmt_ts);
                    ListItem::new(("toc-item", index), label.clone())
                        .density(ListDensity::Compact)
                        .segment(list_corners(place, self.reader_ui.outline.len()))
                        .selected(current == Some(index))
                        .when_some(time, |item, time| item.trailing_text(time))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.reader_ui.toc_open = false;
                            this.jump_reader_block(index, cx);
                        }))
                },
            ))
    }

    /// A paragraph of the note that the reader draws itself.
    fn note_block(
        &mut self,
        address: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let preview = self.preview.clone();
        if let (Some(preview), Some(block)) = (
            &preview,
            address
                .strip_prefix(FIGURE_BLOCK)
                .and_then(|block| block.parse::<usize>().ok()),
        ) {
            return self.note_figure(preview, block, cx);
        }
        match &preview {
            Some(preview) if address == HEAD_BLOCK => {
                self.reader_head(preview, window, cx).into_any_element()
            }
            _ => div().into_any_element(),
        }
    }

    /// A screenshot in the article: pressing it opens the viewer.
    fn note_figure(
        &mut self,
        preview: &Rc<notes::Preview>,
        block: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(PreviewBlock::Image(path)) = preview.blocks.get(block) else {
            return div().into_any_element();
        };
        let frame = self.reader_ui.frame_by_path.get(path).copied();
        // A file that is missing or whose header cannot be read gets the same
        // figure as one that fails to decode.
        let loaded = if frame.is_none() && !self.reader_ui.data_loading {
            Loaded::Failed
        } else {
            let size = self.reader_ui.note.figure_size;
            self.reader_ui.images.get(path, size, cx)
        };
        let label = frame.map_or(course2md::tr!("截图").to_owned(), |frame| {
            frame_label(&self.reader_ui.frames[frame])
        });
        let slot = screenshot_slot(("note-figure-slot", block), Some(loaded));
        let card = MaterialCard::new(("note-figure", block), CardVariant::Filled)
            .accessibility_label(course2md::tr!("放大{}", label))
            .w_full()
            .overflow_hidden()
            .child(slot);
        match frame {
            Some(frame) => card
                .on_click(
                    cx.listener(move |this, _, window, cx| {
                        this.open_reader_image(frame, window, cx)
                    }),
                )
                .into_any_element(),
            None => card.into_any_element(),
        }
    }

    /// What opens the note and heads the screenshots: its facts and version, obstacles to
    /// watching it, its details, and what needs attention.
    fn reader_head(
        &mut self,
        preview: &Rc<notes::Preview>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let meta = &preview.document.meta;
        let platform = if meta.is_local() {
            Some(course2md::tr!("本地视频"))
        } else {
            crate::views::video_site_name(&meta.webpage_url)
        };
        let mut facts: Vec<String> = Vec::new();
        facts.extend(platform.map(str::to_owned));
        if !meta.uploader.trim().is_empty() {
            facts.push(meta.uploader.trim().to_owned());
        }
        if meta.duration > 0. {
            facts.push(course2md::render::fmt_ts(meta.duration));
        }
        if preview.course.slides > 0 {
            facts.push(preview.course.content_description());
        }
        v_flex()
            .w_full()
            .min_w_0()
            .gap(rem(12.))
            .child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .flex_wrap()
                    .items_center()
                    .gap_x(rem(12.))
                    .child(
                        ui::status_line("reader-facts", &facts.join(" · "))
                            .text_size(TEXT_BODY)
                            .text_color(color(ON_SURFACE_VARIANT)),
                    )
                    .children(self.reader_version_menu(preview, window, cx)),
            )
            .children(self.reader_obstacles(cx))
            .children(self.reader_details(preview, cx))
            .children(self.reader_notices(preview, cx))
    }

    /// The other versions of this note, when it has any; its revision and date otherwise
    /// stay in the details.
    fn reader_version_menu(
        &self,
        preview: &Rc<notes::Preview>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let manifest = preview.course.manifest.as_ref()?;
        if self.reader_ui.versions.len() < 2 {
            return None;
        }
        let current = preview.course.dir.clone();
        let items = self.reader_ui.versions.iter().map(|version| {
            let course = version.course.clone();
            MaterialMenuItem::new(version.label.clone())
                .leading(Icon::from(symbols::history()))
                .selected(course.dir == current)
                .on_click(
                    cx.listener(move |this, _, _, cx| this.open_reader_version(course.clone(), cx)),
                )
        });
        let menu = MaterialMenu::new("note-versions").items(items.collect::<Vec<_>>());
        let trigger = material_button("choose-note-version", ButtonVariant::Text)
            .icon(symbols::keyboard_arrow_down())
            .icon_position(ButtonIconPosition::Trailing)
            .disabled(self.reading)
            .child(course2md::tr!(
                "第 {} 版 · {} 生成",
                manifest.revision,
                nav::date_local(manifest.created_at_ms)
            ));
        Some(ui::menu("note-version-menu", trigger, menu, window, cx).into_any_element())
    }

    /// What stands between the reader and the video, with the way past it.
    fn reader_obstacles(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let obstacle = |id: &'static str, text: &'static str, action: AnyElement| {
            h_flex()
                .flex_wrap()
                .items_center()
                .gap(rem(8.))
                .child(
                    accessible_text(id, text)
                        .text_size(TEXT_AUX)
                        .text_color(color(ON_SURFACE_VARIANT)),
                )
                .child(action)
                .into_any_element()
        };
        let mut obstacles = Vec::new();
        let offline = matches!(self.reader_ui.offline_video, OfflineVideo::Available(_));
        if matches!(self.reader_ui.source, Some(nav::SourceTarget::Local(_)))
            && !offline
            && !self.reader_ui.source_available
        {
            obstacles.push(obstacle(
                "missing-original-video",
                course2md::tr!("原视频不可用"),
                self.relocate_source_button("relocate-reader-source", cx),
            ));
        }
        if self.reader_ui.offline_video == OfflineVideo::Missing {
            let opening = self.reader_ui.offline_opening;
            obstacles.push(obstacle(
                "reader-offline-video-missing",
                course2md::tr!("保留的视频已不在"),
                material_button("retry-reader-offline-video", ButtonVariant::Text)
                    .icon(symbols::refresh())
                    .child(course2md::tr!("重试"))
                    .loading(opening, course2md::tr!("重试"))
                    .disabled(opening)
                    .on_click(cx.listener(|this, _, _, cx| this.play_reader_offline_video(cx)))
                    .into_any_element(),
            ));
        }
        obstacles
    }

    fn relocate_source_button(&self, id: &'static str, cx: &mut Context<Self>) -> AnyElement {
        let loading = self.reader_ui.source_loading;
        let label = if loading {
            course2md::tr!("正在核对视频…")
        } else {
            course2md::tr!("找到原视频…")
        };
        material_button(id, ButtonVariant::Text)
            .icon(symbols::folder_open())
            .child(label)
            .loading(loading, label)
            .disabled(loading)
            .on_click(cx.listener(|this, _, window, cx| this.relocate_reader_source(window, cx)))
            .into_any_element()
    }

    /// The note's details in place: revision, source video, subtitle and model facts.
    fn reader_details(
        &mut self,
        preview: &Rc<notes::Preview>,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.reader_ui.details_open {
            return None;
        }
        let mut details: Vec<(&str, SymbolIcon, String)> = Vec::new();
        let local_source = match &self.reader_ui.source {
            Some(nav::SourceTarget::Local(path)) => Some(path.clone()),
            _ => None,
        };
        if let Some(manifest) = &preview.course.manifest {
            let date = nav::date_local(manifest.created_at_ms);
            details.push((
                course2md::tr!("版本"),
                symbols::history(),
                course2md::tr!("第 {} 版 · {} 生成", manifest.revision, date),
            ));
        }
        if let Some(path) = &local_source {
            details.push((
                course2md::tr!("原视频"),
                symbols::movie(),
                path.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
            ));
        }
        let task = preview
            .course
            .manifest
            .as_ref()
            .and_then(|manifest| self.workspace.as_ref()?.state.task(&manifest.task_id));
        if let Some(task) = task {
            let defaults = &task.plan.config.defaults;
            if let Some(subtitle) = &task.plan.source.selected_subtitle {
                details.push((
                    course2md::tr!("所选字幕"),
                    symbols::subtitles(),
                    subtitle.label.clone(),
                ));
            }
            if defaults.transcript_source == Some(course2md::config::TranscriptSource::Asr) {
                if let Some(model) = &defaults.asr_model {
                    details.push((
                        course2md::tr!("语音识别模型"),
                        symbols::mic(),
                        model.clone(),
                    ));
                }
                if let Some(provider) = defaults.provider {
                    details.push((
                        course2md::tr!("识别设备"),
                        symbols::computer(),
                        crate::provider_label(Some(provider)).to_owned(),
                    ));
                }
            }
            if task.plan.config.llm.uses_service() {
                details.push((
                    course2md::tr!("AI 模型"),
                    symbols::auto_fix_high(),
                    task.plan.config.llm.model.clone(),
                ));
            }
        }
        let rows = details
            .into_iter()
            .enumerate()
            .map(|(index, (label, icon, value))| {
                ListItem::new(("reader-detail", index), label)
                    .leading(ListLeading::Icon(icon.into()))
                    .supporting(value)
                    .density(ListDensity::Compact)
            });
        let relocate = local_source
            .filter(|_| self.reader_ui.source_available)
            .map(|_| self.relocate_source_button("relocate-reader-source-detail", cx));
        Some(motion::disclosure(
            "reader-details",
            true,
            v_flex()
                .w_full()
                .min_w_0()
                .gap(rem(8.))
                .child(section_title(
                    "reader-details-title",
                    course2md::tr!("笔记详情"),
                ))
                .child(segmented_list(rows))
                .child(
                    h_flex().flex_wrap().gap(rem(8.)).children(relocate).child(
                        material_button("collapse-reader-details", ButtonVariant::Text)
                            .icon(symbols::keyboard_arrow_up())
                            .child(course2md::tr!("收起"))
                            .accessibility_label(course2md::tr!("收起笔记详情"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.reader_ui.details_open = false;
                                cx.notify();
                            })),
                    ),
                ),
        ))
    }

    /// New-version, incomplete-processing and file-reload notices.
    fn reader_notices(
        &mut self,
        preview: &Rc<notes::Preview>,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let mut notices = Vec::new();
        if let Some(current) = &preview.course.manifest
            && let Some(newer) = self
                .library_view_cache
                .newer_version(&self.courses, current)
        {
            notices.push(motion::enter(
                "reader-new-version-notice",
                notice_banner(
                    "newer-note-version",
                    NoticeKind::Info,
                    course2md::tr!("这份笔记已有新版。"),
                )
                .child(
                    material_button("open-newer-version", ButtonVariant::Outlined)
                        .icon(symbols::history())
                        .child(course2md::tr!("查看新版"))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.open_reader_version(newer.clone(), cx)
                        })),
                ),
            ));
        }
        if let Some(notice) = processing_notice(&preview.processing_issues) {
            notices.push(self.reader_processing_notice(preview, notice, cx));
        }
        if files_need_reload(preview, &self.reader_ui.issues, &self.reader_ui.frames) {
            let course = preview.course.clone();
            let issues: Vec<&str> = preview
                .issues
                .iter()
                .chain(self.reader_ui.issues.iter())
                .map(String::as_str)
                .collect();
            let text = if issues.is_empty() {
                course2md::tr!("部分截图无法读取").to_owned()
            } else {
                issues.join("\n")
            };
            let reading = self.reading;
            notices.push(motion::enter(
                "reader-file-notice",
                notice_banner("reader-file-notice-text", NoticeKind::Danger, text).child(
                    material_button("reload-reader-files", ButtonVariant::Outlined)
                        .icon(symbols::refresh())
                        .child(course2md::tr!("重新读取"))
                        .loading(reading, course2md::tr!("重新读取"))
                        .disabled(reading)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.load_reader_version(course.clone(), true, cx)
                        })),
                ),
            ));
        }
        notices
    }

    /// What did not finish, with the ways to finish it and what was recorded.
    fn reader_processing_notice(
        &mut self,
        preview: &Rc<notes::Preview>,
        notice: String,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let task = preview
            .course
            .manifest
            .as_ref()
            .and_then(|manifest| self.workspace.as_ref()?.state.task(&manifest.task_id));
        let requires_repair = task.is_some_and(crate::task_ui::task_requires_service_repair);
        let can_retry = task.is_some_and(|task| {
            task.handled_by.is_none()
                && !matches!(
                    task.state,
                    workspace::TaskState::Running
                        | workspace::TaskState::Pausing
                        | workspace::TaskState::Queued
                        | workspace::TaskState::Uncertain
                )
                && !task
                    .events
                    .blocked
                    .iter()
                    .any(|blocked| blocked.is_uncertain())
                && task.artifact.as_ref() == Some(&preview.course.dir)
        });
        let id = task.map(|task| task.id.clone());
        let components = preview
            .processing_issues
            .iter()
            .filter_map(|issue| match issue.stage {
                notes::ProcessingStage::Screenshots => Some("screenshots".to_owned()),
                notes::ProcessingStage::Proofreading if !requires_repair => {
                    Some("proofreading".to_owned())
                }
                notes::ProcessingStage::Summary if !requires_repair => Some("summary".to_owned()),
                _ => None,
            })
            .collect::<Vec<_>>();
        let needs_service = preview.processing_issues.iter().any(|issue| {
            matches!(
                issue.stage,
                notes::ProcessingStage::Proofreading
                    | notes::ProcessingStage::Translation
                    | notes::ProcessingStage::Summary
            )
        });
        let mut actions = h_flex().w_full().gap(rem(8.)).flex_wrap();
        if let Some(id) = id {
            if can_retry && !components.is_empty() {
                let retry = id.clone();
                let label = crate::task_ui::retry_components_label(&components);
                actions = actions.child(
                    material_button("reader-retry-incomplete", ButtonVariant::Outlined)
                        .icon(symbols::refresh())
                        .child(label)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.save_reading_position(cx);
                            this.reprocess_task(
                                retry.clone(),
                                components
                                    .iter()
                                    .filter_map(|component| {
                                        course2md::execution::Component::parse(component)
                                    })
                                    .collect(),
                                Vec::new(),
                                cx,
                            );
                        })),
                );
            }
            if can_retry && needs_service {
                let repair = id.clone();
                actions = actions.child(
                    material_button("reader-repair-ai", ButtonVariant::Outlined)
                        .icon(symbols::settings())
                        .child(course2md::tr!("检查 AI 服务后重试"))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.save_reading_position(cx);
                            this.repair_task_service(repair.clone(), window, cx);
                        })),
                );
            }
            actions = actions.child(
                material_button("reader-view-incomplete-task", ButtonVariant::Text)
                    .icon(symbols::task())
                    .child(course2md::tr!("查看任务"))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.save_reading_position(cx);
                        this.select_task(&id, cx);
                        this.navigate(Page::Task, cx);
                        cx.notify();
                    })),
            );
        }
        let open = self.reader_ui.processing_details_open;
        let details = preview
            .processing_issues
            .iter()
            .enumerate()
            .filter_map(|(index, issue)| {
                let detail = issue.detail()?.to_owned();
                Some(
                    v_flex()
                        .gap(rem(4.))
                        .child(
                            accessible_text(
                                ("reader-processing-label", index),
                                issue.stage.label(),
                            )
                            .text_size(TEXT_BODY)
                            .font_weight(FontWeight::MEDIUM),
                        )
                        .child(
                            accessible_text(("reader-processing-detail", index), detail)
                                .text_size(TEXT_BODY)
                                .text_color(color(ON_SURFACE_VARIANT))
                                .whitespace_normal(),
                        ),
                )
            });
        let details: Vec<_> = details.collect();
        if !details.is_empty() {
            actions = actions.child(
                material_button("reader-processing-details", ButtonVariant::Text)
                    .icon(symbols::info())
                    .child(if open {
                        course2md::tr!("收起处理详情")
                    } else {
                        course2md::tr!("查看处理详情")
                    })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.reader_ui.processing_details_open =
                            !this.reader_ui.processing_details_open;
                        cx.notify();
                    })),
            );
        }
        motion::enter(
            "reader-processing-notice",
            notice_banner("reader-incomplete-processing", NoticeKind::Danger, notice)
                .child(actions)
                .child(motion::disclosure(
                    "reader-processing-detail-panel",
                    open,
                    v_flex().w_full().gap(rem(8.)).children(details),
                )),
        )
    }

    /// The tools in the title bar: find, the video, copy and export, the rest. Narrow
    /// windows keep their icons; short ones also switch views here.
    fn reader_tools(
        &mut self,
        preview: &Rc<notes::Preview>,
        metrics: ReaderMetrics,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let icon_only = is_compact(window);
        let reading_note = self.result_tab == 0;
        let shortcut = if cfg!(target_os = "macos") {
            "⌘F"
        } else {
            "Ctrl+F"
        };
        let find_label = course2md::tr!("查找笔记内容");
        let view_toggle = metrics.short.then(|| {
            let (icon, label) = if reading_note {
                (symbols::image(), course2md::tr!("截图"))
            } else {
                (symbols::article(), course2md::tr!("笔记"))
            };
            tool(
                IconButton::new("reader-view-toggle", icon, label).on_click(cx.listener(
                    move |this, _, window, cx| {
                        this.select_reader_view(usize::from(reading_note), window, cx)
                    },
                )),
                "reader-view-toggle-tip",
                label,
            )
        });
        let toc_toggle = if metrics.short {
            self.reader_toc_toggle(metrics, cx)
        } else {
            None
        };
        let find = reading_note.then(|| {
            tool(
                IconButton::new("reader-open-find", symbols::search(), find_label)
                    .toggle(true)
                    .selected(self.reader_ui.find.open)
                    .on_click(cx.listener(|this, _, window, cx| {
                        if this.reader_ui.find.open {
                            this.close_reader_find(window, cx);
                        } else {
                            this.open_reader_find(window, cx);
                        }
                    })),
                "reader-open-find-tip",
                format!("{find_label} {shortcut}"),
            )
        });
        // So short a window gives the video's place to its views; it moves to the menu.
        let play = if metrics.short {
            None
        } else {
            self.reader_play_button(icon_only, cx)
        };
        let export = self.reader_export_menu(preview, icon_only, window, cx);
        let more = self.reader_more_menu(preview, metrics.short, window, cx);
        title_bar_control(
            h_flex()
                .flex_shrink_0()
                .gap(rem(8.))
                .items_center()
                .children(view_toggle)
                .children(toc_toggle)
                .children(find)
                .children(play)
                .child(export)
                .child(more),
        )
        .into_any_element()
    }

    /// Whether there is a video to watch: the kept offline copy, the source page, or the
    /// local file while it is still there.
    fn reader_can_play(&self) -> bool {
        matches!(self.reader_ui.offline_video, OfflineVideo::Available(_))
            || match &self.reader_ui.source {
                Some(nav::SourceTarget::Web(_)) => true,
                Some(nav::SourceTarget::Local(_)) => self.reader_ui.source_available,
                None => false,
            }
    }

    /// Watches the video, the kept offline copy first.
    fn play_reader_video(&mut self, cx: &mut Context<Self>) {
        if matches!(self.reader_ui.offline_video, OfflineVideo::Available(_)) {
            self.play_reader_offline_video(cx);
        } else {
            self.open_reader_source(cx);
        }
    }

    /// The way to watch the video.
    fn reader_play_button(&self, icon_only: bool, cx: &mut Context<Self>) -> Option<AnyElement> {
        let label = course2md::tr!("播放原视频");
        let opening = self.reader_ui.offline_opening;
        if !self.reader_can_play() {
            return None;
        }
        let play = cx.listener(|this: &mut Self, _: &ClickEvent, _, cx| this.play_reader_video(cx));
        Some(if icon_only {
            tool(
                IconButton::new("reader-play", symbols::play_arrow(), label)
                    .disabled(opening)
                    .on_click(play),
                "reader-play-tip",
                label,
            )
            .into_any_element()
        } else {
            material_button("reader-play", ButtonVariant::Outlined)
                .icon(symbols::play_arrow())
                .child(label)
                .loading(opening, label)
                .disabled(opening)
                .on_click(play)
                .into_any_element()
        })
    }

    /// Copying or exporting the whole note.
    fn reader_export_menu(
        &self,
        preview: &Rc<notes::Preview>,
        icon_only: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let exporting = self.reader_exporting();
        let current = self
            .reader_ui
            .export_state
            .pending
            .as_ref()
            .is_some_and(|(_, source)| *source == preview.course.dir);
        let version = preview.course.dir.clone();
        let mut items = Vec::new();
        if let Some(folder) = self.reader_ui.export_folder.clone() {
            items.push(
                MaterialMenuItem::new(course2md::tr!("打开导出文件夹"))
                    .leading(Icon::from(symbols::folder_open()))
                    .divider(true)
                    .on_click(move |_, _, cx| {
                        if let Ok(url) = url::Url::from_directory_path(&folder) {
                            cx.open_url(url.as_str());
                        }
                    }),
            );
        }
        for (markdown, label, icon, copied) in [
            (
                true,
                course2md::tr!("复制整份笔记 · Markdown 文本"),
                symbols::content_copy(),
                course2md::tr!("已复制 Markdown 文本"),
            ),
            (
                false,
                course2md::tr!("复制整份笔记 · 纯文本"),
                symbols::notes(),
                course2md::tr!("已复制纯文本"),
            ),
        ] {
            let source = preview.clone();
            items.push(
                MaterialMenuItem::new(label)
                    .leading(Icon::from(icon))
                    .divider(!markdown)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let text = if markdown {
                            &source.markdown_text
                        } else {
                            &source.plain_text
                        };
                        cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
                        this.message = Some(copied.into());
                        cx.notify();
                    })),
            );
        }
        for (format, label, icon) in [
            (
                course2md::config::OutputFormat::Md,
                course2md::tr!("导出 Markdown（含截图，.zip）…"),
                symbols::download(),
            ),
            (
                course2md::config::OutputFormat::Html,
                course2md::tr!("导出网页文件…"),
                symbols::language(),
            ),
            (
                course2md::config::OutputFormat::Json,
                course2md::tr!("导出 JSON 数据…"),
                symbols::data_object(),
            ),
        ] {
            let version = version.clone();
            items.push(
                MaterialMenuItem::new(label)
                    .leading(Icon::from(icon))
                    .disabled(exporting)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if let Some(course) = this.note_at(&version) {
                            this.export_course(course, format, window, cx);
                        }
                    })),
            );
        }
        let label = course2md::tr!("复制与导出");
        let trigger: AnyElement = if icon_only {
            tool(
                IconButton::new("export-note", symbols::content_copy(), label)
                    .variant(IconButtonVariant::Tonal)
                    .disabled(exporting),
                "export-note-tip",
                label,
            )
            .into_any_element()
        } else {
            material_button("export-note", ButtonVariant::Tonal)
                .icon(symbols::content_copy())
                .child(if current {
                    course2md::tr!("正在导出…")
                } else {
                    label
                })
                .loading(current, label)
                .disabled(exporting)
                .into_any_element()
        };
        ui::menu(
            "export-note-menu",
            trigger,
            MaterialMenu::new("export-note-items")
                .width(360.)
                .items(items),
            window,
            cx,
        )
        .anchor(Anchor::TopRight)
        .into_any_element()
    }

    /// The remaining actions on the note.
    fn reader_more_menu(
        &self,
        preview: &Rc<notes::Preview>,
        with_play: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let source = self.reader_ui.source.clone();
        let missing_local =
            matches!(source, Some(nav::SourceTarget::Local(_))) && !self.reader_ui.source_available;
        // A web source opens on its site; the title bar may be playing the kept copy.
        let source_label = match &source {
            _ if missing_local => course2md::tr!("找到原视频…").to_owned(),
            Some(nav::SourceTarget::Web(url)) => match crate::views::video_site_name(url) {
                Some(site) => course2md::tr!("在 {} 打开", site),
                None => course2md::tr!("在浏览器中打开").to_owned(),
            },
            _ => course2md::tr!("打开原视频").to_owned(),
        };
        let mut items = Vec::new();
        if with_play && self.reader_can_play() {
            items.push(
                MaterialMenuItem::new(course2md::tr!("播放原视频"))
                    .leading(Icon::from(symbols::play_arrow()))
                    .on_click(cx.listener(|this, _, _, cx| this.play_reader_video(cx))),
            );
        }
        items.push(
            MaterialMenuItem::new(course2md::tr!("笔记详情"))
                .leading(Icon::from(symbols::info()))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.reader_ui.details_open = !this.reader_ui.details_open;
                    // They open at the top of the view.
                    if this.reader_ui.details_open {
                        this.reader_list().scroll_to(ListOffset::default());
                    }
                    cx.notify();
                })),
        );
        if source.is_some() {
            items.push(
                MaterialMenuItem::new(source_label)
                    .leading(Icon::from(symbols::movie()))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if missing_local {
                            this.relocate_reader_source(window, cx);
                        } else {
                            this.open_reader_source(cx);
                        }
                    })),
            );
        }
        let dir = preview.course.dir.clone();
        items.push(
            MaterialMenuItem::new(course2md::tr!("显示笔记保存位置"))
                .leading(Icon::from(symbols::folder_open()))
                .on_click(move |_, _, cx| cx.reveal_path(&dir)),
        );
        if let Some(path) = self.reader_ui.exports.get(&preview.course.dir).cloned() {
            items.push(
                MaterialMenuItem::new(course2md::tr!("显示上次导出的文件"))
                    .leading(Icon::from(symbols::folder_open()))
                    .on_click(move |_, _, cx| cx.reveal_path(&path)),
            );
        }
        let label = course2md::tr!("更多笔记操作");
        ui::menu(
            "reader-more-menu",
            tool(
                IconButton::new("reader-more", symbols::more_vert(), label),
                "reader-more-tip",
                label,
            ),
            MaterialMenu::new("reader-more-items")
                .width(280.)
                .items(items),
            window,
            cx,
        )
        .anchor(Anchor::TopRight)
        .into_any_element()
    }

    /// Finding in the current view. The note's bar comes when asked for and shows where
    /// in the matches it is; the screenshots' stays and narrows them.
    fn reader_find_bar(&mut self, cx: &mut Context<Self>) -> Option<Div> {
        let reading_note = self.result_tab == 0;
        if (reading_note && !self.reader_ui.find.open)
            || (!reading_note && self.reader_ui.frames.is_empty())
        {
            return None;
        }
        let input = self.reader_find_input();
        let searching = !input.read(cx).value().trim().is_empty();
        let count = if reading_note {
            self.reader_ui.find.matches.len()
        } else {
            self.reader_ui.gallery.matches.len()
        };
        let status = searching.then(|| {
            accessible_text(
                "find-status",
                if reading_note && count > 0 {
                    format!("{}/{count}", self.reader_ui.find.current + 1)
                } else if count == 0 {
                    // Note and screenshot searches word their empty results the same way.
                    course2md::tr!("无匹配").into()
                } else {
                    course2md::tr!("{} 张", count)
                },
            )
            .role(Role::Status)
            .flex_shrink_0()
            .text_size(TEXT_AUX)
            .text_color(color(ON_SURFACE_VARIANT))
        });
        let step = |id: &'static str,
                    label: &'static str,
                    icon: SymbolIcon,
                    delta: isize,
                    cx: &mut Context<Self>| {
            tool(
                IconButton::new(id, icon, label)
                    .disabled(count == 0)
                    .on_click(cx.listener(move |this, _, _, cx| this.move_reader_match(delta, cx))),
                SharedString::from(format!("{id}-tip")),
                label,
            )
        };
        Some(
            h_flex()
                .w_full()
                .min_w_0()
                .flex_shrink_0()
                .flex_wrap()
                .gap(rem(8.))
                .items_center()
                .child(
                    div()
                        .flex_1()
                        .flex_basis(rem(200.))
                        .min_w_0()
                        .max_w(SEARCH_MAX_WIDTH)
                        .child(SearchField::new(&input).leading(Icon::from(symbols::search()))),
                )
                .children(status)
                .when(reading_note && searching, |bar| {
                    bar.child(step(
                        "previous-match",
                        course2md::tr!("上一处"),
                        symbols::arrow_upward(),
                        -1,
                        cx,
                    ))
                    .child(step(
                        "next-match",
                        course2md::tr!("下一处"),
                        symbols::arrow_downward(),
                        1,
                        cx,
                    ))
                })
                .when(reading_note, |bar| {
                    let label = course2md::tr!("关闭");
                    bar.child(tool(
                        IconButton::new("close-find", symbols::close(), label).on_click(
                            cx.listener(|this, _, window, cx| this.close_reader_find(window, cx)),
                        ),
                        "close-find-tip",
                        label,
                    ))
                }),
        )
    }

    /// The screenshot tab: a virtualized list whose first item carries the note's opening
    /// and the gallery's state, followed by rows of cards.
    fn reader_gallery(
        &mut self,
        preview: &Rc<notes::Preview>,
        metrics: ReaderMetrics,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let rem_size = window.rem_size();
        let gap = f32::from(GALLERY_GAP.to_pixels(rem_size));
        // Each screenshot needs enough width for both the slide and a readable excerpt.
        let column = f32::from(GALLERY_MIN_COLUMN.to_pixels(rem_size));
        let columns = ((metrics.content_width + gap) / (column + gap))
            .floor()
            .clamp(1., 6.) as usize;
        if columns != self.reader_ui.gallery.columns {
            self.set_gallery_items(columns);
        }
        // Cards draw a thumbnail decoded at their own device width.
        let card_width = (metrics.content_width - gap * (columns as f32 - 1.)) / columns as f32;
        self.reader_ui
            .gallery
            .line_width
            .store(card_width as u32, Ordering::Relaxed);
        let thumbnail = Size::thumbnail(card_width * metrics.scale_factor);
        let items = self.reader_ui.gallery.items.clone();
        let matches = self.reader_ui.gallery.matches.clone();
        let preview = preview.clone();
        let desktop = cx.weak_entity();
        let gallery = list(
            self.reader_ui.gallery.list.clone(),
            move |index, window, cx| {
                let Some(item) = items.get(index) else {
                    return div().into_any_element();
                };
                let last = index + 1 == items.len();
                desktop
                    .update(cx, |this, cx| {
                        let content = match item {
                            GalleryItem::Header => {
                                this.gallery_header(&preview, window, cx).into_any_element()
                            }
                            GalleryItem::Row(row) => div()
                                .grid()
                                .grid_cols(columns as u16)
                                .w_full()
                                .min_w_0()
                                .gap(GALLERY_GAP)
                                .children(
                                    matches[row.clone()]
                                        .iter()
                                        .map(|&frame| this.gallery_card(frame, thumbnail, cx)),
                                )
                                .into_any_element(),
                        };
                        // List measurements include padding, not margins.
                        div()
                            .w_full()
                            .min_w_0()
                            .flex_shrink_0()
                            .when(!last, |row| row.pb(GALLERY_GAP))
                            .child(content)
                            .into_any_element()
                    })
                    .unwrap_or_else(|_| div().into_any_element())
            },
        );
        div()
            .id("note-reader")
            .role(Role::Document)
            .relative()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .w_full()
            .child(gallery.size_full())
            .child(
                div().absolute().inset_0().child(
                    Scrollbar::vertical(&self.reader_ui.gallery.list).viewport_from_layout(),
                ),
            )
            .into_any_element()
    }

    /// The gallery's first item: the note's opening, then what it shows instead of
    /// screenshots when it has none to show.
    fn gallery_header(
        &mut self,
        preview: &Rc<notes::Preview>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let head = self.reader_head(preview, window, cx);
        let panel = || {
            v_flex()
                .w_full()
                .py(rem(48.))
                .px(rem(24.))
                .gap(rem(12.))
                .items_center()
                .rounded(RADIUS_HERO)
                .bg(color(SURFACE))
        };
        let state = if self.reader_ui.data_loading && self.reader_ui.frames.is_empty() {
            let label = course2md::tr!("正在读取截图…");
            Some(
                panel()
                    .child(spinner("reader-images-spinner", label, cx))
                    .child(accessible_text("loading-images", label).role(Role::Status)),
            )
        } else if self.reader_ui.frames.is_empty() {
            let failed = preview
                .processing_issues
                .iter()
                .any(|issue| matches!(issue.stage, notes::ProcessingStage::Screenshots));
            Some(
                panel()
                    .child(Icon::from(symbols::image()).color(color(ON_SURFACE_VARIANT)))
                    .child(
                        accessible_text("no-note-images", course2md::tr!("这份笔记没有截图"))
                            .text_size(TEXT_TITLE),
                    )
                    .when(failed, |view| {
                        view.child(
                            accessible_text(
                                "no-note-images-reason",
                                course2md::tr!("截图生成未完成"),
                            )
                            .text_color(color(ON_SURFACE_VARIANT)),
                        )
                    })
                    .child(
                        material_button("empty-images-to-note", ButtonVariant::Outlined)
                            .icon(symbols::article())
                            .child(course2md::tr!("阅读笔记"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.select_reader_view(0, window, cx)
                            })),
                    ),
            )
        } else if self.reader_ui.gallery.matches.is_empty() {
            Some(
                panel()
                    .child(accessible_text(
                        "gallery-no-matches",
                        course2md::tr!("没有对应的截图"),
                    ))
                    .child(
                        material_button("clear-gallery-search", ButtonVariant::Text)
                            .icon(symbols::close())
                            .child(course2md::tr!("清除搜索"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.reader_ui
                                    .gallery
                                    .find
                                    .update(cx, |input, cx| input.set_value("", window, cx));
                            })),
                    ),
            )
        } else {
            None
        };
        v_flex()
            .w_full()
            .min_w_0()
            .gap(rem(12.))
            .child(head)
            .children(state)
    }

    /// One screenshot card: pressing it opens the viewer; below the image are its time, a
    /// jump to the note, and a three-line excerpt of what was said.
    fn gallery_card(
        &mut self,
        index: usize,
        thumbnail: Size,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let frames = self.reader_ui.frames.clone();
        let frame = &frames[index];
        let loaded = frame
            .image
            .as_ref()
            .map(|file| self.reader_ui.images.get(&file.path, thumbnail, cx));
        let openable = matches!(loaded, Some(Loaded::Pending | Loaded::Ready(..)));
        let excerpt = self
            .reader_ui
            .gallery
            .excerpts
            .get(&index)
            .cloned()
            .unwrap_or_default();
        let to_note = course2md::tr!("在笔记中查看");
        let card = MaterialCard::new(
            SharedString::from(frame.anchor.clone()),
            CardVariant::Filled,
        )
        .accessibility_label(course2md::tr!("放大{}", frame_label(frame)))
        .w_full()
        .h_full()
        .min_w_0()
        .overflow_hidden()
        .child(screenshot_slot(("screenshot", index), loaded))
        .child(
            v_flex()
                .w_full()
                .min_w_0()
                .gap(rem(4.))
                .p(rem(12.))
                .child(
                    h_flex()
                        .w_full()
                        .items_center()
                        .gap(rem(8.))
                        .child(
                            accessible_text(
                                ("frame-time", index),
                                course2md::render::fmt_ts(frame.seconds),
                            )
                            .flex_1()
                            .text_size(TEXT_BODY)
                            .font_weight(FontWeight::MEDIUM),
                        )
                        .when_some(frame.body_index, |row, block| {
                            row.child(tool(
                                IconButton::new(
                                    ("frame-to-body", index),
                                    symbols::article(),
                                    to_note,
                                )
                                .button_size(ButtonSize::ExtraSmall)
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        // The card under the button opens the viewer.
                                        cx.stop_propagation();
                                        this.jump_reader_block(block, cx)
                                    },
                                )),
                                ("frame-to-body-tip", index),
                                to_note,
                            ))
                        }),
                )
                .child(
                    div()
                        .w_full()
                        .min_w_0()
                        .text_size(TEXT_BODY)
                        .line_height(relative(1.5))
                        .text_color(color(ON_SURFACE_VARIANT))
                        .child(
                            TextView::markdown(("frame-excerpt", index), excerpt)
                                .markdown_extensions(self.reader_ui.gallery.extensions.clone())
                                .style(note_text::note_style(cx))
                                .selectable(false)
                                .max_lines(3),
                        ),
                ),
        );
        if openable {
            card.on_click(
                cx.listener(move |this, _, window, cx| this.open_reader_image(index, window, cx)),
            )
            .into_any_element()
        } else {
            card.into_any_element()
        }
    }

    pub(crate) fn open_reader_image(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.preview.is_none()
            || self.reader_viewer_open()
            || self
                .reader_ui
                .frames
                .get(index)
                .is_none_or(|frame| frame.image.is_none())
        {
            return;
        }
        let filtered = self.result_tab == 1
            && !self
                .reader_ui
                .gallery
                .find
                .read(cx)
                .value()
                .trim()
                .is_empty();
        let visible: Vec<usize> = if self.result_tab == 1 {
            self.reader_ui.gallery.matches.to_vec()
        } else {
            (0..self.reader_ui.frames.len()).collect()
        };
        let Some(viewer_index) = visible.iter().position(|candidate| *candidate == index) else {
            return;
        };
        crate::import_ui::ConversionFollow::interrupt_for_reader_viewer(
            &mut self.following_conversion,
        );
        self.reader_ui.viewer = Some(ImageViewer {
            open: true,
            focused: false,
            text_scroll: ScrollHandle::new(),
            viewport_scroll: ScrollHandle::new(),
            viewport_size: size(px(0.), px(0.)),
            frames: visible
                .iter()
                .map(|index| self.reader_ui.frames[*index].clone())
                .collect(),
            filtered,
            index: viewer_index,
            zoom: None,
            drag: None,
            text_open: None,
            focus: cx.focus_handle(),
        });
        let _ = window;
        cx.notify();
    }
    /// A screenshot finished decoding: repaint what draws screenshots.
    pub(crate) fn reader_image_loaded(&mut self, cx: &mut Context<Self>) {
        if self.page == Page::Result {
            cx.notify();
        }
    }
    /// Leaving the reader releases its decoded screenshots.
    pub(crate) fn release_reader_images(&mut self, cx: &mut Context<Self>) {
        self.reader_ui.viewer = None;
        self.reader_ui.images.clear(cx);
    }
    pub(crate) fn close_reader_image(&mut self, cx: &mut Context<Self>) {
        if let Some(viewer) = &mut self.reader_ui.viewer {
            viewer.open = false;
        }
        cx.notify();
    }
    fn move_reader_image(&mut self, delta: isize, cx: &mut Context<Self>) {
        if let Some(viewer) = &mut self.reader_ui.viewer {
            viewer.index = (viewer.index as isize + delta)
                .clamp(0, viewer.frames.len().saturating_sub(1) as isize)
                as usize;
            viewer.zoom = None;
            viewer.text_scroll.set_offset(point(px(0.), px(0.)));
            viewer.viewport_scroll.set_offset(point(px(0.), px(0.)));
            cx.notify();
        }
    }
    /// Moves the enlarged image with a held pointer, within its edges.
    fn pan_reader_image(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        let Some(viewer) = &mut self.reader_ui.viewer else {
            return;
        };
        let Some((start, offset)) = viewer.drag else {
            return;
        };
        if event.pressed_button != Some(MouseButton::Left) {
            // The button was released where no event reached the window.
            self.end_reader_image_pan(cx);
            return;
        }
        let max = viewer.viewport_scroll.max_offset();
        let moved = offset + (event.position - start);
        viewer.viewport_scroll.set_offset(point(
            moved.x.clamp(-max.x, px(0.)),
            moved.y.clamp(-max.y, px(0.)),
        ));
        cx.notify();
    }
    fn end_reader_image_pan(&mut self, cx: &mut Context<Self>) {
        if let Some(viewer) = &mut self.reader_ui.viewer
            && viewer.drag.take().is_some()
        {
            cx.notify();
        }
    }
    fn image_fit(viewer: &ImageViewer) -> f32 {
        let Some(image) = &viewer.frames[viewer.index].image else {
            return 0.;
        };
        // The first prepaint records the space that flex layout actually leaves
        // for media after the toolbar, transcript and naturally sized footer.
        // Until then, do not guess a fit from a presumed footer height.
        let width = f32::from(viewer.viewport_size.width);
        let height = f32::from(viewer.viewport_size.height);
        if width <= 0. || height <= 0. {
            return 0.;
        }
        (width / image.width as f32)
            .min(height / image.height as f32)
            .min(1.)
    }
    /// So short a window shows the image or its text, not both.
    fn viewer_is_short(window: &Window) -> bool {
        window.bounds().size.height < rem(420.).to_pixels(window.rem_size())
    }
    /// Text is shown by default when the window has room for it.
    fn viewer_text_open(viewer: &ImageViewer, window: &Window) -> bool {
        viewer
            .text_open
            .unwrap_or(window.bounds().size.height >= rem(448.).to_pixels(window.rem_size()))
    }
    fn zoom_reader_image(&mut self, factor: Option<f32>, window: &Window, cx: &mut Context<Self>) {
        let Some(viewer) = &mut self.reader_ui.viewer else {
            return;
        };
        if Self::viewer_is_short(window) && Self::viewer_text_open(viewer, window) {
            return;
        }
        let Some(image) = viewer.frames[viewer.index].image.clone() else {
            return;
        };
        let old_scale = viewer.zoom.unwrap_or_else(|| Self::image_fit(viewer));
        viewer.zoom = factor.map(|factor| zoomed(old_scale, factor));
        let new_scale = viewer.zoom.unwrap_or_else(|| Self::image_fit(viewer));
        let viewport = viewer.viewport_scroll.bounds().size;
        let offset = viewer.viewport_scroll.offset();
        viewer.viewport_scroll.set_offset(point(
            px(image_zoom_offset(
                f32::from(viewport.width),
                image.width as f32 * old_scale,
                image.width as f32 * new_scale,
                f32::from(offset.x),
            )),
            px(image_zoom_offset(
                f32::from(viewport.height),
                image.height as f32 * old_scale,
                image.height as f32 * new_scale,
                f32::from(offset.y),
            )),
        ));
        cx.notify();
    }
    /// Scrolls the screenshot's text, when it is shown, by `lines` or `pages`.
    fn scroll_image_text(
        &mut self,
        lines: f32,
        pages: f32,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let Some(viewer) = &self.reader_ui.viewer else {
            return;
        };
        let line = f32::from(TEXT_READER.to_pixels(window.rem_size())) * 1.8;
        let page = f32::from(viewer.text_scroll.bounds().size.height) * 0.85;
        let offset = viewer.text_scroll.offset();
        let max = f32::from(viewer.text_scroll.max_offset().y);
        let moved = image_text_offset(f32::from(offset.y), max, lines * line + pages * page);
        if moved != f32::from(offset.y) {
            viewer.text_scroll.set_offset(point(offset.x, px(moved)));
            cx.notify();
        }
    }

    /// The screenshot viewer: a full-screen dialog holding the zoomable image, its text
    /// and what to do with it.
    fn reader_viewer(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let viewer = self.reader_ui.viewer.as_ref()?;
        let open = viewer.open;
        let frame = &viewer.frames[viewer.index];
        let label = frame_label(frame);
        let time = course2md::render::fmt_ts(frame.seconds);
        let text_open = Self::viewer_text_open(viewer, window);
        let text_only = Self::viewer_is_short(window) && text_open;
        let headline = if text_only {
            course2md::tr!("文字 · {}", time)
        } else {
            course2md::tr!("截图 · {}", time)
        };
        let has_text = !frame.transcript.trim().is_empty();
        let media = (!text_only).then(|| self.reader_viewer_media(label.clone(), cx));
        let text = (text_only || (has_text && text_open))
            .then(|| self.reader_viewer_text(text_only, window));
        let controls = self.reader_viewer_controls(window, cx);
        let viewer = self.reader_ui.viewer.as_mut()?;
        let focus = viewer.focus.clone();
        // The dialog takes focus as this frame draws it; its keys live on the content.
        if open && !std::mem::replace(&mut viewer.focused, true) {
            let focus = focus.clone();
            cx.on_next_frame(window, move |_, window, cx| focus.focus(window, cx));
        }
        let content = v_flex()
            .id("reader-image-dialog")
            .key_context("ReaderImage")
            .track_focus(&focus)
            .aria_label(if text_only {
                course2md::tr!("阅读{}对应文字", label)
            } else {
                course2md::tr!("查看{}", label)
            })
            .on_action(cx.listener(|this, _: &PreviousImage, _, cx| this.move_reader_image(-1, cx)))
            .on_action(cx.listener(|this, _: &NextImage, _, cx| this.move_reader_image(1, cx)))
            .on_action(cx.listener(|this, _: &ZoomIn, window, cx| {
                this.zoom_reader_image(Some(ZOOM_STEP_IN), window, cx)
            }))
            .on_action(cx.listener(|this, _: &ZoomOut, window, cx| {
                this.zoom_reader_image(Some(ZOOM_STEP_OUT), window, cx)
            }))
            .on_action(cx.listener(|this, _: &FitImage, window, cx| {
                this.zoom_reader_image(None, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ImageTextUp, window, cx| {
                this.scroll_image_text(-3., 0., window, cx)
            }))
            .on_action(cx.listener(|this, _: &ImageTextDown, window, cx| {
                this.scroll_image_text(3., 0., window, cx)
            }))
            .on_action(cx.listener(|this, _: &ReaderPageUp, window, cx| {
                this.scroll_image_text(0., -1., window, cx)
            }))
            .on_action(cx.listener(|this, _: &ReaderPageDown, window, cx| {
                this.scroll_image_text(0., 1., window, cx)
            }))
            .on_action(cx.listener(|this, _: &ReaderStart, window, cx| {
                this.scroll_image_text(0., f32::NEG_INFINITY, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ReaderEnd, window, cx| {
                this.scroll_image_text(0., f32::INFINITY, window, cx)
            }))
            .flex_1()
            .min_h_0()
            .w_full()
            .gap(rem(8.))
            .pb(rem(12.))
            .children(media)
            .children(text)
            .child(controls);
        let dismiss = cx.weak_entity();
        Some(
            MaterialDialog::new("reader-image-viewer")
                .full_screen(course2md::tr!("关闭截图"), None)
                .headline(headline)
                .content(content)
                .modal(open)
                .on_dismiss(move |_, cx| {
                    let _ = dismiss.update(cx, |this, cx| this.close_reader_image(cx));
                })
                .into_any_element(),
        )
    }

    /// The transcript: selectable note text in its own scroll region.
    fn reader_viewer_text(&self, text_only: bool, window: &Window) -> Stateful<Div> {
        let reader = &self.reader_ui;
        let viewer = reader.viewer.as_ref().expect("the viewer draws its text");
        reader.viewer_line.store(
            f32::from(window.bounds().size.width) as u32,
            Ordering::Relaxed,
        );
        let frame = &viewer.frames[viewer.index];
        div()
            .id("image-details")
            .relative()
            .w_full()
            .min_w_0()
            .min_h_0()
            .when(text_only, |view| view.flex_1())
            .when(!text_only, |view| view.flex_shrink_0().max_h(relative(0.4)))
            .pr(rem(12.))
            .overflow_y_scroll()
            // A sideways swipe scrolls a wide formula, not the text.
            .restrict_scroll_to_axis()
            .track_scroll(&viewer.text_scroll)
            .text_size(TEXT_READER)
            .line_height(relative(1.8))
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .child(Scrollbar::vertical(&viewer.text_scroll)),
            )
            .child(if frame.transcript.trim().is_empty() {
                accessible_text("image-no-text", course2md::tr!("这张截图没有对应文字"))
                    .whitespace_normal()
                    .into_any_element()
            } else {
                TextView::markdown(
                    ("image-transcript", viewer.index),
                    note_text::markdown(&frame.transcript),
                )
                .markdown_extensions(reader.viewer_text.clone())
                .selection_format(SelectionFormat::Source)
                .into_any_element()
            })
    }

    /// The viewer's row of actions: stepping, zoom, then what leads away from it. It wraps
    /// on a narrow window, and icons name themselves in tooltips.
    fn reader_viewer_controls(&mut self, window: &Window, cx: &mut Context<Self>) -> Stateful<Div> {
        let viewer = self
            .reader_ui
            .viewer
            .as_ref()
            .expect("the viewer draws its controls");
        let (index, count) = (viewer.index, viewer.frames.len());
        let frame = &viewer.frames[index];
        let has_text = !frame.transcript.trim().is_empty();
        let text_open = Self::viewer_text_open(viewer, window);
        let short = Self::viewer_is_short(window);
        let text_only = short && text_open;
        let scale = viewer.zoom.unwrap_or_else(|| Self::image_fit(viewer));
        // A file that exists but cannot be decoded is as unreadable as a missing one.
        let readable = frame
            .image
            .as_ref()
            .is_some_and(|image| !self.reader_ui.images.failed(&image.path, Size::Original));
        let number = if viewer.filtered {
            course2md::tr!("匹配截图 {} / {}", index + 1, count)
        } else {
            format!("{} / {count}", index + 1)
        };
        let body_index = frame.body_index;
        let source = self.reader_ui.source.clone();
        let source_available = self.reader_ui.source_available;
        let source_link = source
            .as_ref()
            .and_then(|source| nav::seek_url(source, frame.seconds));
        let group = || h_flex().flex_shrink_0().gap(rem(4.)).items_center();
        let stepping = group()
            .child(viewer_tool(
                "image-previous",
                symbols::chevron_left(),
                course2md::tr!("上一张"),
                course2md::tr!("上一张 · ←"),
                index == 0,
                cx.listener(|this, _, _, cx| this.move_reader_image(-1, cx)),
            ))
            .child(
                accessible_text("image-number", number)
                    .whitespace_nowrap()
                    .text_size(TEXT_BODY),
            )
            .child(viewer_tool(
                "image-next",
                symbols::chevron_right(),
                course2md::tr!("下一张"),
                course2md::tr!("下一张 · →"),
                index + 1 >= count,
                cx.listener(|this, _, _, cx| this.move_reader_image(1, cx)),
            ));
        // An unreadable screenshot has nothing to zoom.
        let zoom = (!text_only && readable).then(|| {
            let percent = (scale > 0.).then(|| format!("{}%", (scale * 100.).round() as u32));
            group()
                .child(viewer_tool(
                    "image-zoom-out",
                    symbols::zoom_out(),
                    course2md::tr!("缩小"),
                    course2md::tr!("缩小 · −"),
                    zoomed(scale, ZOOM_STEP_OUT) >= scale,
                    cx.listener(|this, _, window, cx| {
                        this.zoom_reader_image(Some(ZOOM_STEP_OUT), window, cx)
                    }),
                ))
                .child(
                    accessible_text("image-scale", percent.unwrap_or_else(|| "—".into()))
                        .whitespace_nowrap()
                        .text_size(TEXT_BODY),
                )
                .child(viewer_tool(
                    "image-zoom-in",
                    symbols::zoom_in(),
                    course2md::tr!("放大"),
                    course2md::tr!("放大 · +"),
                    zoomed(scale, ZOOM_STEP_IN) <= scale,
                    cx.listener(|this, _, window, cx| {
                        this.zoom_reader_image(Some(ZOOM_STEP_IN), window, cx)
                    }),
                ))
                .child(viewer_tool(
                    "image-fit",
                    symbols::fit_screen(),
                    course2md::tr!("适合窗口"),
                    course2md::tr!("适合窗口 · 0"),
                    false,
                    cx.listener(|this, _, window, cx| this.zoom_reader_image(None, window, cx)),
                ))
        });
        let action = |id: &'static str, icon: SymbolIcon, label: &'static str| -> MaterialButton {
            material_button(id, ButtonVariant::Text)
                .icon(icon)
                .child(label)
        };
        let to_note = body_index.map(|block| {
            action(
                "image-to-body",
                symbols::article(),
                course2md::tr!("在笔记中查看"),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.close_reader_image(cx);
                this.jump_reader_block(block, cx);
            }))
        });
        let watch = match (source_link, source.filter(|_| source_available)) {
            (Some(url), _) => Some(
                action(
                    "image-to-source",
                    symbols::play_arrow(),
                    course2md::tr!("从此处观看"),
                )
                .on_click(move |_, _, cx| cx.open_url(&url)),
            ),
            (None, Some(source)) => Some(
                action(
                    "image-open-original",
                    symbols::movie(),
                    course2md::tr!("打开原视频"),
                )
                .on_click(move |_, _, cx| match &source {
                    nav::SourceTarget::Web(url) => cx.open_url(url),
                    nav::SourceTarget::Local(path) => cx.open_with_system(path),
                }),
            ),
            (None, None) => None,
        };
        let text_toggle = (has_text || text_only).then(|| {
            action(
                "image-toggle-details",
                if text_only {
                    symbols::image()
                } else {
                    symbols::article()
                },
                if text_only {
                    course2md::tr!("返回图片")
                } else if short {
                    course2md::tr!("查看文字")
                } else if text_open {
                    course2md::tr!("收起文字")
                } else {
                    course2md::tr!("显示文字")
                },
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                if let Some(viewer) = &mut this.reader_ui.viewer {
                    viewer.text_open = Some(!text_open);
                    cx.notify();
                }
            }))
        });
        h_flex()
            .id("reader-image-actions")
            .w_full()
            .min_w_0()
            .flex_shrink_0()
            .flex_wrap()
            .gap(rem(8.))
            .items_center()
            .child(stepping)
            .children(zoom)
            .child(div().flex_1())
            .children(to_note)
            .children(watch)
            .children(text_toggle)
    }

    /// The zoomable image, or a notice when its file cannot be read.
    fn reader_viewer_media(&mut self, label: String, cx: &mut Context<Self>) -> AnyElement {
        let unreadable = || {
            unreadable_screenshot("image-unreadable")
                .flex_1()
                .min_h(rem(112.))
                .w_full()
                .rounded(RADIUS_CARD)
                .into_any_element()
        };
        let Some(viewer) = &self.reader_ui.viewer else {
            return div().into_any_element();
        };
        let scale = viewer.zoom.unwrap_or_else(|| Self::image_fit(viewer));
        let Some(image) = viewer.frames[viewer.index].image.clone() else {
            return unreadable();
        };
        // The neighbours decode ahead so stepping through shows them at once.
        let neighbours: Vec<PathBuf> = [viewer.index.checked_sub(1), Some(viewer.index + 1)]
            .into_iter()
            .flatten()
            .filter_map(|index| Some(viewer.frames.get(index)?.image.as_ref()?.path.clone()))
            .collect();
        let viewport_scroll = viewer.viewport_scroll.clone();
        let owner = cx.weak_entity();
        let width = px(image.width as f32 * scale);
        let height = px(image.height as f32 * scale);
        let viewport = viewer.viewport_size;
        let pannable = width > viewport.width + px(1.) || height > viewport.height + px(1.);
        let dragging = viewer.drag.is_some();
        for path in &neighbours {
            self.reader_ui.images.get(path, Size::Original, cx);
        }
        let picture = match self.reader_ui.images.get(&image.path, Size::Original, cx) {
            Loaded::Ready(original, _) => Some(original),
            // The gallery's thumbnail stands in while the original decodes.
            Loaded::Pending => self.reader_ui.images.any_ready(&image.path),
            Loaded::Failed => return unreadable(),
        };
        let picture = match picture {
            Some(picture) => img(picture)
                .size_full()
                .object_fit(ObjectFit::Contain)
                .into_any_element(),
            None => div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .child(spinner("reader-image-loading", label.clone(), cx))
                .into_any_element(),
        };
        let Some(viewer) = &self.reader_ui.viewer else {
            return div().into_any_element();
        };
        div()
            .on_children_prepainted(move |_, window, cx| {
                let size = viewport_scroll.bounds().size;
                let owner = owner.clone();
                window.defer(cx, move |_, cx| {
                    let _ = owner.update(cx, |this, cx| {
                        if let Some(viewer) = &mut this.reader_ui.viewer
                            && viewer.viewport_size != size
                        {
                            viewer.viewport_size = size;
                            cx.notify();
                        }
                    });
                });
            })
            .id("image-viewport")
            .bg(color(INSET))
            .border_1()
            .border_color(color(OUTLINE_VARIANT))
            .rounded(RADIUS_CARD)
            .flex_1()
            .min_h(rem(112.))
            .w_full()
            .overflow_x_scroll()
            .overflow_y_scroll()
            .track_scroll(&viewer.viewport_scroll)
            // An image larger than the viewport moves with the pointer, and a
            // trackpad pinch zooms it, as in Preview.
            .when(pannable, |viewport| viewport.cursor_grab())
            .when(dragging, |viewport| viewport.cursor_grabbing())
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    if let Some(viewer) = &mut this.reader_ui.viewer {
                        let max = viewer.viewport_scroll.max_offset();
                        if max.x > px(0.) || max.y > px(0.) {
                            viewer.drag = Some((event.position, viewer.viewport_scroll.offset()));
                            // The press pans the image; it must not also start
                            // selecting the transcript below it.
                            window.prevent_default();
                            gpui_kit::component::global_state::GlobalState::suppress_text_selection(
                                cx,
                            );
                            cx.notify();
                        }
                    }
                }),
            )
            // While a pan is held, the whole window follows the pointer, so
            // leaving the image (or the dialog) neither stops nor drops it.
            .when(dragging, |viewport| {
                let owner = cx.weak_entity();
                viewport.child(
                    canvas(
                        |_, _, _| {},
                        move |_, _, window, _| {
                            let moved = owner.clone();
                            window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                                if phase == DispatchPhase::Bubble {
                                    let _ = moved
                                        .update(cx, |this, cx| this.pan_reader_image(event, cx));
                                }
                            });
                            let released = owner.clone();
                            window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
                                if phase == DispatchPhase::Bubble {
                                    let _ = released
                                        .update(cx, |this, cx| this.end_reader_image_pan(cx));
                                }
                            });
                        },
                    )
                    .absolute()
                    .size_0(),
                )
            })
            .on_pinch(cx.listener(|this, event: &PinchEvent, window, cx| {
                this.zoom_reader_image(Some(1. + event.delta), window, cx);
            }))
            .child(
                div()
                    .id("reader-image-canvas")
                    .flex_none()
                    .w(width)
                    .h(height)
                    .min_w(relative(1.))
                    .min_h(relative(1.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .id("enlarged-reader-image")
                            .flex_none()
                            .w(width)
                            .h(height)
                            .role(Role::Image)
                            .aria_label(label)
                            .child(picture),
                    ),
            )
            .into_any_element()
    }

    /// Where the reader returns to, named for its crumb in the title bar.
    pub(crate) fn reader_origin_label(&self) -> &'static str {
        match self.result_origin {
            Page::New => course2md::tr!("主页"),
            Page::Task => course2md::tr!("任务"),
            Page::Settings => course2md::tr!("设置"),
            _ => course2md::tr!("笔记库"),
        }
    }

    /// A note is read where it was opened: a tonal return to that place, the breadcrumb
    /// over the note title, then the reading tools.
    pub(crate) fn reader_title_bar(
        &self,
        bar: Div,
        tools: AnyElement,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui_kit_material::components::TitleBar {
        let title = self
            .preview
            .as_ref()
            .map(|preview| preview.course.title.clone())
            .unwrap_or_default();
        let back = course2md::tr!("返回「{}」", self.reader_origin_label());
        // The crumb names where the note lives, however it was reached; the
        // return button names the origin.
        let library = course2md::tr!("笔记库");
        let crumb = match &self.preview {
            Some(preview) => {
                let folder = self.folder_context(Some(preview.course.dir.clone()));
                match folder.load_error {
                    None => format!("{library} · {}", folder.label),
                    Some(_) => library.to_owned(),
                }
            }
            None => library.to_owned(),
        };
        let height = TITLE_BAR_HEIGHT.to_pixels(window.rem_size());
        // So short a window keeps the bar for its tools; the note opens with its title.
        let short = ReaderMetrics::new(window, false).short;
        crate::views::app_bar(height, window, cx).child(
            bar.gap(rem(12.))
                .child(title_bar_control(
                    div().flex_shrink_0().child(tool(
                        IconButton::new("reader-back", symbols::arrow_back(), back.clone())
                            .variant(IconButtonVariant::Tonal)
                            .on_click(
                                cx.listener(|this, _, _, cx| this.navigate(this.result_origin, cx)),
                            ),
                        "reader-back-tip",
                        back,
                    )),
                ))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .when(short, |title| title.invisible())
                        .child(
                            accessible_text("reader-crumb", crumb)
                                .truncate()
                                .text_size(TEXT_AUX)
                                .text_color(color(ON_SURFACE_VARIANT)),
                        )
                        .child(
                            accessible_text("reader-crumb-title", title)
                                .role(Role::Heading)
                                .truncate()
                                .text_size(TEXT_TITLE)
                                .text_color(color(ON_SURFACE)),
                        ),
                )
                .child(tools),
        )
    }
}

fn load_reader_data(preview: &notes::Preview) -> ReaderData {
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

fn existing_export_folder(dir: &std::path::Path, outputs: &[String]) -> Option<PathBuf> {
    let folder = course2md::artifact::safe_asset_path(dir, "exports").ok()?;
    let has_file = outputs.iter().any(|output| {
        std::path::Path::new(output).starts_with("exports")
            && course2md::artifact::safe_asset_path(dir, output).is_ok_and(|path| path.is_file())
    });
    has_file.then_some(folder)
}
fn processing_notice(issues: &[notes::ProcessingIssue]) -> Option<String> {
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
fn files_need_reload(preview: &notes::Preview, issues: &[String], frames: &[Frame]) -> bool {
    !preview.issues.is_empty()
        || !issues.is_empty()
        || frames.iter().any(|frame| frame.image.is_none())
}

/// The text's scroll offset (zero or negative) after moving `delta` pixels
/// further down, kept within its `max` scroll distance.
fn image_text_offset(offset: f32, max: f32, delta: f32) -> f32 {
    (offset - delta).clamp(-max.max(0.), 0.)
}

/// Preserve the image point under the viewport center while either axis grows
/// from a centered preview into scrollable content, or returns to a fitted image.
fn image_zoom_offset(viewport: f32, old_extent: f32, new_extent: f32, offset: f32) -> f32 {
    if viewport <= 0. || old_extent <= 0. {
        return 0.;
    }
    let old_margin = (viewport - old_extent).max(0.) * 0.5;
    let image_position = ((viewport * 0.5 - offset - old_margin) / old_extent).clamp(0., 1.);
    let new_margin = (viewport - new_extent).max(0.) * 0.5;
    (viewport * 0.5 - new_margin - image_position * new_extent)
        .clamp(-(new_extent - viewport).max(0.), 0.)
}

#[cfg(test)]
mod tests {
    use super::{
        ExportFeedback, ExportState, Frame, GalleryItem, NoteDocument, OfflineVideo,
        OfflineVideoRequest, PreviewBlock, block_time, existing_export_folder, files_need_reload,
        frame_excerpt, gallery_items, gallery_row_of, gallery_top_frame, image_text_offset,
        image_zoom_offset, load_reader_data, matching_frames, outline_current, processing_notice,
    };
    use crate::{notes::Course, source, workspace};
    use gpui::{AppContext as _, ListAlignment, ListOffset, ListState, TestAppContext, px};
    use std::collections::HashMap;

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
    fn a_gallery_resize_keeps_the_same_screenshot() {
        let matches: Vec<usize> = (0..7).collect();
        let items = gallery_items(matches.len(), 3);
        assert_eq!(
            items,
            [
                GalleryItem::Header,
                GalleryItem::Row(0..3),
                GalleryItem::Row(3..6),
                GalleryItem::Row(6..7),
            ]
        );
        let list = ListState::new(items.len(), ListAlignment::Top, px(1000.));
        // The header holds no screenshot.
        assert_eq!(gallery_top_frame(&items, &matches, &list), None);
        list.scroll_to(ListOffset {
            item_ix: 2,
            offset_in_item: px(50.),
        });
        assert_eq!(gallery_top_frame(&items, &matches, &list), Some(3));
        // Two columns: screenshot 3 now sits second in the row 2..4.
        let narrow = gallery_items(matches.len(), 2);
        assert_eq!(gallery_row_of(&narrow, &matches, 3), Some(2));
        assert_eq!(gallery_row_of(&narrow, &matches, 99), None);
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

    fn section(index: usize) -> PreviewBlock {
        PreviewBlock::Heading {
            text: course2md::render::fmt_ts(index as f64 * 60.).into(),
            anchor: format!("section-{index}"),
            seconds: Some(index as f64 * 60.),
        }
    }

    fn paragraph(text: &str, anchor: &str) -> PreviewBlock {
        PreviewBlock::Paragraph {
            text: text.into(),
            anchor: anchor.into(),
        }
    }

    /// A summary, a section with a figure and a formula, and a timeless heading.
    fn note() -> Vec<PreviewBlock> {
        vec![
            PreviewBlock::Heading {
                text: "摘要".into(),
                anchor: "summary".into(),
                seconds: None,
            },
            paragraph("总览 *不是强调*", "summary-tldr"),
            paragraph("第一点", "key-point-0"),
            paragraph("第二点 $x$", "key-point-1"),
            section(1),
            PreviewBlock::Image("/note/frames/a.png".into()),
            paragraph("设 $x$ 为常数\n\n第二段\n$$y$$\n后文", "paragraph-0-0"),
            PreviewBlock::Translation {
                text: "A translation".into(),
                anchor: "translation-0-0".into(),
            },
            section(2),
        ]
    }

    #[test]
    fn the_note_document_says_which_block_each_part_draws() {
        let blocks = note();
        let chapters = HashMap::from([(4, "问题由来".to_owned())]);
        let document = NoteDocument::new("标题 *", &blocks, &chapters, |at| {
            Some(format!("https://video.example/?t={at}"))
        });
        assert!(
            document
                .markdown
                .starts_with("# 标题 *\n\n![](reader:head)")
        );
        assert!(
            document
                .markdown
                .contains("## 问题由来\n\n### 01:00 [▶](https://video.example/?t=60)")
        );
        assert!(document.markdown.contains("- 第一点\n- 第二点 $x$"));
        assert!(document.markdown.contains("> A translation"));
        // A section's screenshot follows its paragraphs.
        assert!(
            document.markdown.find("后文").unwrap()
                < document.markdown.find("![](figure:5)").unwrap()
        );
        let shown: Vec<_> = document.blocks.iter().copied().collect();
        assert_eq!(
            shown,
            [
                None,
                None,
                Some(0),
                Some(1),
                Some(2),
                Some(4),
                Some(4),
                Some(6),
                Some(6),
                Some(6),
                Some(6),
                Some(7),
                Some(5),
                Some(8)
            ]
        );
        assert_eq!(document.item_of(2), Some(4));
        // A later key point is in the same list.
        assert_eq!(document.item_of(3), Some(4));
        assert_eq!(document.block_at(0), Some(0));
        assert_eq!(document.block_at(12), Some(5));
        assert_eq!(document.block_at(99), None);
    }

    /// The reader relies on this: one list item per block of [`NoteDocument`].
    #[gpui::test]
    fn the_view_parses_one_item_per_block_of_the_note_document(cx: &mut TestAppContext) {
        use gpui::{Context, IntoElement, Render, Styled as _, Window, div};
        use gpui_base::text::{TextView, TextViewState};
        struct Note(
            gpui::Entity<TextViewState>,
            gpui_base::text::MarkdownExtensions,
        );
        impl Render for Note {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                use gpui::ParentElement as _;
                div().size_full().child(
                    TextView::new(&self.0)
                        .scrollable(true)
                        .markdown_extensions(self.1.clone()),
                )
            }
        }
        cx.update(|cx| {
            gpui_kit::init(cx);
            gpui_kit_material::init(cx);
        });
        let document = NoteDocument::new("标题", &note(), &HashMap::new(), |_| None);
        let render: crate::note_text::RenderBlock =
            std::sync::Arc::new(|_, _, _| gpui::IntoElement::into_any_element(gpui::div()));
        let extensions = crate::note_text::note_extensions(Default::default(), render);
        let markdown = document.markdown.clone();
        let (view, cx) = cx.add_window_view(move |_, cx| {
            Note(
                cx.new(|cx| TextViewState::markdown(&markdown, cx)),
                extensions,
            )
        });
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.run_until_parked();
        view.read_with(cx, |view, cx| {
            let state = view.0.read(cx);
            assert_eq!(state.list_state().item_count(), document.blocks.len());
            let text = state.rendered_text();
            assert!(
                text.as_str().contains("总览 *不是强调*"),
                "{}",
                text.as_str()
            );
            assert_eq!(text.source(), document.markdown);
        });
    }

    #[test]
    fn the_contents_follow_the_last_heading_at_or_before_the_top() {
        let outline = vec![(2, "一".to_owned(), None), (6, "二".to_owned(), None)];
        assert_eq!(outline_current(&outline, 0), None);
        assert_eq!(outline_current(&outline, 2), Some(2));
        assert_eq!(outline_current(&outline, 5), Some(2));
        assert_eq!(outline_current(&outline, 9), Some(6));
    }
}
