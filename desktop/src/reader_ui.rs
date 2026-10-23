//! Reading navigation is bound to one immutable note version, independently of exports.
use super::*;
use crate::{notes::PreviewBlock, reader_navigation as nav, theme::*};
use gpui_component::{
    button::*,
    menu::{DropdownMenu, PopupMenu, PopupMenuItem},
    scroll::{Scrollbar, ScrollbarMode},
};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    ops::Range,
    rc::Rc,
    sync::{Arc, atomic::AtomicBool},
};

/// The article's width: two section columns, or one column of reading copy.
const READER_MEASURE: Rems = rems(1100. / 14.);
/// Reading copy is set in a serif, as on the canvas; the platform's CJK serif.
const READER_SERIF: &str = if cfg!(target_os = "macos") {
    "Songti SC"
} else {
    "Noto Serif CJK SC"
};
/// Space kept between scrolled text and an always-visible scrollbar.
const SCROLLBAR_GUTTER: Pixels = px(14.);
/// The icon of an empty state.
const EMPTY_STATE_ICON: Rems = rems(32. / 14.);
/// Minimum spacing between polled reading-position saves during scrolling.
const READING_POSITION_INTERVAL: Duration = Duration::from_secs(5);

actions!(
    course2md_reader,
    [
        FindInNote,
        NextMatch,
        PreviousMatch,
        CloseFind,
        PreviousImage,
        NextImage,
        ZoomIn,
        ZoomOut,
        FitImage,
        CloseImage,
        NextReaderView,
        PreviousReaderView,
        ReaderPageUp,
        ReaderPageDown,
        ReaderStart,
        ReaderEnd
    ]
);

#[derive(Clone)]
struct Match {
    block: usize,
    range: Range<usize>,
}
#[derive(Clone)]
struct FrameImage {
    path: PathBuf,
    width: u32,
    height: u32,
}
/// Shape reserved for a screenshot whose file cannot be read.
const MISSING_FRAME_ASPECT: f32 = 16. / 9.;
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
fn frame_aspect(frame: &Frame) -> f32 {
    frame.image.as_ref().map_or(MISSING_FRAME_ASPECT, |image| {
        image.width as f32 / image.height as f32
    })
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
    missing_by_block: HashMap<usize, Vec<usize>>,
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
            || !task.plan.options.keep_video
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
        (media.parent() == Some(work.as_path()) && metadata.is_file() && metadata.len() > 0)
            .then_some(media)
    }
}

struct ImageViewer {
    scroll: ScrollHandle,
    viewport_scroll: ScrollHandle,
    viewport_size: gpui::Size<Pixels>,
    frames: Vec<Frame>,
    filtered: bool,
    index: usize,
    source: Option<nav::SourceTarget>,
    source_available: bool,
    version: PathBuf,
    zoom: Option<f32>,
    details_open: Option<bool>,
    return_focus: Option<FocusHandle>,
    focus: FocusHandle,
    dialogs: [WeakEntity<ImageDialog>; 2],
}

const ZOOM_STEP_IN: f32 = 1.25;
const ZOOM_STEP_OUT: f32 = 0.8;
const ZOOM_MIN: f32 = 0.1;
const ZOOM_MAX: f32 = 4.;

fn zoomed(scale: f32, factor: f32) -> f32 {
    (scale * factor).clamp(ZOOM_MIN, ZOOM_MAX)
}

/// Image dialog geometry: the dialog keeps a 24px window margin on each side
/// up to 1180px, and its content sits inside 17px of padding on each side.
struct ImageDialogMetrics {
    width: f32,
    /// Narrow dialogs shorten labels and fold zoom into a menu.
    compact: bool,
    /// Short dialogs show either the image or its text, not both.
    separate_modes: bool,
    /// Text is shown by default when the window has room for it.
    details_by_default: bool,
}

impl ImageDialogMetrics {
    fn new(window: &Window) -> Self {
        let rem = f32::from(window.rem_size());
        let size = window.bounds().size;
        let width = (f32::from(size.width) - 48.).min(1180.);
        let available = f32::from(size.height - task_dialog_top(window) - px(24.));
        Self {
            width: width.max(280.),
            compact: width - 34. < rem * 52.,
            separate_modes: available < rem * 24.,
            details_by_default: f32::from(size.height) >= rem * 32.,
        }
    }
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

fn reader_export_items(
    mut menu: PopupMenu,
    desktop: WeakEntity<Desktop>,
    course: Course,
    folder: Option<PathBuf>,
    exporting: bool,
) -> PopupMenu {
    if let Some(folder) = folder {
        menu = menu
            .item(
                PopupMenuItem::new(course2md::tr!("打开导出文件夹"))
                    .icon(icons::folder_open())
                    .on_click(move |_, _, cx| {
                        if let Ok(url) = url::Url::from_directory_path(&folder) {
                            cx.open_url(url.as_str());
                        }
                    }),
            )
            .separator();
    }
    for (markdown, label, icon) in [
        (
            false,
            course2md::tr!("复制整份笔记 · 纯文本"),
            icons::content_copy(),
        ),
        (
            true,
            course2md::tr!("复制整份笔记 · Markdown 文本"),
            icons::article(),
        ),
    ] {
        let weak = desktop.clone();
        let version = course.dir.clone();
        menu = menu.item(
            PopupMenuItem::new(label)
                .icon(icon)
                .on_click(move |_, _, cx| {
                    let _ = weak.update(cx, |this, cx| {
                        if let Some(preview) = this
                            .preview
                            .as_ref()
                            .filter(|preview| preview.course.dir == version)
                        {
                            let text = if markdown {
                                &preview.markdown_text
                            } else {
                                &preview.plain_text
                            };
                            cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
                            this.message = Some(
                                if markdown {
                                    course2md::tr!("已复制 Markdown 文本")
                                } else {
                                    course2md::tr!("已复制纯文本")
                                }
                                .into(),
                            );
                            cx.notify();
                        }
                    });
                }),
        );
    }
    menu = menu.separator();
    for (format, label) in [
        (
            course2md::config::OutputFormat::Md,
            course2md::tr!("导出 Markdown（含截图，.zip）…"),
        ),
        (
            course2md::config::OutputFormat::Html,
            course2md::tr!("导出网页文件…"),
        ),
        (
            course2md::config::OutputFormat::Json,
            course2md::tr!("导出 JSON 数据…"),
        ),
    ] {
        let weak = desktop.clone();
        let source = course.clone();
        menu = menu.item(
            PopupMenuItem::new(label)
                .icon(icons::download())
                .disabled(exporting)
                .on_click(move |_, window, cx| {
                    let _ = weak.update(cx, |this, cx| {
                        this.export_course(source.clone(), format, window, cx);
                    });
                }),
        );
    }
    menu
}

pub(crate) struct State {
    controls_scroll: ScrollHandle,
    information_scroll: ScrollHandle,
    toc_scroll: ScrollHandle,
    view_focus: [FocusHandle; 2],
    menu_focus: FocusHandle,
    find: Entity<InputState>,
    gallery_find: Entity<InputState>,
    gallery_search_position: Option<workspace::ReadingPosition>,
    focus: FocusHandle,
    find_open: bool,
    info_open: bool,
    processing_details_open: bool,
    // None follows the available reading width; an explicit choice survives resizing.
    toc_open: Option<bool>,
    find_return_focus: Option<FocusHandle>,
    matches: Rc<Vec<Match>>,
    search_index: Vec<nav::PreparedText>,
    match_index: usize,
    loaded: Option<PathBuf>,
    generation: u64,
    data_loading: bool,
    frames: Rc<Vec<Frame>>,
    frame_by_path: Rc<HashMap<PathBuf, usize>>,
    missing_by_block: Rc<HashMap<usize, Vec<usize>>>,
    /// Screenshots matching the gallery search, in note order.
    gallery_matches: Rc<Vec<usize>>,
    gallery_excerpts: Rc<HashMap<usize, String>>,
    /// The note's contents rail and the chapter titles its headings open.
    outline: Rc<Vec<(usize, String, Option<f64>)>>,
    chapter_starts: Rc<HashMap<usize, String>>,
    versions: Vec<Version>,
    issues: Vec<String>,
    pending_restore: Option<workspace::ReadingPosition>,
    pending_search: Option<Match>,
    restoring: bool,
    restore_generation: u64,
    last_position: Option<(String, workspace::ReadingPosition)>,
    /// A note restore waiting for its target block's first measurement.
    note_refine: Option<NoteRefine>,
    restore_target: Rc<Cell<Option<usize>>>,
    /// Window width, height and rem size the reading position was laid out at.
    layout: (f32, f32, f32),
    note_layout: Rc<RefCell<nav::ReadingLayout>>,
    gallery_layout: Rc<RefCell<nav::ReadingLayout>>,
    // The note tab's article flow is a variable-height `list`: persistent
    // state, the flattened item sequence it describes and per-item focus
    // containers that keep a focused control mounted while scrolled out.
    note_list: ListState,
    note_items: Rc<Vec<NoteItem>>,
    note_focus: Rc<Vec<FocusHandle>>,
    viewer: Option<ImageViewer>,
    exports: BTreeMap<PathBuf, PathBuf>,
    export_folder: Option<PathBuf>,
    export_state: ExportState,
    source_loading: bool,
    /// 进行中的重新定位探测取消标志：换笔记/新探测/完成时置位
    source_probe_cancel: Option<Arc<AtomicBool>>,
    source: Option<nav::SourceTarget>,
    source_available: bool,
    offline_video: OfflineVideo,
    offline_opening: bool,
    window: AnyWindowHandle,
    _subscriptions: Vec<Subscription>,
}

struct NoteRefine {
    position: workspace::ReadingPosition,
    search: Option<Match>,
    generation: u64,
}
impl State {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Desktop>) -> Self {
        let find =
            cx.new(|cx| InputState::new(window, cx).placeholder(course2md::tr!("查找笔记内容")));
        let gallery_find = cx
            .new(|cx| InputState::new(window, cx).placeholder(course2md::tr!("搜索截图对应文字")));
        let subscription = cx.subscribe(&find, |this, _, event, cx| match event {
            InputEvent::Change => {
                this.reader_ui.find_open = !this.reader_ui.find.read(cx).value().trim().is_empty();
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
            if !matches!(event, InputEvent::Change) {
                return;
            }
            this.update_gallery_matches(cx);
            if this.result_tab == 1 {
                let searching = !this
                    .reader_ui
                    .gallery_find
                    .read(cx)
                    .value()
                    .trim()
                    .is_empty();
                if searching {
                    if this.reader_ui.gallery_search_position.is_none() {
                        this.save_reading_position(cx);
                        this.reader_ui.gallery_search_position = this.capture_reading_position();
                    }
                    this.reader_scroll.set_offset(point(px(0.), px(0.)));
                } else if let Some(position) = this.reader_ui.gallery_search_position.take() {
                    this.reader_ui.pending_restore = Some(position);
                    this.reader_ui.restore_generation += 1;
                    this.reader_ui.restoring = false;
                }
                cx.notify();
            }
        });
        cx.bind_keys([
            KeyBinding::new("secondary-f", FindInNote, Some("CourseReader")),
            KeyBinding::new("secondary-g", NextMatch, Some("CourseReader")),
            KeyBinding::new("secondary-shift-g", PreviousMatch, Some("CourseReader")),
            KeyBinding::new("escape", CloseFind, Some("CourseReader")),
            KeyBinding::new("left", PreviousImage, Some("ReaderImage")),
            KeyBinding::new("right", NextImage, Some("ReaderImage")),
            KeyBinding::new("=", ZoomIn, Some("ReaderImage")),
            KeyBinding::new("+", ZoomIn, Some("ReaderImage")),
            KeyBinding::new("-", ZoomOut, Some("ReaderImage")),
            KeyBinding::new("0", FitImage, Some("ReaderImage")),
            KeyBinding::new("escape", CloseImage, Some("ReaderImage")),
            KeyBinding::new("right", NextReaderView, Some("ReaderViews")),
            KeyBinding::new("left", PreviousReaderView, Some("ReaderViews")),
            KeyBinding::new("pageup", ReaderPageUp, Some("CourseReader && !Input")),
            KeyBinding::new("pagedown", ReaderPageDown, Some("CourseReader && !Input")),
            KeyBinding::new("home", ReaderStart, Some("CourseReader && !Input")),
            KeyBinding::new("end", ReaderEnd, Some("CourseReader && !Input")),
        ]);
        let bounds = cx.observe_window_bounds(window, |this, window, cx| {
            this.reader_layout_changed(window, cx)
        });
        // Text scale changes reach the window's rem size through the theme.
        let scale = cx.observe_global_in::<gpui_component::Theme>(window, |this, window, cx| {
            this.reader_layout_changed(window, cx)
        });
        Self {
            controls_scroll: ScrollHandle::new(),
            information_scroll: ScrollHandle::new(),
            toc_scroll: ScrollHandle::new(),
            view_focus: std::array::from_fn(|_| cx.focus_handle()),
            menu_focus: cx.focus_handle(),
            find,
            gallery_find,
            gallery_search_position: None,
            focus: cx.focus_handle(),
            find_open: false,
            info_open: false,
            processing_details_open: false,
            toc_open: None,
            find_return_focus: None,
            matches: Rc::new(Vec::new()),
            search_index: Vec::new(),
            match_index: 0,
            loaded: None,
            generation: 0,
            data_loading: false,
            frames: Rc::default(),
            frame_by_path: Rc::default(),
            missing_by_block: Rc::default(),
            gallery_matches: Rc::default(),
            gallery_excerpts: Rc::default(),
            outline: Rc::default(),
            chapter_starts: Rc::default(),
            versions: Vec::new(),
            issues: Vec::new(),
            pending_restore: None,
            pending_search: None,
            restoring: false,
            restore_generation: 0,
            last_position: None,
            note_refine: None,
            restore_target: Rc::default(),
            layout: reader_window_layout(window),
            note_layout: Rc::default(),
            gallery_layout: Rc::default(),
            note_list: ListState::new(0, ListAlignment::Top, px(1000.)),
            note_items: Rc::default(),
            note_focus: Rc::default(),
            viewer: None,
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
            _subscriptions: vec![subscription, gallery_subscription, bounds, scale],
        }
    }
}
fn reader_window_layout(window: &Window) -> (f32, f32, f32) {
    (
        f32::from(window.bounds().size.width),
        f32::from(window.bounds().size.height),
        f32::from(window.rem_size()),
    )
}

/// One half of the image viewer dialog. It renders from the desktop's viewer
/// state and is notified only when that state changes.
struct ImageDialog {
    desktop: Entity<Desktop>,
    footer: bool,
}
impl Render for ImageDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.desktop.update(cx, |desktop, cx| {
            if self.footer {
                desktop.reader_image_actions(window, cx)
            } else {
                desktop.reader_image_content(window, cx)
            }
        })
    }
}
fn block_text(block: &PreviewBlock) -> Option<&str> {
    match block {
        PreviewBlock::Heading { text, .. }
        | PreviewBlock::Paragraph { text, .. }
        | PreviewBlock::Translation { text, .. } => Some(text.as_ref()),
        _ => None,
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
fn capture_reader_position(
    layout: &nav::ReadingLayout,
    offset: f32,
    anchor: impl FnOnce(usize) -> (Option<String>, Option<f64>),
) -> Option<workspace::ReadingPosition> {
    // The top is a document boundary, not the first measured paragraph. Keep it
    // fixed while images/metadata arrive and when the reading measure changes.
    if offset >= 0. {
        return Some(workspace::ReadingPosition::default());
    }
    let (index, within, height) = layout.top_item(offset)?;
    let (paragraph, seconds) = anchor(index);
    Some(workspace::ReadingPosition {
        paragraph,
        seconds,
        offset,
        within,
        fraction: Some(nav::within_fraction(within, height)),
    })
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
fn restored_reader_offset(
    layout: &nav::ReadingLayout,
    index: Option<usize>,
    position: &workspace::ReadingPosition,
) -> f32 {
    index
        .and_then(|index| layout.restore(index, position.fraction, position.within))
        .unwrap_or(position.offset.min(0.))
}

/// The note tab's article as list items: the title (shown on short windows),
/// the summary as one card, then one item per section: its time, screenshot
/// and paragraphs side by side. Blocks before the first section stay single
/// items. Items keep the block indexing of the full document, so anchors,
/// search targets and reading positions resolve to blocks as before.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NoteItem {
    Title,
    /// The summary blocks `start..end`.
    Summary {
        start: usize,
        end: usize,
    },
    /// A section heading and its blocks `heading..end`; `ordinal` alternates
    /// which side the screenshot takes.
    Section {
        heading: usize,
        end: usize,
        ordinal: usize,
    },
    Block(usize),
}

impl NoteItem {
    /// The document blocks this item draws.
    fn blocks(self) -> std::ops::Range<usize> {
        match self {
            Self::Title => 0..0,
            Self::Summary { start, end } => start..end,
            Self::Section { heading, end, .. } => heading..end,
            Self::Block(block) => block..block + 1,
        }
    }
}

fn note_summary_block(block: &PreviewBlock) -> bool {
    block.is_summary()
}

/// The title item leads every note; it is empty unless short windows scroll
/// the title with the article, so the item sequence never depends on layout.
fn note_items(blocks: &[PreviewBlock]) -> Vec<NoteItem> {
    let mut items = Vec::with_capacity(blocks.len() / 3 + 2);
    items.push(NoteItem::Title);
    let summary_end = blocks
        .iter()
        .position(|block| !note_summary_block(block))
        .unwrap_or(blocks.len());
    if summary_end > 0 {
        items.push(NoteItem::Summary {
            start: 0,
            end: summary_end,
        });
    }
    let mut index = summary_end;
    let mut ordinal = 0;
    while index < blocks.len() {
        if blocks[index].section_index().is_some() {
            let end = blocks[index + 1..]
                .iter()
                .position(|block| block.section_index().is_some())
                .map_or(blocks.len(), |offset| index + 1 + offset);
            items.push(NoteItem::Section {
                heading: index,
                end,
                ordinal,
            });
            ordinal += 1;
            index = end;
        } else {
            items.push(NoteItem::Block(index));
            index += 1;
        }
    }
    items
}

/// `None` at the top of the note, otherwise the top item: the page re-renders
/// when this changes. Items before the first that draws a block (the note's
/// header) count as the top.
fn note_scroll_key(items: &[NoteItem], list: &ListState) -> Option<usize> {
    let top = list.logical_scroll_top();
    let first = items
        .iter()
        .position(|item| note_item_block(*item).is_some())
        .unwrap_or(0);
    (top.item_ix > first || (top.item_ix == first && top.offset_in_item > px(0.)))
        .then_some(top.item_ix)
}

/// The block an item's position resolves to, when it draws any.
fn note_item_block(item: NoteItem) -> Option<usize> {
    let blocks = item.blocks();
    (!blocks.is_empty()).then_some(blocks.start)
}

/// The block at or after the item; decorations such as the title carry none.
fn note_top_block(items: &[NoteItem], item: usize) -> Option<usize> {
    items
        .get(item..)
        .unwrap_or(&[])
        .iter()
        .find_map(|item| note_item_block(*item))
        .or_else(|| {
            items
                .get(..item)
                .unwrap_or(&[])
                .iter()
                .rev()
                .find_map(|item| note_item_block(*item))
        })
}

/// The item that draws `block`.
fn note_block_item(items: &[NoteItem], block: usize) -> Option<usize> {
    items.iter().position(|item| item.blocks().contains(&block))
}

/// The note tab's reading position from the list's logical scroll top: the top
/// item's block anchor, the pixel offset inside it and, once measured, the
/// fraction of its height.
fn note_capture_position(
    items: &[NoteItem],
    blocks: &[PreviewBlock],
    list: &ListState,
    layout: &nav::ReadingLayout,
) -> Option<workspace::ReadingPosition> {
    let top = list.logical_scroll_top();
    // The top is a document boundary, not the first measured paragraph. A
    // position inside the header, which draws no block, reads from the top.
    if top.item_ix == 0 && top.offset_in_item <= px(0.)
        || items.get(top.item_ix) == Some(&NoteItem::Title)
    {
        return Some(workspace::ReadingPosition::default());
    }
    let index = note_top_block(items, top.item_ix)?;
    let within = -f32::from(top.offset_in_item);
    Some(workspace::ReadingPosition {
        paragraph: blocks.get(index).map(|block| block_anchor(block, index)),
        seconds: block_time(blocks, index),
        offset: f32::from(list.scroll_px_offset_for_scrollbar().y).min(0.),
        within,
        fraction: layout
            .item_height(index)
            .map(|height| nav::within_fraction(within, height)),
    })
}

/// Where the note list should scroll to restore a position or reveal a search
/// match: the item index and, once the item is measured, the precise in-item
/// offset (the found line with one line of context, or the saved fractional
/// position). `None` scrolls to the document top.
fn note_restore_target(
    items: &[NoteItem],
    blocks: &[PreviewBlock],
    position: &workspace::ReadingPosition,
    search: Option<(usize, usize)>,
    layout: &nav::ReadingLayout,
) -> Option<(usize, Option<f32>)> {
    let block = search
        .map(|(block, _)| block)
        .or_else(|| note_position_index(blocks, position));
    let item_ix = block.and_then(|block| note_block_item(items, block))?;
    let offset = search
        .and_then(|(block, byte)| layout.search_within(block, byte))
        .or_else(|| {
            block.and_then(|block| {
                layout
                    .item_height(block)
                    .map(|height| -nav::restore_within(position.fraction, position.within, height))
            })
        });
    Some((item_ix, offset))
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

fn frame_excerpt(frame: &Frame, query: &str) -> String {
    let text = frame.transcript.as_str();
    if let Some(found) = nav::text_matches(text, query).first() {
        let mut start = text[..found.start]
            .char_indices()
            .rev()
            .nth(15)
            .map(|(index, _)| index)
            .unwrap_or(0);
        while start > 0
            && text.as_bytes()[start].is_ascii_alphanumeric()
            && text.as_bytes()[start - 1].is_ascii_alphanumeric()
        {
            start -= 1;
        }
        let mut end = text[found.end..]
            .char_indices()
            .nth(100)
            .map(|(index, _)| found.end + index)
            .unwrap_or(text.len());
        while end < text.len()
            && text.as_bytes()[end - 1].is_ascii_alphanumeric()
            && text.as_bytes()[end].is_ascii_alphanumeric()
        {
            end += 1;
        }
        return format!(
            "{}{}{}",
            if start > 0 { "…" } else { "" },
            text[start..end].trim(),
            if end < text.len() { "…" } else { "" },
        );
    }
    if text.trim().is_empty() {
        course2md::tr!("这张截图没有对应文字").into()
    } else {
        text.to_owned()
    }
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
/// Selectable paragraph text with word-level find highlights. Selection plumbing
/// mirrors gpui_base::SelectableText (which accepts no highlight runs).
struct ReaderText {
    id: ElementId,
    handle: Option<gpui_base::TextSelectionHandle>,
    text: SharedString,
    styled_text: StyledText,
    document_order: u64,
    search_target: Option<ReaderSearchTarget>,
}

struct ReaderSearchTarget {
    block: usize,
    byte: usize,
    positions: Rc<RefCell<nav::ReadingLayout>>,
}
impl ReaderText {
    fn new(
        id: impl Into<ElementId>,
        text: impl Into<SharedString>,
        document_order: u64,
        highlights: Vec<(Range<usize>, HighlightStyle)>,
    ) -> Self {
        let text = text.into();
        let styled = StyledText::new(text.clone());
        ReaderText {
            id: id.into(),
            handle: None,
            styled_text: if highlights.is_empty() {
                styled
            } else {
                styled.with_highlights(highlights)
            },
            text,
            document_order,
            search_target: None,
        }
    }

    fn with_search_target(mut self, target: Option<ReaderSearchTarget>) -> Self {
        self.search_target = target;
        self
    }
}
impl IntoElement for ReaderText {
    type Element = Self;
    fn into_element(self) -> Self::Element {
        self
    }
}
impl Element for ReaderText {
    type RequestLayoutState = gpui_base::TextSelectionHandle;
    type PrepaintState = Hitbox;
    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }
    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }
    fn request_layout(
        &mut self,
        global_id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let handle = self.handle.clone().unwrap_or_else(|| {
            window.with_element_state(
                global_id.expect("ReaderText must have a stable element id"),
                |retained: Option<gpui_base::TextSelectionHandle>, _| {
                    let handle = retained.unwrap_or_else(|| {
                        gpui_base::TextSelectionHandle::new(self.text.clone(), cx)
                    });
                    (handle.clone(), handle)
                },
            )
        });
        let (layout_id, ()) = self
            .styled_text
            .request_layout(global_id, inspector_id, window, cx);
        (layout_id, handle)
    }
    fn prepaint(
        &mut self,
        global_id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        handle: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        self.styled_text
            .prepaint(global_id, inspector_id, bounds, &mut (), window, cx);
        if let Some(target) = &self.search_target {
            let layout = self.styled_text.layout();
            if let Some(position) = layout.position_for_index(target.byte) {
                // Window coordinates: see `ReadingLayout`.
                target.positions.borrow_mut().record_search_line(
                    target.block,
                    target.byte,
                    f32::from(position.y),
                    f32::from(layout.line_height()),
                );
            }
        }
        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
        handle.register(
            gpui_base::TextSelectionRegistration::new(hitbox.clone(), bounds)
                .with_document_order(self.document_order)
                .with_text_bounds(vec![bounds]),
            window,
            cx,
        );
        hitbox
    }
    fn paint(
        &mut self,
        global_id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        handle: &mut Self::RequestLayoutState,
        _: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let layout = self.styled_text.layout().clone();
        // Only a participant in the current selection can change its copied text.
        let participating = handle.snapshot(cx).is_some() || handle.has_local_selection(cx);
        let selected_text_before =
            participating.then(|| gpui_base::TextSelection::selected_text(window, cx));
        let projection = handle.update_runs(
            &[
                gpui_base::TextSelectionRun::new(self.text.clone(), layout.clone(), bounds)
                    .with_document_order(self.document_order),
            ],
            cx,
        );
        if let Some(before) = selected_text_before
            && before != gpui_base::TextSelection::selected_text(window, cx)
        {
            window.refresh();
        }
        for range in projection.ranges().iter().flatten().cloned() {
            paint_text_selection(&layout, range, window);
        }
        self.styled_text.paint(
            global_id,
            inspector_id,
            bounds,
            &mut (),
            &mut (),
            window,
            cx,
        );
    }
}
/// Quad painting identical to gpui_base::SelectableText's selection pass.
fn paint_text_selection(layout: &gpui::TextLayout, range: Range<usize>, window: &mut Window) {
    let (Some(start), Some(end)) = (
        layout.position_for_index(range.start),
        layout.position_for_index(range.end),
    ) else {
        return;
    };
    let line_height = layout.line_height();
    let bounds = layout.bounds();
    let quads = if start.y == end.y {
        vec![Bounds::from_corners(
            start,
            Point::new(end.x, end.y + line_height),
        )]
    } else {
        let mut quads = vec![Bounds::from_corners(
            start,
            Point::new(bounds.right(), start.y + line_height),
        )];
        if end.y > start.y + line_height {
            quads.push(Bounds::from_corners(
                Point::new(bounds.left(), start.y + line_height),
                Point::new(bounds.right(), end.y),
            ));
        }
        quads.push(Bounds::from_corners(
            Point::new(bounds.left(), end.y),
            Point::new(end.x, end.y + line_height),
        ));
        quads
    };
    for quad in quads {
        window.paint_quad(PaintQuad {
            bounds: quad,
            background: color(SELECTION).into(),
            corner_radii: gpui::Corners::default(),
            border_widths: gpui::Edges::default(),
            border_color: transparent_black(),
            border_style: gpui::BorderStyle::default(),
        });
    }
}
fn paragraph(
    id: impl Into<ElementId>,
    text: SharedString,
    order: usize,
    highlights: Vec<(Range<usize>, HighlightStyle)>,
    search_target: Option<ReaderSearchTarget>,
) -> Stateful<Div> {
    let id = id.into();
    let view = div()
        .id(id.clone())
        .role(Role::Paragraph)
        .aria_label(text.clone())
        .font_family(READER_SERIF)
        .text_size(TEXT_READER)
        .font_weight(FontWeight::NORMAL)
        .line_height(relative(1.88));
    if crate::math::has_math(&text) {
        return view.child(math_paragraph(&text, TEXT_READER, color(INK).into()));
    }
    view.child(
        ReaderText::new(id, text, order as u64, highlights).with_search_target(search_target),
    )
}

/// Prose with formulas. GPUI text cannot hold inline elements, so the prose is
/// laid out as wrapping tokens (one per CJK character, one per Latin word)
/// around the typeset formulas; a display formula takes a centered row.
fn math_paragraph(text: &str, em: Rems, ink: Hsla) -> Div {
    let mut view = v_flex().w_full().min_w_0().gap_2();
    let mut row = Vec::new();
    let flush = |view: Div, row: &mut Vec<AnyElement>| {
        if row.is_empty() {
            view
        } else {
            view.child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .flex_wrap()
                    .items_center()
                    .children(row.drain(..)),
            )
        }
    };
    for piece in crate::math::pieces(text) {
        match piece {
            crate::math::Piece::Text(prose) => {
                // Closing punctuation never starts a line: it stays with the
                // formula or word before it.
                let split = prose
                    .char_indices()
                    .find(|(_, c)| !closes_line(*c))
                    .map_or(prose.len(), |(index, _)| index);
                let (lead, rest) = prose.split_at(split);
                match (!lead.is_empty()).then(|| row.pop()).flatten() {
                    Some(previous) => row.push(
                        h_flex()
                            .flex_none()
                            .items_center()
                            .child(previous)
                            .child(
                                div()
                                    .whitespace_nowrap()
                                    .child(SharedString::from(lead.to_owned())),
                            )
                            .into_any_element(),
                    ),
                    None => row.extend(prose_tokens(lead)),
                }
                row.extend(prose_tokens(rest));
            }
            crate::math::Piece::Math { latex, display } => {
                let typeset = crate::math::formula(latex, display);
                match (typeset, display) {
                    (Some(formula), true) => {
                        view = flush(view, &mut row).child(
                            h_flex()
                                .id(SharedString::from(format!("display-math-{latex}")))
                                .w_full()
                                .justify_center()
                                .py_2()
                                .overflow_x_scroll()
                                .child(formula.element(em, true, ink)),
                        );
                    }
                    (Some(formula), false) => {
                        row.push(
                            div()
                                .px(em * 0.1)
                                .child(formula.element(em, false, ink))
                                .into_any_element(),
                        );
                    }
                    // Unparseable LaTeX stays readable as its source.
                    (None, _) => row.extend(prose_tokens(latex)),
                }
            }
        }
    }
    flush(view, &mut row)
}

/// Punctuation that may not begin a line (CJK and Latin closing marks).
fn closes_line(c: char) -> bool {
    "，。、；：！？）」』”’》〉】,.;:!?)]}".contains(c)
}

/// Line-break opportunities of prose: Latin words with their trailing space,
/// and every other character on its own.
fn prose_tokens(prose: &str) -> impl Iterator<Item = AnyElement> + '_ {
    let mut tokens = Vec::new();
    let mut start = 0;
    for (index, c) in prose.char_indices() {
        let end = index + c.len_utf8();
        let word = c.is_ascii_alphanumeric() || c.is_ascii_punctuation();
        let next_word = prose[end..]
            .chars()
            .next()
            .is_some_and(|next| next.is_ascii_alphanumeric() || next.is_ascii_punctuation());
        let next_closes = prose[end..].chars().next().is_some_and(closes_line);
        if !(word && next_word) && !next_closes {
            tokens.push(&prose[start..end]);
            start = end;
        }
    }
    if start < prose.len() {
        tokens.push(&prose[start..]);
    }
    tokens.into_iter().map(|token| {
        div()
            .whitespace_nowrap()
            .child(SharedString::from(token.to_owned()))
            .into_any_element()
    })
}

