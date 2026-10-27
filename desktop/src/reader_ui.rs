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

/// Reading copy: 17px with a 1.8 line, in the interface face.
const READER_BODY: Rems = rems(17. / 14.);
/// Space kept between scrolled text and an always-visible scrollbar.
const SCROLLBAR_GUTTER: Pixels = px(14.);
/// The icon of an empty state.
const EMPTY_STATE_ICON: Rems = rems(32. / 14.);
/// Minimum spacing between polled reading-position saves during scrolling.
/// How long scrolling must rest before the reading position is recorded.
const READING_POSITION_SETTLE: Duration = Duration::from_secs(1);

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
        ReaderEnd,
        ImageTextUp,
        ImageTextDown
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
    /// A drag panning the enlarged image: where it started and the scroll
    /// offset then.
    drag: Option<(Point<Pixels>, Point<Pixels>)>,
    details_open: Option<bool>,
    return_focus: Option<FocusHandle>,
    focus: FocusHandle,
    /// The dialog's two halves, notified when the viewer state changes.
    dialogs: [EntityId; 2],
}

const ZOOM_STEP_IN: f32 = 1.25;
const ZOOM_STEP_OUT: f32 = 0.8;
const ZOOM_MIN: f32 = 0.1;
const ZOOM_MAX: f32 = 4.;

fn zoomed(scale: f32, factor: f32) -> f32 {
    (scale * factor).clamp(ZOOM_MIN, ZOOM_MAX)
}