/// Everything a virtualized note-flow item needs. The maps are built when the
/// note or its screenshots load; the list's render closure only builds the
/// visible window of blocks.
struct NoteFlow {
    items: Rc<Vec<NoteItem>>,
    focus: Rc<Vec<FocusHandle>>,
    preview: Rc<notes::Preview>,
    frames: Rc<Vec<Frame>>,
    frame_by_path: Rc<HashMap<PathBuf, usize>>,
    missing_by_block: Rc<HashMap<usize, Vec<usize>>>,
    chapter_starts: Rc<HashMap<usize, String>>,
    source: Option<nav::SourceTarget>,
    matches: Rc<Vec<Match>>,
    match_index: usize,
    find_open: bool,
    data_loading: bool,
    compact: bool,
    short: bool,
    /// Sections put the screenshot and text side by side; narrow, they stack.
    wide: bool,
    layout: Rc<RefCell<nav::ReadingLayout>>,
    restore_target: Rc<Cell<Option<usize>>>,
    note_list: ListState,
    desktop: WeakEntity<Desktop>,
}

impl NoteFlow {
    /// Find highlights group per block; the current match reads stronger.
    /// `matches` arrive in block order, so one binary search scopes a block.
    fn highlight_runs(&self, index: usize) -> Vec<(Range<usize>, HighlightStyle)> {
        if !self.find_open {
            return Vec::new();
        }
        let start = self.matches.partition_point(|found| found.block < index);
        self.matches[start..]
            .iter()
            .take_while(|found| found.block == index)
            .enumerate()
            .map(|(offset, found)| {
                let current = start + offset == self.match_index;
                (
                    found.range.clone(),
                    if current {
                        HighlightStyle {
                            background_color: Some(color(FIND_CURRENT).into()),
                            underline: Some(UnderlineStyle {
                                thickness: px(1.),
                                color: Some(color(FIND_CURRENT_LINE).into()),
                                wavy: false,
                            }),
                            ..Default::default()
                        }
                    } else {
                        HighlightStyle {
                            background_color: Some(color(FIND_HIGHLIGHT).into()),
                            ..Default::default()
                        }
                    },
                )
            })
            .collect()
    }

    fn search_target(&self, index: usize) -> Option<ReaderSearchTarget> {
        if !self.find_open {
            return None;
        }
        self.matches
            .get(self.match_index)
            .filter(|found| found.block == index)
            .map(|found| ReaderSearchTarget {
                block: index,
                byte: found.range.start,
                positions: self.layout.clone(),
            })
    }

    /// One list item: uniform spacing between items, a focus container that
    /// keeps a focused control mounted off-viewport, and the position record
    /// that reading positions and search jumps resolve against. Every block an
    /// item draws records the item's bounds, so a found line inside any of its
    /// paragraphs resolves to an offset within this item.
    fn item_wrapper(&self, index: usize, content: AnyElement) -> Stateful<Div> {
        let item = self.items[index];
        let mut wrapper = div()
            .w_full()
            .min_w_0()
            .flex_shrink_0()
            // Virtual list measurements include padding, not external margins.
            .pb_8();
        let blocks = item.blocks();
        if !blocks.is_empty() {
            let positions = self.layout.clone();
            let restore_target = self.restore_target.clone();
            let desktop = self.desktop.clone();
            wrapper = wrapper.on_children_prepainted(move |bounds, window, cx| {
                let Some(bounds) = bounds.first() else {
                    return;
                };
                // Window coordinates: see `ReadingLayout`.
                let mut positions = positions.borrow_mut();
                for block in blocks.clone() {
                    positions.record(
                        block,
                        f32::from(bounds.top()),
                        f32::from(bounds.size.height),
                    );
                }
                if restore_target
                    .get()
                    .is_some_and(|target| blocks.contains(&target))
                {
                    restore_target.set(None);
                    let desktop = desktop.clone();
                    window.defer(cx, move |window, cx| {
                        let _ = desktop.update(cx, |this, cx| this.refine_note_restore(window, cx));
                    });
                }
            });
        }
        wrapper
            .child(content)
            .id(("note-item", index))
            .track_focus(&self.focus[index])
            .tab_stop(false)
    }

    fn reveal(&self, id: ElementId, child: AnyElement) -> crate::focus_scroll::RevealFocus {
        crate::focus_scroll::RevealFocus::in_list(id, child, self.note_list.clone())
    }
}

/// Render one note-flow item: the scrolling title, the summary card, a
/// section, or a block that precedes the sections.
fn render_note_item(flow: &NoteFlow, item_ix: usize, cx: &mut App) -> AnyElement {
    let Some(&item) = flow.items.get(item_ix) else {
        return div().into_any_element();
    };
    let preview = &flow.preview;
    let content = match item {
        NoteItem::Title => flow
            .desktop
            .update(cx, |this, cx| {
                let ReaderHeaderParts { title, meta } = this.reader_metadata_row(
                    preview,
                    flow.compact,
                    flow.short,
                    HeaderContainer::List(flow.note_list.clone()),
                    cx,
                );
                // Short windows show the facts in the information panel.
                v_flex()
                    .w_full()
                    .min_w_0()
                    .gap_3()
                    .child(title)
                    .when(!flow.short, |header| header.children(meta))
                    .into_any_element()
            })
            .unwrap_or_else(|_| div().into_any_element()),
        NoteItem::Summary { start, end } => render_note_summary(flow, start..end),
        NoteItem::Section {
            heading,
            end,
            ordinal,
        } => render_note_section(flow, heading, end, ordinal),
        NoteItem::Block(index) => render_note_block(flow, index),
    };
    flow.item_wrapper(item_ix, content).into_any_element()
}

/// One block on its own: a paragraph, heading or screenshot outside any section.
fn render_note_block(flow: &NoteFlow, index: usize) -> AnyElement {
    match &flow.preview.blocks[index] {
        PreviewBlock::Heading {
            text,
            anchor,
            seconds,
        } => render_note_heading(
            flow,
            index,
            text.clone(),
            anchor,
            *seconds,
            flow.preview.blocks[index].is_timestamp_heading(),
        ),
        PreviewBlock::Paragraph { text, anchor } => note_paragraph(flow, index, text, anchor),
        PreviewBlock::Translation { text, anchor } => note_translation(flow, index, text, anchor),
        PreviewBlock::Image(path) => render_note_image(flow, index, path),
    }
}

fn note_paragraph(flow: &NoteFlow, index: usize, text: &SharedString, anchor: &str) -> AnyElement {
    paragraph(
        SharedString::from(anchor.to_owned()),
        text.clone(),
        index,
        flow.highlight_runs(index),
        flow.search_target(index),
    )
    .into_any_element()
}

/// A translation reads as a quieter companion to the paragraph above it.
fn note_translation(
    flow: &NoteFlow,
    index: usize,
    text: &SharedString,
    anchor: &str,
) -> AnyElement {
    paragraph(
        SharedString::from(anchor.to_owned()),
        text.clone(),
        index,
        flow.highlight_runs(index),
        flow.search_target(index),
    )
    .text_color(color(GRAY))
    .into_any_element()
}

/// The summary as one card: its overview, then the key points in two columns
/// on wide windows.
fn render_note_summary(flow: &NoteFlow, blocks: std::ops::Range<usize>) -> AnyElement {
    let preview = &flow.preview;
    let mut overview = Vec::new();
    let mut points = Vec::new();
    for index in blocks {
        if let PreviewBlock::Paragraph { text, anchor } = &preview.blocks[index] {
            if anchor.starts_with("key-point-") {
                let point = text.trim_start_matches("• ").to_owned();
                points.push(
                    h_flex()
                        .min_w_0()
                        .items_start()
                        .gap_3()
                        .child(
                            div()
                                .mt(rems(11. / 14.))
                                .size(px(6.))
                                .flex_shrink_0()
                                .rounded_full()
                                .bg(color(ACCENT)),
                        )
                        .child(div().flex_1().min_w_0().child(paragraph(
                            SharedString::from(anchor.clone()),
                            point.into(),
                            index,
                            flow.highlight_runs(index),
                            flow.search_target(index),
                        )))
                        .into_any_element(),
                );
            } else {
                overview.push(note_paragraph(flow, index, text, anchor));
            }
        }
    }
    v_flex()
        .w_full()
        .min_w_0()
        .p(rems(28. / 14.))
        .gap(rems(18. / 14.))
        .rounded(rems(24. / 14.))
        .bg(color(SURFACE))
        .child(
            h_flex()
                .gap_2()
                .items_center()
                .text_color(color(ACCENT))
                .child(icons::sparkles().size(rems(16. / 14.)))
                .child(
                    theme::accessible_text("reader-summary-label", course2md::tr!("摘要"))
                        .role(Role::Heading)
                        .text_size(TEXT_AUX)
                        .font_weight(FontWeight::MEDIUM),
                ),
        )
        .child(
            v_flex()
                .gap_2()
                .text_size(rems(18. / 14.))
                .children(overview),
        )
        .when(!points.is_empty(), |card| {
            card.child(
                div()
                    .grid()
                    .grid_cols(if flow.wide { 2 } else { 1 })
                    .gap_x(rems(40. / 14.))
                    .gap_y(rems(10. / 14.))
                    .children(points),
            )
        })
        .into_any_element()
}

/// A section as on the canvas: the screenshot on one side with a chip that
/// plays the source from its time, and on the other the time, the chapter
/// title where one begins, and the paragraphs. Sides alternate; narrow
/// windows stack them.
fn render_note_section(flow: &NoteFlow, heading: usize, end: usize, ordinal: usize) -> AnyElement {
    let preview = &flow.preview;
    let (anchor, seconds) = match &preview.blocks[heading] {
        PreviewBlock::Heading {
            anchor, seconds, ..
        } => (anchor.clone(), *seconds),
        _ => (String::new(), None),
    };
    let image = (heading + 1..end).find_map(|index| match &preview.blocks[index] {
        PreviewBlock::Image(path) => Some(render_note_image(flow, index, path)),
        _ => None,
    });
    let paragraphs = (heading + 1..end).filter_map(|index| match &preview.blocks[index] {
        PreviewBlock::Paragraph { text, anchor } => Some(note_paragraph(flow, index, text, anchor)),
        PreviewBlock::Translation { text, anchor } => {
            Some(note_translation(flow, index, text, anchor))
        }
        _ => None,
    });
    let chapter = flow.chapter_starts.get(&heading);
    let marks = flow.highlight_runs(heading);
    let time = seconds.map(course2md::render::fmt_ts);
    let missing = flow
        .missing_by_block
        .get(&heading)
        .into_iter()
        .flatten()
        .map(|&frame_index| {
            theme::accessible_text(
                ("missing-body-image", frame_index),
                course2md::tr!("{}无法读取", frame_label(&flow.frames[frame_index])),
            )
            .text_sm()
            .text_color(color(WARNING))
        });
    let title_row = h_flex()
        .id(SharedString::from(anchor))
        .flex_wrap()
        .items_baseline()
        .gap_x_3()
        .role(Role::Heading)
        .aria_label(
            chapter
                .map(|chapter| format!("{} {chapter}", time.clone().unwrap_or_default()))
                .or(time.clone())
                .unwrap_or_default(),
        )
        .children(time.map(|time| {
            div()
                .text_size(TEXT_BODY)
                .font_weight(FontWeight::MEDIUM)
                .text_color(color(ACCENT))
                .child(if marks.is_empty() {
                    time.into_any_element()
                } else {
                    ReaderText::new(
                        ("reader-heading-marks", heading),
                        SharedString::from(time),
                        heading as u64,
                        marks,
                    )
                    .with_search_target(flow.search_target(heading))
                    .into_any_element()
                })
        }))
        .children(chapter.map(|chapter| {
            div()
                .font_family(READER_SERIF)
                .text_size(rems(26. / 14.))
                .line_height(rems(36. / 14.))
                .font_weight(FontWeight::SEMIBOLD)
                .child(chapter.clone())
        }));
    let text = v_flex()
        .flex_1()
        .min_w_0()
        .gap_4()
        .child(title_row)
        .children(missing)
        .children(paragraphs);
    let image_first = !flow.wide || ordinal.is_multiple_of(2);
    let row = if flow.wide {
        h_flex()
            .w_full()
            .min_w_0()
            .items_center()
            .gap(rems(48. / 14.))
    } else {
        v_flex().w_full().min_w_0().gap_5()
    };
    let image = image.map(|image| div().flex_1().min_w_0().child(image));
    v_flex()
        .w_full()
        .min_w_0()
        .pb_8()
        .border_b_1()
        .border_color(color(CARD_LINE))
        .child(if image_first {
            row.children(image).child(text)
        } else {
            row.child(text).children(image)
        })
        .into_any_element()
}

/// A section heading: its time chip, the chapter title where a new chapter of
/// the summary outline begins, and notices for screenshots that cannot be read.
fn render_note_heading(
    flow: &NoteFlow,
    index: usize,
    text: SharedString,
    anchor: &str,
    seconds: Option<f64>,
    timestamp_only: bool,
) -> AnyElement {
    let marks = flow.highlight_runs(index);
    let chapter = flow.chapter_starts.get(&index);
    // Without a chapter title, a heading that is only its time is the chip itself.
    let bare_timestamp = chapter.is_none() && timestamp_only;
    let heading = if !marks.is_empty() {
        Some(
            div()
                .id(("reader-heading-wrap", index))
                .role(Role::Heading)
                .aria_label(text.clone())
                .text_size(TEXT_TITLE)
                .font_weight(FontWeight::SEMIBOLD)
                .child(
                    ReaderText::new(
                        ("reader-heading-marks", index),
                        text.clone(),
                        index as u64,
                        marks,
                    )
                    .with_search_target(flow.search_target(index)),
                )
                .into_any_element(),
        )
    } else if bare_timestamp {
        None
    } else {
        Some(
            theme::accessible_text(
                ("reader-heading", index),
                chapter
                    .cloned()
                    .map_or_else(|| text.clone(), SharedString::from),
            )
            .role(Role::Heading)
            .text_size(TEXT_TITLE)
            .font_weight(FontWeight::SEMIBOLD)
            .into_any_element(),
        )
    };
    let chip = seconds.map(|seconds| {
        let chip = div()
            .text_size(TEXT_READER)
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(color(GRAY))
            .flex_shrink_0()
            .whitespace_nowrap()
            .child(course2md::render::fmt_ts(seconds));
        if bare_timestamp {
            chip.id(("reader-heading", index))
                .role(Role::Heading)
                .aria_label(text.clone())
        } else {
            chip.id(("reader-heading-chip", index))
        }
    });
    let missing = flow
        .missing_by_block
        .get(&index)
        .into_iter()
        .flatten()
        .map(|&frame_index| {
            theme::accessible_text(
                ("missing-body-image", frame_index),
                course2md::tr!("{}无法读取", frame_label(&flow.frames[frame_index])),
            )
            .text_sm()
            .text_color(color(WARNING))
        });
    v_flex()
        .id(SharedString::from(anchor.to_owned()))
        .gap_2()
        .when(index > 0, |view| view.pt_4())
        .child(
            h_flex()
                .flex_wrap()
                .gap_2()
                .items_center()
                .children(chip)
                .children(heading),
        )
        .children(missing)
        .into_any_element()
}

/// A screenshot inside the article: 16:9 and rounded, opening the viewer, with
/// a chip over its corner that plays the source from its moment.
fn render_note_image(flow: &NoteFlow, index: usize, path: &std::path::Path) -> AnyElement {
    let frame_index = flow.frame_by_path.get(path).copied();
    let frame = frame_index.map(|i| &flow.frames[i]);
    if frame.is_none() && !flow.data_loading {
        return theme::accessible_text(
            ("unreadable-inline-image", index),
            course2md::tr!("截图无法读取"),
        )
        .text_sm()
        .text_color(color(WARNING))
        .into_any_element();
    }
    let label = frame
        .map(frame_label)
        .unwrap_or_else(|| course2md::tr!("截图").into());
    let desktop = flow.desktop.clone();
    let seek = frame.and_then(|frame| {
        let url = nav::seek_url(flow.source.as_ref()?, frame.seconds)?;
        let time = course2md::render::fmt_ts(frame.seconds);
        Some(
            control(("seek-image", index))
                .absolute()
                .left(rems(12. / 14.))
                .bottom(rems(12. / 14.))
                .h(rems(32. / 14.))
                .min_h(rems(32. / 14.))
                .pl(rems(8. / 14.))
                .pr(rems(12. / 14.))
                .rounded_full()
                .bg(gpui::rgba(0x101416c7))
                .text_color(gpui::white())
                .text_size(TEXT_AUX)
                .icon(icons::play_arrow())
                .label(time.clone())
                .accessibility_label(course2md::tr!("从 {} 播放原视频", time))
                .on_click(move |_, _, cx| cx.open_url(&url)),
        )
    });
    flow.reveal(
        ("reveal-note-image", index).into(),
        div()
            .relative()
            .w_full()
            .child(
                control(("note-image", index))
                    .ghost()
                    .p_0()
                    .w_full()
                    .h_auto()
                    .min_h(px(0.))
                    .border_0()
                    .rounded(RADIUS_CARD)
                    .overflow_hidden()
                    .aspect_ratio(16. / 9.)
                    .bg(color(SURFACE_HIGH))
                    .accessibility_label(course2md::tr!("放大{}", label))
                    .disabled(frame.is_none())
                    // The image fills the frame itself, not the button's centered
                    // content box, and shows the whole slide.
                    .relative()
                    .child(
                        img(path.to_path_buf())
                            .absolute()
                            .inset_0()
                            .size_full()
                            .rounded(RADIUS_CARD)
                            .object_fit(ObjectFit::Contain)
                            .with_fallback(move || {
                                theme::accessible_text(
                                    ("failed-reader-image", index),
                                    course2md::tr!("截图无法读取"),
                                )
                                .into_any_element()
                            }),
                    )
                    .on_click(move |_, window, cx| {
                        if let Some(frame_index) = frame_index {
                            let _ = desktop.update(cx, |this, cx| {
                                this.open_reader_image(frame_index, window, cx);
                            });
                        }
                    }),
            )
            .children(seek)
            .into_any_element(),
    )
    .into_any_element()
}

impl Desktop {
    fn reader_exporting(&self) -> bool {
        self.reader_ui.export_state.pending.is_some()
    }

    pub(super) fn reader_viewer_open(&self) -> bool {
        self.reader_ui.viewer.is_some()
    }