/// Image dialog geometry from the shared viewer margins and padding.
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
        let width =
            f32::from(size.width - IMAGE_DIALOG_MARGIN * 2.).min(f32::from(IMAGE_DIALOG_MAX_WIDTH));
        let available = f32::from(size.height - task_dialog_top(window) - IMAGE_DIALOG_MARGIN);
        Self {
            width: width.max(f32::from(IMAGE_DIALOG_MIN_WIDTH)),
            compact: width - f32::from(IMAGE_DIALOG_PADDING) * 2. < rem * 52.,
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
    version: PathBuf,
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
            true,
            course2md::tr!("复制整份笔记 · Markdown 文本"),
            icons::content_copy(),
        ),
        (
            false,
            course2md::tr!("复制整份笔记 · 纯文本"),
            icons::text_lines(),
        ),
    ] {
        let weak = desktop.clone();
        let version = version.clone();
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
    for (format, label, icon) in [
        (
            course2md::config::OutputFormat::Md,
            course2md::tr!("导出 Markdown（含截图，.zip）…"),
            icons::download(),
        ),
        (
            course2md::config::OutputFormat::Html,
            course2md::tr!("导出网页文件…"),
            icons::globe(),
        ),
        (
            course2md::config::OutputFormat::Json,
            course2md::tr!("导出 JSON 数据…"),
            icons::braces(),
        ),
    ] {
        let weak = desktop.clone();
        let version = version.clone();
        menu = menu.item(
            PopupMenuItem::new(label)
                .icon(icon)
                .disabled(exporting)
                .on_click(move |_, window, cx| {
                    let _ = weak.update(cx, |this, cx| {
                        if let Some(course) = this.note_at(&version) {
                            this.export_course(course, format, window, cx);
                        }
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
    /// The contents entry last scrolled into view, so the rail follows the
    /// reading position without fighting a reader who scrolls it.
    toc_revealed: Option<usize>,
    view_focus: [FocusHandle; 2],
    menu_focus: FocusHandle,
    find: FindState,
    gallery: GalleryState,
    restore: RestoreState,
    focus: FocusHandle,
    info_open: bool,
    processing_details_open: bool,
    // None follows the available reading width; an explicit choice survives resizing.
    toc_open: Option<bool>,
    loaded: Option<PathBuf>,
    generation: u64,
    data_loading: bool,
    frames: Rc<Vec<Frame>>,
    frame_by_path: Rc<HashMap<PathBuf, usize>>,
    missing_by_block: Rc<HashMap<usize, Vec<usize>>>,
    /// The note's contents rail and the chapter titles its headings open.
    outline: Rc<Vec<(usize, String, Option<f64>)>>,
    chapter_starts: Rc<HashMap<usize, String>>,
    versions: Vec<Version>,
    issues: Vec<String>,
    note_layout: Rc<RefCell<nav::ReadingLayout>>,
    // The note tab's article flow is a variable-height `list`: persistent
    // state, the flattened item sequence it describes and per-item focus
    // containers that keep a focused control mounted while scrolled out.
    note_list: ListState,
    note_items: Rc<Vec<NoteItem>>,
    note_focus: Rc<Vec<FocusHandle>>,
    viewer: Option<ImageViewer>,
    /// Screenshot thumbnails and originals, decoded off the UI thread.
    images: crate::reader_images::ReaderImages,
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

/// In-note find: its input, the matches and the current one.
struct FindState {
    input: Entity<InputState>,
    open: bool,
    return_focus: Option<FocusHandle>,
    matches: Rc<Vec<Match>>,
    /// The note text prepared once per load for matching.
    prepared: Vec<nav::PreparedText>,
    current: usize,
}

/// The screenshot tab: its search, matching frames and virtualized rows.
struct GalleryState {
    find: Entity<InputState>,
    search_position: Option<workspace::ReadingPosition>,
    /// Screenshots matching the gallery search, in note order.
    matches: Rc<Vec<usize>>,
    /// Each matching screenshot's excerpt and the find ranges inside it.
    excerpts: Rc<HashMap<usize, GalleryExcerpt>>,
    // The screenshot tab is a variable-height `list` of grid rows, like the
    // note tab: its items, per-row focus containers and the column count
    // the rows were cut for.
    list: ListState,
    items: Rc<Vec<GalleryItem>>,
    focus: Rc<Vec<FocusHandle>>,
    columns: usize,
    layout: Rc<RefCell<nav::ReadingLayout>>,
}

/// Returning to a saved reading position or search match after a load,
/// a tab switch or a reflow, and recording the position while reading.
struct RestoreState {
    pending: Option<workspace::ReadingPosition>,
    search: Option<Match>,
    active: bool,
    generation: u64,
    last_position: Option<(String, workspace::ReadingPosition)>,
    /// The pending record of a settling scroll; replacing it restarts the wait.
    save: Option<Task<()>>,
    /// A note restore waiting for its target block's first measurement.
    note_refine: Option<NoteRefine>,
    target: Rc<Cell<Option<usize>>>,
    /// Window width, height and rem size the reading position was laid out at.
    window_layout: (f32, f32, f32),
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
                let query = this.reader_ui.find.input.read(cx).value();
                this.reader_ui.find.open = !query.trim().is_empty();
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
                    .gallery
                    .find
                    .read(cx)
                    .value()
                    .trim()
                    .is_empty();
                if searching {
                    if this.reader_ui.gallery.search_position.is_none() {
                        this.save_reading_position(cx);
                        this.reader_ui.gallery.search_position = this.capture_reading_position();
                    }
                    this.reader_ui.gallery.list.scroll_to(ListOffset::default());
                } else if let Some(position) = this.reader_ui.gallery.search_position.take() {
                    this.reader_ui.restore.pending = Some(position);
                    this.reader_ui.restore.generation += 1;
                    this.reader_ui.restore.active = false;
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
            // The screenshot's text scrolls from the keyboard as well.
            KeyBinding::new("up", ImageTextUp, Some("ReaderImage")),
            KeyBinding::new("down", ImageTextDown, Some("ReaderImage")),
            KeyBinding::new("pageup", ReaderPageUp, Some("ReaderImage")),
            KeyBinding::new("pagedown", ReaderPageDown, Some("ReaderImage")),
            KeyBinding::new("home", ReaderStart, Some("ReaderImage")),
            KeyBinding::new("end", ReaderEnd, Some("ReaderImage")),
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
        let gallery_list = ListState::new(1, ListAlignment::Top, px(1000.));
        let desktop = cx.weak_entity();
        gallery_list.set_scroll_handler(move |_, _, cx| {
            let desktop = desktop.clone();
            cx.defer(move |cx| {
                let _ = desktop.update(cx, |this, cx| this.reading_position_moved(cx));
            });
        });
        Self {
            controls_scroll: ScrollHandle::new(),
            information_scroll: ScrollHandle::new(),
            toc_scroll: ScrollHandle::new(),
            toc_revealed: None,
            view_focus: std::array::from_fn(|_| cx.focus_handle()),
            menu_focus: cx.focus_handle(),
            find: FindState {
                input: find,
                open: false,
                return_focus: None,
                matches: Rc::new(Vec::new()),
                prepared: Vec::new(),
                current: 0,
            },
            gallery: GalleryState {
                find: gallery_find,
                search_position: None,
                matches: Rc::default(),
                excerpts: Rc::default(),
                list: gallery_list,
                items: Rc::new(vec![GalleryItem::Header]),
                focus: Rc::new(vec![cx.focus_handle()]),
                columns: 1,
                layout: Rc::default(),
            },
            restore: RestoreState {
                pending: None,
                search: None,
                active: false,
                generation: 0,
                last_position: None,
                save: None,
                note_refine: None,
                target: Rc::default(),
                window_layout: reader_window_layout(window),
            },
            focus: cx.focus_handle(),
            info_open: false,
            processing_details_open: false,
            toc_open: None,
            loaded: None,
            generation: 0,
            data_loading: false,
            frames: Rc::default(),
            frame_by_path: Rc::default(),
            missing_by_block: Rc::default(),
            outline: Rc::default(),
            chapter_starts: Rc::default(),
            versions: Vec::new(),
            issues: Vec::new(),
            note_layout: Rc::default(),
            note_list: ListState::new(0, ListAlignment::Top, px(1000.)),
            note_items: Rc::default(),
            note_focus: Rc::default(),
            viewer: None,
            images: Default::default(),
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
/// One item of the virtualized screenshot gallery.
#[derive(Clone, Debug, PartialEq, Eq)]
enum GalleryItem {
    /// The title on compact windows and the gallery's loading, empty or
    /// no-match state.
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

/// A screenshot's three-line excerpt and its find matches.
struct GalleryExcerpt {
    text: SharedString,
    marks: Vec<Range<usize>>,
}

/// The screenshot tab's reading position: the first screenshot of the top
/// row and how far into that row the view starts. Rows are measured in
/// window coordinates under their first screenshot (see `ReadingLayout`).
fn gallery_capture_position(
    items: &[GalleryItem],
    matches: &[usize],
    list: &ListState,
    layout: &nav::ReadingLayout,
    anchor: impl FnOnce(usize) -> (Option<String>, Option<f64>),
) -> Option<workspace::ReadingPosition> {
    let top = list.logical_scroll_top();
    // The top is a document boundary: the header, however tall, reads as it.
    let Some(GalleryItem::Row(row)) = items.get(top.item_ix) else {
        return Some(workspace::ReadingPosition::default());
    };
    let first = *matches.get(row.start)?;
    let within = -f32::from(top.offset_in_item);
    let (paragraph, seconds) = anchor(first);
    Some(workspace::ReadingPosition {
        paragraph,
        seconds,
        offset: f32::from(list.scroll_px_offset_for_scrollbar().y).min(0.),
        within,
        fraction: layout
            .item_height(first)
            .map(|height| nav::within_fraction(within, height)),
    })
}

/// Where a saved gallery position opens: the row now holding its screenshot,
/// at the same proportion of that row once the row has been measured.
fn gallery_restore_offset(
    items: &[GalleryItem],
    matches: &[usize],
    frame: Option<usize>,
    position: &workspace::ReadingPosition,
    layout: &nav::ReadingLayout,
) -> ListOffset {
    let found = frame.and_then(|frame| {
        let at = matches.iter().position(|index| *index == frame)?;
        items
            .iter()
            .enumerate()
            .find_map(|(item_ix, item)| match item {
                GalleryItem::Row(row) if row.contains(&at) => Some((item_ix, matches[row.start])),
                _ => None,
            })
    });
    let Some((item_ix, first)) = found else {
        return ListOffset::default();
    };
    let within = match layout.item_height(first) {
        Some(height) => nav::restore_within(position.fraction, position.within, height),
        None => position.within.min(0.),
    };
    ListOffset {
        item_ix,
        offset_in_item: px(-within),
    }
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

/// What the page draws from the note's scroll position: whether the article
/// has left its top, and the contents entry the top of the text is in.
fn note_view_key(
    items: &[NoteItem],
    list: &ListState,
    outline: &[(usize, String, Option<f64>)],
) -> (bool, Option<usize>) {
    let top_block = note_top_block(items, list.logical_scroll_top().item_ix).unwrap_or(0);
    (
        note_scroll_key(items, list).is_some(),
        outline_current(outline, top_block),
    )
}

/// The contents entry that holds `block`: the last heading at or before it.
fn outline_current(outline: &[(usize, String, Option<f64>)], block: usize) -> Option<usize> {
    outline
        .iter()
        .rev()
        .find(|(index, _, _)| *index <= block)
        .map(|(index, _, _)| *index)
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

/// How far a card excerpt moves its cut to reach a word boundary.
const EXCERPT_WORD_SLACK: usize = 24;

fn frame_excerpt(frame: &Frame, query: &str) -> String {
    use unicode_segmentation::UnicodeSegmentation;
    let text = frame.transcript.as_str();
    if let Some(found) = nav::text_matches(text, query).first() {
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
        let mut end = text[found.end..]
            .grapheme_indices(true)
            .nth(100)
            .map(|(index, _)| found.end + index)
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
        for (range, piece) in crate::note_text::piece_sources(text) {
            if matches!(piece, crate::math::Piece::Math { .. }) {
                if range.start < start && start < range.end {
                    start = range.start;
                }
                if range.start < end && end < range.end {
                    end = range.end;
                }
            }
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
/// Selectable note text with find highlights (see `note_text`). A search
/// target also records the current match's line once it is laid out, so
/// find can scroll to it.
fn reader_text(
    id: impl Into<ElementId>,
    text: impl Into<SharedString>,
    document_order: u64,
    highlights: Vec<(Range<usize>, HighlightStyle)>,
    search_target: Option<ReaderSearchTarget>,
) -> crate::note_text::NoteText {
    let text = crate::note_text::NoteText::new(id, text)
        .document_order(document_order)
        .selection_color(color(READING_SELECTION).into())
        .highlights(highlights);
    match search_target {
        Some(target) => text.on_source_position(target.byte, move |position, line_height| {
            // Window coordinates: see `ReadingLayout`.
            target.positions.borrow_mut().record_search_line(
                target.block,
                target.byte,
                f32::from(position.y),
                f32::from(line_height),
            );
        }),
        None => text,
    }
}

struct ReaderSearchTarget {
    block: usize,
    byte: usize,
    positions: Rc<RefCell<nav::ReadingLayout>>,
}
/// A paragraph of note text in the reading type; `copy` says how it copies.
fn paragraph(
    id: impl Into<ElementId>,
    text: SharedString,
    order: usize,
    highlights: Vec<(Range<usize>, HighlightStyle)>,
    search_target: Option<ReaderSearchTarget>,
    copy: crate::note_text::Copy,
    place: TextPlace,
) -> Stateful<Div> {
    let id = id.into();
    div()
        .id(id.clone())
        .role(Role::Paragraph)
        .aria_label(text.clone())
        .text_size(READER_BODY)
        .font_weight(FontWeight::NORMAL)
        .line_height(relative(1.8))
        .child(
            reader_text(id, text, order as u64, highlights, search_target)
                .copy(copy)
                .width_group(place.group())
                .surface(place.surface().into()),
        )
}

/// Where a paragraph sits: paragraphs in one place share a width (so a
/// newly shown one places its formulas right away) and a surface.
#[derive(Clone, Copy)]
enum TextPlace {
    Article,
    Summary,
    KeyPoint,
    Viewer,
}

impl TextPlace {
    fn group(self) -> &'static str {
        match self {
            Self::Article => "article",
            Self::Summary => "summary",
            Self::KeyPoint => "key-point",
            Self::Viewer => "viewer",
        }
    }

    fn surface(self) -> Rgba {
        match self {
            Self::Summary | Self::KeyPoint => summary_tint(),
            Self::Article | Self::Viewer => color(SURFACE),
        }
    }
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
    /// The article card's inline padding.
    inset: Rems,
    /// The decode width of the article's screenshot figures.
    figure_size: crate::reader_images::Size,
    layout: Rc<RefCell<nav::ReadingLayout>>,
    restore_target: Rc<Cell<Option<usize>>>,
    note_list: ListState,
    desktop: WeakEntity<Desktop>,
}

impl NoteFlow {
    /// Warnings for the screenshots of block `index` that could not be read.
    fn missing_images(&self, index: usize) -> impl Iterator<Item = Stateful<Div>> + '_ {
        self.missing_by_block
            .get(&index)
            .into_iter()
            .flatten()
            .map(|&frame_index| {
                missing_image_notice(
                    ("missing-body-image", frame_index),
                    course2md::tr!("{}无法读取", frame_label(&self.frames[frame_index])),
                )
            })
    }

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
        let (first, last) = (index == 0, index + 1 == self.items.len());
        // Items paint the article card together: the first rounds its top,
        // the last its bottom, and the 28px rhythm is padding inside the card.
        // Virtual list measurements include padding, not external margins.
        let card = div()
            .w_full()
            .min_w_0()
            .bg(color(SURFACE))
            .px(self.inset)
            .pb(rems(28. / 14.))
            .when(first, |card| {
                card.rounded_t(RADIUS_HERO).pt(if self.compact {
                    rems(24. / 14.)
                } else {
                    rems(40. / 14.)
                })
            })
            .when(last, |card| {
                card.rounded_b(RADIUS_HERO).pb(if self.compact {
                    rems(32. / 14.)
                } else {
                    rems(56. / 14.)
                })
            })
            .child(content);
        let mut wrapper = div()
            .w_full()
            .min_w_0()
            .flex_shrink_0()
            .when(last, |wrapper| wrapper.pb(rems(48. / 14.)));
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
            .child(card)
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
    // The item's screenshots, decoded at the figure's width by the reader's
    // bounded cache rather than at full size for the rest of the session.
    let pictures: HashMap<usize, crate::reader_images::Loaded> = item
        .blocks()
        .filter_map(|index| match &preview.blocks[index] {
            PreviewBlock::Image(path) => Some((index, path.clone())),
            _ => None,
        })
        .map(|(index, path)| {
            let loaded = flow
                .desktop
                .update(cx, |this, cx| {
                    this.reader_ui
                        .images
                        .get(&path, flow.figure_size, Desktop::reader_images, cx)
                })
                .unwrap_or(crate::reader_images::Loaded::Pending);
            (index, loaded)
        })
        .collect();
    let content = match item {
        NoteItem::Title => flow
            .desktop
            .update(cx, |this, cx| {
                let ReaderHeaderParts { title, meta } = this.reader_metadata_row(
                    preview,
                    flow.compact,
                    flow.short,
                    flow.note_list.clone(),
                    cx,
                );
                let notices = if flow.short {
                    this.reader_notices(preview, NoticeScroll::List(flow.note_list.clone()), cx)
                } else {
                    Vec::new()
                };
                v_flex()
                    .w_full()
                    .min_w_0()
                    .gap_3()
                    .children(notices)
                    .child(title)
                    .children(meta)
                    .into_any_element()
            })
            .unwrap_or_else(|_| div().into_any_element()),
        NoteItem::Summary { start, end } => render_note_summary(flow, start..end),
        NoteItem::Section {
            heading,
            end,
            ordinal,
        } => render_note_section(flow, heading, end, ordinal, &pictures, cx),
        NoteItem::Block(index) => render_note_block(flow, index, &pictures, cx),
    };
    flow.item_wrapper(item_ix, content).into_any_element()
}

/// One block on its own: a paragraph, heading or screenshot outside any section.
fn render_note_block(
    flow: &NoteFlow,
    index: usize,
    pictures: &HashMap<usize, crate::reader_images::Loaded>,
    cx: &App,
) -> AnyElement {
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
            cx,
        ),
        PreviewBlock::Paragraph { text, anchor } => note_paragraph(flow, index, text, anchor),
        PreviewBlock::Translation { text, anchor } => note_translation(flow, index, text, anchor),
        PreviewBlock::Image(path) => render_note_image(flow, index, path, pictures.get(&index), cx),
    }
}

fn note_paragraph(flow: &NoteFlow, index: usize, text: &SharedString, anchor: &str) -> AnyElement {
    paragraph(
        SharedString::from(anchor.to_owned()),
        text.clone(),
        index,
        flow.highlight_runs(index),
        flow.search_target(index),
        crate::note_text::Copy::Plain,
        TextPlace::Article,
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
        crate::note_text::Copy::Plain,
        TextPlace::Article,
    )
    .text_color(color(GRAY))
    .into_any_element()
}

/// The summary as a tinted panel inside the article: its overview, then the
/// key points as a list.
fn render_note_summary(flow: &NoteFlow, blocks: std::ops::Range<usize>) -> AnyElement {
    let preview = &flow.preview;
    let mut overview = Vec::new();
    let mut points = Vec::new();
    for index in blocks {
        if let PreviewBlock::Paragraph { text, anchor } = &preview.blocks[index] {
            if anchor.starts_with("key-point-") {
                let point = text.trim_start_matches("• ").to_owned();
                // Find measures the block text, bullet included.
                let bullet = text.len() - point.len();
                let marks = flow
                    .highlight_runs(index)
                    .into_iter()
                    .filter(|(range, _)| range.start >= bullet)
                    .map(|(range, style)| (range.start - bullet..range.end - bullet, style))
                    .collect();
                let target = flow.search_target(index).map(|mut target| {
                    target.byte = target.byte.saturating_sub(bullet);
                    target
                });
                points.push(
                    h_flex()
                        .min_w_0()
                        .items_start()
                        .gap_3()
                        .child(
                            div()
                                .mt(rems(12. / 14.))
                                .size(px(5.))
                                .flex_shrink_0()
                                .rounded_full()
                                .bg(color(ON_SECONDARY_CONTAINER)),
                        )
                        .child(
                            div().flex_1().min_w_0().child(
                                paragraph(
                                    SharedString::from(anchor.clone()),
                                    point.into(),
                                    index,
                                    marks,
                                    target,
                                    crate::note_text::Copy::ListItem,
                                    TextPlace::KeyPoint,
                                )
                                .text_size(rems(16. / 14.)),
                            ),
                        )
                        .into_any_element(),
                );
            } else {
                overview.push(
                    paragraph(
                        SharedString::from(anchor.to_owned()),
                        text.clone(),
                        index,
                        flow.highlight_runs(index),
                        flow.search_target(index),
                        crate::note_text::Copy::Plain,
                        TextPlace::Summary,
                    )
                    .text_size(rems(16. / 14.))
                    .into_any_element(),
                );
            }
        }
    }
    v_flex()
        .w_full()
        .min_w_0()
        .px(if flow.compact {
            rems(16. / 14.)
        } else {
            rems(24. / 14.)
        })
        .py(rems(20. / 14.))
        .gap(rems(12. / 14.))
        .rounded(rems(20. / 14.))
        .bg(summary_tint())
        .child(
            h_flex()
                .gap_2()
                .items_center()
                .text_color(color(ON_SECONDARY_CONTAINER))
                .child(icons::sparkles().size(rems(18. / 14.)))
                .child(
                    theme::accessible_text("reader-summary-label", course2md::tr!("摘要"))
                        .role(Role::Heading)
                        .text_size(TEXT_BODY)
                        .font_weight(FontWeight::SEMIBOLD),
                ),
        )
        .child(v_flex().gap_2().children(overview))
        .when(!points.is_empty(), |card| {
            card.child(v_flex().gap(rems(6. / 14.)).children(points))
        })
        .into_any_element()
}

/// The summary panel: the secondary container lightened toward the article.
fn summary_tint() -> Rgba {
    blend(color(SURFACE), color(SECONDARY_CONTAINER), 0.55)
}

/// A chip that plays the source from a moment, or the bare time when the
/// source cannot seek there. `marks` are find highlights on the time.
fn watch_from_chip(
    flow: &NoteFlow,
    heading: usize,
    seconds: f64,
    marks: Vec<(Range<usize>, HighlightStyle)>,
    cx: &App,
) -> AnyElement {
    let time = course2md::render::fmt_ts(seconds);
    let time_text = if marks.is_empty() {
        div().child(time.clone()).into_any_element()
    } else {
        reader_text(
            ("reader-heading-marks", heading),
            SharedString::from(time.clone()),
            heading as u64,
            marks,
            flow.search_target(heading),
        )
        .into_any_element()
    };
    let height = rems(36. / 14.);
    match flow
        .source
        .as_ref()
        .and_then(|source| nav::seek_url(source, seconds))
    {
        Some(url) => flow
            .reveal(
                ("reveal-watch-from", heading).into(),
                tonal_pill(("watch-from", heading), cx)
                    .h(height)
                    .min_h(height)
                    .flex_shrink_0()
                    .rounded(RADIUS_PILL)
                    .pl(rems(10. / 14.))
                    .pr(rems(1.))
                    .gap(rems(6. / 14.))
                    .text_size(rems(13. / 14.))
                    .icon(icons::play())
                    .child(time_text)
                    .child(course2md::tr!("从此处观看"))
                    .accessibility_label(course2md::tr!("从 {} 播放原视频", time))
                    .on_click(move |_, _, cx| cx.open_url(&url))
                    .into_any_element(),
            )
            .inline()
            .into_any_element(),
        None => h_flex()
            .id(("section-time", heading))
            .flex_shrink_0()
            .items_center()
            .h(rems(30. / 14.))
            .px(rems(10. / 14.))
            .rounded(RADIUS_SMALL)
            .bg(color(CANVAS))
            .text_size(rems(13. / 14.))
            .font_weight(FontWeight::MEDIUM)
            .text_color(color(GRAY))
            .child(time_text)
            .into_any_element(),
    }
}

/// A section: the chapter title where one begins with the chip that plays
/// the source from its time, the paragraphs, then the screenshot.
fn render_note_section(
    flow: &NoteFlow,
    heading: usize,
    end: usize,
    _ordinal: usize,
    pictures: &HashMap<usize, crate::reader_images::Loaded>,
    cx: &App,
) -> AnyElement {
    let preview = &flow.preview;
    let (anchor, seconds) = match &preview.blocks[heading] {
        PreviewBlock::Heading {
            anchor, seconds, ..
        } => (anchor.clone(), *seconds),
        _ => (String::new(), None),
    };
    let images = (heading + 1..end).filter_map(|index| match &preview.blocks[index] {
        PreviewBlock::Image(path) => Some(render_note_image(
            flow,
            index,
            path,
            pictures.get(&index),
            cx,
        )),
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
    let title_row = h_flex()
        .id(SharedString::from(anchor))
        .w_full()
        .flex_wrap()
        .items_center()
        .gap_3()
        .role(Role::Heading)
        .aria_label(
            chapter
                .map(|chapter| format!("{} {chapter}", time.clone().unwrap_or_default()))
                .or(time.clone())
                .unwrap_or_default(),
        )
        .children(chapter.map(|chapter| {
            div()
                .flex_1()
                .flex_basis(rems(240. / 14.))
                .min_w_0()
                .text_size(rems(24. / 14.))
                .line_height(rems(32. / 14.))
                .font_weight(FontWeight::MEDIUM)
                // Selectable with the paragraphs, so a copied section keeps its title.
                .child(reader_text(
                    ("reader-chapter", heading),
                    SharedString::from(chapter.clone()),
                    heading as u64,
                    Vec::new(),
                    None,
                ))
        }))
        .children(seconds.map(|seconds| watch_from_chip(flow, heading, seconds, marks, cx)));
    v_flex()
        .w_full()
        .min_w_0()
        .gap_4()
        .child(title_row)
        .children(flow.missing_images(heading))
        .children(paragraphs)
        .children(images.map(|image| div().w_full().mt_2().child(image)))
        .into_any_element()
}

/// A heading outside the sections, with its time chip when it has one.
fn render_note_heading(
    flow: &NoteFlow,
    index: usize,
    text: SharedString,
    anchor: &str,
    seconds: Option<f64>,
    timestamp_only: bool,
    cx: &App,
) -> AnyElement {
    let marks = flow.highlight_runs(index);
    let chapter = flow.chapter_starts.get(&index);
    // Without a chapter title, a heading that is only its time is the chip itself.
    let bare_timestamp = chapter.is_none() && timestamp_only;
    let heading_style = |view: Stateful<Div>| {
        view.role(Role::Heading)
            .flex_1()
            .flex_basis(rems(240. / 14.))
            .min_w_0()
            .text_size(rems(24. / 14.))
            .line_height(rems(32. / 14.))
            .font_weight(FontWeight::MEDIUM)
    };
    let (heading, chip_marks) = if bare_timestamp {
        (None, marks)
    } else if !marks.is_empty() {
        (
            Some(
                heading_style(div().id(("reader-heading-wrap", index)))
                    .aria_label(text.clone())
                    .child(reader_text(
                        ("reader-heading-marks", index),
                        text.clone(),
                        index as u64,
                        marks,
                        flow.search_target(index),
                    ))
                    .into_any_element(),
            ),
            Vec::new(),
        )
    } else {
        (
            Some({
                let title = chapter
                    .cloned()
                    .map_or_else(|| text.clone(), SharedString::from);
                heading_style(div().id(("reader-heading", index)))
                    .aria_label(title.clone())
                    .child(reader_text(
                        ("reader-heading-text", index),
                        title,
                        index as u64,
                        Vec::new(),
                        None,
                    ))
                    .into_any_element()
            }),
            Vec::new(),
        )
    };
    let chip = seconds.map(|seconds| watch_from_chip(flow, index, seconds, chip_marks, cx));
    v_flex()
        .id(SharedString::from(anchor.to_owned()))
        .gap_2()
        .when(bare_timestamp, |view| {
            view.role(Role::Heading).aria_label(text.clone())
        })
        .child(
            h_flex()
                .w_full()
                .flex_wrap()
                .gap_3()
                .items_center()
                .children(heading)
                .children(chip),
        )
        .children(flow.missing_images(index))
        .into_any_element()
}

/// A screenshot inside the article: a 16:9 figure with its time in the
/// corner and a button that opens the viewer.
fn render_note_image(
    flow: &NoteFlow,
    index: usize,
    path: &std::path::Path,
    picture: Option<&crate::reader_images::Loaded>,
    cx: &App,
) -> AnyElement {
    use crate::reader_images::Loaded;
    let frame_index = flow.frame_by_path.get(path).copied();
    let frame = frame_index.map(|i| &flow.frames[i]);
    // A file that is missing or whose header cannot be read gets the same
    // figure and warning as one that fails to decode.
    let picture = if frame.is_none() && !flow.data_loading {
        Some(&Loaded::Failed)
    } else {
        picture
    };
    let label = frame
        .map(frame_label)
        .unwrap_or_else(|| course2md::tr!("截图").into());
    let open = {
        let desktop = flow.desktop.clone();
        move |window: &mut Window, cx: &mut App| {
            if let Some(frame_index) = frame_index {
                let _ = desktop.update(cx, |this, cx| {
                    this.open_reader_image(frame_index, window, cx);
                });
            }
        }
    };
    let open_click = open.clone();
    let time = frame.map(|frame| course2md::render::fmt_ts(frame.seconds));
    flow.reveal(
        ("reveal-note-image", index).into(),
        div()
            .id(("note-figure", index))
            .relative()
            .w_full()
            .aspect_ratio(16. / 9.)
            .rounded(FIGURE_RADIUS)
            .overflow_hidden()
            .bg(color(SURFACE_HIGH))
            .when(frame.is_some(), |figure| {
                figure
                    .cursor_pointer()
                    .on_click(move |_, window, cx| open_click(window, cx))
            })
            .map(|figure| match picture {
                Some(Loaded::Ready(image, fresh)) => {
                    let picture = img(image.clone())
                        .absolute()
                        .inset_0()
                        .size_full()
                        .rounded(FIGURE_RADIUS)
                        .object_fit(ObjectFit::Contain);
                    figure.child(if *fresh {
                        crate::motion::enter(("figure-in", index), picture)
                    } else {
                        picture.into_any_element()
                    })
                }
                Some(Loaded::Failed) => figure.child(
                    unreadable_screenshot(("failed-reader-image", index))
                        .absolute()
                        .inset_0(),
                ),
                // The same quiet placeholder as the screenshot cards.
                _ => figure.child(screenshot_placeholder()),
            })
            .children(time.map(image_time_chip))
            .when(frame.is_some(), |figure| {
                figure.child(
                    div()
                        .absolute()
                        .right(rems(12. / 14.))
                        .bottom(rems(12. / 14.))
                        .child(
                            image_corner_button(("note-image", index), icons::maximize(), cx)
                                .accessibility_label(course2md::tr!("放大{}", label))
                                .tooltip(course2md::tr!("放大"))
                                .on_click(move |_, window, cx| {
                                    // The figure under the button opens the viewer too.
                                    cx.stop_propagation();
                                    open(window, cx)
                                }),
                        ),
                )
            })
            .into_any_element(),
    )
    .into_any_element()
}

/// The viewer's media area for a screenshot that cannot be read: the same
/// warning fill as its card, over the space the image would take.
fn unreadable_viewer_image() -> Stateful<Div> {
    unreadable_screenshot("image-unreadable")
        .flex_1()
        .min_h(rems(8.))
        .w_full()
        .rounded(RADIUS_CARD)
}

/// A screenshot whose file cannot be read, in the space its image takes:
/// the warning fill with the image glyph and message centred, the same in
/// a card, an article figure and the viewer.
fn unreadable_screenshot(id: impl Into<ElementId>) -> Stateful<Div> {
    let id = id.into();
    v_flex()
        .id(id.clone())
        .items_center()
        .justify_center()
        .gap_2()
        .p_4()
        .bg(color(WARNING_BG))
        .text_color(color(WARNING))
        .child(icons::image().size(EMPTY_STATE_ICON))
        .child(
            theme::accessible_text(
                ElementId::NamedChild(Arc::new(id), "text".into()),
                course2md::tr!("截图无法读取"),
            )
            .text_size(TEXT_AUX)
            .text_center(),
        )
}

/// A screenshot still decoding: a quiet image glyph on its tile, one calm
/// placeholder per figure rather than a spinner on every one.
fn screenshot_placeholder() -> Div {
    div()
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .child(icons::image().size(EMPTY_STATE_ICON).text_color(Rgba {
            a: 0.45,
            ..color(GRAY)
        }))
}

/// A scroll region's bar in the page gutter just past its trailing edge,
/// clear of the rounded card surfaces it scrolls.
/// What a notice's focused control scrolls into view: the bounded notice
/// region above the views, or the note or screenshot list it heads.
enum NoticeScroll {
    Region(ScrollHandle),
    List(ListState),
}

/// The share of a compact reader's height that its notices may take before
/// they scroll.
const NOTICES_SHARE: f32 = 0.4;

fn gutter_scrollbar(scrollbar: Scrollbar) -> Div {
    div()
        .absolute()
        .top_0()
        .bottom_0()
        .right(-SCROLLBAR_GUTTER)
        .w(SCROLLBAR_GUTTER)
        // Its track follows this gutter, not the list's own viewport.
        .child(scrollbar.viewport_from_layout())
}

/// The article card keeps a rounded top edge where the scroll region cuts
/// it, and text scrolling past that edge fades out instead of being cut
/// through a line. At the top of the note it coincides with the card's own
/// corners and padding, so nothing shows.
fn scrolled_card_top() -> Div {
    let corner = |left: bool| {
        div()
            .absolute()
            .top_0()
            .when(left, |corner| corner.left_0())
            .when(!left, |corner| corner.right_0())
            .size(RADIUS_HERO)
            .overflow_hidden()
            .bg(color(CANVAS))
            .child(
                div()
                    .absolute()
                    .top_0()
                    .when(left, |circle| circle.left_0())
                    .when(!left, |circle| circle.right_0())
                    .size(RADIUS_HERO * 2.)
                    .rounded_full()
                    .bg(color(SURFACE)),
            )
    };
    div()
        .absolute()
        .top_0()
        .left_0()
        .right_0()
        .h(rems(24. / 14.))
        .bg(linear_gradient(
            180.,
            linear_color_stop(color(SURFACE), 0.),
            linear_color_stop(
                Rgba {
                    a: 0.,
                    ..color(SURFACE)
                },
                1.,
            ),
        ))
        .child(corner(true))
        .child(corner(false))
}

/// Screenshot figures in the article.
const FIGURE_RADIUS: Pixels = px(20.);

/// A screenshot's time over its lower-left corner, on the image scrim.
/// A contents entry's state layer sits on whatever it rests on (the page
/// beside the article, the surface over it, or the current entry's tonal
/// fill), so hover never replaces the selection or darkens a dark palette.
fn toc_item_variant(on: bool, side: bool, cx: &App) -> ButtonCustomVariant {
    let (base, ink) = if on {
        (color(SECONDARY_CONTAINER), color(ON_SECONDARY_CONTAINER))
    } else if side {
        (color(CANVAS), color(INK))
    } else {
        (color(SURFACE), color(INK))
    };
    ButtonCustomVariant::new(cx)
        .color(Rgba { a: 0., ..base }.into())
        .foreground(ink.into())
        .hover(blend(base, ink, 0.08).into())
        .active(blend(base, ink, 0.10).into())
        .shadow(false)
}

fn image_time_chip(time: String) -> Div {
    div()
        .absolute()
        .left(rems(12. / 14.))
        .bottom(rems(12. / 14.))
        .px(rems(10. / 14.))
        .py(rems(3. / 14.))
        .rounded(RADIUS_SMALL)
        .bg(image_scrim())
        .text_color(on_image_scrim())
        .text_size(TEXT_AUX)
        .font_weight(FontWeight::MEDIUM)
        .child(time)
}

/// A 40px round action over a screenshot's corner, on a near-opaque surface.
fn image_corner_button(id: impl Into<ElementId>, icon: Icon, cx: &App) -> Button {
    icon_button(id, rems(40. / 14.), true, cx)
        .bg(Rgba {
            a: 0.92,
            ..color(SURFACE)
        })
        .text_color(color(INK))
        .icon(icon)
}

/// A screenshot that could not be read, in place of its figure.
fn missing_image_notice(id: impl Into<ElementId>, text: impl Into<SharedString>) -> Stateful<Div> {
    h_flex()
        .id(id)
        .w_full()
        .min_w_0()
        .items_center()
        .gap_2()
        .px(rems(16. / 14.))
        .py(rems(12. / 14.))
        .rounded(rems(12. / 14.))
        .bg(color(WARNING_BG))
        .text_color(color(WARNING))
        .text_size(TEXT_AUX)
        .child(icons::image().size(ICON_SLOT).flex_shrink_0())
        .child(div().flex_1().min_w_0().child(text.into()))
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
            let list = &self.reader_ui.gallery.list;
            let step = list.viewport_bounds().size.height * 0.85 * direction;
            list.scroll_by(-step);
        }
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
        gallery_capture_position(
            &self.reader_ui.gallery.items,
            &self.reader_ui.gallery.matches,
            &self.reader_ui.gallery.list,
            &self.reader_ui.gallery.layout.borrow(),
            |index| {
                self.reader_ui
                    .frames
                    .get(index)
                    .map(|frame| (Some(frame.anchor.clone()), Some(frame.seconds)))
                    .unwrap_or((None, None))
            },
        )
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
        self.reader_ui.restore.search = None;
        self.reader_ui.restore.pending = Some(
            self.reading_key()
                .and_then(|key| self.workspace.as_ref()?.state.positions.get(&key))
                .cloned()
                .unwrap_or_default(),
        );
        self.reader_ui.restore.generation += 1;
        self.reader_ui.restore.active = false;
        self.reader_ui.restore.last_position = None;
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
        let Some(position) = self.reader_ui.restore.pending.take() else {
            return;
        };
        self.reader_ui.restore.active = true;
        let search = self.reader_ui.restore.search.take();
        let generation = self.reader_ui.restore.generation;
        let key = self.reading_key();
        cx.on_next_frame(window, move |this, _, cx| {
            if this.reader_ui.restore.generation != generation || this.reading_key() != key {
                return;
            }
            if this.result_tab == 0 {
                this.apply_note_restore(position, search, true, cx);
                return;
            }
            let target = gallery_restore_offset(
                &this.reader_ui.gallery.items,
                &this.reader_ui.gallery.matches,
                this.position_index(&position),
                &position,
                &this.reader_ui.gallery.layout.borrow(),
            );
            this.reader_ui.gallery.list.scroll_to(target);
            this.finish_reader_restore(cx);
        });
    }

    fn finish_reader_restore(&mut self, cx: &mut Context<Self>) {
        self.reader_ui.restore.active = false;
        self.reader_ui.restore.last_position = None;
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
            .restore
            .target
            .set(note_item_block(self.reader_ui.note_items[item_ix]));
        self.reader_ui.restore.note_refine = Some(NoteRefine {
            position,
            search,
            generation: self.reader_ui.restore.generation,
        });
        cx.notify();
    }

    fn refine_note_restore(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let Some(refine) = self.reader_ui.restore.note_refine.take() else {
            return;
        };
        if refine.generation == self.reader_ui.restore.generation && self.result_tab == 0 {
            self.apply_note_restore(refine.position, refine.search, false, cx);
        }
    }

    /// Window resizes and text-scale changes reflow the reader: keep the
    /// reading position of the previous layout.
    fn reader_layout_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let layout = reader_window_layout(window);
        if self.reader_ui.restore.window_layout == layout {
            return;
        }
        self.reader_ui.restore.window_layout = layout;
        if self.page != Page::Result || self.preview.is_none() {
            return;
        }
        if self.reader_ui.restore.pending.is_none() && !self.reader_ui.restore.active {
            self.reader_ui.restore.pending = self.capture_reading_position();
            self.reader_ui.restore.generation += 1;
        }
        self.reader_ui.note_layout.borrow_mut().clear();
        self.reader_ui.gallery.layout.borrow_mut().clear();
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
        self.reader_ui.images.clear(cx);
        self.reader_ui.toc_revealed = None;
        self.reader_ui.toc_scroll.set_offset(point(px(0.), px(0.)));
        self.reader_ui.frame_by_path = Rc::default();
        self.reader_ui.missing_by_block = Rc::default();
        self.reader_ui.gallery.matches = Rc::default();
        self.reader_ui.gallery.excerpts = Rc::default();
        self.set_gallery_items(self.reader_ui.gallery.columns, cx);
        self.reader_ui.versions.clear();
        self.reader_ui.issues.clear();
        self.reader_ui.source = None;
        // A different note: stop the relocation probe in flight.
        if let Some(cancel) = self.reader_ui.source_probe_cancel.take() {
            cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        self.reader_ui.source_loading = false;
        self.reader_ui.source_available = false;
        self.reader_ui.export_folder = None;
        self.reader_ui.offline_video = OfflineVideo::NotRequested;
        self.reader_ui.offline_opening = false;
        Rc::make_mut(&mut self.reader_ui.find.matches).clear();
        self.reader_ui.gallery.search_position = None;
        self.reader_ui.find.open = false;
        self.reader_ui.info_open = false;
        self.reader_ui.processing_details_open = false;
        self.rebuild_note_flow(cx);
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
        self.reader_ui.gallery.layout.borrow_mut().clear();
        // Scrolling repaints the page only when what it shows from the
        // position changes: whether the article has left its top, and the
        // contents entry the text is in. GPUI calls the handler while it
        // still borrows the list state, so the position is read once the
        // scroll has been applied.
        let (entity, list) = (cx.weak_entity(), self.reader_ui.note_list.clone());
        let (items, outline) = (
            self.reader_ui.note_items.clone(),
            self.reader_ui.outline.clone(),
        );
        let last = Rc::new(Cell::new(note_view_key(&items, &list, &outline)));
        self.reader_ui
            .note_list
            .set_scroll_handler(move |_, _, cx| {
                let (entity, list, items, outline, last) = (
                    entity.clone(),
                    list.clone(),
                    items.clone(),
                    outline.clone(),
                    last.clone(),
                );
                cx.defer(move |cx| {
                    let key = note_view_key(&items, &list, &outline);
                    let changed = last.replace(key) != key;
                    let _ = entity.update(cx, |this, cx| {
                        if changed {
                            cx.notify();
                        }
                        this.reading_position_moved(cx);
                    });
                });
            });
    }

    /// Search the screenshots once per query change: the matching frames,
    /// their excerpts and the find ranges inside each excerpt.
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
                        GalleryExcerpt {
                            text: text.into(),
                            marks,
                        },
                    )
                })
                .collect(),
        );
        self.reader_ui.gallery.matches = Rc::new(matches);
        self.set_gallery_items(self.reader_ui.gallery.columns, cx);
    }

    /// Cut the matching screenshots into rows of `columns`.
    fn set_gallery_items(&mut self, columns: usize, cx: &App) {
        let items = gallery_items(self.reader_ui.gallery.matches.len(), columns);
        self.reader_ui.gallery.columns = columns;
        if *self.reader_ui.gallery.items == items {
            return;
        }
        let focus: Vec<FocusHandle> = (0..items.len()).map(|_| cx.focus_handle()).collect();
        let list = &self.reader_ui.gallery.list;
        let top = list.logical_scroll_top();
        list.reset(items.len());
        list.splice_focusable(0..items.len(), focus.iter().cloned().map(Some));
        // Keep the view near the same row; a pending restore refines it.
        list.scroll_to(ListOffset {
            item_ix: top.item_ix.min(items.len().saturating_sub(1)),
            offset_in_item: top.offset_in_item,
        });
        self.reader_ui.gallery.items = Rc::new(items);
        self.reader_ui.gallery.focus = Rc::new(focus);
        self.reader_ui.gallery.layout.borrow_mut().clear();
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
            // Parsing the note and the offline check are blocking file/subprocess work; see crate::spawn_blocking_io.
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
                if this.reader_ui.restore.pending.is_none() && !this.reader_ui.restore.active {
                    this.reader_ui.restore.pending = this.capture_reading_position();
                }
                this.reader_ui.frames = Rc::new(data.frames);
                this.reader_ui.frame_by_path = Rc::new(data.frame_by_path);
                this.reader_ui.missing_by_block = Rc::new(data.missing_by_block);
                this.update_gallery_matches(cx);
                this.reader_ui.gallery.layout.borrow_mut().clear();
                this.reader_ui.versions = data.versions;
                this.reader_ui.issues = data.issues;
                this.reader_ui.export_folder = data.export_folder;
                this.reader_ui.source = source;
                this.reader_ui.source_available = source_available;
                this.reader_ui.offline_video = offline_video;
                this.reader_ui.data_loading = false;
                this.reader_ui.restore.generation += 1;
                this.reader_ui.restore.active = false;
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
                // Cancel a previous probe that is still running before starting another.
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
                // Clear only this probe's flag: a later probe holds a different Arc.
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
        if !self.reader_ui.find.open {
            self.reader_ui.find.return_focus = window.focused(cx);
        }
        let input = if self.result_tab == 0 {
            self.reader_ui.find.open = true;
            self.reader_ui.find.input.clone()
        } else {
            self.reader_ui.gallery.find.clone()
        };
        input.update(cx, |input, cx| input.focus(window, cx));
        if self.result_tab == 0 {
            self.update_reader_matches(cx);
        }
        cx.notify();
    }
    fn close_reader_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.reader_ui.find.open = false;
        let input = if self.result_tab == 0 {
            self.reader_ui.find.input.clone()
        } else {
            self.reader_ui.gallery.find.clone()
        };
        input.update(cx, |input, cx| input.set_value("", window, cx));
        if let Some(focus) = self.reader_ui.find.return_focus.take() {
            focus.focus(window, cx);
        } else {
            self.reader_ui.focus.focus(window, cx);
        }
        cx.notify();
    }
    fn update_reader_matches(&mut self, cx: &mut Context<Self>) {
        let query = self.reader_ui.find.input.read(cx).value().to_string();
        if let Some(preview) = &self.preview
            && self.reader_ui.find.prepared.len() != preview.blocks.len()
        {
            self.reader_ui.find.prepared = preview
                .blocks
                .iter()
                .map(|block| {
                    block_text(block)
                        .map(nav::prepare_text)
                        .unwrap_or_else(|| nav::prepare_text(""))
                })
                .collect();
        }
        self.reader_ui.find.matches = self
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
                                .find
                                .prepared
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
        self.reader_ui.find.current = 0;
        cx.notify();
    }
    fn move_reader_match(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.result_tab != 0 {
            return;
        }
        let count = self.reader_ui.find.matches.len();
        if count == 0 {
            return;
        }
        self.reader_ui.find.current =
            (self.reader_ui.find.current as isize + delta).rem_euclid(count as isize) as usize;
        let found = self.reader_ui.find.matches[self.reader_ui.find.current].clone();
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
        self.reader_ui.restore.search = Some(found);
    }
    fn jump_reader_block(&mut self, index: usize, fraction: f32, cx: &mut Context<Self>) {
        self.reader_ui.restore.search = None;
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
        self.reader_ui.restore.pending = Some(workspace::ReadingPosition {
            paragraph: Some(block_anchor(block, index)),
            seconds: block_time(&preview.blocks, index),
            fraction: Some(fraction),
            ..Default::default()
        });
        self.reader_ui.restore.generation += 1;
        self.reader_ui.restore.active = false;
        cx.notify();
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
                        this.sync_reader_tab_stops();
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
    /// the end of the current view (where the note's card closes), as
    /// `capture.rs` names them.
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
                    if this.result_tab == 0 {
                        this.reader_ui.note_list.scroll_to_end();
                    } else {
                        this.reader_ui.gallery.list.scroll_to_end();
                    }
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
                "info" => self.reader_ui.info_open = true,
                "toc" => self.reader_ui.toc_open = Some(true),
                "images" => {
                    self.result_tab = 1;
                    self.sync_reader_tab_stops();
                }
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
                let retry = course.dir.clone();
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
                                outline_pill(SharedString::from(format!("{id}:retry")), cx)
                                    .icon(icons::refresh())
                                    .label(course2md::tr!("重试导出"))
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
            self.sync_reader_tab_stops();
            if index == 1
                && !self
                    .reader_ui
                    .gallery
                    .find
                    .read(cx)
                    .value()
                    .trim()
                    .is_empty()
            {
                self.reader_ui.restore.pending = None;
                self.reader_ui.gallery.list.scroll_to(ListOffset::default());
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
        self.reader_ui.images.next_frame();
        self.schedule_reader_restore(window, cx);
        let outline = self.reader_ui.outline.clone();
        let metrics = ReaderMetrics::new(window, self.result_tab, !outline.is_empty());
        let (controls, tools) = self.reader_controls(&preview, &outline, metrics, window, cx);
        let tabs = (!metrics.short).then(|| self.reader_tabs_row(&preview, &outline, metrics, cx));
        let article = if self.result_tab == 0 {
            self.reader_note_article(&preview, metrics, cx)
        } else {
            self.reader_gallery(&preview, metrics, window, cx)
        };
        let body = self.reader_body(article, &outline, metrics, cx);
        (
            crate::motion::state_enter(
                SharedString::from(format!("reader-open:{}", preview.course.dir.display())),
                self.reader_root(metrics, cx)
                    .children(controls)
                    .children(tabs)
                    .child(v_flex().flex_1().min_h_0().w_full().child(body)),
            ),
            Some(tools.into_any_element()),
        )
    }

    /// Opening a note: the return stays available beside a quiet progress card.
    fn reader_opening(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let origin = self.reader_origin_label();
        crate::motion::enter(
            "reader-opening",
            v_flex()
                .w_full()
                .max_w(ARTICLE_MAX)
                .mx_auto()
                .gap(rems(20. / 14.))
                .pt_2()
                .child(
                    icon_button("reader-loading-back", BUTTON_MEDIUM, true, cx)
                        .icon(crate::icons::arrow_left())
                        .accessibility_label(course2md::tr!("返回「{}」", origin))
                        .tooltip(course2md::tr!("返回「{}」", origin))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.navigate(this.result_origin, cx);
                        })),
                )
                .child(
                    h_flex()
                        .w_full()
                        .items_center()
                        .gap_3()
                        .px(rems(48. / 14.))
                        .py(rems(40. / 14.))
                        .rounded(RADIUS_HERO)
                        .bg(color(SURFACE))
                        .child(crate::motion::spinner("reader-opening-spinner", cx))
                        .child(
                            theme::accessible_text("opening-note", course2md::tr!("正在打开笔记…"))
                                .role(Role::Status)
                                .text_color(color(GRAY)),
                        ),
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
                    this.reader_ui.gallery.list.scroll_to(ListOffset::default());
                }
                this.reading_position_moved(cx);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ReaderEnd, _, cx| {
                if this.result_tab == 0 {
                    this.reader_ui.note_list.scroll_to_end();
                } else {
                    this.reader_ui.gallery.list.scroll_to_end();
                }
                this.reading_position_moved(cx);
                cx.notify();
            }))
            .on_action(cx.listener(move |this, _: &CloseFind, window, cx| {
                if !toc_fits_beside && this.reader_ui.toc_open == Some(true) {
                    this.reader_ui.toc_open = Some(false);
                    cx.notify();
                } else if (this.result_tab == 0 && this.reader_ui.find.open)
                    || (this.result_tab == 1
                        && !this.reader_ui.gallery.find.read(cx).value().is_empty())
                {
                    this.close_reader_find(window, cx);
                } else {
                    cx.propagate();
                }
            }))
            .h_full()
            .min_h_0()
            .w(px(metrics.reader_width))
            .ml(px(metrics.frame_offset))
            .pt_2()
            .gap(rems(20. / 14.))
            .when(metrics.compact, |view| view.gap_3())
    }

    /// Export feedback, version loading, notices and the details panel above
    /// the views, and the tools the title bar shows. Narrow windows also put
    /// the open note search here.
    fn reader_controls(
        &mut self,
        preview: &Rc<notes::Preview>,
        outline: &[(usize, String, Option<f64>)],
        metrics: ReaderMetrics,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (Option<AnyElement>, Div) {
        let ReaderMetrics {
            compact,
            available_height,
            ..
        } = metrics;
        let controls_scroll = self.reader_ui.controls_scroll.clone();
        let toolbar = self.reader_toolbar(preview, outline, &metrics, window, cx);
        let mut items: Vec<AnyElement> = Vec::new();
        if toolbar.compact_search.is_some() || toolbar.short_matches.is_some() {
            items.push(
                v_flex()
                    .w_full()
                    .min_w_0()
                    .gap_2()
                    .children(toolbar.compact_search)
                    .children(toolbar.short_matches)
                    .into_any_element(),
            );
        }
        if let Some(feedback) = self.course_export_feedback(&preview.course, cx) {
            items.push(feedback.into_any_element());
        }
        if self.reading {
            items.push(crate::motion::enter(
                "reader-version-loading",
                h_flex()
                    .items_center()
                    .gap_2()
                    .child(crate::motion::spinner("reader-version-spinner", cx))
                    .child(
                        theme::accessible_text(
                            "reader-version-loading-label",
                            course2md::tr!("正在切换版本…"),
                        )
                        .role(Role::Status)
                        .text_color(color(GRAY)),
                    ),
            ));
        }
        // A short reader shows its notices at the top of the note or the
        // screenshots, scrolling away with them, so a notice is never cut
        // to fit the space above the views.
        if !metrics.short {
            items.extend(self.reader_notices(
                preview,
                NoticeScroll::Region(controls_scroll.clone()),
                cx,
            ));
        }
        let information_panel = if self.reader_ui.info_open {
            self.reader_info_panel(preview, compact, None, available_height, window, cx)
        } else {
            None
        };
        if items.is_empty() && information_panel.is_none() {
            return (None, toolbar.bar);
        }
        // Notices scroll in a bounded region on small windows so the views
        // and the text below them keep most of the height. The region holds
        // a typical notice whole; when they need more, its scrollbar stays
        // shown so a notice never looks cut off.
        let notices = (!items.is_empty()).then(|| {
            let bound = compact.then(|| px(available_height * NOTICES_SHARE));
            let overflows = bound.is_some_and(|bound| {
                controls_scroll.max_offset().y > px(0.)
                    && controls_scroll.bounds().size.height <= bound
            });
            div()
                .relative()
                .w_full()
                .min_w_0()
                .flex_shrink_0()
                .child(
                    v_flex()
                        .id("reader-controls")
                        .w_full()
                        .min_w_0()
                        .min_h_0()
                        .gap_3()
                        .when_some(bound, |view, bound| view.max_h(bound))
                        .when(overflows, |view| view.pr(SCROLLBAR_GUTTER))
                        .overflow_y_scroll()
                        .track_scroll(&controls_scroll)
                        .children(items),
                )
                .when(overflows, |region| {
                    region.child(
                        gpui_component::scroll::Scrollbar::vertical(&controls_scroll)
                            .mode(ScrollbarMode::Always),
                    )
                })
        });
        let controls = v_flex()
            .w_full()
            .min_w_0()
            .min_h_0()
            .flex_shrink_0()
            .gap_3()
            .children(notices)
            .children(information_panel);
        (Some(controls.into_any_element()), toolbar.bar)
    }

    /// The views as connected tabs, the contents toggle when the contents
    /// cannot sit beside the article, and the version choice.
    fn reader_tabs_row(
        &mut self,
        preview: &Rc<notes::Preview>,
        outline: &[(usize, String, Option<f64>)],
        metrics: ReaderMetrics,
        cx: &mut Context<Self>,
    ) -> Div {
        let reading_note = self.result_tab == 0;
        let shots = preview.course.slides;
        let mut row = h_flex()
            .w_full()
            .min_w_0()
            .flex_shrink_0()
            .flex_wrap()
            .gap_3()
            .items_center()
            .child(
                div().flex_shrink_0().child(
                    SingleChoiceGroup::new("reader-tabs", course2md::tr!("阅读视图"))
                        .tabs()
                        .height(rems(44. / 14.))
                        .options([
                            ("note", course2md::tr!("笔记").to_owned()),
                            (
                                "images",
                                if shots > 0 {
                                    course2md::tr!("截图 · {}", shots)
                                } else {
                                    course2md::tr!("截图").to_owned()
                                },
                            ),
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
                ),
            )
            .child(div().flex_1());
        if reading_note && !outline.is_empty() && !metrics.toc_fits_beside {
            let toc_on = self.reader_ui.toc_open.unwrap_or(false);
            row = row.child(
                quiet("note-contents")
                    .h(rems(36. / 14.))
                    .min_h(rems(36. / 14.))
                    .icon(icons::toc())
                    .label(course2md::tr!("阅读目录"))
                    .selected(toc_on)
                    .accessibility_label(if toc_on {
                        course2md::tr!("收起目录")
                    } else {
                        course2md::tr!("阅读目录")
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.reader_ui.restore.pending = this.capture_reading_position();
                        this.reader_ui.restore.generation += 1;
                        this.reader_ui.toc_open = Some(!toc_on);
                        cx.notify();
                    })),
            );
        }
        // A single version needs no selector; its revision and date stay in
        // the note details.
        if let Some(manifest) = &preview.course.manifest
            && self.reader_ui.versions.len() > 1
        {
            let weak = cx.weak_entity();
            let versions = self.reader_ui.versions.clone();
            let current = preview.course.dir.clone();
            let chip = rems(36. / 14.);
            row = row.child(
                quiet("choose-note-version")
                    .h(chip)
                    .min_h(chip)
                    .pl(rems(1.))
                    .pr(rems(10. / 14.))
                    .gap(rems(6. / 14.))
                    .rounded(RADIUS_SMALL)
                    .border_1()
                    .border_color(color(CONTROL))
                    .label_text_size(rems(13. / 14.))
                    .label(if metrics.compact {
                        course2md::tr!("第 {} 版", manifest.revision)
                    } else {
                        course2md::tr!(
                            "第 {} 版 · {} 生成",
                            manifest.revision,
                            nav::date_local(manifest.created_at_ms)
                        )
                    })
                    .child(
                        crate::icons::chevron_down()
                            .size(rems(16. / 14.))
                            .flex_shrink_0(),
                    )
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
                    }),
            );
        }
        row
    }

    /// The note tab's article: a variable-height list that builds only the
    /// visible window of blocks and its overdraw. Its items paint one surface
    /// card together, so the card scrolls with the text.
    fn reader_note_article(
        &mut self,
        preview: &Rc<notes::Preview>,
        metrics: ReaderMetrics,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let article_width = metrics.article_width();
        // Narrow cards keep more of their width for text.
        let inset = if article_width < metrics.rem * 40. {
            20. / 14.
        } else {
            48. / 14.
        };
        let figure_width = (article_width - inset * metrics.rem * 2.) * metrics.scale_factor;
        let flow = NoteFlow {
            items: self.reader_ui.note_items.clone(),
            focus: self.reader_ui.note_focus.clone(),
            preview: preview.clone(),
            frames: self.reader_ui.frames.clone(),
            frame_by_path: self.reader_ui.frame_by_path.clone(),
            missing_by_block: self.reader_ui.missing_by_block.clone(),
            chapter_starts: self.reader_ui.chapter_starts.clone(),
            source: self.reader_source(),
            matches: self.reader_ui.find.matches.clone(),
            match_index: self.reader_ui.find.current,
            find_open: self.reader_ui.find.open,
            data_loading: self.reader_ui.data_loading,
            compact: metrics.compact,
            short: metrics.short,
            inset: rems(inset),
            figure_size: crate::reader_images::Size::thumbnail(figure_width),
            layout: self.reader_ui.note_layout.clone(),
            restore_target: self.reader_ui.restore.target.clone(),
            note_list: self.reader_ui.note_list.clone(),
            desktop: cx.weak_entity(),
        };
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
            .child(
                list(self.reader_ui.note_list.clone(), move |index, _, cx| {
                    render_note_item(&flow, index, cx)
                })
                .w_full()
                .h_full(),
            )
            .child(scrolled_card_top())
            .child(gutter_scrollbar(crate::backend::vertical_scrollbar_for(
                &self.reader_ui.note_list,
            )))
            // Dragging the scrollbar moves the list without its scroll handler;
            // the release records the settled position (unchanged ones are skipped).
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.reading_position_moved(cx)),
            )
            .into_any_element()
    }

    /// The contents beside the article on wide windows, or over it on narrow
    /// ones once asked for.
    fn reader_body(
        &mut self,
        article: AnyElement,
        outline: &[(usize, String, Option<f64>)],
        metrics: ReaderMetrics,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if outline.is_empty() || self.result_tab != 0 {
            return article;
        }
        let top = self.reader_ui.note_list.logical_scroll_top();
        let top_block = note_top_block(&self.reader_ui.note_items, top.item_ix).unwrap_or(0);
        let current = outline_current(outline, top_block);
        if current != self.reader_ui.toc_revealed {
            let position_of =
                |block: Option<usize>| outline.iter().position(|(index, ..)| Some(*index) == block);
            let previous = position_of(self.reader_ui.toc_revealed);
            self.reader_ui.toc_revealed = current;
            // The rail's first child is its title, entries follow in order
            // and its closing space comes last. Reading on reveals the entry
            // after the current one too (or that closing space), so the
            // current entry never rests on the rail's edge; reading back
            // reveals the current entry itself.
            if let Some(position) = position_of(current) {
                let ahead = previous.is_none_or(|previous| position > previous);
                self.reader_ui
                    .toc_scroll
                    .scroll_to_item(position + if ahead { 2 } else { 1 });
            }
        }
        if metrics.toc_fits_beside {
            h_flex()
                .flex_1()
                .min_h_0()
                .w_full()
                .gap(TOC_GAP)
                .items_stretch()
                .child(self.reader_toc_panel(outline, current, Some(metrics.toc_width), cx))
                .child(article)
                .into_any_element()
        } else if self.reader_ui.toc_open == Some(true) {
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
                        .rounded(RADIUS_HERO)
                        .bg(color(SURFACE))
                        .child(self.reader_toc_panel(outline, current, None, cx)),
                )
                .into_any_element()
        } else {
            article
        }
    }
}