    fn scroll_reader_page(&mut self, direction: f32, cx: &mut Context<Self>) {
        if self.result_tab == 0 {
            let step = self.reader_ui.note_list.viewport_bounds().size.height * 0.85 * direction;
            self.reader_ui.note_list.scroll_by(-step);
        } else {
            let step = self.reader_scroll.bounds().size.height * 0.85 * direction;
            let offset = self.reader_scroll.offset() + point(px(0.), step);
            self.reader_scroll.set_offset(offset);
        }
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
    fn capture_reading_position(&self) -> Option<workspace::ReadingPosition> {
        let preview = self.preview.as_ref()?;
        if self.result_tab == 0 {
            return note_capture_position(
                &self.reader_ui.note_items,
                &preview.blocks,
                &self.reader_ui.note_list,
                &self.reader_ui.note_layout.borrow(),
            );
        }
        capture_reader_position(
            &self.reader_ui.gallery_layout.borrow(),
            f32::from(self.reader_scroll.offset().y),
            |index| {
                self.reader_ui
                    .frames
                    .get(index)
                    .map(|frame| (Some(frame.anchor.clone()), Some(frame.seconds)))
                    .unwrap_or((None, None))
            },
        )
    }
    pub fn save_reading_position(&mut self, cx: &mut Context<Self>) {
        self.persist_reading_position(false, cx);
    }
    /// Polled while the reader scrolls: persist at most once per interval, so
    /// continuous scrolling does not clone and rewrite the workspace each tick.
    /// Navigation and exit call `save_reading_position` for an immediate flush.
    pub fn poll_reading_position(&mut self, cx: &mut Context<Self>) {
        self.persist_reading_position(true, cx);
    }
    fn persist_reading_position(&mut self, throttled: bool, cx: &mut Context<Self>) {
        if self.page != Page::Result
            || self.reading
            || (self.result_tab == 1 && self.reader_ui.gallery_search_position.is_some())
            || self.reader_ui.restoring
            || self.reader_ui.pending_restore.is_some()
        {
            return;
        }
        let (Some(key), Some(position)) = (self.reading_key(), self.capture_reading_position())
        else {
            return;
        };
        if self.reader_ui.last_position.as_ref() == Some(&(key.clone(), position.clone())) {
            return;
        }
        // Skipped ticks leave `last_position` stale, keeping the position dirty
        // for the next poll or the immediate flush on navigation and exit.
        if throttled
            && self
                .reader_position_saved_at
                .is_some_and(|saved| saved.elapsed() < READING_POSITION_INTERVAL)
        {
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
        self.reader_position_saved_at = Some(Instant::now());
        self.reader_ui.last_position = Some((key, position));
    }
    pub fn restore_reading_position(&mut self, cx: &mut Context<Self>) {
        self.ensure_reader_data(cx);
        self.reader_ui.pending_search = None;
        self.reader_ui.pending_restore = Some(
            self.reading_key()
                .and_then(|key| self.workspace.as_ref()?.state.positions.get(&key))
                .cloned()
                .unwrap_or_default(),
        );
        self.reader_ui.restore_generation += 1;
        self.reader_ui.restoring = false;
        self.reader_ui.last_position = None;
        cx.notify();
    }
    fn position_index(&self, position: &workspace::ReadingPosition) -> Option<usize> {
        if self.result_tab == 0 {
            note_position_index(&self.preview.as_ref()?.blocks, position)
        } else {
            position
                .paragraph
                .as_ref()
                .and_then(|anchor| {
                    self.reader_ui
                        .frames
                        .iter()
                        .position(|frame| frame.anchor == *anchor)
                })
                .or_else(|| {
                    position.seconds.and_then(|seconds| {
                        nav::nearest_time(
                            self.reader_ui
                                .frames
                                .iter()
                                .enumerate()
                                .map(|(i, frame)| (i, Some(frame.seconds))),
                            seconds,
                        )
                        .map(|(i, _)| i)
                    })
                })
        }
    }
    fn schedule_reader_restore(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.result_tab == 1 && self.reader_ui.data_loading {
            return;
        }
        let Some(position) = self.reader_ui.pending_restore.take() else {
            return;
        };
        self.reader_ui.restoring = true;
        let search = self.reader_ui.pending_search.take();
        let generation = self.reader_ui.restore_generation;
        let key = self.reading_key();
        cx.on_next_frame(window, move |this, _, cx| {
            if this.reader_ui.restore_generation != generation || this.reading_key() != key {
                return;
            }
            if this.result_tab == 0 {
                this.apply_note_restore(position, search, true, cx);
                return;
            }
            let layout = this.reader_ui.gallery_layout.borrow();
            let target = restored_reader_offset(&layout, this.position_index(&position), &position);
            drop(layout);
            let maximum = this.reader_scroll.max_offset().y;
            this.reader_scroll
                .set_offset(point(px(0.), px(target).max(-maximum).min(px(0.))));
            this.finish_reader_restore(cx);
        });
    }

    fn finish_reader_restore(&mut self, cx: &mut Context<Self>) {
        self.reader_ui.restoring = false;
        self.reader_ui.last_position = None;
        cx.notify();
    }

    /// Position the virtualized note flow at a saved position or search match.
    /// An item that has not been measured is scrolled to first; the precise
    /// in-item offset (the found line, or the fractional position) follows
    /// when its measurement arrives (`refine_note_restore`).
    fn apply_note_restore(
        &mut self,
        position: workspace::ReadingPosition,
        search: Option<Match>,
        may_refine: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(preview) = self.preview.clone() else {
            self.finish_reader_restore(cx);
            return;
        };
        let list = &self.reader_ui.note_list;
        let target = note_restore_target(
            &self.reader_ui.note_items,
            &preview.blocks,
            &position,
            search
                .as_ref()
                .map(|found| (found.block, found.range.start)),
            &self.reader_ui.note_layout.borrow(),
        );
        let Some((item_ix, precise)) = target else {
            list.scroll_to(ListOffset {
                item_ix: 0,
                offset_in_item: px(0.),
            });
            self.finish_reader_restore(cx);
            return;
        };
        let offset = precise.unwrap_or(-position.within);
        list.scroll_to(ListOffset {
            item_ix,
            offset_in_item: px(offset.max(0.)),
        });
        if precise.is_some() || !may_refine {
            self.finish_reader_restore(cx);
            return;
        }
        self.reader_ui
            .restore_target
            .set(note_item_block(self.reader_ui.note_items[item_ix]));
        self.reader_ui.note_refine = Some(NoteRefine {
            position,
            search,
            generation: self.reader_ui.restore_generation,
        });
        cx.notify();
    }

    fn refine_note_restore(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let Some(refine) = self.reader_ui.note_refine.take() else {
            return;
        };
        if refine.generation == self.reader_ui.restore_generation && self.result_tab == 0 {
            self.apply_note_restore(refine.position, refine.search, false, cx);
        }
    }

    /// Window resizes and text-scale changes reflow the reader: keep the
    /// reading position of the previous layout.
    fn reader_layout_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let layout = reader_window_layout(window);
        if self.reader_ui.layout == layout {
            return;
        }
        self.reader_ui.layout = layout;
        if self.page != Page::Result || self.preview.is_none() {
            return;
        }
        if self.reader_ui.pending_restore.is_none() && !self.reader_ui.restoring {
            self.reader_ui.pending_restore = self.capture_reading_position();
            self.reader_ui.restore_generation += 1;
        }
        self.reader_ui.note_layout.borrow_mut().clear();
        self.reader_ui.gallery_layout.borrow_mut().clear();
        self.reader_ui.note_list.remeasure();
        cx.notify();
    }
    pub(crate) fn sync_reader_tab_stops(&mut self) {
        for (index, focus) in self.reader_ui.view_focus.iter_mut().enumerate() {
            *focus = focus.clone().tab_stop(index == self.result_tab);
        }
    }
    pub(crate) fn ensure_reader_data(&mut self, cx: &mut Context<Self>) {
        let Some(preview) = &self.preview else {
            return;
        };
        if self.reader_ui.loaded.as_ref() == Some(&preview.course.dir) {
            return;
        }
        self.reader_ui.loaded = Some(preview.course.dir.clone());
        self.reader_ui
            .controls_scroll
            .set_offset(point(px(0.), px(0.)));
        self.reader_ui
            .information_scroll
            .set_offset(point(px(0.), px(0.)));
        self.reader_ui.frames = Rc::default();
        self.reader_ui.frame_by_path = Rc::default();
        self.reader_ui.missing_by_block = Rc::default();
        self.reader_ui.gallery_matches = Rc::default();
        self.reader_ui.versions.clear();
        self.reader_ui.issues.clear();
        self.reader_ui.source = None;
        // 换笔记：中止进行中的重新定位探测
        if let Some(cancel) = self.reader_ui.source_probe_cancel.take() {
            cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        self.reader_ui.source_loading = false;
        self.reader_ui.source_available = false;
        self.reader_ui.export_folder = None;
        self.reader_ui.offline_video = OfflineVideo::NotRequested;
        self.reader_ui.offline_opening = false;
        Rc::make_mut(&mut self.reader_ui.matches).clear();
        self.reader_ui.gallery_search_position = None;
        self.reader_ui.find_open = false;
        self.reader_ui.info_open = false;
        self.reader_ui.processing_details_open = false;
        self.rebuild_note_flow(cx);
        // Clearing an input needs the window, which this entity is being
        // updated inside; clear both searches once the update ends.
        let inputs = [
            self.reader_ui.find.clone(),
            self.reader_ui.gallery_find.clone(),
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

    /// The note's list items, focus containers, contents and chapter titles
    /// follow the loaded note.
    fn rebuild_note_flow(&mut self, cx: &mut Context<Self>) {
        let Some(preview) = self.preview.clone() else {
            return;
        };
        let items = note_items(&preview.blocks);
        let focus: Vec<FocusHandle> = (0..items.len()).map(|_| cx.focus_handle()).collect();
        let list = &self.reader_ui.note_list;
        list.reset(items.len());
        list.splice_focusable(0..items.len(), focus.iter().cloned().map(Some));
        self.reader_ui.note_items = Rc::new(items);
        self.reader_ui.note_focus = Rc::new(focus);
        self.reader_ui.outline = Rc::new(reader_outline(&preview));
        self.reader_ui.chapter_starts =
            Rc::new(chapter_starts(&preview.blocks, &summary_outline(&preview)));
        self.reader_ui.note_layout.borrow_mut().clear();
        self.reader_ui.gallery_layout.borrow_mut().clear();
    }

    fn update_gallery_matches(&mut self, cx: &App) {
        let query = self.reader_ui.gallery_find.read(cx).value();
        let matches = matching_frames(&self.reader_ui.frames, &query);
        self.reader_ui.gallery_excerpts = Rc::new(
            matches
                .iter()
                .map(|index| {
                    (
                        *index,
                        frame_excerpt(&self.reader_ui.frames[*index], &query),
                    )
                })
                .collect(),
        );
        self.reader_ui.gallery_matches = Rc::new(matches);
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
            // 解析全文与离线检查是同步文件/子进程工作；见 crate::spawn_blocking_io 的说明
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
                if this.reader_ui.pending_restore.is_none() && !this.reader_ui.restoring {
                    this.reader_ui.pending_restore = this.capture_reading_position();
                }
                this.reader_ui.frames = Rc::new(data.frames);
                this.reader_ui.frame_by_path = Rc::new(data.frame_by_path);
                this.reader_ui.missing_by_block = Rc::new(data.missing_by_block);
                this.update_gallery_matches(cx);
                this.reader_ui.gallery_layout.borrow_mut().clear();
                this.reader_ui.versions = data.versions;
                this.reader_ui.issues = data.issues;
                this.reader_ui.export_folder = data.export_folder;
                this.reader_ui.source = source;
                this.reader_ui.source_available = source_available;
                this.reader_ui.offline_video = offline_video;
                this.reader_ui.data_loading = false;
                this.reader_ui.restore_generation += 1;
                this.reader_ui.restoring = false;
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
    fn reader_source(&self) -> Option<nav::SourceTarget> {
        self.reader_ui.source.clone()
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
    fn open_reader_source(&mut self, seconds: Option<f64>, cx: &mut Context<Self>) {
        let Some(source) = self.reader_source() else {
            return;
        };
        if let Some(url) = seconds.and_then(|time| nav::seek_url(&source, time)) {
            cx.open_url(&url);
            return;
        }
        match source {
            nav::SourceTarget::Web(url) => cx.open_url(&url),
            nav::SourceTarget::Local(path) if path.is_file() => cx.open_with_system(&path),
            nav::SourceTarget::Local(path) => {
                self.message = Some(course2md::tr!(
                    "原视频不在 {}，请用「找到原视频…」指定新位置",
                    path.display()
                ));
                cx.notify();
            }
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
            let stored = cancel.clone();
            let probing = cancel.clone();
            let _ = this.update(cx, |this, cx| {
                // 前一次探测若还在跑，先取消再开始新的
                if let Some(previous) = this.reader_ui.source_probe_cancel.take() {
                    previous.store(true, std::sync::atomic::Ordering::Relaxed);
                }
                this.reader_ui.source_probe_cancel = Some(stored);
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
                // 只清自己的标志：更晚开始的探测持有另一个 Arc
                if this
                    .reader_ui
                    .source_probe_cancel
                    .as_ref()
                    .is_some_and(|current| Arc::ptr_eq(current, &cancel))
                {
                    this.reader_ui.source_probe_cancel = None;
                }
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
    pub fn open_reader_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.preview.is_none() {
            return;
        }
        if !self.reader_ui.find_open {
            self.reader_ui.find_return_focus = window.focused(cx);
        }
        let input = if self.result_tab == 0 {
            self.reader_ui.find_open = true;
            self.reader_ui.find.clone()
        } else {
            self.reader_ui.gallery_find.clone()
        };
        input.update(cx, |input, cx| input.focus(window, cx));
        if self.result_tab == 0 {
            self.update_reader_matches(cx);
        }
        cx.notify();
    }
    fn close_reader_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.reader_ui.find_open = false;
        let input = if self.result_tab == 0 {
            self.reader_ui.find.clone()
        } else {
            self.reader_ui.gallery_find.clone()
        };
        input.update(cx, |input, cx| input.set_value("", window, cx));
        if let Some(focus) = self.reader_ui.find_return_focus.take() {
            focus.focus(window, cx);
        } else {
            self.reader_ui.focus.focus(window, cx);
        }
        cx.notify();
    }
    fn update_reader_matches(&mut self, cx: &mut Context<Self>) {
        let query = self.reader_ui.find.read(cx).value().to_string();
        if let Some(preview) = &self.preview
            && self.reader_ui.search_index.len() != preview.blocks.len()
        {
            self.reader_ui.search_index = preview
                .blocks
                .iter()
                .map(|block| {
                    block_text(block)
                        .map(nav::prepare_text)
                        .unwrap_or_else(|| nav::prepare_text(""))
                })
                .collect();
        }
        self.reader_ui.matches = self
            .preview
            .as_ref()
            .map(|preview| {
                Rc::new(
                    preview
                        .blocks
                        .iter()
                        .enumerate()
                        .flat_map(|(block, _)| {
                            self.reader_ui
                                .search_index
                                .get(block)
                                .map(|prepared| nav::text_matches_prepared(prepared, &query))
                                .unwrap_or_default()
                                .into_iter()
                                .map(move |range| Match { block, range })
                        })
                        .collect::<Vec<_>>(),
                )
            })
            .unwrap_or_default();
        self.reader_ui.match_index = 0;
        cx.notify();
    }
    fn move_reader_match(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.result_tab != 0 {
            return;
        }
        let count = self.reader_ui.matches.len();
        if count == 0 {
            return;
        }
        self.reader_ui.match_index =
            (self.reader_ui.match_index as isize + delta).rem_euclid(count as isize) as usize;
        let found = self.reader_ui.matches[self.reader_ui.match_index].clone();
        let fraction = self
            .preview
            .as_ref()
            .and_then(|preview| preview.blocks.get(found.block))
            .and_then(block_text)
            .map(|text| {
                text[..found.range.start].chars().count() as f32
                    / text.chars().count().max(1) as f32
            })
            .unwrap_or(0.);
        self.jump_reader_block(found.block, fraction, cx);
        self.reader_ui.pending_search = Some(found);
    }
    fn jump_reader_block(&mut self, index: usize, fraction: f32, cx: &mut Context<Self>) {
        self.reader_ui.pending_search = None;
        if self.result_tab != 0 {
            self.save_reading_position(cx);
            self.result_tab = 0;
            self.sync_reader_tab_stops();
        }
        let Some(preview) = &self.preview else {
            return;
        };
        let Some(block) = preview.blocks.get(index) else {
            return;
        };
        self.reader_ui.pending_restore = Some(workspace::ReadingPosition {
            paragraph: Some(block_anchor(block, index)),
            seconds: block_time(&preview.blocks, index),
            fraction: Some(fraction),
            ..Default::default()
        });
        self.reader_ui.restore_generation += 1;
        self.reader_ui.restoring = false;
        cx.notify();
    }
    fn open_reader_version(&mut self, course: Course, cx: &mut Context<Self>) {
        self.load_reader_version(course, false, cx);
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
                        this.sync_reader_tab_stops();
                        if refresh {
                            this.reader_ui.loaded = None;
                        }
                        // 事件路径触发数据加载（渲染不再负责）
                        this.ensure_reader_data(cx);
                        let has_saved_position = this.reading_key().is_some_and(|key| {
                            this.workspace.as_ref().is_some_and(|workspace| {
                                workspace.state.positions.contains_key(&key)
                            })
                        });
                        this.restore_reading_position(cx);
                        if refresh {
                            this.reader_ui.pending_restore = position.clone();
                        }
                        if !refresh
                            && !has_saved_position
                            && let Some(seconds) = position.and_then(|position| position.seconds)
                        {
                            let blocks = &this.preview.as_ref().unwrap().blocks;
                            let nearest = nav::nearest_time(heading_times(blocks), seconds);
                            if let Some((index, exact)) = nearest {
                                this.reader_ui.pending_restore = Some(workspace::ReadingPosition {
                                    seconds: Some(seconds),
                                    fraction: Some(0.),
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
                                this.reader_ui.pending_restore = Some(Default::default());
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
    pub fn export_course(
        &mut self,
        course: Course,
        format: course2md::config::OutputFormat,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let version = course.dir.clone();
        let Some(generation) = self.reader_ui.export_state.begin(version.clone()) else {
            return;
        };
        let title = self.course_display_title(&course);
        let stem = title
            .chars()
            .map(|c| if "/\\:*?\"<>|".contains(c) { '_' } else { c })
            .collect::<String>();
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
        let source = course.dir.clone();
        let dismiss = quiet(SharedString::from(format!("{id}:dismiss")))
            .icon(icons::close())
            .w(CONTROL_HEIGHT)
            .px_0()
            .accessibility_label(course2md::tr!("关闭这份笔记的导出提示"))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.reader_ui.export_state.feedback.remove(&source);
                cx.notify();
            }));
        Some(match feedback.result {
            Ok(path) => {
                let label = exported_file_label(&path);
                h_flex()
                    .w_full()
                    .min_w_0()
                    .gap_2()
                    .items_center()
                    .child(
                        icons::check()
                            .size(ICON_SLOT)
                            .flex_shrink_0()
                            .text_color(color(SUCCESS)),
                    )
                    .child(
                        theme::accessible_text(SharedString::from(id.clone()), label.clone())
                            .aria_label(course2md::tr!("《{}》{}", feedback.title, label))
                            .flex_1()
                            .min_w_0()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_size(TEXT_AUX)
                            .font_weight(FontWeight::NORMAL)
                            .text_color(color(GRAY)),
                    )
                    .child(
                        quiet(SharedString::from(format!("{id}:reveal")))
                            .icon(icons::folder_open())
                            .label(course2md::tr!("打开导出文件夹"))
                            .on_click(move |_, _, cx| cx.reveal_path(&path)),
                    )
                    .child(dismiss)
            }
            Err(error) => {
                let retry = course.clone();
                v_flex()
                    .w_full()
                    .min_w_0()
                    .gap_1()
                    .child(
                        theme::accessible_text(
                            SharedString::from(id.clone()),
                            course2md::tr!("导出未完成"),
                        )
                        .text_size(TEXT_AUX)
                        .text_color(color(DANGER)),
                    )
                    .child(
                        theme::accessible_text(SharedString::from(format!("{id}:detail")), error)
                            .text_size(TEXT_AUX)
                            .font_weight(FontWeight::NORMAL)
                            .text_color(color(GRAY))
                            .whitespace_normal(),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(
                                outline_pill(SharedString::from(format!("{id}:retry")))
                                    .icon(icons::refresh())
                                    .label(course2md::tr!("重试导出"))
                                    .disabled(self.reader_exporting())
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.export_course(
                                            retry.clone(),
                                            feedback.format,
                                            window,
                                            cx,
                                        );
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
            self.sync_reader_tab_stops();
            if index == 1
                && !self
                    .reader_ui
                    .gallery_find
                    .read(cx)
                    .value()
                    .trim()
                    .is_empty()
            {
                self.reader_ui.pending_restore = None;
                self.reader_scroll.set_offset(point(px(0.), px(0.)));
            } else {
                self.restore_reading_position(cx);
            }
        }
        self.reader_ui.view_focus[index].focus(window, cx);
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
        self.schedule_reader_restore(window, cx);
        let outline = self.reader_ui.outline.clone();
        let metrics = ReaderMetrics::new(
            window,
            self.reader_ui.toc_open == Some(true),
            !outline.is_empty(),
        );
        let (controls, scrolling_title, tools) =
            self.reader_controls(&preview, &outline, metrics, window, cx);
        let article = if self.result_tab == 0 {
            self.reader_note_article(&preview, metrics, cx)
        } else {
            self.reader_gallery(&preview, metrics, scrolling_title, window, cx)
        };
        let body = self.reader_body(article, &outline, metrics, cx);
        (
            crate::motion::state_enter(
                SharedString::from(format!("reader-open:{}", preview.course.dir.display())),
                self.reader_root(metrics, cx)
                    .child(controls)
                    .child(v_flex().flex_1().min_h_0().w_full().child(body)),
            ),
            Some(tools.into_any_element()),
        )
    }

    fn reader_opening(&mut self, cx: &mut Context<Self>) -> AnyElement {
        crate::motion::enter(
            "reader-opening",
            v_flex()
                .w_full()
                .gap_4()
                .py_4()
                .child(
                    quiet("reader-loading-back")
                        .icon(crate::icons::arrow_left())
                        .label(course2md::tr!("返回"))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.navigate(this.result_origin, cx);
                        })),
                )
                .child(
                    h_flex()
                        .items_center()
                        .gap_2()
                        .child(crate::motion::spinner("reader-opening-spinner", cx))
                        .child(theme::accessible_text(
                            "opening-note",
                            course2md::tr!("正在打开笔记…"),
                        )),
                ),
        )
    }

    /// The page frame and its keyboard actions.
    fn reader_root(&mut self, metrics: ReaderMetrics, cx: &mut Context<Self>) -> Stateful<Div> {
        let toc_fits_beside = metrics.toc_fits_beside;
        v_flex()
            .id("reader-page")
            .track_focus(&self.reader_ui.focus)
            .key_context("CourseReader")
            .on_action(
                cx.listener(|this, _: &FindInNote, window, cx| this.open_reader_find(window, cx)),
            )
            .on_action(cx.listener(|this, _: &NextMatch, _, cx| this.move_reader_match(1, cx)))
            .on_action(cx.listener(|this, _: &PreviousMatch, _, cx| this.move_reader_match(-1, cx)))
            .on_action(cx.listener(|this, _: &ReaderPageUp, _, cx| this.scroll_reader_page(1., cx)))
            .on_action(
                cx.listener(|this, _: &ReaderPageDown, _, cx| this.scroll_reader_page(-1., cx)),
            )
            .on_action(cx.listener(|this, _: &ReaderStart, _, cx| {
                if this.result_tab == 0 {
                    this.reader_ui.note_list.scroll_to(ListOffset {
                        item_ix: 0,
                        offset_in_item: px(0.),
                    });
                } else {
                    this.reader_scroll.set_offset(point(px(0.), px(0.)));
                }
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ReaderEnd, _, cx| {
                if this.result_tab == 0 {
                    this.reader_ui.note_list.scroll_to_end();
                } else {
                    this.reader_scroll.scroll_to_bottom();
                }
                cx.notify();
            }))
            .on_action(cx.listener(move |this, _: &CloseFind, window, cx| {
                if !toc_fits_beside && this.reader_ui.toc_open == Some(true) {
                    this.reader_ui.toc_open = Some(false);
                    cx.notify();
                } else if (this.result_tab == 0 && this.reader_ui.find_open)
                    || (this.result_tab == 1
                        && !this.reader_ui.gallery_find.read(cx).value().is_empty())
                {
                    this.close_reader_find(window, cx);
                } else {
                    cx.propagate();
                }
            }))
            .h_full()
            .min_h_0()
            .w_full()
            .max_w(px(metrics.reader_width))
            .mx_auto()
            .gap_3()
            .when(metrics.compact, |view| view.gap_2())
    }

    /// Tool bar, title, facts, export feedback, notices and the details panel.
    /// Compact layouts return the title to scroll with the screenshot grid.
    fn reader_controls(
        &mut self,
        preview: &Rc<notes::Preview>,
        outline: &[(usize, String, Option<f64>)],
        metrics: ReaderMetrics,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (AnyElement, Option<AnyElement>, Div) {
        let ReaderMetrics {
            compact,
            short,
            available_height,
            ..
        } = metrics;
        // Short windows reserve fixed space for reading controls. Document
        // identity scrolls with the content instead of consuming a second header.
        let controls_scroll = self.reader_ui.controls_scroll.clone();
        let mut page = v_flex()
            .id("reader-controls")
            .w_full()
            .min_w_0()
            .gap_3()
            .flex_shrink_0()
            .when(compact, |view| view.gap_1())
            // The header is as tall as its content; only the details panel
            // below it is bounded.
            .when(!compact, |view| view.pt_2().pb(rems(32. / 14.)));
        let ReaderHeaderParts { title, meta } = self.reader_metadata_row(
            preview,
            compact,
            short,
            if compact {
                HeaderContainer::Fixed
            } else {
                HeaderContainer::Scroll(controls_scroll.clone())
            },
            cx,
        );
        let (heading, scrolling_title) = if compact {
            (None, Some(title))
        } else {
            (Some(title), None)
        };
        let toolbar = self.reader_toolbar(preview, outline, &metrics, window, cx);
        if toolbar.compact_search.is_some() || toolbar.short_matches.is_some() {
            page = page.child(
                v_flex()
                    .w_full()
                    .min_w_0()
                    .gap_2()
                    .pb_2()
                    .children(toolbar.compact_search)
                    .children(toolbar.short_matches),
            );
        }
        // The return leads the tool bar; title and facts share the article's
        // left edge below it.
        // The note tab draws its header as the list's first item, so it
        // scrolls away with the text instead of collapsing above it, which
        // would move every line by the header's height.
        let header_in_list = self.result_tab == 0;
        let (inline_meta, panel_meta) = if short { (None, meta) } else { (meta, None) };
        if header_in_list {
            page = page.pb_2();
        } else {
            page = page.children(heading).children(inline_meta);
        }
        let (fixed_header, mut page) = if compact {
            (
                Some(page),
                v_flex()
                    .id("reader-extra-controls")
                    .w_full()
                    .min_w_0()
                    .min_h_0()
                    .flex_shrink_0()
                    .gap_2()
                    .max_h(px(available_height * 0.28))
                    .overflow_y_scroll()
                    .track_scroll(&controls_scroll),
            )
        } else {
            (None, page)
        };
        page = page.children(self.course_export_feedback(&preview.course, cx));
        if self.reading {
            page = page.child(crate::motion::enter(
                "reader-version-loading",
                h_flex()
                    .items_center()
                    .gap_2()
                    .child(crate::motion::spinner("reader-version-spinner", cx))
                    .child(theme::accessible_text(
                        "reader-version-loading-label",
                        course2md::tr!("正在切换版本…"),
                    )),
            ));
        }
        page = page.children(self.reader_notices(preview, &controls_scroll, cx));
        let information_panel =
            self.reader_info_panel(preview, compact, panel_meta, available_height, window, cx);
        let controls = v_flex()
            .w_full()
            .min_w_0()
            .min_h_0()
            .flex_shrink_0()
            .children(fixed_header)
            .child(page)
            .when(self.reader_ui.info_open, |view| {
                view.gap_2().children(information_panel)
            });
        (controls.into_any_element(), scrolling_title, toolbar.bar)
    }

    /// The note tab's article: a variable-height list that builds only the
    /// visible window of blocks and its overdraw. One text measure, regular
    /// 1.7-line body copy and spacing between sections, as in the HTML export.
    fn reader_note_article(
        &mut self,
        preview: &Rc<notes::Preview>,
        metrics: ReaderMetrics,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // Side-by-side sections need room for a legible screenshot and measure.
        let design_px = metrics.reader_width / metrics.rem * 14.;
        let toc_beside = self.reader_ui.toc_open == Some(true) && metrics.toc_fits_beside;
        let wide = design_px - if toc_beside { 260. } else { 0. } >= 840.;
        let flow = NoteFlow {
            items: self.reader_ui.note_items.clone(),
            focus: self.reader_ui.note_focus.clone(),
            preview: preview.clone(),
            frames: self.reader_ui.frames.clone(),
            frame_by_path: self.reader_ui.frame_by_path.clone(),
            missing_by_block: self.reader_ui.missing_by_block.clone(),
            chapter_starts: self.reader_ui.chapter_starts.clone(),
            source: self.reader_source(),
            matches: self.reader_ui.matches.clone(),
            match_index: self.reader_ui.match_index,
            find_open: self.reader_ui.find_open,
            data_loading: self.reader_ui.data_loading,
            compact: metrics.compact,
            short: metrics.short,
            wide,
            layout: self.reader_ui.note_layout.clone(),
            restore_target: self.reader_ui.restore_target.clone(),
            note_list: self.reader_ui.note_list.clone(),
            desktop: cx.weak_entity(),
        };
        // The page re-renders when the top item changes: the header collapses
        // once the article leaves its top, and the outline follows the text.
        // GPUI calls this while it still borrows the list state, so the
        // position is read once the scroll has been applied.
        let entity = cx.weak_entity();
        let list_state = self.reader_ui.note_list.clone();
        let items = self.reader_ui.note_items.clone();
        let last = Rc::new(std::cell::Cell::new(note_scroll_key(&items, &list_state)));
        self.reader_ui
            .note_list
            .set_scroll_handler(move |_, _, cx| {
                let (entity, list_state, items, last) = (
                    entity.clone(),
                    list_state.clone(),
                    items.clone(),
                    last.clone(),
                );
                cx.defer(move |cx| {
                    let key = note_scroll_key(&items, &list_state);
                    if last.replace(key) != key {
                        let _ = entity.update(cx, |_, cx| cx.notify());
                    }
                });
            });
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
            .max_w(READER_MEASURE)
            .child(
                list(self.reader_ui.note_list.clone(), move |index, _, cx| {
                    render_note_item(&flow, index, cx)
                })
                .w_full()
                .h_full()
                .py_3(),
            )
            .child(crate::backend::vertical_scrollbar_for(
                &self.reader_ui.note_list,
            ))
            .into_any_element()
    }

    /// The article with the contents rail beside it, or over it when narrow.
    /// Reading starts with an uninterrupted article; contents are an explicit choice.
    fn reader_body(
        &mut self,
        article: AnyElement,
        outline: &[(usize, String, Option<f64>)],
        metrics: ReaderMetrics,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let toc_open =
            self.reader_ui.toc_open.unwrap_or(false) && !outline.is_empty() && self.result_tab == 0;
        if !toc_open {
            return article;
        }
        let top = self.reader_ui.note_list.logical_scroll_top();
        let top_block = note_top_block(&self.reader_ui.note_items, top.item_ix).unwrap_or(0);
        let current = outline
            .iter()
            .rev()
            .find(|(index, _, _)| *index <= top_block)
            .map(|(index, _, _)| *index);
        if metrics.toc_fits_beside {
            h_flex()
                .flex_1()
                .min_h_0()
                .w_full()
                .gap(TOC_GAP)
                .items_stretch()
                .child(article)
                .child(self.reader_toc_panel(outline, current, true, cx))
                .into_any_element()
        } else {
            div()
                .relative()
                .flex_1()
                .min_h_0()
                .w_full()
                .h_full()
                .child(article)
                .child(
                    div()
                        .absolute()
                        .inset_0()
                        .bg(color(SURFACE))
                        .child(self.reader_toc_panel(outline, current, false, cx)),
                )
                .into_any_element()
        }
    }
}

/// Reader geometry at the current window size and text scale.
#[derive(Clone, Copy)]
struct ReaderMetrics {
    reader_width: f32,
    toc_fits_beside: bool,
    available_height: f32,
    compact: bool,
    short: bool,
    rem: f32,
}

const TOC_GAP: Pixels = px(24.);
const GALLERY_GAP: f32 = 16.;
const TOC_ROW_PADDING: Rems = rems(8. / 14.);
/// Space the shell keeps above and below the page, under its title bar.
const PAGE_VERTICAL_PADDING: Pixels = px(48.);
const TOC_BESIDE_MIN_WIDTH: Rems = rems(56.);
const COMPACT_MIN_WIDTH: Rems = rems(60.);
const COMPACT_MIN_HEIGHT: Rems = rems(36.);
const SHORT_MAX_HEIGHT: Rems = rems(24.);
/// Inline actions inside text rows are shorter than standalone controls.
const INLINE_ACTION_HEIGHT: Rems = rems(1.6);

impl ReaderMetrics {
    fn new(window: &Window, toc_open: bool, has_outline: bool) -> Self {
        let rem = window.rem_size();
        let pixels = |value: Rems| f32::from(value.to_pixels(rem));
        let gap = f32::from(TOC_GAP);
        // A single frame aligns navigation, title, tools and document content.
        // Reserve the same reading axis when the user hides the adjacent contents;
        // a note without contents keeps the frame at the text measure so the
        // tools end where the text ends.
        let measure = pixels(READER_MEASURE);
        let frame_width = crate::views::shell_content_width(Page::Result, window)
            .min(measure + pixels(TOC_PANEL) + gap);
        let available_height = f32::from(
            window.bounds().size.height - title_bar_height(window) - PAGE_VERTICAL_PADDING,
        )
        .max(0.);
        Self {
            reader_width: if toc_open && has_outline {
                frame_width
            } else {
                frame_width.min(measure)
            },
            toc_fits_beside: frame_width >= pixels(TOC_BESIDE_MIN_WIDTH) + gap,
            available_height,
            compact: frame_width < pixels(COMPACT_MIN_WIDTH)
                || available_height < pixels(COMPACT_MIN_HEIGHT),
            short: available_height < pixels(SHORT_MAX_HEIGHT),
            rem: f32::from(rem),
        }
    }
}

/// Where the note header is drawn, so a focused control in it can be revealed.
enum HeaderContainer {
    Fixed,
    Scroll(ScrollHandle),
    List(ListState),
}

struct ReaderHeaderParts {
    title: AnyElement,
    meta: Option<AnyElement>,
}

struct ReaderToolbarParts {
    bar: Div,
    compact_search: Option<Div>,
    short_matches: Option<Div>,
}

impl Desktop {
    /// Where the reader returns to, named for its crumb in the title bar.
    pub(crate) fn reader_origin_label(&self) -> &'static str {
        match self.result_origin {
            Page::New => course2md::tr!("主页"),
            Page::Task => course2md::tr!("任务"),
            Page::Settings => course2md::tr!("设置"),
            _ => course2md::tr!("笔记库"),
        }
    }

    fn relocate_source_button(&self, id: &'static str, cx: &mut Context<Self>) -> Button {
        let loading = self.reader_ui.source_loading;
        quiet(id)
            .icon(crate::icons::folder_open())
            .label(if loading {
                course2md::tr!("正在核对视频…")
            } else {
                course2md::tr!("找到原视频…")
            })
            .loading(loading)
            .disabled(loading)
            .on_click(cx.listener(|this, _, window, cx| this.relocate_reader_source(window, cx)))
    }

    /// Return row, title and the meta facts line. Compact layouts get the meta
    /// line back for the information panel instead of an inline row.
    fn reader_metadata_row(
        &mut self,
        preview: &Rc<notes::Preview>,
        compact: bool,
        short_reader: bool,
        container: HeaderContainer,
        cx: &mut Context<Self>,
    ) -> ReaderHeaderParts {
        let reveal = |id: &'static str, child: AnyElement| match &container {
            HeaderContainer::Fixed => child,
            HeaderContainer::Scroll(scroll) => {
                crate::focus_scroll::RevealFocus::new(id, child, scroll.clone())
                    .inline()
                    .into_any_element()
            }
            HeaderContainer::List(list) => {
                crate::focus_scroll::RevealFocus::in_list(id, child, list.clone())
                    .inline()
                    .into_any_element()
            }
        };
        // As on the canvas: the source and facts, the title in the reading
        // serif, then the ways to watch the video.
        let meta = &preview.document.meta;
        let platform = if meta.is_local() {
            Some(course2md::tr!("本地视频"))
        } else {
            crate::views::video_site_name(&meta.webpage_url)
        };
        let facts = preview.facts.join(" · ");
        let title = v_flex()
            .w_full()
            .min_w_0()
            .gap(rems(1.))
            .when(platform.is_some() || !facts.is_empty(), |header| {
                header.child(
                    h_flex()
                        .gap_2()
                        .flex_wrap()
                        .items_center()
                        .children(platform.map(|platform| {
                            div()
                                .h(rems(24. / 14.))
                                .px_2()
                                .flex()
                                .items_center()
                                .rounded(px(6.))
                                .bg(color(SURFACE_HIGH))
                                .text_size(TEXT_AUX)
                                .font_weight(FontWeight::MEDIUM)
                                .child(platform)
                        }))
                        .when(!facts.is_empty(), |row| {
                            row.child(
                                theme::accessible_text("reader-facts", facts.clone())
                                    .text_color(color(GRAY)),
                            )
                        }),
                )
            })
            .child(
                theme::accessible_text("reader-title", preview.course.title.clone())
                    .role(Role::Heading)
                    .min_w_0()
                    .whitespace_normal()
                    .font_family(READER_SERIF)
                    .text_size(if compact {
                        rems(30. / 14.)
                    } else {
                        rems(40. / 14.)
                    })
                    .line_height(relative(1.3))
                    .font_weight(FontWeight::SEMIBOLD),
            )
            .when(short_reader, |title| title.flex_shrink_0())
            .into_any_element();
        let mut meta_facts = h_flex().gap_2().flex_wrap().items_center();
        let offline = matches!(self.reader_ui.offline_video, OfflineVideo::Available(_));
        let play = |id: &'static str| {
            tonal_pill(id, cx)
                .icon(icons::play_arrow())
                .label(course2md::tr!("播放原视频"))
        };
        if offline {
            meta_facts = meta_facts.child(reveal(
                "reveal-reader-offline-video",
                play("reader-offline-video")
                    .loading(self.reader_ui.offline_opening)
                    .disabled(self.reader_ui.offline_opening)
                    .on_click(cx.listener(|this, _, _, cx| this.play_reader_offline_video(cx)))
                    .into_any_element(),
            ));
        }
        if let Some(source) = self.reader_source() {
            match &source {
                nav::SourceTarget::Web(url) => {
                    let open = if offline {
                        quiet("reader-source")
                            .text_color(color(ACCENT))
                            .icon(icons::external_link())
                            .label(match platform {
                                Some(platform) => course2md::tr!("在 {} 打开", platform),
                                None => course2md::tr!("在浏览器中打开").to_owned(),
                            })
                    } else {
                        play("reader-source")
                    };
                    meta_facts = meta_facts.child(reveal(
                        "reveal-reader-source",
                        open.accessibility_label(course2md::tr!("打开原视频：{}", url))
                            .on_click(
                                cx.listener(|this, _, _, cx| this.open_reader_source(None, cx)),
                            )
                            .into_any_element(),
                    ));
                }
                nav::SourceTarget::Local(path) if !offline => {
                    let exists = self.reader_ui.source_available;
                    meta_facts = meta_facts
                        .when(exists, |row| {
                            row.child(reveal(
                                "reveal-reader-source",
                                play("reader-source")
                                    .tooltip(path.display().to_string())
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.open_reader_source(None, cx)
                                    }))
                                    .into_any_element(),
                            ))
                        })
                        .when(!exists, |row| {
                            row.child(
                                theme::accessible_text(
                                    "missing-original-video",
                                    course2md::tr!("原视频不可用"),
                                )
                                .text_size(TEXT_AUX)
                                .text_color(color(GRAY)),
                            )
                            .child(reveal(
                                "reveal-relocate-reader-source",
                                self.relocate_source_button("relocate-reader-source", cx)
                                    .into_any_element(),
                            ))
                        });
                }
                nav::SourceTarget::Local(_) => {}
            }
        }
        if self.reader_ui.offline_video == OfflineVideo::Missing {
            meta_facts = meta_facts
                .child(
                    theme::accessible_text(
                        "reader-offline-video-missing",
                        course2md::tr!("保留的视频已不在"),
                    )
                    .text_size(TEXT_AUX)
                    .text_color(color(GRAY)),
                )
                .child(reveal(
                    "reveal-retry-reader-offline-video",
                    quiet("retry-reader-offline-video")
                        .icon(icons::refresh())
                        .label(course2md::tr!("重试"))
                        .loading(self.reader_ui.offline_opening)
                        .disabled(self.reader_ui.offline_opening)
                        .on_click(cx.listener(|this, _, _, cx| this.play_reader_offline_video(cx)))
                        .into_any_element(),
                ));
        }
        // A single version needs no selector. Keep its revision and date in
        // generation information; multiple versions retain a visible choice.
        if let Some(manifest) = &preview.course.manifest {
            meta_facts = meta_facts.child(div().flex_1());
            if self.reader_ui.versions.len() > 1 {
                let weak = cx.weak_entity();
                let versions = self.reader_ui.versions.clone();
                let current = preview.course.dir.clone();
                meta_facts = meta_facts.child(reveal(
                    "reveal-choose-note-version",
                    (quiet("choose-note-version")
                        .icon(icons::history())
                        .label(course2md::tr!("第 {} 版", manifest.revision))
                        .child(crate::icons::chevron_down().size_4().flex_shrink_0())
                        .min_h(INLINE_ACTION_HEIGHT)
                        .disabled(self.reading)
                        .dropdown_menu(move |menu, _, _| {
                            let mut menu = menu;
                            for version in &versions {
                                let weak = weak.clone();
                                let course = version.course.clone();
                                let label = if course.dir == current {
                                    course2md::tr!("{}（当前）", version.label)
                                } else {
                                    version.label.clone()
                                };
                                menu = menu.item(
                                    PopupMenuItem::new(label)
                                        .icon(icons::history())
                                        .checked(course.dir == current)
                                        .on_click(move |_, _, cx| {
                                            let _ = weak.update(cx, |this, cx| {
                                                this.open_reader_version(course.clone(), cx)
                                            });
                                        }),
                                );
                            }
                            menu
                        }))
                    .into_any_element(),
                ));
            }
        }
        let meta = if !facts.is_empty()
            || self.reader_source().is_some()
            || self.reader_ui.offline_video != OfflineVideo::NotRequested
            || preview.course.manifest.is_some()
        {
            Some(meta_facts.into_any_element())
        } else {
            None
        };
        ReaderHeaderParts { title, meta }
    }

    /// Find field, match navigation, view switcher, contents toggle, export
    /// menu and the overflow menu.
    fn reader_toolbar(
        &mut self,
        preview: &Rc<notes::Preview>,
        headings: &[(usize, String, Option<f64>)],
        metrics: &ReaderMetrics,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> ReaderToolbarParts {
        let (compact, short_reader) = (metrics.compact, metrics.short);
        let reading_note = self.result_tab == 0;
        let (search, short_matches) = self.reader_find_controls(short_reader, window, cx);
        let mut toolbar =
            h_flex()
                .flex_shrink_0()
                .gap_2()
                .items_center()
                .when(!short_reader, |row| {
                    row.child(
                        SingleChoiceGroup::new("reader-tabs", course2md::tr!("阅读视图"))
                            .tabs()
                            .options([
                                ("note", course2md::tr!("笔记")),
                                ("images", course2md::tr!("截图")),
                            ])
                            .icon("note", icons::article())
                            .icon("images", icons::image())
                            .focus_handles(self.reader_ui.view_focus.iter().cloned())
                            .selected(if reading_note { "note" } else { "images" })
                            .on_change(cx.listener(|this, selected: &SharedString, window, cx| {
                                this.select_reader_view(
                                    usize::from(selected.as_ref() == "images"),
                                    window,
                                    cx,
                                );
                            })),
                    )
                });
        let search_visible = !reading_note || self.reader_ui.find_open;
        let mut search_row = h_flex()
            .min_w_0()
            .gap_2()
            .items_center()
            .when(search_visible, |row| row.w(rems(240. / 14.)).child(search))
            .when(!search_visible, |row| {
                row.child(
                    quiet("reader-open-find")
                        .icon(icons::search())
                        .w(CONTROL_HEIGHT)
                        .px_0()
                        .accessibility_label(course2md::tr!("查找笔记内容"))
                        .tooltip(format!(
                            "{} {}",
                            course2md::tr!("查找笔记内容"),
                            if cfg!(target_os = "macos") {
                                "⌘F"
                            } else {
                                "Ctrl+F"
                            }
                        ))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.open_reader_find(window, cx);
                        })),
                )
            });
        if !headings.is_empty() && reading_note {
            let toc_on = self.reader_ui.toc_open.unwrap_or(false);
            search_row = search_row.child(
                quiet("note-contents")
                    .icon(icons::toc())
                    .label(course2md::tr!("目录"))
                    .selected(toc_on)
                    .accessibility_label(if toc_on {
                        course2md::tr!("收起目录")
                    } else {
                        course2md::tr!("打开目录")
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.reader_ui.pending_restore = this.capture_reading_position();
                        this.reader_ui.restore_generation += 1;
                        this.reader_ui.toc_open = Some(!toc_on);
                        cx.notify();
                    })),
            );
        }
        let (compact_search, short_search) = if short_reader {
            (None, Some(search_row))
        } else if compact {
            (Some(search_row.w_full()), None)
        } else {
            toolbar = toolbar.child(search_row);
            (None, None)
        };
        let output_weak = cx.entity().downgrade();
        let exporting = self.reader_exporting();
        let exporting_current = self
            .reader_ui
            .export_state
            .pending
            .as_ref()
            .is_some_and(|(_, source)| *source == preview.course.dir);
        let export_course = preview.course.clone();
        let export_folder = self.reader_ui.export_folder.clone();
        if !short_reader {
            toolbar = toolbar.child(
                outline_pill("export-note")
                    .icon(icons::download())
                    .label(if exporting_current {
                        course2md::tr!("正在导出…")
                    } else {
                        course2md::tr!("导出")
                    })
                    .accessibility_label(course2md::tr!("复制或导出整份笔记"))
                    .child(crate::icons::chevron_down().size_4().flex_shrink_0())
                    .loading(exporting_current)
                    .disabled(exporting)
                    .dropdown_menu(move |menu, _, _| {
                        reader_export_items(
                            menu,
                            output_weak.clone(),
                            export_course.clone(),
                            export_folder.clone(),
                            exporting,
                        )
                    }),
            );
        }
        toolbar = toolbar
            .child(self.reader_more_menu(preview, short_reader, cx))
            .children(short_search);
        ReaderToolbarParts {
            bar: toolbar,
            compact_search,
            short_matches,
        }
    }

    /// The search field for the current view and its match count; short
    /// windows return the count and match navigation as a separate row.
    fn reader_find_controls(
        &mut self,
        short_reader: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> (Div, Option<Div>) {
        let reading_note = self.result_tab == 0;
        let input = if reading_note {
            self.reader_ui.find.clone()
        } else {
            self.reader_ui.gallery_find.clone()
        };
        let mut search = h_flex()
            .min_w_0()
            .flex_1()
            .when(!short_reader, |row| row.flex_basis(rems(18.)))
            .gap_1()
            .items_center()
            .child(theme::search_field(
                &input,
                if reading_note {
                    course2md::tr!("查找笔记内容")
                } else {
                    course2md::tr!("搜索截图对应文字")
                },
                CONTROL_HEIGHT,
                window,
                cx,
            ));
        let mut short_matches = None;
        if !input.read(cx).value().trim().is_empty() {
            let count = if reading_note {
                self.reader_ui.matches.len()
            } else {
                self.reader_ui.gallery_matches.len()
            };
            let mut matches = h_flex().gap_1().items_center().child(
                theme::accessible_text(
                    "find-status",
                    if reading_note && count > 0 {
                        format!("{}/{count}", self.reader_ui.match_index + 1)
                    } else if count == 0 {
                        // 笔记与图片搜索的零结果用同一说法
                        course2md::tr!("无匹配").into()
                    } else {
                        course2md::tr!("{} 张", count)
                    },
                )
                .flex_shrink_0()
                .whitespace_nowrap()
                .text_size(TEXT_AUX)
                .text_color(color(GRAY)),
            );
            if reading_note {
                for (id, label, icon, delta) in [
                    (
                        "previous-match",
                        course2md::tr!("上一处"),
                        crate::icons::arrow_up(),
                        -1,
                    ),
                    (
                        "next-match",
                        course2md::tr!("下一处"),
                        crate::icons::arrow_down(),
                        1,
                    ),
                ] {
                    matches =
                        matches.child(
                            quiet(id)
                                .icon(icon)
                                .w(CONTROL_HEIGHT)
                                .px_0()
                                .accessibility_label(label)
                                .tooltip(label)
                                .disabled(count == 0)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.move_reader_match(delta, cx)
                                })),
                        );
                }
            }
            if short_reader {
                short_matches = Some(matches.w_full().justify_end());
            } else {
                search = search.child(matches);
            }
        }
        (search, short_matches)
    }

    /// The overflow menu; on short windows it also switches views and exports.
    fn reader_more_menu(
        &mut self,
        preview: &Rc<notes::Preview>,
        short_reader: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let reading_note = self.result_tab == 0;
        let exporting = self.reader_exporting();
        let more_weak = cx.weak_entity();
        let original = self.reader_source();
        let has_source = original.is_some();
        let missing_local = matches!(original, Some(nav::SourceTarget::Local(_)))
            && !self.reader_ui.source_available;
        let exported = self.reader_ui.exports.get(&preview.course.dir).cloned();
        let short_export_course = preview.course.clone();
        let short_export_folder = self.reader_ui.export_folder.clone();
        quiet("reader-more")
            .when(short_reader, |button| {
                button.track_focus(&self.reader_ui.menu_focus)
            })
            .icon(if short_reader {
                if reading_note {
                    icons::article()
                } else {
                    icons::image()
                }
            } else {
                icons::ellipsis()
            })
            .when(short_reader, |button| {
                button
                    .label(if reading_note {
                        course2md::tr!("笔记")
                    } else {
                        course2md::tr!("截图")
                    })
                    .child(crate::icons::chevron_down().size_4().flex_shrink_0())
            })
            .when(!short_reader, |button| button.w(CONTROL_HEIGHT).px_0())
            .accessibility_label(if short_reader {
                course2md::tr!("阅读视图与笔记操作")
            } else {
                course2md::tr!("更多笔记操作")
            })
            .dropdown_menu(move |menu, window, cx| {
                let details = more_weak.clone();
                let source = more_weak.clone();
                let files = more_weak.clone();
                let mut menu = menu;
                if short_reader {
                    for (index, label, icon) in [
                        (0, course2md::tr!("笔记"), icons::article()),
                        (1, course2md::tr!("截图"), icons::image()),
                    ] {
                        let weak = more_weak.clone();
                        menu = menu.item(
                            PopupMenuItem::new(label)
                                .icon(icon)
                                .checked((index == 0) == reading_note)
                                .on_click(move |_, window, cx| {
                                    let _ = weak.update(cx, |this, cx| {
                                        this.select_reader_view(index, window, cx);
                                        this.reader_ui.menu_focus.focus(window, cx);
                                    });
                                }),
                        );
                    }
                    let output = more_weak.clone();
                    let course = short_export_course.clone();
                    let folder = short_export_folder.clone();
                    menu = menu
                        .separator()
                        .submenu_with_icon(
                            Some(icons::download()),
                            course2md::tr!("复制与导出"),
                            window,
                            cx,
                            move |menu, _, _| {
                                reader_export_items(
                                    menu,
                                    output.clone(),
                                    course.clone(),
                                    folder.clone(),
                                    exporting,
                                )
                            },
                        )
                        .separator();
                }
                menu = menu
                    .item(
                        PopupMenuItem::new(course2md::tr!("笔记详情"))
                            .icon(icons::info())
                            .on_click(move |_, _, cx| {
                                let _ = details.update(cx, |this, cx| {
                                    this.reader_ui.info_open = !this.reader_ui.info_open;
                                    cx.notify();
                                });
                            }),
                    )
                    .when(has_source, |menu| {
                        menu.item(
                            PopupMenuItem::new(if missing_local {
                                course2md::tr!("找到原视频…")
                            } else {
                                course2md::tr!("打开原视频")
                            })
                            .icon(icons::movie())
                            .on_click(move |_, window, cx| {
                                let _ = source.update(cx, |this, cx| {
                                    if missing_local {
                                        this.relocate_reader_source(window, cx);
                                    } else {
                                        this.open_reader_source(None, cx);
                                    }
                                });
                            }),
                        )
                    })
                    .separator()
                    .item(
                        PopupMenuItem::new(course2md::tr!("显示笔记保存位置"))
                            .icon(icons::folder_open())
                            .on_click(move |_, _, cx| {
                                let _ = files.update(cx, |this, cx| {
                                    if let Some(preview) = &this.preview {
                                        cx.reveal_path(&preview.course.dir);
                                    }
                                });
                            }),
                    );
                if let Some(path) = exported.clone() {
                    menu = menu.item(
                        PopupMenuItem::new(course2md::tr!("显示上次导出的文件"))
                            .icon(icons::folder_open())
                            .on_click(move |_, _, cx| cx.reveal_path(&path)),
                    );
                }
                menu
            })
    }

    /// Collapsible note details: revision, source video, subtitle and model facts.
    fn reader_info_panel(
        &mut self,
        preview: &Rc<notes::Preview>,
        compact: bool,
        compact_metadata: Option<AnyElement>,
        available_height: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let mut details = Vec::new();
        let local_source = self.reader_source().and_then(|source| match source {
            nav::SourceTarget::Local(path) => Some(path),
            _ => None,
        });
        if let Some(manifest) = &preview.course.manifest {
            let date = nav::date_local(manifest.created_at_ms);
            details.push((
                course2md::tr!("版本"),
                icons::history(),
                course2md::tr!("第 {} 版 · {} 生成", manifest.revision, date),
            ));
        }
        if let Some(path) = &local_source {
            details.push((
                course2md::tr!("原视频"),
                icons::movie(),
                path.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
            ));
        }
        if let Some(manifest) = &preview.course.manifest
            && let Some(task) = self.workspace.as_ref().and_then(|workspace| {
                workspace
                    .state
                    .tasks
                    .iter()
                    .find(|task| task.id == manifest.task_id)
            })
        {
            if let Some(subtitle) = &task.plan.source.selected_subtitle {
                details.push((
                    course2md::tr!("所选字幕"),
                    icons::subtitles(),
                    subtitle.label.clone(),
                ));
            }
            if task.plan.config.defaults.transcript_source
                == Some(course2md::config::TranscriptSource::Asr)
            {
                if let Some(model) = &task.plan.config.defaults.asr_model {
                    details.push((course2md::tr!("语音识别模型"), icons::mic(), model.clone()));
                }
                if let Some(provider) = task.plan.config.defaults.provider {
                    details.push((
                        course2md::tr!("识别设备"),
                        icons::computer(),
                        crate::provider_label(Some(provider)).to_owned(),
                    ));
                }
            }
            if task.plan.config.llm.uses_service() {
                details.push((
                    course2md::tr!("AI 模型"),
                    icons::auto_fix(),
                    task.plan.config.llm.model.clone(),
                ));
            }
        }
        if !details.is_empty() || compact_metadata.is_some() {
            let information_scroll = self.reader_ui.information_scroll.clone();
            // The panel is at most 16rem tall; its scrolling viewport is what
            // remains after the header row, the padding, the row gap and the
            // 1px border on each edge.
            let rem_size = f32::from(window.rem_size());
            let frame_height = (available_height * 0.42).min(rem_size * 16.);
            let padding = if compact {
                rem_size * 0.5
            } else {
                rem_size * 0.75
            };
            let viewport_height = (frame_height
                - f32::from(CONTROL_HEIGHT.to_pixels(window.rem_size()))
                - padding * 2.
                - rem_size * 0.5
                - 2.)
                .max(rem_size * 2.);
            let mut content =
                v_flex()
                    .w_full()
                    .min_w_0()
                    .gap_2()
                    .pr(SCROLLBAR_GUTTER)
                    .children(details.into_iter().enumerate().map(
                        |(index, (label, icon, value))| {
                            detail_row(
                                ("reader-info-label", index),
                                label,
                                icon,
                                theme::accessible_text(("reader-info-value", index), value)
                                    .whitespace_normal()
                                    .text_size(TEXT_BODY)
                                    .font_weight(FontWeight::NORMAL),
                            )
                            .flex_shrink_0()
                        },
                    ))
                    .children(compact_metadata);
            if local_source.is_some() && self.reader_ui.source_available {
                content = content.child(
                    crate::focus_scroll::RevealFocus::new(
                        "reveal-relocate-reader-source-detail",
                        self.relocate_source_button("relocate-reader-source-detail", cx)
                            .into_any_element(),
                        information_scroll.clone(),
                    )
                    .inline(),
                );
            }
            Some(
                crate::motion::disclosure(
                    "reader-information-panel",
                    self.reader_ui.info_open,
                    v_flex()
                        .w_full()
                        .min_w_0()
                        .flex_shrink_0()
                        .gap_2()
                        .p_3()
                        .when(compact, |panel| panel.p_2())
                        .bg(color(INSET))
                        .border_1()
                        .border_color(color(HAIRLINE))
                        .rounded(RADIUS_CARD)
                        .child(
                            h_flex()
                                .w_full()
                                .min_w_0()
                                .items_center()
                                .gap_2()
                                .child(semantic_label(
                                    "reader-information-title",
                                    course2md::tr!("笔记详情"),
                                    crate::icons::info(),
                                ))
                                .child(div().flex_1())
                                .child(
                                    quiet("collapse-reader-information")
                                        .icon(crate::icons::chevron_up())
                                        .label(course2md::tr!("收起"))
                                        .accessibility_label(course2md::tr!("收起笔记详情"))
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.reader_ui.info_open = false;
                                            this.reader_ui.focus.focus(window, cx);
                                            cx.notify();
                                        })),
                                ),
                        )
                        .child(
                            div()
                                .relative()
                                .w_full()
                                .min_w_0()
                                .child(
                                    div()
                                        .id("reader-information-scroll")
                                        .w_full()
                                        .min_w_0()
                                        .max_h(px(viewport_height))
                                        .overflow_y_scroll()
                                        .track_scroll(&information_scroll)
                                        .child(content),
                                )
                                .child(crate::backend::vertical_scrollbar(&information_scroll)),
                        ),
                )
                .into_any_element(),
            )
        } else {
            None
        }
    }

    /// New-version, incomplete-processing and file-reload notices.
    fn reader_notices(
        &mut self,
        preview: &Rc<notes::Preview>,
        controls_scroll: &ScrollHandle,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let reveal = |id: &'static str, child: AnyElement| {
            crate::focus_scroll::RevealFocus::new(id, child, controls_scroll.clone()).inline()
        };
        let mut notices = Vec::new();
        if let Some(current) = &preview.course.manifest
            && let Some(newer) = self
                .courses
                .iter()
                .find(|course| {
                    course.manifest.as_ref().is_some_and(|m| {
                        m.course_id == current.course_id && m.revision > current.revision
                    })
                })
                .cloned()
        {
            notices.push(crate::motion::enter(
                "reader-new-version-notice",
                notice_banner(
                    "newer-note-version",
                    NoticeKind::Info,
                    course2md::tr!("这份笔记已有新版。"),
                )
                .child(reveal(
                    "reveal-open-newer-version",
                    (outline_pill("open-newer-version")
                        .icon(icons::history())
                        .label(course2md::tr!("查看新版"))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.open_reader_version(newer.clone(), cx)
                        })))
                    .into_any_element(),
                )),
            ));
        }
        if let Some(notice) = processing_notice(&preview.processing_issues) {
            let mut problem = v_flex()
                .gap_3()
                .p_4()
                .bg(color(WARNING_BG))
                .border_1()
                .border_color(color(WARNING_BG))
                .rounded(RADIUS_CARD)
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .flex_wrap()
                        .child(badge(BadgeKind::Warning).child(course2md::tr!("需要处理")))
                        .child(
                            theme::accessible_text("reader-incomplete-processing", notice)
                                .text_sm(),
                        ),
                );
            let task_id = preview
                .course
                .manifest
                .as_ref()
                .map(|manifest| manifest.task_id.clone())
                .filter(|id| {
                    self.workspace.as_ref().is_some_and(|workspace| {
                        workspace.state.tasks.iter().any(|task| &task.id == id)
                    })
                });
            let mut actions = h_flex().gap_2().flex_wrap();
            if let Some(id) = task_id {
                let task = self
                    .workspace
                    .as_ref()
                    .and_then(|workspace| workspace.state.tasks.iter().find(|task| task.id == id));
                let requires_repair =
                    task.is_some_and(crate::task_ui::task_requires_service_repair);
                let can_retry = task.is_some_and(|task| {
                    task.handled_by.is_none()
                        && !matches!(
                            task.state,
                            workspace::TaskState::Running
                                | workspace::TaskState::Pausing
                                | workspace::TaskState::Queued
                                | workspace::TaskState::Uncertain
                        )
                        && !task.blocked.iter().any(|blocked| blocked.is_uncertain())
                        && task.artifact.as_ref() == Some(&preview.course.dir)
                });
                let components = preview
                    .processing_issues
                    .iter()
                    .filter_map(|issue| match issue.stage {
                        notes::ProcessingStage::Screenshots => Some("screenshots".to_owned()),
                        notes::ProcessingStage::Proofreading if !requires_repair => {
                            Some("proofreading".to_owned())
                        }
                        notes::ProcessingStage::Summary if !requires_repair => {
                            Some("summary".to_owned())
                        }
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                if can_retry && !components.is_empty() {
                    let retry = id.clone();
                    let label = crate::task_ui::retry_components_label(&components);
                    actions = actions.child(
                        outline_pill("reader-retry-incomplete")
                            .icon(icons::refresh())
                            .label(label)
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
                if can_retry
                    && preview.processing_issues.iter().any(|issue| {
                        matches!(
                            issue.stage,
                            notes::ProcessingStage::Proofreading
                                | notes::ProcessingStage::Translation
                                | notes::ProcessingStage::Summary
                        )
                    })
                {
                    let repair = id.clone();
                    actions = actions.child(
                        outline_pill("reader-repair-ai")
                            .icon(icons::settings())
                            .label(course2md::tr!("检查 AI 服务后重试"))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.save_reading_position(cx);
                                this.repair_task_service(repair.clone(), window, cx);
                            })),
                    );
                }
                actions = actions.child(reveal(
                    "reveal-reader-view-incomplete-task",
                    (quiet("reader-view-incomplete-task")
                        .icon(icons::task())
                        .label(course2md::tr!("查看任务"))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.save_reading_position(cx);
                            this.select_task(&id, cx);
                            this.navigate(Page::Task, cx);
                            cx.notify();
                        })))
                    .into_any_element(),
                ));
            }
            let has_details = preview
                .processing_issues
                .iter()
                .any(|issue| issue.detail().is_some());
            if has_details {
                actions = actions.child(reveal(
                    "reveal-reader-processing-details",
                    (quiet("reader-processing-details")
                        .icon(crate::icons::info())
                        .label(if self.reader_ui.processing_details_open {
                            course2md::tr!("收起处理详情")
                        } else {
                            course2md::tr!("查看处理详情")
                        })
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.reader_ui.processing_details_open =
                                !this.reader_ui.processing_details_open;
                            cx.notify();
                        })))
                    .into_any_element(),
                ));
            }
            problem = problem.child(actions);
            let mut processing_details = v_flex().gap_2();
            for (index, issue) in preview.processing_issues.iter().enumerate() {
                if let Some(detail) = issue.detail() {
                    processing_details = processing_details
                        .child(
                            theme::accessible_text(
                                ("reader-processing-detail-label", index),
                                issue.stage.label(),
                            )
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM),
                        )
                        .child(
                            theme::accessible_text(
                                ("reader-processing-detail", index),
                                detail.to_owned(),
                            )
                            .text_sm()
                            .text_color(color(GRAY)),
                        );
                }
            }
            problem = problem.child(crate::motion::disclosure(
                "reader-processing-detail-panel",
                self.reader_ui.processing_details_open,
                processing_details,
            ));
            notices.push(crate::motion::enter("reader-processing-notice", problem));
        }
        if files_need_reload(preview, &self.reader_ui.issues, &self.reader_ui.frames) {
            let course = preview.course.clone();
            let issues = preview
                .issues
                .iter()
                .chain(self.reader_ui.issues.iter())
                .collect::<Vec<_>>();
            let messages = v_flex()
                .flex_1()
                .min_w(px(240.))
                .gap_2()
                .when(issues.is_empty(), |view| {
                    view.child(
                        theme::accessible_text(
                            "reader-unreadable-images",
                            course2md::tr!("部分截图无法读取"),
                        )
                        .text_sm()
                        .text_color(color(WARNING)),
                    )
                })
                .children(issues.into_iter().enumerate().map(|(index, issue)| {
                    theme::accessible_text(("reader-issue", index), issue.clone())
                        .text_sm()
                        .text_color(color(WARNING))
                }));
            notices.push(crate::motion::enter(
                "reader-file-notice",
                h_flex()
                    .w_full()
                    .min_w_0()
                    .items_center()
                    .flex_wrap()
                    .gap_3()
                    .p_3()
                    .bg(color(WARNING_BG))
                    .rounded(RADIUS_CARD)
                    .child(
                        icons::warning()
                            .size(ICON_SLOT)
                            .flex_shrink_0()
                            .text_color(color(WARNING)),
                    )
                    .child(messages)
                    .child(reveal(
                        "reveal-reload-reader-files",
                        (outline_pill("reload-reader-files")
                            .icon(icons::refresh())
                            .label(course2md::tr!("重新读取"))
                            .loading(self.reading)
                            .disabled(self.reading)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.load_reader_version(course.clone(), true, cx)
                            })))
                        .into_any_element(),
                    )),
            ));
        }
        notices
    }

    /// Screenshot grid: loading, empty and no-match states plus measured cards.
    fn reader_gallery(
        &mut self,
        preview: &Rc<notes::Preview>,
        metrics: ReaderMetrics,
        scrolling_title: Option<AnyElement>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let gallery_query = self.reader_ui.gallery_find.read(cx).value().to_string();
        let gallery_indices = self.reader_ui.gallery_matches.clone();
        let measured_scroll = self.reader_scroll.clone();
        let item_layout = self.reader_ui.gallery_layout.clone();
        let measured = |index: usize, child: AnyElement| {
            let positions = item_layout.clone();
            let scroll = measured_scroll.clone();
            div()
                .w_full()
                .min_w_0()
                .flex_shrink_0()
                .on_children_prepainted(move |bounds, _, _| {
                    if let Some(bounds) = bounds.first() {
                        positions.borrow_mut().record(
                            index,
                            f32::from(bounds.top() - scroll.bounds().top() - scroll.offset().y),
                            f32::from(bounds.size.height),
                        );
                    }
                })
                .child(child)
        };
        let mut article = v_flex()
            .id("note-reader")
            .role(Role::Document)
            .aria_label(preview.course.title.clone())
            .flex_1()
            .min_h_0()
            .min_w_0()
            .w_full()
            .h_full()
            .overflow_y_scroll()
            .track_scroll(&self.reader_scroll)
            .gap_3()
            .py_3()
            .children(scrolling_title);
        if self.reader_ui.data_loading && self.reader_ui.frames.is_empty() {
            article = article.child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .py_6()
                    .child(crate::motion::spinner("reader-images-spinner", cx))
                    .child(theme::accessible_text(
                        "loading-images",
                        course2md::tr!("正在读取截图…"),
                    )),
            );
        } else if self.reader_ui.frames.is_empty() {
            article = article.child(crate::motion::enter(
                "reader-images-empty",
                v_flex()
                    .w_full()
                    .py_8()
                    .px_6()
                    .gap_4()
                    .items_center()
                    .bg(color(INSET))
                    .rounded(RADIUS_CARD)
                    .child(
                        icons::image()
                            .size(EMPTY_STATE_ICON)
                            .text_color(color(GRAY)),
                    )
                    .child(
                        theme::accessible_text(
                            "no-note-images",
                            course2md::tr!("这份笔记没有截图"),
                        )
                        .text_size(TEXT_TITLE)
                        .font_weight(FontWeight::SEMIBOLD),
                    )
                    .when(
                        preview.processing_issues.iter().any(|issue| {
                            matches!(issue.stage, notes::ProcessingStage::Screenshots)
                        }),
                        |view| {
                            view.child(
                                theme::accessible_text(
                                    "no-note-images-reason",
                                    course2md::tr!("截图生成未完成"),
                                )
                                .text_color(color(GRAY)),
                            )
                        },
                    )
                    .child(
                        outline_pill("empty-images-to-note")
                            .icon(icons::article())
                            .label(course2md::tr!("阅读笔记"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.select_reader_view(0, window, cx)
                            })),
                    ),
            ));
        }
        if !self.reader_ui.frames.is_empty() && gallery_indices.is_empty() {
            article = article.child(
                v_flex()
                    .w_full()
                    .py_8()
                    .gap_3()
                    .items_center()
                    .child(semantic_label(
                        "gallery-no-matches",
                        course2md::tr!("没有对应的截图"),
                        icons::search(),
                    ))
                    .child(
                        quiet("clear-gallery-search")
                            .icon(icons::close())
                            .label(course2md::tr!("清除搜索"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.reader_ui
                                    .gallery_find
                                    .update(cx, |input, cx| input.set_value("", window, cx));
                            })),
                    ),
            );
        }
        // Each screenshot needs enough width for both the slide and a readable
        // text excerpt. The final row keeps the same column widths as the rest.
        let gallery_columns = ((metrics.reader_width + GALLERY_GAP)
            / (metrics.rem * 20. + GALLERY_GAP))
            .floor()
            .clamp(1., 3.) as u16;
        // GPUI rounds each rendered line to whole pixels. Reserve that same
        // three-line height so enlarged text cannot run into the action row.
        let excerpt_line_height =
            px((f32::from(TEXT_BODY.to_pixels(window.rem_size())) * 1.6).round());
        let mut grid = div()
            .grid()
            .grid_cols(gallery_columns)
            .w_full()
            .min_w_0()
            .flex_shrink_0()
            .gap(px(GALLERY_GAP));
        for index in gallery_indices.iter().copied() {
            let card = self.gallery_card(index, &gallery_query, excerpt_line_height, cx);
            grid = grid.child(measured(index, card).h_full());
        }
        article = article.child(grid);
        div()
            .relative()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .w_full()
            .h_full()
            .child(article)
            .child(crate::backend::vertical_scrollbar(&self.reader_scroll))
            .into_any_element()
    }

    /// One screenshot card: the image, its time with a link to the note, and
    /// a three-line excerpt of what was said.
    fn gallery_card(
        &self,
        index: usize,
        query: &str,
        excerpt_line_height: Pixels,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let reveal = |id: ElementId, child: AnyElement| {
            crate::focus_scroll::RevealFocus::new(id, child, self.reader_scroll.clone())
        };
        {
            let frame = &self.reader_ui.frames[index];
            let label = frame_label(frame);
            let mut card = v_flex()
                .id(SharedString::from(frame.anchor.clone()))
                .w_full()
                .h_full()
                .min_w_0()
                .bg(color(SURFACE))
                .border_1()
                .border_color(color(CARD_LINE))
                .rounded(RADIUS_CARD)
                .overflow_hidden();
            if let Some(image) = &frame.image {
                card = card.child(reveal(
                    ("reveal-screenshot", index).into(),
                    (control(("screenshot", index))
                        .ghost()
                        .p_0()
                        .w_full()
                        .h_auto()
                        .min_h(px(0.))
                        .rounded_t(RADIUS_CARD)
                        .rounded_b(px(0.))
                        .aspect_ratio(frame_aspect(frame))
                        .accessibility_label(course2md::tr!("放大{}", label))
                        .child(
                            img(image.path.clone())
                                .size_full()
                                .rounded_t(RADIUS_CARD)
                                .object_fit(ObjectFit::Contain)
                                .with_fallback(move || {
                                    theme::accessible_text(
                                        ("failed-reader-image", index),
                                        course2md::tr!("截图无法读取"),
                                    )
                                    .into_any_element()
                                }),
                        )
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.open_reader_image(index, window, cx)
                        })))
                    .into_any_element(),
                ));
            } else {
                card = card.child(
                    v_flex()
                        .w_full()
                        .aspect_ratio(MISSING_FRAME_ASPECT)
                        .justify_center()
                        .p_4()
                        .bg(color(INSET))
                        .child(
                            theme::accessible_text(
                                ("missing-frame", index),
                                course2md::tr!("截图无法读取"),
                            )
                            .text_size(TEXT_AUX)
                            .text_color(color(WARNING)),
                        ),
                );
            }
            let mut card_body = v_flex().flex_1().min_w_0().p_3().gap_2().child(
                h_flex()
                    .items_center()
                    .flex_wrap()
                    .gap_2()
                    .child(
                        theme::accessible_text(
                            ("frame-title", index),
                            course2md::render::fmt_ts(frame.seconds),
                        )
                        .role(Role::Heading)
                        .flex_1()
                        .text_size(TEXT_BODY)
                        .font_weight(FontWeight::SEMIBOLD),
                    )
                    .when_some(frame.body_index, |row, body_index| {
                        row.child(
                            reveal(
                                ("reveal-frame-to-body", index).into(),
                                quiet(("frame-to-body", index))
                                    .icon(icons::article())
                                    .label(course2md::tr!("正文"))
                                    .accessibility_label(course2md::tr!("在笔记中查看"))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.jump_reader_block(body_index, 0., cx)
                                    }))
                                    .into_any_element(),
                            )
                            .inline(),
                        )
                    }),
            );
            let excerpt = self
                .reader_ui
                .gallery_excerpts
                .get(&index)
                .cloned()
                .unwrap_or_else(|| frame_excerpt(frame, query));
            let excerpt_marks = nav::text_matches(&excerpt, query)
                .into_iter()
                .map(|range| {
                    (
                        range,
                        HighlightStyle {
                            background_color: Some(color(FIND_HIGHLIGHT).into()),
                            ..Default::default()
                        },
                    )
                })
                .collect::<Vec<_>>();
            // The three-line preview is a defined content slot; the full
            // transcript remains readable in the image viewer and article.
            // Without highlights, native ellipsis marks the cut: StyledText
            // clips silently, which reads like a broken sentence.
            card_body = card_body.child(
                div()
                    .id(("frame-excerpt", index))
                    .role(Role::Paragraph)
                    .aria_label(excerpt.clone())
                    .w_full()
                    .min_w_0()
                    .max_h(excerpt_line_height * 3.)
                    .flex_shrink_0()
                    .whitespace_normal()
                    .text_size(TEXT_BODY)
                    .font_weight(FontWeight::NORMAL)
                    .line_height(excerpt_line_height)
                    .text_color(color(GRAY))
                    .when(excerpt_marks.is_empty(), |view| {
                        view.text_ellipsis().line_clamp(3).child(excerpt.clone())
                    })
                    .when(!excerpt_marks.is_empty(), |view| {
                        view.child(StyledText::new(excerpt).with_highlights(excerpt_marks))
                    }),
            );
            card.child(card_body).into_any_element()
        }
    }

    /// Contents are a reading rail; persistent selection belongs to the row
    /// surface so pointer feedback cannot erase the current chapter.
    fn reader_toc_panel(
        &self,
        headings: &[(usize, String, Option<f64>)],
        current: Option<usize>,
        side: bool,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let mut panel = v_flex()
            .id("note-toc")
            .gap_1()
            .py_3()
            .overflow_y_scroll()
            .track_scroll(&self.reader_ui.toc_scroll);
        panel = if side {
            panel
                .w(TOC_PANEL)
                .flex_shrink_0()
                .h_full()
                .min_h_0()
                .pl_4()
                .border_l_1()
                .border_color(color(HAIRLINE))
        } else {
            panel.w_full().h_full().min_h_0().px_3()
        };
        panel = panel.child(
            h_flex()
                .items_center()
                .gap_2()
                .flex_shrink_0()
                .mb_2()
                .child(semantic_label("toc-title", course2md::tr!("目录"), icons::toc()).flex_1())
                .when(!side, |row| {
                    row.child(
                        quiet("close-note-toc")
                            .icon(icons::close())
                            .label(course2md::tr!("关闭"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.reader_ui.toc_open = Some(false);
                                cx.notify();
                            })),
                    )
                }),
        );
        for (index, label, seconds) in headings {
            let index = *index;
            let on = current == Some(index);
            let trailing_time =
                seconds.filter(|seconds| *label != course2md::render::fmt_ts(*seconds));
            panel = panel.child(crate::focus_scroll::RevealFocus::new(
                ("reveal-toc-item", index),
                div()
                    .w_full()
                    .min_w_0()
                    .rounded(RADIUS_PILL)
                    .when(on, |row| row.bg(color(SECONDARY_CONTAINER)))
                    .child(
                        control(("toc-item", index))
                            .ghost()
                            .w_full()
                            .h_auto()
                            .min_h(CONTROL_HEIGHT)
                            .p(TOC_ROW_PADDING)
                            .justify_start()
                            .rounded(RADIUS_PILL)
                            .tooltip(label.clone())
                            .accessibility_label(if on {
                                course2md::tr!("当前章节：{}", label)
                            } else {
                                course2md::tr!("转到 {}", label)
                            })
                            .child(
                                h_flex()
                                    .w_full()
                                    .min_w_0()
                                    .gap_2()
                                    // The time stays on the first line; only the title column wraps.
                                    .items_start()
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .whitespace_nowrap()
                                            .text_ellipsis()
                                            .line_height(relative(1.4))
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(color(if on {
                                                ON_SECONDARY_CONTAINER
                                            } else {
                                                INK
                                            }))
                                            .child(label.clone()),
                                    )
                                    .when_some(trailing_time, |row, seconds| {
                                        row.child(
                                            div()
                                                .flex_shrink_0()
                                                .text_size(TEXT_AUX)
                                                .text_color(color(if on {
                                                    ON_SECONDARY_CONTAINER
                                                } else {
                                                    GRAY
                                                }))
                                                .child(course2md::render::fmt_ts(seconds)),
                                        )
                                    }),
                            )
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if !side {
                                    this.reader_ui.toc_open = Some(false);
                                }
                                this.jump_reader_block(index, 0., cx);
                            })),
                    ),
                self.reader_ui.toc_scroll.clone(),
            ));
        }
        panel
    }
    fn open_reader_image(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(preview) = &self.preview else {
            return;
        };
        if self
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
                .gallery_find
                .read(cx)
                .value()
                .trim()
                .is_empty();
        let visible: Vec<usize> = if self.result_tab == 1 {
            self.reader_ui.gallery_matches.to_vec()
        } else {
            (0..self.reader_ui.frames.len()).collect()
        };
        let Some(viewer_index) = visible.iter().position(|candidate| *candidate == index) else {
            return;
        };
        crate::import_ui::ConversionFollow::interrupt_for_reader_viewer(
            &mut self.following_conversion,
        );
        let desktop = cx.entity();
        let content = cx.new(|_| ImageDialog {
            desktop: desktop.clone(),
            footer: false,
        });
        let footer = cx.new(|_| ImageDialog {
            desktop,
            footer: true,
        });
        let focus = cx.focus_handle();
        self.reader_ui.viewer = Some(ImageViewer {
            scroll: ScrollHandle::new(),
            viewport_scroll: ScrollHandle::new(),
            viewport_size: size(px(0.), px(0.)),
            frames: visible
                .iter()
                .map(|index| self.reader_ui.frames[*index].clone())
                .collect(),
            filtered,
            index: viewer_index,
            source: self.reader_source(),
            source_available: self.reader_ui.source_available,
            version: preview.course.dir.clone(),
            zoom: None,
            details_open: None,
            return_focus: window.focused(cx),
            focus: focus.clone(),
            dialogs: [content.downgrade(), footer.downgrade()],
        });
        let weak = cx.weak_entity();
        window.open_dialog(cx, move |dialog, window, _| {
            let closed = weak.clone();
            let content = content.clone();
            let top = task_dialog_top(window);
            dialog
                .w(px(ImageDialogMetrics::new(window).width))
                .h((window.bounds().size.height - top - px(24.)).max(px(0.)))
                .min_h_0()
                .margin_top(top)
                .overlay_closable(false)
                .close_button(false)
                .content(move |body, _, _| body.min_h_0().child(content.clone()))
                .footer(footer.clone())
                .on_close(move |_, window, cx| {
                    let _ =
                        closed.update(cx, |this, cx| this.restore_reader_image_focus(window, cx));
                })
        });
        focus.focus(window, cx);
        cx.notify();
    }
    /// The dialog renders from the viewer state; only its changes repaint it.
    fn notify_image_viewer(&self, cx: &mut Context<Self>) {
        if let Some(viewer) = &self.reader_ui.viewer {
            for dialog in &viewer.dialogs {
                let _ = dialog.update(cx, |_, cx| cx.notify());
            }
        }
    }
    fn restore_reader_image_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(focus) = self
            .reader_ui
            .viewer
            .take()
            .and_then(|viewer| viewer.return_focus)
        {
            focus.focus(window, cx);
        }
        cx.notify();
    }
    fn close_reader_image(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.close_dialog(cx);
        self.restore_reader_image_focus(window, cx);
    }
    fn move_reader_image(&mut self, delta: isize, cx: &mut Context<Self>) {
        if let Some(viewer) = &mut self.reader_ui.viewer {
            viewer.index = (viewer.index as isize + delta)
                .clamp(0, viewer.frames.len().saturating_sub(1) as isize)
                as usize;
            viewer.zoom = None;
            viewer.scroll.set_offset(point(px(0.), px(0.)));
            viewer.viewport_scroll.set_offset(point(px(0.), px(0.)));
            self.notify_image_viewer(cx);
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
    fn image_details_open(viewer: &ImageViewer, metrics: &ImageDialogMetrics) -> bool {
        viewer.details_open.unwrap_or(metrics.details_by_default)
    }
    fn zoom_reader_image(&mut self, factor: Option<f32>, window: &Window, cx: &mut Context<Self>) {
        let metrics = ImageDialogMetrics::new(window);
        let Some(viewer) = &mut self.reader_ui.viewer else {
            return;
        };
        if metrics.separate_modes && Self::image_details_open(viewer, &metrics) {
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
        self.notify_image_viewer(cx);
    }
    /// Keyboard actions shared by the dialog body and its footer.
    fn image_viewer_actions(
        &self,
        element: Stateful<Div>,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        element
            .key_context("ReaderImage")
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
            .on_action(
                cx.listener(|this, _: &CloseImage, window, cx| this.close_reader_image(window, cx)),
            )
    }
    fn image_zoom_controls(&self, scale: f32, compact: bool, cx: &mut Context<Self>) -> AnyElement {
        let fit = self
            .reader_ui
            .viewer
            .as_ref()
            .is_some_and(|viewer| viewer.zoom.is_none());
        let can_zoom_in = zoomed(scale, ZOOM_STEP_IN) > scale;
        let can_zoom_out = zoomed(scale, ZOOM_STEP_OUT) < scale;
        let percent = (scale > 0.).then(|| format!("{}%", (scale * 100.).round() as u32));
        if compact {
            let weak = cx.weak_entity();
            return quiet("image-zoom-menu")
                .icon(icons::zoom_in())
                .label(percent.unwrap_or_else(|| course2md::tr!("缩放").into()))
                .child(crate::icons::chevron_down().size_4().flex_shrink_0())
                .accessibility_label(course2md::tr!("缩放图片"))
                .dropdown_menu(move |menu, _, _| {
                    let larger = weak.clone();
                    let smaller = weak.clone();
                    let fitted = weak.clone();
                    menu.item(
                        PopupMenuItem::new(course2md::tr!("放大 · +"))
                            .icon(icons::zoom_in())
                            .disabled(!can_zoom_in)
                            .on_click(move |_, window, cx| {
                                let _ = larger.update(cx, |this, cx| {
                                    this.zoom_reader_image(Some(ZOOM_STEP_IN), window, cx)
                                });
                            }),
                    )
                    .item(
                        PopupMenuItem::new(course2md::tr!("缩小 · −"))
                            .icon(icons::zoom_out())
                            .disabled(!can_zoom_out)
                            .on_click(move |_, window, cx| {
                                let _ = smaller.update(cx, |this, cx| {
                                    this.zoom_reader_image(Some(ZOOM_STEP_OUT), window, cx)
                                });
                            }),
                    )
                    .separator()
                    .item(
                        PopupMenuItem::new(course2md::tr!("适合窗口 · 0"))
                            .icon(icons::fit_screen())
                            .checked(fit)
                            .on_click(move |_, window, cx| {
                                let _ = fitted.update(cx, |this, cx| {
                                    this.zoom_reader_image(None, window, cx)
                                });
                            }),
                    )
                })
                .into_any_element();
        }
        h_flex()
            .gap_2()
            .items_center()
            .child(
                control("image-zoom-out")
                    .icon(icons::zoom_out())
                    .w(CONTROL_HEIGHT)
                    .px_0()
                    .accessibility_label(course2md::tr!("缩小"))
                    .tooltip(course2md::tr!("缩小 · −"))
                    .disabled(!can_zoom_out)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.zoom_reader_image(Some(ZOOM_STEP_OUT), window, cx)
                    })),
            )
            .child(
                theme::accessible_text("image-scale", percent.unwrap_or_else(|| "—".into()))
                    .whitespace_nowrap()
                    .text_size(TEXT_BODY),
            )
            .child(
                control("image-zoom-in")
                    .icon(icons::zoom_in())
                    .w(CONTROL_HEIGHT)
                    .px_0()
                    .accessibility_label(course2md::tr!("放大"))
                    .tooltip(course2md::tr!("放大 · +"))
                    .disabled(!can_zoom_in)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.zoom_reader_image(Some(ZOOM_STEP_IN), window, cx)
                    })),
            )
            .child(
                control("image-fit")
                    .icon(icons::fit_screen())
                    .w(CONTROL_HEIGHT)
                    .px_0()
                    .accessibility_label(course2md::tr!("适合窗口"))
                    .tooltip(course2md::tr!("适合窗口 · 0"))
                    .on_click(
                        cx.listener(|this, _, window, cx| this.zoom_reader_image(None, window, cx)),
                    ),
            )
            .into_any_element()
    }
    fn reader_image_content(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(viewer) = &self.reader_ui.viewer else {
            return div().into_any_element();
        };
        let metrics = ImageDialogMetrics::new(window);
        let compact = metrics.compact;
        let frame = &viewer.frames[viewer.index];
        let index = viewer.index;
        let count = viewer.frames.len();
        let label = frame_label(frame);
        let scale = viewer.zoom.unwrap_or_else(|| Self::image_fit(viewer));
        let details_open = Self::image_details_open(viewer, &metrics);
        let text_only = metrics.separate_modes && details_open;
        let time = course2md::render::fmt_ts(frame.seconds);
        let heading = if text_only {
            course2md::tr!("文字 · {}", time)
        } else if compact {
            time
        } else {
            course2md::tr!("截图 · {}", time)
        };
        let header = h_flex()
            .gap_2()
            .items_center()
            .flex_wrap()
            .flex_shrink_0()
            .child(
                theme::accessible_text("image-title", heading)
                    .role(Role::Heading)
                    .aria_label(label.clone())
                    .whitespace_nowrap()
                    .text_size(if compact { TEXT_BODY } else { TEXT_TITLE })
                    .font_weight(FontWeight::SEMIBOLD),
            )
            .child(div().flex_1())
            .child(
                control("image-previous")
                    .icon(crate::icons::chevron_left())
                    .w(CONTROL_HEIGHT)
                    .px_0()
                    .accessibility_label(course2md::tr!("上一张"))
                    .tooltip(course2md::tr!("上一张 · ←"))
                    .disabled(index == 0)
                    .on_click(cx.listener(|this, _, _, cx| this.move_reader_image(-1, cx))),
            )
            .child(
                theme::accessible_text(
                    "image-number",
                    if viewer.filtered {
                        course2md::tr!("匹配截图 {} / {}", index + 1, count)
                    } else {
                        format!("{} / {count}", index + 1)
                    },
                )
                .whitespace_nowrap()
                .text_size(TEXT_BODY),
            )
            .child(
                control("image-next")
                    .icon(crate::icons::chevron_right())
                    .w(CONTROL_HEIGHT)
                    .px_0()
                    .accessibility_label(course2md::tr!("下一张"))
                    .tooltip(course2md::tr!("下一张 · →"))
                    .disabled(index + 1 >= count)
                    .on_click(cx.listener(|this, _, _, cx| this.move_reader_image(1, cx))),
            )
            .when(!text_only, |row| {
                row.child(self.image_zoom_controls(scale, compact, cx))
            });
        let media = (!text_only).then(|| self.image_viewer_media(scale, label.clone(), cx));
        let viewer = self.reader_ui.viewer.as_ref().expect("checked above");
        let frame = &viewer.frames[viewer.index];
        let has_text = !frame.transcript.is_empty();
        let details = (text_only || (has_text && details_open)).then(|| {
            v_flex()
                .id("image-details")
                .w_full()
                // Short windows show either the image or its text, giving the
                // chosen content the whole available viewport.
                .when(text_only, |view| view.flex_1())
                .when(!text_only, |view| view.flex_shrink_0().max_h(relative(0.4)))
                .min_h_0()
                .gap_2()
                .pr_3()
                .overflow_y_scroll()
                .track_scroll(&viewer.scroll)
                .child(if has_text {
                    paragraph(
                        "image-transcript",
                        course2md::tr!("这段讲解：{}", frame.transcript).into(),
                        0,
                        Vec::new(),
                        None,
                    )
                    .flex_shrink_0()
                    .into_any_element()
                } else {
                    theme::accessible_text("image-no-text", course2md::tr!("这张截图没有对应文字"))
                        .text_size(TEXT_READER)
                        .whitespace_normal()
                        .into_any_element()
                })
        });
        let scrollbar = details
            .is_some()
            .then(|| Scrollbar::vertical(&viewer.scroll).mode(ScrollbarMode::Always));
        let body = v_flex()
            .id("reader-image-dialog")
            .relative()
            .role(Role::Dialog)
            .aria_label(if text_only {
                course2md::tr!("阅读{}对应文字", label)
            } else {
                course2md::tr!("查看{}", label)
            })
            .track_focus(&viewer.focus)
            .gap_2()
            .h_full()
            .w_full()
            .min_h_0()
            .child(header)
            .children(media)
            .children(details)
            .children(scrollbar);
        self.image_viewer_actions(body, cx).into_any_element()
    }

    /// The zoomable image, or a notice when its file cannot be read.
    fn image_viewer_media(&self, scale: f32, label: String, cx: &mut Context<Self>) -> AnyElement {
        let Some(viewer) = &self.reader_ui.viewer else {
            return div().into_any_element();
        };
        let Some(image) = &viewer.frames[viewer.index].image else {
            return theme::accessible_text("image-unreadable", course2md::tr!("截图无法读取"))
                .into_any_element();
        };
        let viewport_scroll = viewer.viewport_scroll.clone();
        let owner = cx.weak_entity();
        let version = viewer.version.clone();
        let width = px(image.width as f32 * scale);
        let height = px(image.height as f32 * scale);
        div()
            .on_children_prepainted(move |_, window, cx| {
                let size = viewport_scroll.bounds().size;
                let owner = owner.clone();
                let version = version.clone();
                window.defer(cx, move |_, cx| {
                    let _ = owner.update(cx, |this, cx| {
                        if let Some(viewer) = &mut this.reader_ui.viewer
                            && viewer.version == version
                            && viewer.viewport_size != size
                        {
                            viewer.viewport_size = size;
                            this.notify_image_viewer(cx);
                        }
                    });
                });
            })
            .id("image-viewport")
            .bg(color(INSET))
            .border_1()
            .border_color(color(HAIRLINE))
            .rounded(RADIUS_CARD)
            .flex_1()
            .min_h_0()
            .w_full()
            .overflow_x_scroll()
            .overflow_y_scroll()
            .track_scroll(&viewer.viewport_scroll)
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
                            .child(
                                img(image.path.clone())
                                    .size_full()
                                    .object_fit(ObjectFit::Contain)
                                    .with_fallback(|| {
                                        // One viewer exists at a time, so the id is unique.
                                        theme::accessible_text(
                                            "failed-reader-image-enlarged",
                                            course2md::tr!("截图无法读取"),
                                        )
                                        .into_any_element()
                                    }),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn reader_image_actions(&mut self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(viewer) = &self.reader_ui.viewer else {
            return div().into_any_element();
        };
        let metrics = ImageDialogMetrics::new(window);
        let compact = metrics.compact;
        let frame = &viewer.frames[viewer.index];
        let has_text = !frame.transcript.is_empty();
        let details_open = Self::image_details_open(viewer, &metrics);
        let text_only = metrics.separate_modes && details_open;
        let version = viewer.version.clone();
        let body_index = frame.body_index;
        let source_link = viewer
            .source
            .as_ref()
            .and_then(|source| nav::seek_url(source, frame.seconds));
        let original = viewer.source.clone().filter(|_| viewer.source_available);
        let mut actions = h_flex()
            .id("reader-image-actions")
            .gap_2()
            .w_full()
            .flex_wrap()
            .flex_shrink_0();
        if let Some(body_index) = body_index {
            actions = actions.child(
                control("image-to-body")
                    .icon(icons::article())
                    .label(if compact {
                        course2md::tr!("正文")
                    } else {
                        course2md::tr!("在笔记中查看")
                    })
                    .accessibility_label(course2md::tr!("在笔记中查看"))
                    .when(compact, |button| {
                        button.tooltip(course2md::tr!("在笔记中查看"))
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        let current = this
                            .preview
                            .as_ref()
                            .is_some_and(|preview| preview.course.dir == version);
                        this.close_reader_image(window, cx);
                        if current {
                            this.jump_reader_block(body_index, 0., cx);
                        }
                    })),
            );
        }
        if let Some(url) = source_link {
            actions = actions.child(
                control("image-to-source")
                    .icon(icons::play_arrow())
                    .ghost()
                    .label(if compact {
                        course2md::tr!("观看")
                    } else {
                        course2md::tr!("从此处观看")
                    })
                    .accessibility_label(course2md::tr!("从此处观看原视频"))
                    .when(compact, |button| {
                        button.tooltip(course2md::tr!("从此处观看原视频"))
                    })
                    .on_click(move |_, _, cx| cx.open_url(&url)),
            );
        } else if let Some(source) = original {
            actions = actions.child(
                control("image-open-original")
                    .icon(icons::movie())
                    .ghost()
                    .label(if compact {
                        course2md::tr!("原视频")
                    } else {
                        course2md::tr!("打开原视频")
                    })
                    .accessibility_label(course2md::tr!("打开原视频"))
                    .when(compact, |button| {
                        button.tooltip(course2md::tr!("打开原视频"))
                    })
                    .on_click(move |_, _, cx| match &source {
                        nav::SourceTarget::Web(url) => cx.open_url(url),
                        nav::SourceTarget::Local(path) => cx.open_with_system(path),
                    }),
            );
        }
        if has_text || text_only {
            actions = actions.child(
                quiet("image-toggle-details")
                    .icon(if text_only {
                        icons::image()
                    } else {
                        icons::article()
                    })
                    .label(if text_only {
                        course2md::tr!("返回图片")
                    } else if metrics.separate_modes {
                        course2md::tr!("查看文字")
                    } else if details_open {
                        course2md::tr!("收起文字")
                    } else {
                        course2md::tr!("显示文字")
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(viewer) = &mut this.reader_ui.viewer {
                            viewer.details_open = Some(!details_open);
                            this.notify_image_viewer(cx);
                        }
                    })),
            );
        }
        let actions = actions.child(div().flex_1()).child(
            control("image-close")
                .icon(crate::icons::close())
                .label(course2md::tr!("关闭"))
                .accessibility_label(course2md::tr!("关闭截图"))
                .on_click(cx.listener(|this, _, window, cx| this.close_reader_image(window, cx))),
        );
        self.image_viewer_actions(actions, cx).into_any_element()
    }
}

fn load_reader_data(preview: &notes::Preview) -> ReaderData {
    // Typeset formulas here, off the UI thread, so drawing only looks them up.
    crate::math::prepare(preview.blocks.iter().filter_map(block_text));
    let mut data = ReaderData::default();
    let dir = &preview.course.dir;
    data.export_folder = existing_export_folder(dir, &preview.outputs);
    if let Some(manifest) = &preview.course.manifest {
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
                .or_else(|| {
                    nav::nearest_time(heading_times(&preview.blocks), frame.t).map(|(i, _)| i)
                });
            let frame = Frame::new(
                format!("frame:{index}:{}", frame.image),
                course2md::artifact::safe_asset_path(dir, &frame.image).ok(),
                frame.t,
                transcript,
                body_index,
            );
            match (&frame.image, frame.body_index) {
                (Some(image), _) => {
                    data.frame_by_path
                        .entry(image.path.clone())
                        .or_insert(index);
                }
                (None, Some(block)) => data.missing_by_block.entry(block).or_default().push(index),
                (None, None) => {}
            }
            data.frames.push(frame);
        }
        let parent = dir
            .parent()
            .filter(|parent| parent.file_name().is_some_and(|name| name == "versions"));
        if let Some(parent) = parent {
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
        }
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
fn processing_notice(issues: &[crate::notes::ProcessingIssue]) -> Option<String> {
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
fn files_need_reload(preview: &crate::notes::Preview, issues: &[String], frames: &[Frame]) -> bool {
    !preview.issues.is_empty()
        || !issues.is_empty()
        || frames.iter().any(|frame| frame.image.is_none())
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
        ExportFeedback, ExportState, Frame, OfflineVideo, OfflineVideoRequest, PreviewBlock,
        block_anchor, block_time, capture_reader_position, existing_export_folder,
        files_need_reload, frame_excerpt, image_zoom_offset, load_reader_data, matching_frames,
        nav, note_position_index, processing_notice, restored_reader_offset,
    };
    use crate::{ConversionOptions, notes::Course, source, workspace};

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
    }

    fn reading_blocks() -> Vec<PreviewBlock> {
        vec![
            PreviewBlock::Heading {
                text: "摘要".into(),
                anchor: "summary".into(),
                seconds: None,
            },
            PreviewBlock::Paragraph {
                text: "摘要正文".into(),
                anchor: "summary-tldr".into(),
            },
            PreviewBlock::Heading {
                text: "第一节".into(),
                anchor: "section-1".into(),
                seconds: Some(5.),
            },
            PreviewBlock::Paragraph {
                text: "可在放大字号后继续阅读的长正文".into(),
                anchor: "section-1-text".into(),
            },
        ]
    }

    #[test]
    fn reading_top_survives_late_measurement_and_enlarged_text() {
        let blocks = reading_blocks();
        let mut initial = nav::ReadingLayout::default();
        initial.record(1, 48., 60.);
        let position = capture_reader_position(&initial, 0., |index| {
            (
                Some(block_anchor(&blocks[index], index)),
                block_time(&blocks, index),
            )
        })
        .unwrap();
        // Loading initially exposes only the paragraph. Its later measured
        // heading and a larger type scale must not replace the document top.
        let mut resized = nav::ReadingLayout::default();
        resized.record(0, 12., 36.);
        resized.record(1, 64., 120.);
        assert!(position.paragraph.is_none());
        assert_eq!(
            restored_reader_offset(&resized, note_position_index(&blocks, &position), &position),
            0.
        );
    }

    #[test]
    fn reading_summary_navigation_returns_to_the_heading_after_reflow() {
        let blocks = reading_blocks();
        let position = workspace::ReadingPosition {
            paragraph: Some("summary".into()),
            fraction: Some(0.),
            ..Default::default()
        };
        let index = note_position_index(&blocks, &position);
        assert_eq!(index, Some(0));
        for (top, height) in [(12., 22.), (16., 44.)] {
            let mut layout = nav::ReadingLayout::default();
            layout.record(0, top, height);
            layout.record(1, top + height + 12., 120.);
            let offset = restored_reader_offset(&layout, index, &position);
            assert_eq!(layout.top_item(offset).map(|(index, _, _)| index), Some(0));
        }
    }

    #[test]
    fn reading_resize_keeps_the_same_paragraph_and_progress() {
        let blocks = reading_blocks();
        let mut initial = nav::ReadingLayout::default();
        initial.record(3, 220., 200.);
        let position = capture_reader_position(&initial, -270., |index| {
            (
                Some(block_anchor(&blocks[index], index)),
                block_time(&blocks, index),
            )
        })
        .unwrap();
        let mut resized = nav::ReadingLayout::default();
        resized.record(3, 400., 400.);
        let offset =
            restored_reader_offset(&resized, note_position_index(&blocks, &position), &position);
        assert_eq!(offset, -500.);
        let (index, within, height) = resized.top_item(offset).unwrap();
        assert_eq!(index, 3);
        assert_eq!(nav::within_fraction(within, height), 0.25);
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
                options: ConversionOptions {
                    keep_video: true,
                    ..Default::default()
                },
                subtitle: None,
                config: Default::default(),
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
            stages: Default::default(),
            error: None,
            artifact: Some(course.dir.clone()),
            outcomes: None,
            unread: false,
            logs: Vec::new(),
            blocked: Vec::new(),
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
            OfflineVideo::Available(media.canonicalize().unwrap())
        );
        assert!(OfflineVideoRequest::for_version(&second, &task, &library).is_none());
        let mut changed = task.clone();
        changed.artifact = Some(second.dir.clone());
        assert!(OfflineVideoRequest::for_version(&first, &changed, &library).is_none());
        changed = task.clone();
        changed.plan.source_id = "another-video".into();
        assert!(OfflineVideoRequest::for_version(&first, &changed, &library).is_none());
        changed = task.clone();
        changed.plan.options.keep_video = false;
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
        assert_eq!(old_data.missing_by_block.get(&0), Some(&vec![0]));
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
}