/// Reader geometry at the current window size and text scale.
#[derive(Clone, Copy)]
struct ReaderMetrics {
    /// The frame shared by the view tabs and the content below them.
    reader_width: f32,
    /// The frame's leading edge in window coordinates, which the title bar's
    /// return button shares, and how far that is past the page column's edge.
    frame_left: f32,
    frame_offset: f32,
    /// The contents column when it sits beside the article.
    toc_width: f32,
    toc_fits_beside: bool,
    /// The note tab shows its contents beside the article.
    toc_beside: bool,
    available_height: f32,
    compact: bool,
    short: bool,
    rem: f32,
    scale_factor: f32,
}

const TOC_GAP: Pixels = px(24.);
const GALLERY_GAP: f32 = 12.;
/// The article card's widest measure, and the narrowest it gets beside the contents.
const ARTICLE_MAX: Rems = rems(820. / 14.);
const ARTICLE_MIN_BESIDE: Rems = rems(560. / 14.);
const TOC_MIN: Rems = rems(220. / 14.);
const TOC_MAX: Rems = rems(280. / 14.);
/// Space the shell keeps above and below the page, under its title bar.
const PAGE_VERTICAL_PADDING: Pixels = px(48.);
const COMPACT_MIN_WIDTH: Rems = rems(60.);
/// The note search field in the title bar, and how narrow it may become.
const READER_SEARCH_WIDTH: Rems = rems(240. / 14.);
const READER_SEARCH_MIN_WIDTH: Rems = rems(140. / 14.);
const COMPACT_MIN_HEIGHT: Rems = rems(36.);
const SHORT_MAX_HEIGHT: Rems = rems(24.);
/// The screenshot grid's narrowest column, and its images' corners.
const GALLERY_MIN_COLUMN: f32 = 260.;
const GALLERY_IMAGE_RADIUS: Pixels = px(16.);