/// Tests for the virtualized note flow: item flattening, bounded rendering and
/// reading-position round trips through the persistent list state.
#[cfg(test)]
mod flow_tests {
    use super::{
        NoteItem, nav, note_block_item, note_capture_position, note_item_block, note_items,
        note_restore_target, note_scroll_key, note_top_block,
    };
    use crate::notes::PreviewBlock;
    use gpui::{
        Context, InteractiveElement as _, IntoElement, ListAlignment, ListOffset, ListState,
        ParentElement as _, Render, Styled as _, TestAppContext, Window, div, list, px,
    };
    use std::cell::RefCell;
    use std::rc::Rc;

    fn blocks(count: usize) -> Vec<PreviewBlock> {
        (0..count)
            .map(|index| {
                if index % 10 == 0 {
                    PreviewBlock::Heading {
                        text: format!("章节 {index}").into(),
                        anchor: format!("h-{index}"),
                        seconds: Some(index as f64 * 30.),
                    }
                } else {
                    PreviewBlock::Paragraph {
                        text: format!("正文段落 {index}。").repeat(8).into(),
                        anchor: format!("p-{index}"),
                    }
                }
            })
            .collect()
    }

    #[test]
    fn note_items_group_the_summary_and_each_section() {
        let section = |index: usize| PreviewBlock::Heading {
            text: format!("第 {index} 节").into(),
            anchor: format!("section-{index}"),
            seconds: Some(index as f64 * 60.),
        };
        let mut note = vec![
            PreviewBlock::Heading {
                text: "摘要".into(),
                anchor: "summary".into(),
                seconds: None,
            },
            PreviewBlock::Paragraph {
                text: "总览".into(),
                anchor: "summary-tldr".into(),
            },
            PreviewBlock::Paragraph {
                text: "要点".into(),
                anchor: "key-point-0".into(),
            },
        ];
        note.extend(blocks(2));
        note.push(section(1));
        note.extend(blocks(2).into_iter().skip(1));
        note.push(section(2));
        let items = note_items(&note);
        assert_eq!(
            items,
            [
                NoteItem::Title,
                NoteItem::Summary { start: 0, end: 3 },
                NoteItem::Block(3),
                NoteItem::Block(4),
                NoteItem::Section {
                    heading: 5,
                    end: 7,
                    ordinal: 0
                },
                NoteItem::Section {
                    heading: 7,
                    end: 8,
                    ordinal: 1
                },
            ]
        );
        // Every block resolves to the item that draws it.
        assert_eq!(note_block_item(&items, 0), Some(1));
        assert_eq!(note_block_item(&items, 2), Some(1));
        assert_eq!(note_block_item(&items, 6), Some(4));
        assert_eq!(note_top_block(&items, 0), Some(0));
        assert_eq!(note_top_block(&items, 4), Some(5));
        assert_eq!(note_item_block(items[0]), None);

        assert_eq!(note_items(&[]), [NoteItem::Title]);
    }