impl ReaderMetrics {
    fn new(window: &Window, tab: usize, has_outline: bool) -> Self {
        let rem = window.rem_size();
        let pixels = |value: Rems| f32::from(value.to_pixels(rem));
        let gap = f32::from(TOC_GAP);
        let shell = crate::views::shell_content_width(Page::Result, window);
        // The contents and the article together, or the screenshot grid.
        let full = shell.min(pixels(TOC_MAX) + gap + pixels(ARTICLE_MAX));
        let toc_fits_beside = full >= pixels(TOC_MIN) + gap + pixels(ARTICLE_MIN_BESIDE);
        let available_height = f32::from(
            window.bounds().size.height - reader_title_bar_height(window) - PAGE_VERTICAL_PADDING,
        )
        .max(0.);
        // A note without contents beside it keeps the frame at the article's
        // measure, so the tabs end where the text ends.
        let wanted = if tab == 0 && !(has_outline && toc_fits_beside) {
            shell.min(pixels(ARTICLE_MAX))
        } else {
            full
        };
        // One leading ruler for the return button, the view tabs and the
        // contents rail: the frame stays centred in the page column unless
        // that would put it left of the title bar's content, which the
        // traffic lights push in; then it starts where the return button does.
        let window_width = f32::from(window.bounds().size.width);
        let column_left = (window_width - shell) / 2.;
        let frame_left = ((window_width - wanted) / 2.)
            .max(column_left)
            .max(crate::views::title_bar_leading());
        let reader_width = wanted.min(window_width - column_left - frame_left).max(0.);
        Self {
            reader_width,
            frame_left,
            frame_offset: frame_left - column_left,
            toc_width: (full - gap - pixels(ARTICLE_MAX)).clamp(pixels(TOC_MIN), pixels(TOC_MAX)),
            toc_fits_beside,
            toc_beside: tab == 0 && has_outline && toc_fits_beside,
            available_height,
            compact: full < pixels(COMPACT_MIN_WIDTH)
                || available_height < pixels(COMPACT_MIN_HEIGHT),
            short: available_height < pixels(SHORT_MAX_HEIGHT),
            rem: f32::from(rem),
            scale_factor: window.scale_factor(),
        }
    }

    /// The article card's width in the current frame.
    fn article_width(&self) -> f32 {
        let beside = if self.toc_beside {
            self.toc_width + f32::from(TOC_GAP)
        } else {
            0.
        };
        (self.reader_width - beside).min(820. * self.rem / 14.)
    }
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

    /// The article's opening: the title, its facts as chips, and the
    /// obstacles to watching the video with their recovery actions.
    fn reader_metadata_row(
        &mut self,
        preview: &Rc<notes::Preview>,
        compact: bool,
        short_reader: bool,
        list: ListState,
        cx: &mut Context<Self>,
    ) -> ReaderHeaderParts {
        // The header is the note list's first item: a focused control in it
        // scrolls the list to stay visible.
        let reveal = |id: &'static str, child: AnyElement| {
            crate::focus_scroll::RevealFocus::in_list(id, child, list.clone())
                .inline()
                .into_any_element()
        };
        let meta = &preview.document.meta;
        let platform = if meta.is_local() {
            Some(course2md::tr!("本地视频"))
        } else {
            crate::views::video_site_name(&meta.webpage_url)
        };
        let chip = |id: &'static str, icon: Option<Icon>, text: String| {
            h_flex()
                .id(id)
                .flex_shrink_0()
                .items_center()
                .gap(rems(6. / 14.))
                .h(rems(28. / 14.))
                .px(rems(10. / 14.))
                .rounded(RADIUS_SMALL)
                .bg(color(CANVAS))
                .text_size(rems(13. / 14.))
                .text_color(color(GRAY))
                .children(icon.map(|icon| icon.size(rems(1.)).flex_shrink_0()))
                .child(theme::accessible_text(
                    SharedString::from(format!("{id}-text")),
                    text,
                ))
        };
        let source = match (platform, meta.uploader.trim()) {
            (Some(platform), "") => Some(platform.to_owned()),
            (Some(platform), uploader) => Some(format!("{platform} · {uploader}")),
            (None, "") => None,
            (None, uploader) => Some(uploader.to_owned()),
        };
        let chips = h_flex()
            .w_full()
            .flex_wrap()
            .gap_2()
            .children(source.map(|text| chip("reader-source-chip", Some(icons::tv()), text)))
            .when(meta.duration > 0., |row| {
                row.child(chip(
                    "reader-duration-chip",
                    None,
                    course2md::render::fmt_ts(meta.duration),
                ))
            })
            .when(preview.course.slides > 0, |row| {
                row.child(chip(
                    "reader-shots-chip",
                    None,
                    preview.course.content_description(),
                ))
            });
        let title = v_flex()
            .w_full()
            .min_w_0()
            .gap_3()
            .child(
                theme::accessible_text("reader-title", preview.course.title.clone())
                    .role(Role::Heading)
                    .min_w_0()
                    .whitespace_normal()
                    .text_size(if compact {
                        rems(26. / 14.)
                    } else {
                        rems(32. / 14.)
                    })
                    .line_height(if compact {
                        rems(34. / 14.)
                    } else {
                        rems(42. / 14.)
                    })
                    .font_weight(FontWeight::MEDIUM),
            )
            .child(chips)
            .when(short_reader, |title| title.flex_shrink_0())
            .into_any_element();
        // Playing lives in the title bar; what stays here are the obstacles.
        let mut obstacles = h_flex().gap_2().flex_wrap().items_center();
        let mut has_obstacle = false;
        let offline = matches!(self.reader_ui.offline_video, OfflineVideo::Available(_));
        if let Some(nav::SourceTarget::Local(_)) = self.reader_source()
            && !offline
            && !self.reader_ui.source_available
        {
            has_obstacle = true;
            obstacles = obstacles
                .child(
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
                ));
        }
        if self.reader_ui.offline_video == OfflineVideo::Missing {
            has_obstacle = true;
            obstacles = obstacles
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
        ReaderHeaderParts {
            title,
            meta: has_obstacle.then(|| obstacles.into_any_element()),
        }
    }

    /// What the title bar's play button does: the kept offline copy first,
    /// then the source.
    fn play_reader_video(&mut self, cx: &mut Context<Self>) {
        if matches!(self.reader_ui.offline_video, OfflineVideo::Available(_)) {
            if !self.reader_ui.offline_opening {
                self.play_reader_offline_video(cx);
            }
        } else {
            self.open_reader_source(None, cx);
        }
    }

    /// Whether the title bar offers the video (see `reader_play_button`).
    fn reader_can_play(&self) -> bool {
        matches!(self.reader_ui.offline_video, OfflineVideo::Available(_))
            || match self.reader_source() {
                Some(nav::SourceTarget::Web(_)) => true,
                Some(nav::SourceTarget::Local(_)) => self.reader_ui.source_available,
                None => false,
            }
    }

    /// Whether the title bar is too narrow to hold the note title readably
    /// beside the reading tools, measured from the tools it will show at the
    /// current text scale. Then it takes its tight form: the view menu keeps
    /// only its icon, and the title takes the breadcrumb's line and wraps.
    fn reader_bar_tight(&self, metrics: &ReaderMetrics, window: &Window) -> bool {
        if !is_compact(window) {
            return false;
        }
        let rem = metrics.rem;
        let scaled = |value: f32| value * rem / 14.;
        let button = f32::from(BUTTON_MEDIUM.to_pixels(window.rem_size()));
        let reading_note = self.result_tab == 0;
        let mut tools = Vec::new();
        if reading_note {
            tools.push(button);
        }
        if self.reader_can_play() {
            tools.push(button);
        }
        if metrics.short {
            // The view menu: padding, icon, gaps, label and chevron.
            let label = if reading_note {
                course2md::tr!("笔记")
            } else {
                course2md::tr!("截图")
            };
            tools.push(
                scaled(12. + 16. + 8. + 8. + 16. + 12.)
                    + shaped_width(label, rem, FontWeight::MEDIUM, window),
            );
        } else {
            // The icon-only export menu and the ⋯ menu.
            tools.push(scaled(14. + 20. + 8. + 18. + 12.));
            tools.push(button);
        }
        let gaps = 8. * tools.len().saturating_sub(1) as f32;
        let window_width = f32::from(window.bounds().size.width);
        let trailing = (window_width - metrics.frame_left - metrics.reader_width)
            .max(crate::window_controls::title_bar_trailing(window));
        let room = window_width
            - metrics.frame_left
            - trailing
            - button
            - 12. * 2.
            - tools.iter().sum::<f32>()
            - gaps;
        room < scaled(160.)
    }

    /// The title bar's way to watch the video: the kept offline copy first,
    /// then the source page or the local file when it is still there.
    fn reader_play_button(&self, icon_only: bool, cx: &mut Context<Self>) -> Option<Button> {
        let label = course2md::tr!("播放原视频");
        let button = outline_pill("reader-play", cx)
            .h(BUTTON_MEDIUM)
            .min_h(BUTTON_MEDIUM)
            .flex_shrink_0()
            .icon(icons::play())
            .accessibility_label(label)
            .when(icon_only, |button| {
                button
                    .w(BUTTON_MEDIUM)
                    .min_w(BUTTON_MEDIUM)
                    .px_0()
                    .tooltip(label)
            })
            .when(!icon_only, |button| {
                button
                    .pl(rems(16. / 14.))
                    .pr(rems(20. / 14.))
                    .label_text_size(rems(15. / 14.))
                    .label(label)
            });
        if matches!(self.reader_ui.offline_video, OfflineVideo::Available(_)) {
            return Some(
                button
                    .loading(self.reader_ui.offline_opening)
                    .disabled(self.reader_ui.offline_opening)
                    .on_click(cx.listener(|this, _, _, cx| this.play_reader_offline_video(cx))),
            );
        }
        match self.reader_source()? {
            nav::SourceTarget::Web(url) => Some(
                button
                    .accessibility_label(course2md::tr!("打开原视频：{}", url))
                    .on_click(cx.listener(|this, _, _, cx| this.open_reader_source(None, cx))),
            ),
            nav::SourceTarget::Local(path) if self.reader_ui.source_available => Some(
                button
                    .when(!icon_only, |button| {
                        button.tooltip(path.display().to_string())
                    })
                    .on_click(cx.listener(|this, _, _, cx| this.open_reader_source(None, cx))),
            ),
            nav::SourceTarget::Local(_) => None,
        }
    }

    /// The title bar's reading tools: note search, the video, copy and
    /// export, and the remaining actions. Narrow windows keep only icons.
    fn reader_toolbar(
        &mut self,
        preview: &Rc<notes::Preview>,
        _headings: &[(usize, String, Option<f64>)],
        metrics: &ReaderMetrics,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> ReaderToolbarParts {
        let (compact, short_reader) = (metrics.compact, metrics.short);
        let icon_only = is_compact(window);
        let reading_note = self.result_tab == 0;
        // The reading tools sit in the title bar: presses on them are theirs,
        // not the start of a window move.
        let mut toolbar = crate::window_controls::title_bar_control(
            h_flex().min_w_0().flex_shrink(1.).gap_2().items_center(),
        );
        let mut compact_search = None;
        let mut short_matches = None;
        // The screenshot view carries its own search above the grid.
        if reading_note {
            if self.reader_ui.find.open {
                let (search, matches) = self.reader_find_controls(short_reader, window, cx);
                short_matches = matches;
                let search_row = h_flex().min_w_0().gap_2().items_center().child(search);
                if compact || short_reader {
                    compact_search = Some(search_row.w_full());
                } else {
                    toolbar = toolbar.child(
                        search_row
                            .w(READER_SEARCH_WIDTH)
                            .flex_shrink(1.)
                            .min_w(READER_SEARCH_MIN_WIDTH),
                    );
                }
            }
            if !self.reader_ui.find.open || compact || short_reader {
                let shortcut = if cfg!(target_os = "macos") {
                    "⌘F"
                } else {
                    "Ctrl+F"
                };
                toolbar = toolbar.child(
                    icon_button("reader-open-find", BUTTON_MEDIUM, false, cx)
                        .icon(icons::search())
                        .selected(self.reader_ui.find.open)
                        .accessibility_label(course2md::tr!("查找笔记内容"))
                        .tooltip(format!("{} {shortcut}", course2md::tr!("查找笔记内容")))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.open_reader_find(window, cx);
                        })),
                );
            }
        }
        // Where the app draws the window's caption buttons, a tight bar also
        // gives them the play button's room, so the note title keeps as much
        // as it has where the system draws them; playing moves to the menu.
        let tight = self.reader_bar_tight(metrics, window);
        let play_in_menu = tight && crate::window_controls::caption_width(window) > 0.;
        if !play_in_menu {
            toolbar = toolbar.children(self.reader_play_button(icon_only, cx));
        }
        let output_weak = cx.entity().downgrade();
        let exporting = self.reader_exporting();
        let exporting_current = self
            .reader_ui
            .export_state
            .pending
            .as_ref()
            .is_some_and(|(_, source)| *source == preview.course.dir);
        let export_version = preview.course.dir.clone();
        let export_folder = self.reader_ui.export_folder.clone();
        if !short_reader {
            toolbar = toolbar.child(
                tonal_pill("export-note", cx)
                    .h(BUTTON_MEDIUM)
                    .min_h(BUTTON_MEDIUM)
                    .flex_shrink_0()
                    .pl(rems(18. / 14.))
                    .pr(rems(16. / 14.))
                    .when(icon_only, |button| button.pl(rems(1.)).pr(rems(12. / 14.)))
                    .icon(icons::content_copy())
                    .when(!icon_only, |button| {
                        button
                            .label_text_size(rems(15. / 14.))
                            .label(if exporting_current {
                                course2md::tr!("正在导出…")
                            } else {
                                course2md::tr!("复制与导出")
                            })
                    })
                    .accessibility_label(course2md::tr!("复制或导出整份笔记"))
                    .tooltip(course2md::tr!("复制与导出"))
                    .child(
                        crate::icons::chevron_down()
                            .size(rems(18. / 14.))
                            .flex_shrink_0(),
                    )
                    .loading(exporting_current)
                    .disabled(exporting)
                    .dropdown_menu(move |menu, _, _| {
                        reader_export_items(
                            menu.min_w(px(320.)),
                            output_weak.clone(),
                            export_version.clone(),
                            export_folder.clone(),
                            exporting,
                        )
                    }),
            );
        }
        toolbar = toolbar.child(div().flex_shrink_0().child(self.reader_more_menu(
            preview,
            short_reader,
            tight,
            play_in_menu && self.reader_can_play(),
            cx,
        )));
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
            self.reader_ui.find.input.clone()
        } else {
            self.reader_ui.gallery.find.clone()
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
                if reading_note {
                    BUTTON_MEDIUM
                } else {
                    rems(56. / 14.)
                },
                window,
                cx,
            ));
        let mut short_matches = None;
        if !input.read(cx).value().trim().is_empty() {
            let count = if reading_note {
                self.reader_ui.find.matches.len()
            } else {
                self.reader_ui.gallery.matches.len()
            };
            let mut matches = h_flex().gap_1().items_center().child(
                theme::accessible_text(
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
        tight: bool,
        play: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let reading_note = self.result_tab == 0;
        let view_label = if reading_note {
            course2md::tr!("笔记")
        } else {
            course2md::tr!("截图")
        };
        let exporting = self.reader_exporting();
        let more_weak = cx.weak_entity();
        let original = self.reader_source();
        let has_source = original.is_some();
        let missing_local = matches!(original, Some(nav::SourceTarget::Local(_)))
            && !self.reader_ui.source_available;
        let exported = self.reader_ui.exports.get(&preview.course.dir).cloned();
        // A web source opens on its site; the title bar may be playing the
        // kept offline copy instead.
        let source_label = match &original {
            Some(nav::SourceTarget::Web(url)) => match crate::views::video_site_name(url) {
                Some(site) => course2md::tr!("在 {} 打开", site),
                None => course2md::tr!("在浏览器中打开").to_owned(),
            },
            _ => course2md::tr!("打开原视频").to_owned(),
        };
        let short_export_version = preview.course.dir.clone();
        let short_export_folder = self.reader_ui.export_folder.clone();
        let more = if short_reader {
            quiet("reader-more")
        } else {
            icon_button("reader-more", BUTTON_MEDIUM, false, cx)
        };
        more.when(short_reader, |button| {
            button.track_focus(&self.reader_ui.menu_focus)
        })
        .icon(if short_reader {
            if reading_note {
                icons::article()
            } else {
                icons::image()
            }
        } else {
            icons::more_vertical()
        })
        // A tight title bar keeps the menu's icon and chevron; the current
        // view's name moves to its tooltip.
        .when(short_reader && !tight, |button| button.label(view_label))
        .when(short_reader, |button| {
            button.child(crate::icons::chevron_down().size_4().flex_shrink_0())
        })
        .when(short_reader && tight, |button| button.tooltip(view_label))
        .when(!short_reader, |button| {
            button.tooltip(course2md::tr!("更多笔记操作"))
        })
        .accessibility_label(if short_reader {
            course2md::tr!("阅读视图与笔记操作")
        } else {
            course2md::tr!("更多笔记操作")
        })
        .dropdown_menu(move |menu, window, cx| {
            let details = more_weak.clone();
            let player = more_weak.clone();
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
                let version = short_export_version.clone();
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
                                version.clone(),
                                folder.clone(),
                                exporting,
                            )
                        },
                    )
                    .separator();
            }
            if play {
                menu = menu
                    .item(
                        PopupMenuItem::new(course2md::tr!("播放原视频"))
                            .icon(icons::play())
                            .on_click(move |_, _, cx| {
                                let _ = player.update(cx, |this, cx| this.play_reader_video(cx));
                            }),
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
                            course2md::tr!("找到原视频…").to_owned()
                        } else {
                            source_label.clone()
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
            // The panel is at most 16rem tall; its body scrolls in what is left.
            let rem_size = f32::from(window.rem_size());
            let frame_height = (available_height * 0.42).min(rem_size * 16.);
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
                    bounded_scroll_panel(
                        px(frame_height),
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
                        div()
                            .id("reader-information-scroll")
                            .w_full()
                            .min_w_0()
                            .overflow_y_scroll()
                            .track_scroll(&information_scroll)
                            .child(content),
                        crate::backend::vertical_scrollbar(&information_scroll),
                    )
                    .w_full()
                    .min_w_0()
                    .flex_shrink_0()
                    .gap_2()
                    .px(rems(20. / 14.))
                    .py(rems(16. / 14.))
                    .when(compact, |panel| panel.p_3())
                    .bg(color(SURFACE))
                    .rounded(SETTINGS_CARD_RADIUS),
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
        scroll: NoticeScroll,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let reveal = |id: &'static str, child: AnyElement| match &scroll {
            NoticeScroll::Region(handle) => {
                crate::focus_scroll::RevealFocus::new(id, child, handle.clone()).inline()
            }
            NoticeScroll::List(list) => {
                crate::focus_scroll::RevealFocus::in_list(id, child, list.clone()).inline()
            }
        };
        let mut notices = Vec::new();
        if let Some(current) = &preview.course.manifest
            && let Some(newer) = self
                .library_view_cache
                .newer_version(&self.courses, current)
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
                    (outline_pill("open-newer-version", cx)
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
                .px(rems(20. / 14.))
                .py(rems(16. / 14.))
                .bg(color(WARNING_BG))
                .rounded(rems(20. / 14.))
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
                        outline_pill("reader-retry-incomplete", cx)
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
                        outline_pill("reader-repair-ai", cx)
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
                    .px(rems(20. / 14.))
                    .py(rems(12. / 14.))
                    .bg(color(WARNING_BG))
                    .rounded(rems(20. / 14.))
                    .child(
                        icons::warning()
                            .size(ICON_SLOT)
                            .flex_shrink_0()
                            .text_color(color(WARNING)),
                    )
                    .child(messages)
                    .child(reveal(
                        "reveal-reload-reader-files",
                        (outline_pill("reload-reader-files", cx)
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
    /// The screenshot tab: a virtualized list whose first item carries the
    /// compact title and the gallery's state, followed by rows of cards. Only
    /// the visible rows and their overdraw are built.
    fn reader_gallery(
        &mut self,
        preview: &Rc<notes::Preview>,
        metrics: ReaderMetrics,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // Each screenshot needs enough width for both the slide and a readable
        // text excerpt. The final row keeps the same column widths as the rest.
        let column = GALLERY_MIN_COLUMN * metrics.rem / 14.;
        let columns = ((metrics.reader_width + GALLERY_GAP) / (column + GALLERY_GAP))
            .floor()
            .clamp(1., 6.) as usize;
        if columns != self.reader_ui.gallery.columns {
            self.set_gallery_items(columns, cx);
        }
        // GPUI rounds each rendered line to whole pixels. Reserve that same
        // three-line height so enlarged text cannot run into the action row.
        let excerpt_line_height =
            px((f32::from(TEXT_BODY.to_pixels(window.rem_size())) * 1.5).round());
        // Cards draw a thumbnail decoded at their own device width, inside
        // the card's 10px padding.
        let card_width = (metrics.reader_width - GALLERY_GAP * (columns as f32 - 1.))
            / columns as f32
            - 20. * metrics.rem / 14.;
        let thumbnail = crate::reader_images::Size::thumbnail(card_width * window.scale_factor());
        let items = self.reader_ui.gallery.items.clone();
        let focus = self.reader_ui.gallery.focus.clone();
        let layout = self.reader_ui.gallery.layout.clone();
        let matches = self.reader_ui.gallery.matches.clone();
        let preview_for_items = preview.clone();
        let desktop = cx.weak_entity();
        let gallery = list(self.reader_ui.gallery.list.clone(), move |index, _, cx| {
            let Some(item) = items.get(index) else {
                return div().into_any_element();
            };
            let last = index + 1 == items.len();
            let content = desktop
                .update(cx, |this, cx| match item {
                    GalleryItem::Header => this
                        .gallery_header(&preview_for_items, metrics, cx)
                        .into_any_element(),
                    GalleryItem::Row(row) => this
                        .gallery_row(
                            &matches[row.clone()],
                            columns,
                            excerpt_line_height,
                            thumbnail,
                            cx,
                        )
                        .into_any_element(),
                })
                .unwrap_or_else(|_| div().into_any_element());
            let mut wrapper = div().w_full().min_w_0().flex_shrink_0();
            if let GalleryItem::Row(row) = item {
                // Virtual list measurements include padding, not margins.
                wrapper = wrapper.when(!last, |row| row.pb(px(GALLERY_GAP)));
                let first = matches[row.start];
                let positions = layout.clone();
                wrapper = wrapper.on_children_prepainted(move |bounds, _, _| {
                    if let Some(bounds) = bounds.first() {
                        // Window coordinates: see `ReadingLayout`.
                        positions.borrow_mut().record(
                            first,
                            f32::from(bounds.top()),
                            f32::from(bounds.size.height),
                        );
                    }
                });
            }
            wrapper
                .child(content)
                .id(("gallery-item", index))
                .track_focus(&focus[index])
                .tab_stop(false)
                .into_any_element()
        });
        // The search stays above the grid while the screenshots scroll.
        let search = (!self.reader_ui.frames.is_empty()).then(|| {
            let (field, matches) = self.reader_find_controls(true, window, cx);
            h_flex()
                .w_full()
                .min_w_0()
                .flex_shrink_0()
                .flex_wrap()
                .gap_3()
                .items_center()
                .child(
                    div()
                        .flex_1()
                        .max_w(rems(560. / 14.))
                        .min_w(rems(200. / 14.))
                        .child(field),
                )
                .children(matches.map(|matches| matches.w_auto().flex_shrink_0()))
        });
        v_flex()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .w_full()
            .h_full()
            .gap(rems(16. / 14.))
            .children(search)
            .child(
                div()
                    .id("note-reader")
                    .role(Role::Document)
                    .aria_label(preview.course.title.clone())
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .w_full()
                    .child(gallery.w_full().h_full().pb(rems(48. / 14.)))
                    .child(gutter_scrollbar(crate::backend::vertical_scrollbar_for(
                        &self.reader_ui.gallery.list,
                    )))
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| this.reading_position_moved(cx)),
                    ),
            )
            .into_any_element()
    }

    /// The gallery's first item: the title on compact windows, then loading,
    /// empty or no-match state when there is one.
    fn gallery_header(
        &mut self,
        preview: &Rc<notes::Preview>,
        metrics: ReaderMetrics,
        cx: &mut Context<Self>,
    ) -> Div {
        let mut header = v_flex().w_full().min_w_0().gap_3();
        if metrics.short {
            let list = self.reader_ui.gallery.list.clone();
            header = header.children(self.reader_notices(preview, NoticeScroll::List(list), cx));
        }
        let mut empty = true;
        if self.reader_ui.data_loading && self.reader_ui.frames.is_empty() {
            empty = false;
            header = header.child(
                h_flex()
                    .w_full()
                    .gap_3()
                    .items_center()
                    .justify_center()
                    .py(rems(48. / 14.))
                    .bg(color(SURFACE))
                    .rounded(RADIUS_HERO)
                    .child(crate::motion::spinner("reader-images-spinner", cx))
                    .child(theme::accessible_text(
                        "loading-images",
                        course2md::tr!("正在读取截图…"),
                    )),
            );
        } else if self.reader_ui.frames.is_empty() {
            empty = false;
            header = header.child(crate::motion::enter(
                "reader-images-empty",
                v_flex()
                    .w_full()
                    .py(rems(48. / 14.))
                    .px_6()
                    .gap_4()
                    .items_center()
                    .bg(color(SURFACE))
                    .rounded(RADIUS_HERO)
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
                        outline_pill("empty-images-to-note", cx)
                            .icon(icons::article())
                            .label(course2md::tr!("阅读笔记"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.select_reader_view(0, window, cx)
                            })),
                    ),
            ));
        } else if self.reader_ui.gallery.matches.is_empty() {
            empty = false;
            header = header.child(
                v_flex()
                    .w_full()
                    .py(rems(48. / 14.))
                    .gap_3()
                    .items_center()
                    .bg(color(SURFACE))
                    .rounded(RADIUS_HERO)
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
                                    .gallery
                                    .find
                                    .update(cx, |input, cx| input.set_value("", window, cx));
                            })),
                    ),
            );
        }
        // Spacing before the first row, as the grid's gap did inside one column.
        header.when(
            !empty && !self.reader_ui.gallery.matches.is_empty(),
            |header| header.pb_3(),
        )
    }

    /// One grid row of screenshot cards with equal column widths.
    fn gallery_row(
        &mut self,
        frames: &[usize],
        columns: usize,
        excerpt_line_height: Pixels,
        thumbnail: crate::reader_images::Size,
        cx: &mut Context<Self>,
    ) -> Div {
        let cards: Vec<_> = frames
            .iter()
            .map(|&index| {
                div().w_full().min_w_0().h_full().child(self.gallery_card(
                    index,
                    excerpt_line_height,
                    thumbnail,
                    cx,
                ))
            })
            .collect();
        div()
            .grid()
            .grid_cols(columns as u16)
            .w_full()
            .min_w_0()
            .gap(px(GALLERY_GAP))
            .children(cards)
    }

    /// One screenshot card: the 16:9 image with its time, a jump to the
    /// note, and a three-line excerpt of what was said. The whole card opens
    /// the viewer, as its corner button does for the keyboard.
    fn gallery_card(
        &mut self,
        index: usize,
        excerpt_line_height: Pixels,
        thumbnail: crate::reader_images::Size,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        use crate::reader_images::Loaded;
        let list = self.reader_ui.gallery.list.clone();
        let reveal = move |id: ElementId, child: AnyElement| {
            crate::focus_scroll::RevealFocus::in_list(id, child, list.clone())
        };
        let frames = self.reader_ui.frames.clone();
        let frame = &frames[index];
        let label = frame_label(frame);
        let time = course2md::render::fmt_ts(frame.seconds);
        let loaded = frame.image.as_ref().map(|file| {
            self.reader_ui
                .images
                .get(&file.path, thumbnail, Self::reader_images, cx)
        });
        let openable = matches!(loaded, Some(Loaded::Pending | Loaded::Ready(..)));
        let unreadable = || {
            unreadable_screenshot(("missing-frame", index))
                .absolute()
                .inset_0()
        };
        let image = div()
            .id(("screenshot-frame", index))
            .relative()
            .w_full()
            .aspect_ratio(16. / 9.)
            .rounded(GALLERY_IMAGE_RADIUS)
            .overflow_hidden()
            .bg(color(SURFACE_HIGH))
            .map(|image| match loaded {
                Some(Loaded::Ready(thumbnail, fresh)) => {
                    let picture = img(thumbnail)
                        .absolute()
                        .inset_0()
                        .size_full()
                        .rounded(GALLERY_IMAGE_RADIUS)
                        .object_fit(ObjectFit::Contain);
                    // A thumbnail that just arrived fades in over its
                    // placeholder; one scrolled back into view is already there.
                    image.child(if fresh {
                        crate::motion::enter(("thumbnail-in", index), picture)
                    } else {
                        picture.into_any_element()
                    })
                }
                Some(Loaded::Pending) => image.child(screenshot_placeholder()),
                Some(Loaded::Failed) | None => image.child(unreadable()),
            })
            .child(
                h_flex()
                    .absolute()
                    .right(rems(8. / 14.))
                    .bottom(rems(8. / 14.))
                    .gap_2()
                    .when_some(frame.body_index, |row, body_index| {
                        row.child(
                            reveal(
                                ("reveal-frame-to-body", index).into(),
                                image_corner_button(("frame-to-body", index), icons::article(), cx)
                                    .accessibility_label(course2md::tr!("在笔记中查看"))
                                    .tooltip(course2md::tr!("在笔记中查看"))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        // The card under the button opens the viewer.
                                        cx.stop_propagation();
                                        this.jump_reader_block(body_index, 0., cx)
                                    }))
                                    .into_any_element(),
                            )
                            .inline(),
                        )
                    })
                    .when(openable, |row| {
                        row.child(
                            reveal(
                                ("reveal-screenshot", index).into(),
                                image_corner_button(("screenshot", index), icons::maximize(), cx)
                                    .accessibility_label(course2md::tr!("放大{}", label))
                                    .tooltip(course2md::tr!("放大"))
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        cx.stop_propagation();
                                        this.open_reader_image(index, window, cx)
                                    }))
                                    .into_any_element(),
                            )
                            .inline(),
                        )
                    }),
            );
        let image = image.child(
            image_time_chip(time.clone())
                .left(rems(8. / 14.))
                .bottom(rems(8. / 14.))
                .px(rems(8. / 14.))
                .py(rems(2. / 14.))
                .rounded(px(6.)),
        );
        let (excerpt, marks) = match self.reader_ui.gallery.excerpts.get(&index) {
            Some(found) => (found.text.clone(), found.marks.as_slice()),
            None => (SharedString::default(), &[][..]),
        };
        let excerpt_marks = marks
            .iter()
            .cloned()
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
        // transcript remains readable in the image viewer and article. An
        // ellipsis marks the cut, and formulas are typeset in the line.
        let excerpt = div()
            .id(("frame-excerpt", index))
            .role(Role::Paragraph)
            .aria_label(format!("{time} {excerpt}"))
            .w_full()
            .min_w_0()
            .px(rems(6. / 14.))
            .max_h(excerpt_line_height * 3.)
            .flex_shrink_0()
            .whitespace_normal()
            .text_size(TEXT_BODY)
            .font_weight(FontWeight::NORMAL)
            .line_height(excerpt_line_height)
            .text_color(color(GRAY))
            .overflow_hidden()
            .text_ellipsis()
            .line_clamp(3)
            .child(
                crate::note_text::NoteText::new(("frame-excerpt-text", index), excerpt)
                    .highlights(excerpt_marks)
                    .width_group("screenshot-card")
                    .preview(),
            );
        v_flex()
            .id(SharedString::from(frame.anchor.clone()))
            .w_full()
            .h_full()
            .min_w_0()
            .pt(rems(10. / 14.))
            .px(rems(10. / 14.))
            .pb(rems(1.))
            .gap(rems(10. / 14.))
            .bg(color(SURFACE))
            .rounded(SETTINGS_CARD_RADIUS)
            .when(openable, |card| {
                card.cursor_pointer()
                    .hover(|card| card.bg(blend(color(SURFACE), color(INK), 0.08)))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_reader_image(index, window, cx)
                    }))
            })
            .child(image)
            .child(excerpt)
            .into_any_element()
    }

    /// Contents are a reading rail; persistent selection belongs to the row
    /// surface so pointer feedback cannot erase the current chapter. Beside
    /// the article it takes `width`; over it on narrow windows it fills the pane.
    fn reader_toc_panel(
        &self,
        headings: &[(usize, String, Option<f64>)],
        current: Option<usize>,
        width: Option<f32>,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let side = width.is_some();
        let mut panel = v_flex()
            .id("note-toc")
            .role(Role::Navigation)
            .aria_label(course2md::tr!("阅读目录"))
            .gap(px(2.))
            .overflow_y_scroll()
            .track_scroll(&self.reader_ui.toc_scroll);
        panel = match width {
            Some(width) => panel.w(px(width)).flex_shrink_0().h_full().min_h_0(),
            None => panel.w_full().h_full().min_h_0().p_3(),
        };
        panel = panel.child(
            h_flex()
                .items_center()
                .gap_2()
                .flex_shrink_0()
                .pt(rems(4. / 14.))
                .pb(rems(10. / 14.))
                .px(rems(16. / 14.))
                .child(
                    theme::accessible_text("toc-title", course2md::tr!("阅读目录"))
                        .role(Role::Heading)
                        .flex_1()
                        .text_size(TEXT_BODY)
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(color(GRAY)),
                )
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
        let row_height = rems(48. / 14.);
        for (index, label, seconds) in headings {
            let index = *index;
            let on = current == Some(index);
            let trailing_time =
                seconds.filter(|seconds| *label != course2md::render::fmt_ts(*seconds));
            let fg = color(if on { ON_SECONDARY_CONTAINER } else { INK });
            panel = panel.child(crate::focus_scroll::RevealFocus::new(
                ("reveal-toc-item", index),
                div()
                    .w_full()
                    .min_w_0()
                    .flex_shrink_0()
                    .rounded(RADIUS_PILL)
                    .when(on, |row| row.bg(color(SECONDARY_CONTAINER)))
                    .child(
                        control(("toc-item", index))
                            .custom(toc_item_variant(on, side, cx))
                            .w_full()
                            .h_auto()
                            .min_h(row_height)
                            .pl(rems(16. / 14.))
                            .pr(rems(1.))
                            .py(rems(8. / 14.))
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
                                    .gap(rems(10. / 14.))
                                    .items_center()
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .whitespace_nowrap()
                                            .text_ellipsis()
                                            .text_size(TEXT_BODY)
                                            .line_height(relative(1.4))
                                            .font_weight(if on {
                                                FontWeight::SEMIBOLD
                                            } else {
                                                FontWeight::NORMAL
                                            })
                                            .text_color(fg)
                                            .child(label.clone()),
                                    )
                                    .when_some(trailing_time, |row, seconds| {
                                        row.child(
                                            div()
                                                .flex_shrink_0()
                                                .text_size(rems(12. / 14.))
                                                .font_weight(FontWeight::NORMAL)
                                                .text_color(if on { fg } else { color(GRAY) })
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
        // The rail's closing space is a child, so revealing the last entry
        // keeps that space below it instead of resting it on the edge.
        panel.child(div().flex_shrink_0().h(rems(1.)))
    }
    pub(crate) fn open_reader_image(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(preview) = &self.preview else {
            return;
        };
        // One viewer at a time: a second dialog would share the first one's
        // state and focus, and closing either would strand the other empty.
        if self.reader_ui.viewer.is_some() && window.has_active_dialog(cx) {
            return;
        }
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
        let desktop = cx.entity();
        // The viewer's two halves render from its state and are notified only
        // when that state changes.
        let content =
            crate::views::DesktopView::notified(desktop.clone(), cx, |desktop, window, cx| {
                desktop.reader_image_content(window, cx).into_any_element()
            });
        let footer = crate::views::DesktopView::notified(desktop, cx, |desktop, window, cx| {
            desktop.reader_image_actions(window, cx).into_any_element()
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
            drag: None,
            details_open: None,
            return_focus: window.focused(cx),
            focus: focus.clone(),
            dialogs: [content.entity_id(), footer.entity_id()],
        });
        let weak = cx.weak_entity();
        window.open_dialog(cx, move |dialog, window, _| {
            let closed = weak.clone();
            let content = content.clone();
            let top = task_dialog_top(window);
            dialog
                .material_frame()
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
    /// A screenshot finished decoding: repaint what draws screenshots.
    pub(crate) fn reader_image_loaded(&mut self, cx: &mut Context<Self>) {
        if self.page == Page::Result {
            cx.notify();
            self.notify_image_viewer(cx);
        }
    }
    /// Leaving the reader releases its decoded screenshots.
    pub(crate) fn release_reader_images(&mut self, cx: &mut Context<Self>) {
        self.reader_ui.images.clear(cx);
    }
    /// Where the reader keeps its decoded screenshots.
    fn reader_images(this: &mut Self) -> &mut crate::reader_images::ReaderImages {
        &mut this.reader_ui.images
    }
    /// The dialog renders from the viewer state; only its changes repaint it.
    fn notify_image_viewer(&self, cx: &mut Context<Self>) {
        if let Some(viewer) = &self.reader_ui.viewer {
            for dialog in viewer.dialogs {
                App::notify(cx, dialog);
            }
        }
    }
    fn restore_reader_image_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.reader_ui.images.pin(Vec::new());
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
        self.notify_image_viewer(cx);
    }
    fn end_reader_image_pan(&mut self, cx: &mut Context<Self>) {
        if let Some(viewer) = &mut self.reader_ui.viewer
            && viewer.drag.take().is_some()
        {
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
    /// Scrolls the screenshot's text, when it is shown, by lines or pages.
    fn scroll_image_text(
        &mut self,
        amount: ImageTextScroll,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let Some(viewer) = &self.reader_ui.viewer else {
            return;
        };
        let line = f32::from(READER_BODY.to_pixels(window.rem_size())) * 1.8;
        let page = f32::from(viewer.scroll.bounds().size.height) * 0.85;
        let delta = match amount {
            ImageTextScroll::Lines(lines) => lines * line,
            ImageTextScroll::Pages(pages) => pages * page,
        };
        let offset = viewer.scroll.offset();
        let max = f32::from(viewer.scroll.max_offset().y);
        let moved = image_text_offset(f32::from(offset.y), max, delta);
        if moved != f32::from(offset.y) {
            viewer.scroll.set_offset(point(offset.x, px(moved)));
            self.notify_image_viewer(cx);
        }
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
            .on_action(cx.listener(|this, _: &ImageTextUp, window, cx| {
                this.scroll_image_text(ImageTextScroll::Lines(-3.), window, cx)
            }))
            .on_action(cx.listener(|this, _: &ImageTextDown, window, cx| {
                this.scroll_image_text(ImageTextScroll::Lines(3.), window, cx)
            }))
            .on_action(cx.listener(|this, _: &ReaderPageUp, window, cx| {
                this.scroll_image_text(ImageTextScroll::Pages(-1.), window, cx)
            }))
            .on_action(cx.listener(|this, _: &ReaderPageDown, window, cx| {
                this.scroll_image_text(ImageTextScroll::Pages(1.), window, cx)
            }))
            .on_action(cx.listener(|this, _: &ReaderStart, window, cx| {
                this.scroll_image_text(ImageTextScroll::Pages(f32::NEG_INFINITY), window, cx)
            }))
            .on_action(cx.listener(|this, _: &ReaderEnd, window, cx| {
                this.scroll_image_text(ImageTextScroll::Pages(f32::INFINITY), window, cx)
            }))
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
        // A file that exists but cannot be decoded is as unreadable as a missing one.
        let readable = frame.image.as_ref().is_some_and(|image| {
            !self
                .reader_ui
                .images
                .failed(&image.path, crate::reader_images::Size::Original)
        });
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
            // An unreadable screenshot has nothing to zoom.
            .when(!text_only && readable, |row| {
                row.child(self.image_zoom_controls(scale, compact, cx))
            });
        let media = (!text_only).then(|| self.image_viewer_media(scale, label.clone(), cx));
        let Some(viewer) = self.reader_ui.viewer.as_ref() else {
            return div().into_any_element();
        };
        let frame = &viewer.frames[viewer.index];
        let has_text = !frame.transcript.is_empty();
        let details = (text_only || (has_text && details_open)).then(|| {
            v_flex()
                .id("image-details")
                .w_full()
                // Short windows show either the image or its text, giving the
                // chosen content the whole available viewport.
                .when(text_only, |view| view.flex_1())
                .when(!text_only, |view| view.flex_shrink_0().max_h(relative(0.5)))
                .min_h_0()
                .gap_2()
                .pr_3()
                .overflow_y_scroll()
                // A sideways swipe scrolls a wide formula, not the text.
                .restrict_scroll_to_axis()
                .track_scroll(&viewer.scroll)
                .child(if has_text {
                    // The caption leads the text but is not copied with it.
                    paragraph(
                        "image-transcript",
                        frame.transcript.clone().into(),
                        0,
                        Vec::new(),
                        None,
                        crate::note_text::Copy::Labeled(course2md::tr!("这段讲解：{}", "").into()),
                        TextPlace::Viewer,
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
    fn image_viewer_media(
        &mut self,
        scale: f32,
        label: String,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        use crate::reader_images::{Loaded, Size};
        let Some(viewer) = &self.reader_ui.viewer else {
            return div().into_any_element();
        };
        let Some(image) = viewer.frames[viewer.index].image.clone() else {
            return unreadable_viewer_image().into_any_element();
        };
        // The neighbours decode ahead so stepping through shows them at once.
        let neighbours: Vec<PathBuf> = [viewer.index.checked_sub(1), Some(viewer.index + 1)]
            .into_iter()
            .flatten()
            .filter_map(|index| Some(viewer.frames.get(index)?.image.as_ref()?.path.clone()))
            .collect();
        let viewport_scroll = viewer.viewport_scroll.clone();
        let owner = cx.weak_entity();
        let version = viewer.version.clone();
        let width = px(image.width as f32 * scale);
        let height = px(image.height as f32 * scale);
        let viewport = viewer.viewport_size;
        let pannable = width > viewport.width + px(1.) || height > viewport.height + px(1.);
        let dragging = viewer.drag.is_some();
        let images = &mut self.reader_ui.images;
        images.pin(
            std::iter::once(image.path.clone())
                .chain(neighbours.iter().cloned())
                .collect(),
        );
        for path in &neighbours {
            images.get(path, Size::Original, Self::reader_images, cx);
        }
        let picture = match images.get(&image.path, Size::Original, Self::reader_images, cx) {
            Loaded::Ready(original, _) => Some(original),
            // The gallery's thumbnail stands in while the original decodes.
            Loaded::Pending => images.any_ready(&image.path),
            Loaded::Failed => {
                return unreadable_viewer_image().into_any_element();
            }
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
                .child(crate::motion::spinner("reader-image-loading", cx))
                .into_any_element(),
        };
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
            // An image larger than the viewport moves with the pointer, and a
            // trackpad pinch zooms it, as in Preview.
            .when(pannable, |viewport| viewport.cursor_grab())
            .when(viewer.drag.is_some(), |viewport| viewport.cursor_grabbing())
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
                            gpui_component::global_state::GlobalState::suppress_text_selection(cx);
                            this.notify_image_viewer(cx);
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

/// How far a key moves the screenshot's text: lines, or pages of its viewport.
#[derive(Clone, Copy)]
enum ImageTextScroll {
    Lines(f32),
    Pages(f32),
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

/// A panel whose header stays put and whose body scrolls once the panel
/// reaches `max_height`. Flex layout gives the body the rest of the height,
/// so no caller subtracts header, padding or border sizes by hand.
fn bounded_scroll_panel(
    max_height: Pixels,
    header: impl IntoElement,
    body: Stateful<Div>,
    scrollbar: impl IntoElement,
) -> Div {
    v_flex().max_h(max_height).child(header).child(
        div()
            .relative()
            .flex()
            .flex_col()
            .w_full()
            .min_w_0()
            .flex_shrink(1.)
            .min_h_0()
            .child(body.flex_shrink(1.).min_h_0())
            .child(scrollbar),
    )
}

#[cfg(test)]
mod tests {
    use super::{
        ExportFeedback, ExportState, Frame, GalleryItem, OfflineVideo, OfflineVideoRequest,
        PreviewBlock, block_time, existing_export_folder, files_need_reload, frame_excerpt,
        gallery_capture_position, gallery_items, gallery_restore_offset, image_text_offset,
        image_zoom_offset, load_reader_data, matching_frames, nav, processing_notice,
    };
    use crate::{notes::Course, source, workspace};

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
    fn the_gallery_top_survives_late_measurement_and_new_columns() {
        use gpui::{ListAlignment, ListState, px};
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
        let layout = nav::ReadingLayout::default();
        let position =
            gallery_capture_position(&items, &matches, &list, &layout, |_| unreachable!()).unwrap();
        assert_eq!(position, workspace::ReadingPosition::default());
        let narrow = gallery_items(matches.len(), 1);
        let target = gallery_restore_offset(&narrow, &matches, None, &position, &layout);
        assert_eq!((target.item_ix, target.offset_in_item), (0, px(0.)));
    }

    #[test]
    fn a_gallery_resize_keeps_the_same_screenshot_and_progress() {
        use gpui::{ListAlignment, ListOffset, ListState, px};
        let matches: Vec<usize> = (0..7).collect();
        let items = gallery_items(matches.len(), 3);
        let list = ListState::new(items.len(), ListAlignment::Top, px(1000.));
        list.scroll_to(ListOffset {
            item_ix: 2,
            offset_in_item: px(50.),
        });
        let mut layout = nav::ReadingLayout::default();
        layout.record(3, 400., 200.);
        let position = gallery_capture_position(&items, &matches, &list, &layout, |index| {
            (Some(format!("frame-{index}")), None)
        })
        .unwrap();
        assert_eq!(position.paragraph.as_deref(), Some("frame-3"));
        assert_eq!(position.fraction, Some(0.25));
        // Two columns: screenshot 3 now sits second in the row 2..4.
        let narrow = gallery_items(matches.len(), 2);
        let mut resized = nav::ReadingLayout::default();
        resized.record(2, 100., 400.);
        let target = gallery_restore_offset(&narrow, &matches, Some(3), &position, &resized);
        assert_eq!((target.item_ix, target.offset_in_item), (2, px(100.)));
        // Before that row is measured, the saved pixel offset stands in.
        let unmeasured = nav::ReadingLayout::default();
        let target = gallery_restore_offset(&narrow, &matches, Some(3), &position, &unmeasured);
        assert_eq!((target.item_ix, target.offset_in_item), (2, px(50.)));
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
        note_restore_target, note_scroll_key, note_top_block, outline_current,
    };
    use crate::notes::PreviewBlock;
    use gpui::{
        Context, InteractiveElement as _, IntoElement, ListAlignment, ListOffset, ListState,
        ParentElement as _, Render, StatefulInteractiveElement as _, Styled as _, TestAppContext,
        Window, div, list, px,
    };
    use std::cell::RefCell;
    use std::rc::Rc;

    struct TitleBarHarness {
        width: f32,
    }

    impl Render for TitleBarHarness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            use gpui_component::h_flex;
            let fixed = |name: &'static str, width: f32| {
                div()
                    .flex_shrink_0()
                    .w(px(width))
                    .h(px(28.))
                    .debug_selector(move || name.into())
            };
            // As in gpui-component's TitleBar: a box that never shrinks below
            // its content, around the app's bar of the window's width.
            h_flex().w(px(self.width)).child(
                h_flex().flex_shrink_0().flex_1().child(
                    h_flex()
                        .w(px(self.width))
                        .min_w_0()
                        .debug_selector(|| "bar".into())
                        .child(fixed("home", 140.))
                        .child(div().flex_1().min_w_0().child("Course title"))
                        .child(
                            h_flex()
                                .min_w_0()
                                .flex_shrink(1.)
                                .gap_2()
                                .child(fixed("play", 260.))
                                .child(
                                    h_flex()
                                        .w(super::READER_SEARCH_WIDTH)
                                        .flex_shrink(1.)
                                        .min_w(super::READER_SEARCH_MIN_WIDTH)
                                        .debug_selector(|| "search".into())
                                        .child(div().flex_1().min_w_0().h(px(28.))),
                                )
                                .child(fixed("export", 120.))
                                .child(fixed("more", 32.)),
                        ),
                ),
            )
        }
    }

    #[gpui::test]
    fn the_search_field_gives_way_before_export_leaves_the_title_bar(cx: &mut TestAppContext) {
        let (view, cx) = cx.add_window_view(|_, _| TitleBarHarness { width: 1400. });
        cx.update(|window, cx| {
            window.set_rem_size(px(17.5));
            window.draw(cx).clear(cx)
        });
        let wide = cx.debug_bounds("search").unwrap().size.width;
        assert_eq!(wide, px(240. / 14. * 17.5));
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.width = 800.;
                cx.notify();
            });
            window.draw(cx).clear(cx);
        });
        let bar = cx.debug_bounds("bar").unwrap();
        let export = cx.debug_bounds("export").unwrap();
        let more = cx.debug_bounds("more").unwrap();
        let search = cx.debug_bounds("search").unwrap();
        assert!(more.right() <= bar.right() + px(0.5), "{more:?} {bar:?}");
        assert_eq!(export.size.width, px(120.));
        assert!(search.size.width < wide && search.size.width >= px(140. / 14. * 17.5));
    }

    struct PanelHarness {
        rows: usize,
    }

    impl Render for PanelHarness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().size_full().child(
                super::bounded_scroll_panel(
                    px(200.),
                    div().h(px(40.)).debug_selector(|| "panel-header".into()),
                    div()
                        .id("panel-body")
                        .w_full()
                        .overflow_y_scroll()
                        .debug_selector(|| "panel-body".into())
                        .children((0..self.rows).map(|_| div().h(px(30.)))),
                    div(),
                )
                .p(px(12.))
                .border_1()
                .debug_selector(|| "panel".into()),
            )
        }
    }

    #[gpui::test]
    fn the_information_panel_scrolls_its_body_inside_its_maximum_height(cx: &mut TestAppContext) {
        let (view, cx) = cx.add_window_view(|_, _| PanelHarness { rows: 40 });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let panel = cx.debug_bounds("panel").unwrap();
        let header = cx.debug_bounds("panel-header").unwrap();
        let body = cx.debug_bounds("panel-body").unwrap();
        assert_eq!(panel.size.height, px(200.));
        assert!(body.top() >= header.bottom());
        assert!(
            body.bottom() <= panel.bottom() - px(12.) + px(0.5),
            "{body:?} {panel:?}"
        );
        assert!(
            body.size.height > px(100.),
            "the body takes the remaining height: {body:?}"
        );
        // Short content keeps its natural height instead of filling the maximum.
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.rows = 2;
                cx.notify();
            });
            window.draw(cx).clear(cx);
        });
        let panel = cx.debug_bounds("panel").unwrap();
        let body = cx.debug_bounds("panel-body").unwrap();
        assert_eq!(body.size.height, px(60.));
        assert!(panel.size.height < px(200.));
    }

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
    fn the_contents_follow_the_last_heading_at_or_before_the_top() {
        let outline = vec![(2, "一".to_owned(), None), (6, "二".to_owned(), None)];
        assert_eq!(outline_current(&outline, 0), None);
        assert_eq!(outline_current(&outline, 2), Some(2));
        assert_eq!(outline_current(&outline, 5), Some(2));
        assert_eq!(outline_current(&outline, 9), Some(6));
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

/// The reader's header holds 48px actions with 8px above and below.
pub(crate) fn reader_title_bar_height(window: &Window) -> Pixels {
    BUTTON_MEDIUM.to_pixels(window.rem_size()) + px(16.)
}

impl Desktop {
    /// A note is read where it was opened: a tonal return to that place, the
    /// breadcrumb over the note title, then the reading tools.
    pub(crate) fn reader_title_bar(
        &self,
        bar: Div,
        tools: AnyElement,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> TitleBar {
        let title = self
            .preview
            .as_ref()
            .map(|preview| preview.course.title.clone())
            .unwrap_or_default();
        let origin = self.reader_origin_label();
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
        // The return button starts on the reader frame's leading ruler and
        // the reading tools end on its trailing edge, so the bar, the view
        // tabs and the contents line up at every width.
        let frame = ReaderMetrics::new(window, self.result_tab, !self.reader_ui.outline.is_empty());
        let window_width = f32::from(window.bounds().size.width);
        let trailing = (window_width - frame.frame_left - frame.reader_width)
            .max(crate::window_controls::title_bar_trailing(window));
        let tight = self.reader_bar_tight(&frame, window);
        crate::views::apply_shell_title_bar_chrome(
            TitleBar::new().h(reader_title_bar_height(window)),
        )
        .child(
            bar.pl(px(frame.frame_left))
                .pr(px(trailing))
                .gap(px(12.))
                .child(crate::window_controls::title_bar_control(
                    icon_button("reader-back", BUTTON_MEDIUM, true, cx)
                        .icon(icons::arrow_left())
                        .accessibility_label(course2md::tr!("返回「{}」", origin))
                        .tooltip(course2md::tr!("返回「{}」", origin))
                        .on_click(
                            cx.listener(|this, _, _, cx| this.navigate(this.result_origin, cx)),
                        ),
                ))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        // In the tight form the breadcrumb (also named by the
                        // return button) gives its line to the title.
                        .when(!tight, |column| {
                            column.child(
                                accessible_text("reader-crumb", crumb)
                                    .truncate()
                                    .text_size(rems(13. / 14.))
                                    .line_height(rems(18. / 14.))
                                    .text_color(color(GRAY)),
                            )
                        })
                        .child(
                            accessible_text("reader-crumb-title", title)
                                .role(Role::Heading)
                                .map(|title| {
                                    if tight {
                                        title
                                            .whitespace_normal()
                                            .text_ellipsis()
                                            .line_clamp(2)
                                            .line_height(rems(24. / 14.))
                                    } else {
                                        title.truncate().line_height(rems(28. / 14.))
                                    }
                                })
                                .text_size(rems(22. / 14.))
                                .font_weight(FontWeight::NORMAL)
                                .text_color(color(INK)),
                        ),
                )
                .child(tools),
        )
    }
}