    #[test]
    fn the_note_top_ignores_the_empty_title_item() {
        let note = blocks(30);
        let items = note_items(&note);
        let list = ListState::new(items.len(), ListAlignment::Top, px(1000.));
        assert_eq!(note_scroll_key(&items, &list), None);
        // The title draws nothing beside the header, so the first block's top
        // is still the top of the note.
        list.scroll_to(ListOffset {
            item_ix: 1,
            offset_in_item: px(0.),
        });
        assert_eq!(note_scroll_key(&items, &list), None);
        list.scroll_to(ListOffset {
            item_ix: 1,
            offset_in_item: px(5.),
        });
        assert_eq!(note_scroll_key(&items, &list), Some(1));
        list.scroll_to(ListOffset {
            item_ix: 3,
            offset_in_item: px(0.),
        });
        assert_eq!(note_scroll_key(&items, &list), Some(3));
    }

    #[test]
    fn note_position_round_trips_through_the_list_state() {
        let note = blocks(200);
        let items = note_items(&note);
        let list = ListState::new(items.len(), ListAlignment::Top, px(1000.));
        let heights: Vec<f32> = (0..items.len())
            .map(|i| 40. + (i % 7) as f32 * 30.)
            .collect();

        // The document top is a boundary, not the first paragraph.
        assert_eq!(
            note_capture_position(&items, &note, &list, &nav::ReadingLayout::default()),
            Some(crate::workspace::ReadingPosition::default())
        );
        assert_eq!(
            note_restore_target(
                &items,
                &note,
                &crate::workspace::ReadingPosition::default(),
                None,
                &nav::ReadingLayout::default(),
            ),
            None
        );

        // Scroll into the note; the rendered window records its measurements.
        list.scroll_to(ListOffset {
            item_ix: 54,
            offset_in_item: px(13.),
        });
        let mut layout = nav::ReadingLayout::default();
        for (index, height) in heights.iter().enumerate().take(60).skip(50) {
            layout.record(index, 0., *height);
        }
        let position = note_capture_position(&items, &note, &list, &layout).unwrap();
        assert_eq!(position.paragraph.as_deref(), Some("p-53"));
        assert_eq!(position.seconds, Some(1500.));
        assert_eq!(position.within, -13.);
        assert_eq!(position.fraction, Some(13. / heights[53]));
        assert!(position.offset < 0.);

        // Scroll away: the next frame re-records only the new window, so the
        // restore first lands on the item, then refines once it is measured.
        list.scroll_to(ListOffset {
            item_ix: 0,
            offset_in_item: px(0.),
        });
        let mut layout = nav::ReadingLayout::default();
        for (index, height) in heights.iter().enumerate().take(10) {
            layout.record(index, 0., *height);
        }
        assert_eq!(
            note_restore_target(&items, &note, &position, None, &layout),
            Some((54, None))
        );
        list.scroll_to(ListOffset {
            item_ix: 54,
            offset_in_item: px((-position.within).max(0.)),
        });
        for (index, height) in heights.iter().enumerate().take(64).skip(50) {
            layout.record(index, 0., *height);
        }
        assert_eq!(
            note_restore_target(&items, &note, &position, None, &layout),
            Some((54, Some(13.)))
        );
        list.scroll_to(ListOffset {
            item_ix: 54,
            offset_in_item: px(13.),
        });
        let restored = note_capture_position(&items, &note, &list, &layout).unwrap();
        assert_eq!(restored, position);

        // A find match resolves to its recorded line, one line of context up.
        layout.record_search_line(53, 40, 100., 20.);
        assert_eq!(
            note_restore_target(&items, &note, &position, Some((53, 40)), &layout),
            Some((54, Some(80.)))
        );
    }

    /// The same list wiring the reader uses: variable-height items recording
    /// their window geometry during prepaint.
    struct NoteListHarness {
        list: ListState,
        layout: Rc<RefCell<nav::ReadingLayout>>,
        blocks: Vec<PreviewBlock>,
        items: Rc<Vec<NoteItem>>,
    }

    impl NoteListHarness {
        fn new(count: usize) -> Self {
            let blocks = blocks(count);
            let items = note_items(&blocks);
            Self {
                list: ListState::new(items.len(), ListAlignment::Top, px(1000.)),
                layout: Rc::default(),
                blocks,
                items: Rc::new(items),
            }
        }
    }

    impl Render for NoteListHarness {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            let layout = self.layout.clone();
            let items = self.items.clone();
            div().size_full().child(
                list(self.list.clone(), move |index, _window, _cx| {
                    let positions = layout.clone();
                    let items = items.clone();
                    div()
                        .w_full()
                        .on_children_prepainted(move |bounds, _, _| {
                            let Some(bounds) = bounds.first() else {
                                return;
                            };
                            if let Some(block) = note_item_block(items[index]) {
                                positions.borrow_mut().record(
                                    block,
                                    f32::from(bounds.top()),
                                    f32::from(bounds.size.height),
                                );
                            }
                        })
                        .child(
                            div()
                                .w_full()
                                .h(px(40. + (index % 7) as f32 * 30.))
                                .debug_selector(move || format!("note-item-{index}")),
                        )
                        .into_any_element()
                })
                .w_full()
                .h_full(),
            )
        }
    }

    #[gpui::test]
    fn note_list_reaches_the_end_and_round_trips_a_reading_position(cx: &mut TestAppContext) {
        let (view, cx) = cx.add_window_view(|_, _| NoteListHarness::new(500));
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.list.scroll_to_end();
                cx.notify();
            });
            window.draw(cx).clear(cx);
        });
        assert!(
            cx.debug_bounds("note-item-500").is_some(),
            "the last block renders after jumping to the end"
        );

        // Reading position round trip with records produced by real prepaint.
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.list.scroll_to(ListOffset {
                    item_ix: 251,
                    offset_in_item: px(17.),
                });
                cx.notify();
            });
            window.draw(cx).clear(cx);
        });
        let position = cx.update(|_, cx| view.read(cx).capture());
        assert_eq!(position.paragraph.as_deref(), Some("h-250"));
        assert_eq!(position.within, -17.);

        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.layout.borrow_mut().clear();
                view.list.scroll_to(ListOffset {
                    item_ix: 0,
                    offset_in_item: px(0.),
                });
                cx.notify();
            });
            window.draw(cx).clear(cx);
        });
        // First pass lands on the unmeasured item; its measurement refines it.
        cx.update(|window, cx| {
            view.update(cx, |view, _| view.restore_rough(&position));
            window.draw(cx).clear(cx);
        });
        cx.update(|window, cx| {
            view.update(cx, |view, _| view.restore_precise(&position));
            window.draw(cx).clear(cx);
        });
        cx.update(|_, cx| assert_eq!(view.read(cx).capture(), position));
    }

    impl NoteListHarness {
        fn capture(&self) -> crate::workspace::ReadingPosition {
            note_capture_position(&self.items, &self.blocks, &self.list, &self.layout.borrow())
                .unwrap()
        }

        fn restore_rough(&mut self, position: &crate::workspace::ReadingPosition) {
            let (item_ix, offset) = note_restore_target(
                &self.items,
                &self.blocks,
                position,
                None,
                &self.layout.borrow(),
            )
            .unwrap();
            assert_eq!(offset, None, "the scrolled-away item is unmeasured");
            self.list.scroll_to(ListOffset {
                item_ix,
                offset_in_item: px((-position.within).max(0.)),
            });
        }

        fn restore_precise(&mut self, position: &crate::workspace::ReadingPosition) {
            let (item_ix, offset) = note_restore_target(
                &self.items,
                &self.blocks,
                position,
                None,
                &self.layout.borrow(),
            )
            .unwrap();
            let offset = offset.expect("the landed item is measured by now");
            self.list.scroll_to(ListOffset {
                item_ix,
                offset_in_item: px(offset.max(0.)),
            });
        }
    }
}
