//! Reading one note, independently of its exports. The note is one
//! Markdown document in a scrollable `TextView` (see `note_text`), which owns selection,
//! highlights and virtualization; the screenshots are a list of rows beside it.
use crate::{
    Change, Course, Page,
    exports_ui::NoteExports,
    folder_tree::{Command, command_context_menu, with_context_menu},
    note_text,
    notes::{self, PreviewBlock},
    reader_images::{Loaded, ReaderImages, Size},
    reader_navigation as nav, source, symbols, ui, workspace,
};
use desktop_reader_core::reader::*;
use desktop_store::records::{FolderPlace, Records};
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
    components::{ContextMenuHandle, MaterialContextMenu, title_bar_control},
    icons::{Icon, SymbolIcon},
    motion,
    prelude::*,
};
use std::{
    cell::Cell,
    collections::HashMap,
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

/// The screenshot viewer. It outlives closing, so the dialog can play its exit.
struct ImageViewer {
    open: bool,
    /// Its content has taken focus from the dialog.
    focused: bool,
    text_scroll: ScrollHandle,
    viewport_scroll: ScrollHandle,
    viewport_size: gpui::Size<Pixels>,
    /// The space for the image and its text together, as last laid out.
    body_size: gpui::Size<Pixels>,
    frames: Vec<Frame>,
    filtered: bool,
    index: usize,
    zoom: Option<f32>,
    /// A drag panning the enlarged image: where it started and the scroll offset then.
    drag: Option<(Point<Pixels>, Point<Pixels>)>,
    text_open: Option<bool>,
    focus: FocusHandle,
    /// The ⋮ button's screenshot commands: the button, and Shift+F10 or the
    /// Menu key anywhere in the viewer, open them beside it.
    menu: ContextMenuHandle,
    /// From the window's bottom to the top of the action row, as last laid out:
    /// a snackbar shown over the viewer stays above it.
    actions_clearance: Rc<Cell<Option<Pixels>>>,
}

/// Where the viewer puts a screenshot's text.
#[derive(Clone, Copy, PartialEq, Eq)]
enum TextPlacement {
    /// Instead of the image, in a window too short for both.
    Only,
    /// In a column beside the image.
    Beside,
    /// Under the image, at most part of the height.
    Below,
}

/// The width of the text column beside the image: a comfortable reading measure.
const VIEWER_TEXT_COLUMN: Rems = rem(360.);
/// The space the text keeps clear for its scrollbar.
const VIEWER_TEXT_GUTTER: Rems = rem(12.);
/// Between the image and the text column beside it.
const VIEWER_BESIDE_GAP: Rems = rem(24.);
/// Between the image and the text below it.
const VIEWER_BELOW_GAP: Rems = rem(8.);
/// The most of the viewer's height the text below the image takes.
const VIEWER_BELOW_TEXT: f32 = 0.4;

/// Whether an image (`width`, `height`) fits at least as large beside a text column
/// (`column` wide with its gap) as above a long text, in a body of `body` pixels.
/// Below, the text takes up to [`VIEWER_BELOW_TEXT`] of the height; neither layout
/// enlarges the image past its own size.
fn text_beside(body: (f32, f32), image: (f32, f32), column: f32, below_gap: f32) -> bool {
    let (width, height) = body;
    if width <= 0. || height <= 0. || image.0 <= 0. || image.1 <= 0. {
        return false;
    }
    let fit = |w: f32, h: f32| (w / image.0).min(h / image.1).clamp(0., 1.);
    let beside = fit(width - column, height);
    let below = fit(width, height * (1. - VIEWER_BELOW_TEXT) - below_gap);
    beside >= below
}

const ZOOM_STEP_IN: f32 = 1.25;
const ZOOM_STEP_OUT: f32 = 0.8;
/// One of the viewer's icons: `tip` also names its key.
fn viewer_tool(
    id: &'static str,
    icon: SymbolIcon,
    label: &'static str,
    tip: &'static str,
    disabled: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    MaterialTooltip::plain(SharedString::from(format!("{id}-tip")), tip).trigger(
        IconButton::new(id, icon, label)
            .disabled(disabled)
            .on_click(on_click),
    )
}

/// How the reader reads its open note again.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Reload {
    /// The same note, after its files could not all be read.
    Reread,
    /// A task has replaced the note.
    Replaced,
}

/// What the reader asks of the window around it.
pub(crate) enum ReaderEvent {
    /// Leave the reader for this page.
    Navigate(Page),
    /// Show this task on the task page.
    OpenTask(String),
    /// Redo these parts of this task's note.
    Reprocess {
        id: String,
        components: Vec<course2md::execution::Component>,
    },
    /// Choose an AI service for this task's repair.
    RepairService(String),
    /// Say this where the window says things.
    Message(String),
    /// A record the reader writes could not be saved.
    RecordError(String),
    /// The screenshot viewer opened over the note.
    ViewerOpened,
}

impl EventEmitter<ReaderEvent> for ReaderPage {}

/// The reader: one note, its screenshots, find, contents and viewer. It reads
/// the shared records and exports, and asks the window for anything beyond the note.
pub(crate) struct ReaderPage {
    records: Entity<Records>,
    exports: Entity<NoteExports>,
    /// The note being read, or on its way.
    pub(crate) opened: OpenedNote,
    /// The window shows the reader: positions are recorded only then.
    pub(crate) visible: bool,
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
    /// A task replaced the open note, and reading the new one failed.
    replaced_unreadable: bool,
    frames: Rc<Vec<Frame>>,
    frame_by_path: Rc<HashMap<PathBuf, usize>>,
    /// The note's contents and the chapter titles its headings open.
    outline: Rc<Vec<(usize, String, Option<f64>)>>,
    issues: Vec<String>,
    viewer: Option<ImageViewer>,
    viewer_text: MarkdownExtensions,
    viewer_line: Arc<AtomicU32>,
    /// The image copy in flight: an older one finishing late is dropped.
    image_copy: crate::clipboard::Latest,
    export_folder: Option<PathBuf>,
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
    /// Capture harness: go to the end once the restore has laid the view out.
    #[cfg(feature = "capture")]
    then_end: bool,
}

impl ReaderPage {
    pub(crate) fn new(
        records: Entity<Records>,
        exports: Entity<NoteExports>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let find = cx.new(|cx| InputState::new(window, cx).placeholder(find_placeholder()));
        let gallery_find =
            cx.new(|cx| InputState::new(window, cx).placeholder(gallery_find_placeholder()));
        let subscription = cx.subscribe(&find, |this, _, event, cx| match event {
            InputEvent::Change => {
                let query = this.find.input.read(cx).value();
                this.find.open |= !query.trim().is_empty();
                this.update_reader_matches(cx);
                if this.opened.result_tab == 0 {
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
            if this.find.text.as_ref() != Some(&text) {
                this.find.text = Some(text);
                this.find.prepared = None;
                if this.find.open {
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
        let watcher = cx.weak_entity();
        let open_link: note_text::OpenLink = Arc::new(move |url, _, cx| {
            let url = url.to_owned();
            let _ = watcher.update(cx, |this, cx| this.open_watch_link(&url, cx));
        });
        let line_width = Arc::new(AtomicU32::new(0));
        let viewer_line = Arc::new(AtomicU32::new(0));
        let gallery_line = Arc::new(AtomicU32::new(0));
        let exports_changed = cx.observe(&exports, |_, _, cx| cx.notify());
        Self {
            records,
            exports,
            opened: Default::default(),
            visible: false,
            images: Default::default(),
            note: NoteState {
                view: note,
                list: note_list,
                extensions: note_text::note_extensions(line_width.clone(), render_block, open_link),
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
            replaced_unreadable: false,
            frames: Rc::default(),
            frame_by_path: Rc::default(),
            outline: Rc::default(),
            issues: Vec::new(),
            viewer: None,
            viewer_text: note_text::extensions(viewer_line.clone()),
            viewer_line,
            image_copy: crate::clipboard::Latest::default(),
            export_folder: None,
            source_loading: false,
            source_probe_cancel: None,
            source: None,
            source_available: false,
            offline_video: OfflineVideo::NotRequested,
            offline_opening: false,
            window: window.window_handle(),
            _subscriptions: vec![subscription, gallery_subscription, parsed, exports_changed],
        }
    }
}

/// The address of the note's opening: its facts, notices and details.
const HEAD_BLOCK: &str = "reader:head";
/// The address of the space after the note's last block.
const END_BLOCK: &str = "reader:end";
/// The card's inset above the title, inside the scrolled document, so text scrolls
/// on to the card's edge instead of being cut inside it.
const START_BLOCK: &str = "reader:start";
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
            (format!("![]({START_BLOCK})"), None),
            (
                format!("# {}", note_text::plain_inline_markdown(title)),
                None,
            ),
            (format!("![]({HEAD_BLOCK})"), None),
        ];
        // A section's screenshot follows its paragraphs.
        let mut figures: Vec<(String, Option<usize>)> = Vec::new();
        // The key points from `index` on, as one list, and the index after them.
        let key_points = |mut index: usize| {
            let mut list = Vec::new();
            while let Some(PreviewBlock::Paragraph { text, anchor }) = blocks.get(index)
                && anchor.starts_with("key-point-")
            {
                list.push(format!("- {}", note_text::inline_markdown(text)));
                index += 1;
            }
            (list.join("\n"), index)
        };
        let mut index = 0;
        while let Some(block) = blocks.get(index) {
            if let PreviewBlock::Heading { .. } = block {
                parts.append(&mut figures);
                if let Some(chapter) = chapters.get(&index) {
                    let chapter = note_text::plain_inline_markdown(chapter);
                    parts.push((format!("## {chapter}"), Some(index)));
                }
            }
            match block {
                // The summary is one panel: its overview and its key points.
                PreviewBlock::Heading { anchor, .. } if anchor == "summary" => {
                    let start = index;
                    let mut summary = Vec::new();
                    index += 1;
                    while let Some(PreviewBlock::Paragraph { text, anchor }) = blocks.get(index)
                        && (anchor == "summary-tldr" || anchor.starts_with("key-point-"))
                    {
                        if anchor.starts_with("key-point-") {
                            let (list, next) = key_points(index);
                            summary.push(list);
                            index = next;
                        } else {
                            summary.extend(note_text::blocks(text));
                            index += 1;
                        }
                    }
                    if !summary.is_empty() {
                        parts.push((
                            note_text::surface(note_text::SUMMARY_SURFACE, &summary.join("\n\n")),
                            Some(start),
                        ));
                    }
                    continue;
                }
                PreviewBlock::Heading { text, seconds, .. } => {
                    let level = if block.is_timestamp_heading() {
                        "###"
                    } else {
                        "##"
                    };
                    let heading = note_text::plain_inline_markdown(text);
                    // A section's time and the control that plays from it are one pill.
                    let heading = match seconds.and_then(&seek) {
                        Some(url) if block.is_timestamp_heading() => {
                            note_text::source_link(&heading, &url)
                        }
                        Some(url) => format!("{heading} {}", note_text::source_link("", &url)),
                        None => heading,
                    };
                    parts.push((format!("{level} {heading}"), Some(index)));
                }
                // The key points are one list.
                PreviewBlock::Paragraph { anchor, .. } if anchor.starts_with("key-point-") => {
                    let (list, next) = key_points(index);
                    parts.push((list, Some(index)));
                    index = next;
                    continue;
                }
                PreviewBlock::Paragraph { text, .. } => parts.extend(
                    note_text::blocks(text)
                        .into_iter()
                        .map(|block| (block, Some(index))),
                ),
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
        // The card's inset below the last block, inside the scrolled document.
        parts.push((format!("![]({END_BLOCK})"), None));
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

/// The first screenshot of the gallery's top row.
fn gallery_top_frame(items: &[GalleryItem], matches: &[usize], list: &ListState) -> Option<usize> {
    let GalleryItem::Row(row) = items.get(list.logical_scroll_top().item_ix)? else {
        return None;
    };
    matches.get(row.start).copied()
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
pub(crate) const TITLE_BAR_HEIGHT: Rems = rem(56.);
/// The widest the search field grows.
const SEARCH_MAX_WIDTH: Rems = rem(560.);

impl ReaderMetrics {
    /// The width the page gives its views: all of it beside the contents, else the
    /// note's measure.
    fn view_width(&self, rem_size: Pixels) -> f32 {
        if self.toc_beside {
            self.content_width
        } else {
            self.content_width
                .min(f32::from(ARTICLE_MAX.to_pixels(rem_size)))
        }
    }

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

impl ReaderPage {
    fn workspace<'a>(&self, cx: &'a App) -> Option<&'a workspace::Workspace> {
        self.records.read(cx).workspace.as_ref()
    }

    fn library<'a>(&self, cx: &'a App) -> &'a desktop_store::library::LibraryState {
        &self.records.read(cx).library
    }

    fn with_workspace<R>(
        &self,
        cx: &mut App,
        change: impl FnOnce(&mut workspace::Workspace) -> R,
    ) -> Option<R> {
        self.records
            .change(cx, |records, _| records.workspace.as_mut().map(change))
    }

    fn course_location<'a>(
        &self,
        course: &Course,
        cx: &'a App,
    ) -> Option<&'a workspace::LibraryLocation> {
        self.records.read(cx).course_location(course)
    }

    fn reader_exporting(&self, cx: &App) -> bool {
        self.exports.read(cx).exporting()
    }

    pub(super) fn reader_viewer_open(&self) -> bool {
        self.viewer.as_ref().is_some_and(|viewer| viewer.open)
    }

    /// While the viewer is open, how far above the window's bottom a snackbar
    /// must stay to leave the viewer's action row uncovered.
    pub(super) fn viewer_snackbar_clearance(&self) -> Option<Pixels> {
        self.viewer
            .as_ref()
            .filter(|viewer| viewer.open)
            .and_then(|viewer| viewer.actions_clearance.get())
    }

    /// The list of the current view.
    fn reader_list(&self) -> &ListState {
        if self.opened.result_tab == 0 {
            &self.note.list
        } else {
            &self.gallery.list
        }
    }

    fn scroll_reader_page(&mut self, direction: f32, cx: &mut Context<Self>) {
        let list = self.reader_list();
        list.scroll_by(-list.viewport_bounds().size.height * 0.85 * direction);
        self.reading_position_moved(cx);
        cx.notify();
    }
    fn reading_key(&self, cx: &App) -> Option<String> {
        let preview = self.opened.preview.as_ref()?;
        Some(match &preview.course.manifest {
            Some(manifest) => manifest_reading_key(&manifest.course_id, self.opened.result_tab),
            None => {
                let identity = self
                    .course_location(&preview.course, cx)
                    .and_then(|library| {
                        preview
                            .course
                            .dir
                            .strip_prefix(&library.root)
                            .ok()
                            .map(|relative| format!("{}:{}", library.id, relative.display()))
                    })
                    .unwrap_or_else(|| preview.course.dir.display().to_string());
                format!("{identity}:{}", self.opened.result_tab)
            }
        })
    }
    /// The current view's top block and how far into it the view starts.
    fn capture_reading_position(&self) -> Option<workspace::ReadingPosition> {
        let preview = self.opened.preview.as_ref()?;
        let top = self.reader_list().logical_scroll_top();
        let (paragraph, seconds) = if self.opened.result_tab == 0 {
            let index = self.note.document.block_at(top.item_ix)?;
            // Above the first block the note draws, the note reads from its top.
            if self.note.document.blocks.get(top.item_ix)?.is_none() {
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
            let gallery = &self.gallery;
            let Some(frame) = gallery_top_frame(&gallery.items, &gallery.matches, &gallery.list)
            else {
                return Some(Default::default());
            };
            let frame = self.frames.get(frame)?;
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
        self.restore.save = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(READING_POSITION_SETTLE)
                .await;
            let _ = this.update(cx, |this, cx| this.save_reading_position(cx));
        }));
    }
    pub fn save_reading_position(&mut self, cx: &mut Context<Self>) {
        if !self.visible
            || self.opened.reading
            || (self.opened.result_tab == 1 && self.gallery.search_position.is_some())
            || self.restore.active
            || self.restore.pending.is_some()
        {
            return;
        }
        let (Some(key), Some(position)) = (self.reading_key(cx), self.capture_reading_position())
        else {
            return;
        };
        if self.restore.last_position.as_ref() == Some(&(key.clone(), position.clone())) {
            return;
        }
        if let Some(Err(error)) = self.with_workspace(cx, |workspace| {
            workspace.transaction(|state| {
                state.positions.insert(key.clone(), position.clone());
                Ok(())
            })
        }) {
            cx.emit(ReaderEvent::RecordError(course2md::tr!(
                "阅读位置尚未保存：{}",
                format!("{error:#}")
            )));
            cx.notify();
            return;
        }
        self.restore.last_position = Some((key, position));
    }
    pub fn restore_reading_position(&mut self, cx: &mut Context<Self>) {
        self.ensure_reader_data(cx);
        let saved = self
            .reading_key(cx)
            .and_then(|key| self.workspace(cx)?.state.positions.get(&key))
            .cloned()
            .unwrap_or_default();
        self.return_reader_to(saved, cx);
        self.restore.last_position = None;
    }
    /// Asks the current view for `position` once it can show it.
    fn return_reader_to(&mut self, position: workspace::ReadingPosition, cx: &mut Context<Self>) {
        self.restore.pending = Some(position);
        self.restore.generation += 1;
        self.restore.active = false;
        cx.notify();
    }
    /// The screenshot `position` names: by anchor, or the nearest by time.
    fn frame_of_position(&self, position: &workspace::ReadingPosition) -> Option<usize> {
        let frames = &self.frames;
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
        let note = &self.note;
        note.list.item_count() == note.document.blocks.len()
            && note.view.read(cx).rendered_text().source() == note.document.markdown
    }
    fn schedule_reader_restore(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.restore.pending.is_none()
            || (self.opened.result_tab == 1 && self.data_loading)
            || (self.opened.result_tab == 0 && !self.note_is_current(cx))
        {
            return;
        }
        let Some(position) = self.restore.pending.take() else {
            return;
        };
        self.restore.active = true;
        let generation = self.restore.generation;
        let key = self.reading_key(cx);
        // After this frame has laid out what it just received.
        cx.on_next_frame(window, move |this, _, cx| {
            if this.restore.generation != generation || this.reading_key(cx) != key {
                return;
            }
            let target = this.reader_list_target(&position);
            this.reader_list().scroll_to(target);
            #[cfg(feature = "capture")]
            if std::mem::take(&mut this.restore.then_end) {
                this.reader_list().scroll_to_end();
            }
            this.restore.active = false;
            this.restore.last_position = None;
            cx.notify();
        });
    }
    /// Where the current view's list shows `position`.
    fn reader_list_target(&self, position: &workspace::ReadingPosition) -> ListOffset {
        let item = if self.opened.result_tab == 0 {
            self.opened.preview.as_ref().and_then(|preview| {
                let block = note_position_index(&preview.blocks, position)?;
                self.note.document.item_of(block)
            })
        } else {
            let gallery = &self.gallery;
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
        let Some(preview) = self.opened.preview.clone() else {
            return;
        };
        if self.loaded.as_ref() == Some(&preview.course.dir) {
            return;
        }
        self.loaded = Some(preview.course.dir.clone());
        self.frames = Rc::default();
        self.release_reader_images(cx);
        self.toc_revealed = None;
        self.toc_scroll.set_offset(point(px(0.), px(0.)));
        self.frame_by_path = Rc::default();
        self.gallery.matches = Rc::default();
        self.gallery.excerpts = Rc::default();
        self.set_gallery_items(self.gallery.columns);
        self.issues.clear();
        self.source = None;
        // A different note: stop the relocation probe in flight.
        if let Some(cancel) = self.source_probe_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        self.source_loading = false;
        self.source_available = false;
        self.export_folder = None;
        self.offline_video = OfflineVideo::NotRequested;
        self.offline_opening = false;
        self.gallery.search_position = None;
        self.find.open = false;
        self.find.matches = Rc::default();
        self.details_open = false;
        self.processing_details_open = false;
        self.rebuild_note_document(&preview, cx);
        // Clearing an input needs the window, which this entity is being
        // updated inside; clear both searches once the update ends.
        let inputs = [self.find.input.clone(), self.gallery.find.clone()];
        let window = self.window;
        cx.defer(move |cx| {
            let _ = window.update(cx, |_, window, cx| {
                for input in &inputs {
                    input.update(cx, |input, cx| input.set_value("", window, cx));
                }
            });
        });
        self.refresh_reader_data(cx);
    }

    /// The note's name as the library shows it, after a rename: the bar and the
    /// document's heading both read it. The published files keep their title.
    pub(crate) fn retitle(&mut self, title: String, cx: &mut Context<Self>) {
        let Some(preview) = self.opened.preview.as_mut() else {
            return;
        };
        if preview.course.title == title {
            return;
        }
        Rc::make_mut(preview).course.title = title;
        let preview = preview.clone();
        // A note whose data has not loaded yet builds its document with the name then.
        if self.loaded.as_ref() == Some(&preview.course.dir) {
            self.rebuild_note_document(&preview, cx);
        }
    }

    /// The note's document and contents follow the loaded note.
    fn rebuild_note_document(&mut self, preview: &notes::Preview, cx: &mut Context<Self>) {
        let source = self.unmapped_reader_source(cx);
        let chapters = chapter_starts(&preview.blocks, &summary_outline(preview));
        let document = NoteDocument::new(&preview.course.title, &preview.blocks, &chapters, |at| {
            nav::watch_url(source.as_ref()?, at)
        });
        self.outline = Rc::new(reader_outline(preview));
        self.note.view.update(cx, |view, cx| {
            view.clear_range_highlights(cx);
            view.set_text(&document.markdown, cx);
        });
        self.note.document = document;
    }

    /// Search the screenshots once per query change: the matching frames and their
    /// excerpts.
    fn update_gallery_matches(&mut self, cx: &App) {
        let query = self.gallery.find.read(cx).value();
        let matches = matching_frames(&self.frames, &query);
        self.gallery.excerpts = Rc::new(
            matches
                .iter()
                .map(|index| {
                    let text = frame_excerpt(&self.frames[*index], &query);
                    let marks = nav::text_matches(&text, &query);
                    (
                        *index,
                        note_text::marked_markdown(&text, &marks, color(FIND_HIGHLIGHT)),
                    )
                })
                .collect(),
        );
        self.gallery.matches = Rc::new(matches);
        self.set_gallery_items(self.gallery.columns);
    }

    /// A search keeps the position it left, to return to when it is cleared.
    fn gallery_search_changed(&mut self, cx: &mut Context<Self>) {
        self.update_gallery_matches(cx);
        if self.opened.result_tab != 1 {
            return;
        }
        let searching = !self.gallery.find.read(cx).value().trim().is_empty();
        if searching {
            if self.gallery.search_position.is_none() {
                self.save_reading_position(cx);
                self.gallery.search_position = self.capture_reading_position();
            }
            self.gallery.list.scroll_to(ListOffset::default());
        } else if let Some(position) = self.gallery.search_position.take() {
            self.return_reader_to(position, cx);
        }
        cx.notify();
    }

    /// Cut the matching screenshots into rows of `columns`, keeping the top screenshot
    /// in view.
    fn set_gallery_items(&mut self, columns: usize) {
        let gallery = &mut self.gallery;
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
            .opened
            .preview
            .as_ref()
            .map(|preview| preview.as_ref().clone())
        else {
            return;
        };
        let path = preview.course.dir.clone();
        let source = self.unmapped_reader_source(cx);
        let offline_request = self.reader_offline_video_request(cx);
        let locations = self
            .workspace(cx)
            .map(|workspace| {
                workspace
                    .state
                    .libraries
                    .iter()
                    .map(|library| (library.root.clone(), library.previous_roots.clone()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        self.generation += 1;
        let generation = self.generation;
        self.data_loading = true;
        self.offline_opening = false;
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
                .await
                .unwrap_or_default();
            let _ = this.update(cx, |this, cx| {
                if this.generation != generation
                    || this
                        .opened
                        .preview
                        .as_ref()
                        .is_none_or(|preview| preview.course.dir != path)
                {
                    return;
                }
                let top = (this.opened.result_tab == 1
                    && this.restore.pending.is_none()
                    && !this.restore.active)
                    .then(|| this.capture_reading_position())
                    .flatten();
                this.frames = Rc::new(data.frames);
                this.frame_by_path = Rc::new(data.frame_by_path);
                this.update_gallery_matches(cx);
                this.issues = data.issues;
                this.export_folder = data.export_folder;
                this.source = source;
                this.source_available = source_available;
                this.offline_video = offline_video;
                this.data_loading = false;
                if let Some(top) = top {
                    this.return_reader_to(top, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }
    fn reader_source_key(&self) -> Option<String> {
        let preview = self.opened.preview.as_ref()?;
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
                .unwrap_or_else(|| preview.course.dir.display().to_string()),
        )
    }
    fn reader_offline_video_request(&self, cx: &App) -> Option<OfflineVideoRequest> {
        let course = &self.opened.preview.as_ref()?.course;
        let manifest = course.manifest.as_ref()?;
        let state = &self.workspace(cx)?.state;
        let task = state.task(&manifest.task_id)?;
        let library = state.library(&task.plan.library_id)?;
        OfflineVideoRequest::for_note(course, task, library)
    }
    fn play_reader_offline_video(&mut self, cx: &mut Context<Self>) {
        if self.offline_opening {
            return;
        }
        let Some(request) = self.reader_offline_video_request(cx) else {
            return;
        };
        let note = request.note_dir.clone();
        let generation = self.generation;
        self.offline_opening = true;
        cx.spawn(async move |this, cx| {
            let status = crate::spawn_blocking_io(move || request.inspect())
                .await
                .unwrap_or_default();
            let _ = this.update(cx, |this, cx| {
                if this.generation != generation
                    || this
                        .opened
                        .preview
                        .as_ref()
                        .is_none_or(|preview| preview.course.dir != note)
                {
                    return;
                }
                this.offline_opening = false;
                if this.visible {
                    if let OfflineVideo::Available(path) = &status {
                        cx.open_with_system(path);
                    } else {
                        cx.emit(ReaderEvent::Message(
                            course2md::tr!("保留的视频已不在").into(),
                        ));
                    }
                }
                this.offline_video = status;
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    fn unmapped_reader_source(&self, cx: &App) -> Option<nav::SourceTarget> {
        if let Some(path) = self
            .reader_source_key()
            .and_then(|key| self.workspace(cx)?.state.reader_sources.get(&key))
        {
            return Some(nav::SourceTarget::Local(path.clone()));
        }
        let meta = &self.opened.preview.as_ref()?.document.meta;
        nav::source_target(&meta.webpage_url, meta.is_local())
    }
    /// Follows a 从此处观看 address: a web page in the browser, a local video from its time.
    fn open_watch_link(&mut self, url: &str, cx: &mut Context<Self>) {
        match nav::local_seek_seconds(url) {
            Some(seconds) => self.watch_local_from(seconds, cx),
            None => cx.open_url(url),
        }
    }

    /// Opens the note's local video at `seconds`: in a player installed here that can
    /// start at a time, or else the system's player, saying where to go.
    fn watch_local_from(&mut self, seconds: f64, cx: &mut Context<Self>) {
        let Some(nav::SourceTarget::Local(path)) = self.source.clone() else {
            return;
        };
        if !path.is_file() {
            self.open_reader_source(cx);
            return;
        }
        let time = course2md::render::fmt_ts(seconds);
        let player = nav::seeking_player(
            &path,
            seconds,
            std::env::var_os("PATH").as_deref(),
            std::env::home_dir().as_deref(),
        );
        // Capture runs log what they would open, and open nothing.
        if cfg!(feature = "capture") {
            eprintln!(
                "capture: watch {} from {time} with {player:?}",
                path.display()
            );
            return;
        }
        let started = player.is_some_and(|(program, arguments)| {
            std::process::Command::new(program)
                .args(arguments)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                // The player outlives the call; a thread waits on it so its exit
                // is reaped instead of left as a zombie.
                .map(|mut child| {
                    std::thread::spawn(move || {
                        let _ = child.wait();
                    });
                })
                .is_ok()
        });
        if !started {
            cx.open_with_system(&path);
            cx.emit(ReaderEvent::Message(course2md::tr!(
                "已用默认播放器打开原视频，请跳到 {}",
                time
            )));
            cx.notify();
        }
    }

    fn open_reader_source(&mut self, cx: &mut Context<Self>) {
        match self.source.clone() {
            Some(nav::SourceTarget::Web(url)) => cx.open_url(&url),
            Some(nav::SourceTarget::Local(path)) if path.is_file() => cx.open_with_system(&path),
            Some(nav::SourceTarget::Local(path)) => {
                cx.emit(ReaderEvent::Message(course2md::tr!(
                    "原视频不在 {}，请用「找到原视频…」指定新位置",
                    path.display()
                )));
                cx.notify();
            }
            None => {}
        }
    }
    fn relocate_reader_source(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.source_loading {
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
                this.source_probe_cancel = Some(cancel);
                this.source_loading = true;
                cx.notify();
            });
            let input = path.to_string_lossy().into_owned();
            let identity = key.clone();
            let checked = crate::spawn_blocking_io(move || {
                let source::SourceProbe::Single(source) =
                    source::probe(input, false, false, probing)?
                else {
                    anyhow::bail!(course2md::tr!("请选择可读取的视频文件"));
                };
                anyhow::ensure!(
                    !crate::source::is_local_identity(&identity) || identity == source.identity,
                    course2md::tr!("这个文件与生成笔记时的视频内容不同，请选择同一个原视频")
                );
                Ok::<_, anyhow::Error>(())
            });
            let checked = checked.await.unwrap_or_else(|| {
                Err(anyhow::anyhow!(course2md::tr!(
                    "检查视频时意外中断，请重试"
                )))
            });
            let _ = this.update(cx, |this, cx| {
                this.source_loading = false;
                this.source_probe_cancel = None;
                let result = checked.and_then(|_| {
                    this.with_workspace(cx, |workspace| {
                        workspace.transaction(|state| {
                            state.reader_sources.insert(key, path.clone());
                            Ok(())
                        })
                    })
                    .ok_or_else(|| {
                        anyhow::anyhow!(course2md::tr!("暂时无法保存原视频位置，请重试"))
                    })
                    .and_then(|saved| saved)
                });
                let message = match result {
                    Ok(()) => {
                        this.refresh_reader_data(cx);
                        course2md::tr!("已定位原视频：{}", path.display())
                    }
                    Err(error) => course2md::tr!("原视频位置没有更改：{}", error.to_string()),
                };
                cx.emit(ReaderEvent::Message(message));
                cx.notify();
            });
        })
        .detach();
    }
    /// The search input of the current view.
    fn reader_find_input(&self) -> Entity<InputState> {
        if self.opened.result_tab == 0 {
            self.find.input.clone()
        } else {
            self.gallery.find.clone()
        }
    }
    pub fn open_reader_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.opened.preview.is_none() {
            return;
        }
        if !self.find.open {
            self.find.return_focus = window.focused(cx);
        }
        self.find.open |= self.opened.result_tab == 0;
        self.reader_find_input()
            .update(cx, |input, cx| input.focus(window, cx));
        if self.opened.result_tab == 0 {
            self.update_reader_matches(cx);
        }
        cx.notify();
    }
    fn close_reader_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.find.open = false;
        self.find.matches = Rc::default();
        self.note
            .view
            .update(cx, |view, cx| view.clear_range_highlights(cx));
        self.reader_find_input()
            .update(cx, |input, cx| input.set_value("", window, cx));
        // Back to the note, where the reading goes on. What held focus before find
        // opened may be a menu's trigger that a pointer last touched.
        self.find.return_focus = None;
        self.focus.focus(window, cx);
        cx.notify();
    }
    fn update_reader_matches(&mut self, cx: &mut Context<Self>) {
        let query = self.find.input.read(cx).value().to_string();
        let find = &mut self.find;
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
        self.find.painted = Some(color(FIND_CURRENT));
        let find = &self.find;
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
        self.note.view.update(cx, |view, cx| {
            // Ranges of older text wait for the new parse, which searches again.
            let _ = view.set_range_highlights(highlights, cx);
        });
    }
    fn move_reader_match(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.opened.result_tab != 0 {
            return;
        }
        let count = self.find.matches.len();
        if count == 0 {
            return;
        }
        let find = &mut self.find;
        find.current = (find.current as isize + delta).rem_euclid(count as isize) as usize;
        let found = find.matches[find.current].clone();
        self.paint_reader_matches(cx);
        self.note.view.update(cx, |view, cx| {
            let _ = view.reveal_range(found, cx);
        });
        self.reading_position_moved(cx);
        cx.notify();
    }
    /// Shows note block `index` at the top of the note tab.
    fn jump_reader_block(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.opened.result_tab != 0 {
            self.save_reading_position(cx);
            self.opened.result_tab = 0;
        }
        let Some(preview) = &self.opened.preview else {
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
    /// Reads the open note `course` again in place. `Reread` keeps the exact position;
    /// `Replaced` finds the same moment of the video in a note a task has just
    /// replaced, whose blocks may differ.
    pub(crate) fn reload_note(&mut self, course: Course, reload: Reload, cx: &mut Context<Self>) {
        // A read of another note goes on; one of this note, which may have read it
        // before the task replaced it, gives way to this one.
        let other_read = self.opened.reading
            && self
                .opened
                .opening_course
                .as_ref()
                .is_some_and(|opening| *opening != course.dir);
        if other_read
            || self
                .opened
                .preview
                .as_ref()
                .is_none_or(|preview| preview.course.dir != course.dir)
        {
            return;
        }
        self.save_reading_position(cx);
        let position = self
            .capture_reading_position()
            .map(|position| match reload {
                Reload::Reread => position,
                // A replaced note's anchors name other blocks: the time is what carries
                // over, and without one the note opens at its top.
                Reload::Replaced => workspace::ReadingPosition {
                    seconds: position.seconds,
                    ..Default::default()
                },
            });
        self.replaced_unreadable = false;
        self.opened.opening_course = None;
        let tab = self.opened.result_tab;
        self.opened.read_generation += 1;
        let generation = self.opened.read_generation;
        self.opened.reading = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = crate::spawn_blocking_io(move || notes::read_preview(course))
                .await
                .unwrap_or_else(|| Err(anyhow::anyhow!(course2md::tr!("打开时意外中断，请重试"))));
            let _ = this.update(cx, |this, cx| {
                if this.opened.read_generation != generation {
                    return;
                }
                this.opened.reading = false;
                match result {
                    Ok(mut preview) => {
                        preview.course.title =
                            this.library(cx).course_display_title(&preview.course);
                        this.opened.preview = Some(Rc::new(preview));
                        this.opened.result_tab = tab;
                        this.loaded = None;
                        if reload == Reload::Replaced && this.visible {
                            cx.emit(ReaderEvent::Message(course2md::tr!("笔记已更新").into()));
                        }
                        // The event path loads the data; rendering no longer does.
                        this.ensure_reader_data(cx);
                        this.restore_reading_position(cx);
                        if position.is_some() {
                            this.restore.pending = position;
                        }
                    }
                    // The note on screen is gone from disk: the reader says so until
                    // it is read again.
                    Err(_) if reload == Reload::Replaced => this.replaced_unreadable = true,
                    Err(error) => cx.emit(ReaderEvent::Message(course2md::tr!(
                        "笔记暂时无法读取：{}",
                        error.to_string()
                    ))),
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
            // Once the view's own restore has its content laid out, not after a guess
            // at how long that takes: the restore runs on the frame after the note or
            // the screenshots reach their list.
            self.restore.then_end = true;
            if self.restore.pending.is_none() && !self.restore.active {
                self.restore_reading_position(cx);
            }
        }
        if let Some(index) = shot {
            self.open_reader_image(index, window, cx);
        }
        cx.notify();
    }

    /// Debug validation: show these reader panels (`,`-separated) on the open note.
    pub(crate) fn show_panels(&mut self, panels: &str) {
        for panel in panels.split(',').map(str::trim) {
            match panel {
                "find" => self.find.open = true,
                "info" => self.details_open = true,
                "toc" => self.toc_open = true,
                "images" => self.opened.result_tab = 1,
                _ => {}
            }
        }
    }

    /// The note at `dir`: the library's shared copy, or the open note
    /// when the library has not listed it yet. Copies only when it must.
    pub(crate) fn note_at(&self, dir: &std::path::Path, cx: &App) -> Option<Rc<Course>> {
        self.records.read(cx).note_at(dir).or_else(|| {
            self.opened
                .preview
                .as_ref()
                .filter(|preview| preview.course.dir == dir)
                .map(|preview| Rc::new(preview.course.clone()))
        })
    }

    fn select_reader_view(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.show_reader_view(index, cx);
        self.focus.focus(window, cx);
    }

    /// Switches the view and leaves focus where it is: the view switch keeps it, so its
    /// arrow keys go on choosing.
    fn show_reader_view(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.opened.result_tab != index {
            self.save_reading_position(cx);
            self.opened.result_tab = index;
            let searching = !self.gallery.find.read(cx).value().trim().is_empty();
            if index == 1 && searching {
                self.restore.pending = None;
                self.gallery.list.scroll_to(ListOffset::default());
            } else {
                self.restore_reading_position(cx);
            }
        }
        cx.notify();
    }

    /// The page; its reading tools are in the title bar (see `title_bar`).
    fn reader_page(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(preview) = self.opened.preview.clone() else {
            return self.reader_opening(cx);
        };
        self.images.next_frame();
        self.schedule_reader_restore(window, cx);
        if !self.find.matches.is_empty() && self.find.painted != Some(color(FIND_CURRENT)) {
            self.paint_reader_matches(cx);
        }
        let metrics = ReaderMetrics::new(window, !self.outline.is_empty());
        let strip = self.reader_strip(&preview, cx);
        let attention = (!metrics.short)
            .then(|| self.reader_attention(&preview, cx))
            .flatten();
        let tabs = (!metrics.short).then(|| self.reader_tabs(&preview, metrics, cx));
        let find = self.reader_find_bar(cx);
        let view = if self.opened.result_tab == 0 {
            self.reader_article(&preview, metrics, cx)
        } else {
            self.reader_gallery(&preview, metrics, window, cx)
        };
        let viewer = self.reader_viewer(window, cx);
        motion::state_enter(
            SharedString::from(format!("reader-open:{}", preview.course.dir.display())),
            v_flex()
                .id("reader-page")
                .key_context("CourseReader")
                .track_focus(&self.focus)
                .on_action(cx.listener(|this, _: &NextMatch, _, cx| this.move_reader_match(1, cx)))
                .on_action(
                    cx.listener(|this, _: &PreviousMatch, _, cx| this.move_reader_match(-1, cx)),
                )
                .on_action(cx.listener(|this, _: &CloseFind, window, cx| {
                    let searching = !this.reader_find_input().read(cx).value().is_empty();
                    if this.toc_open {
                        this.toc_open = false;
                        cx.notify();
                    } else if this.find.open || searching {
                        this.close_reader_find(window, cx);
                    } else {
                        cx.propagate();
                    }
                }))
                .on_action(
                    cx.listener(|this, _: &ReaderPageUp, _, cx| this.scroll_reader_page(1., cx)),
                )
                .on_action(
                    cx.listener(|this, _: &ReaderPageDown, _, cx| this.scroll_reader_page(-1., cx)),
                )
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
                // A note alone keeps both its views at its own measure, so the view
                // switch stays put between them and the gallery fills what it has (V9).
                .when(!metrics.toc_beside, |page| {
                    page.max_w(ARTICLE_MAX).mx_auto()
                })
                .pb(rem(16.))
                .gap(rem(12.))
                .children(strip)
                .children(attention)
                .children(tabs)
                .children(find)
                .child(view)
                .children(viewer),
        )
        .into_any_element()
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
                .child(ui::tip_button(
                    "reader-loading-back",
                    symbols::arrow_back(),
                    label,
                    |button| {
                        button
                            .variant(IconButtonVariant::Tonal)
                            .tone(ButtonTone::Neutral)
                            .on_click(cx.listener(|this, _, _, cx| {
                                cx.emit(ReaderEvent::Navigate(this.opened.result_origin));
                            }))
                    },
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

    /// What passes over the note while reading it, kept in view: a finished export, a
    /// task replacing the note, and the note being read again.
    fn reader_strip(
        &mut self,
        preview: &Rc<notes::Preview>,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        let feedback = self
            .exports
            .update(cx, |exports, cx| exports.feedback(&preview.course, cx));
        let line = |id: &'static str, lead: AnyElement, text: &'static str| {
            h_flex()
                .flex_wrap()
                .items_center()
                .gap(rem(8.))
                .child(lead)
                .child(
                    accessible_text(SharedString::from(format!("{id}-label")), text)
                        .role(Role::Status)
                        .text_color(color(ON_SURFACE_VARIANT)),
                )
        };
        let icon = |icon: SymbolIcon, role| {
            Icon::from(icon)
                .size(rem(20.))
                .color(color(role))
                .into_any_element()
        };
        let reading = self.opened.reading.then(|| {
            let text = course2md::tr!("正在读取笔记…");
            motion::enter(
                "reader-reading",
                line(
                    "reader-reading",
                    spinner("reader-reading-spinner", text, cx).into_any_element(),
                    text,
                ),
            )
        });
        // The note was replaced and could not be read: the one on screen is gone.
        let unreadable = (!self.opened.reading && self.replaced_unreadable).then(|| {
            let course = preview.course.clone();
            motion::enter(
                "reader-replaced-unreadable",
                line(
                    "reader-replaced-unreadable",
                    icon(symbols::warning(), WARNING),
                    course2md::tr!("笔记已更新，但暂时无法读取"),
                )
                .child(
                    material_button("reader-replaced-reread", ButtonVariant::Text)
                        .icon(symbols::refresh())
                        .child(course2md::tr!("重新读取"))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let mut course = course.clone();
                            course.manifest = None;
                            this.reload_note(course, Reload::Replaced, cx)
                        })),
                ),
            )
        });
        // The task that will replace this note: the note stays readable meanwhile, and
        // is read again in place once the task has replaced it.
        let updating = (!self.opened.reading)
            .then(|| self.replacing_task(preview, cx))
            .flatten()
            .map(|(id, state)| {
                use workspace::TaskState::*;
                let (lead, text) = match state {
                    // A still icon: a spinner would draw the reader every frame for as
                    // long as the task runs.
                    Running | Pausing => (
                        icon(symbols::refresh(), ON_SURFACE_VARIANT),
                        course2md::tr!("正在更新这份笔记，完成后这里会自动刷新"),
                    ),
                    Queued => (
                        icon(symbols::schedule(), ON_SURFACE_VARIANT),
                        course2md::tr!("这份笔记等待更新，完成后这里会自动刷新"),
                    ),
                    Paused => (
                        icon(symbols::pause(), ON_SURFACE_VARIANT),
                        course2md::tr!("这份笔记的更新已暂停"),
                    ),
                    _ => (
                        icon(symbols::warning(), WARNING),
                        course2md::tr!("这份笔记的更新未完成，这里仍是原来的笔记"),
                    ),
                };
                motion::enter(
                    "reader-updating",
                    line("reader-updating", lead, text).child(
                        material_button("reader-updating-task", ButtonVariant::Text)
                            .tone(ButtonTone::Neutral)
                            .icon(symbols::task())
                            .child(course2md::tr!("查看任务"))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.save_reading_position(cx);
                                cx.emit(ReaderEvent::OpenTask(id.clone()));
                            })),
                    ),
                )
            });
        (feedback.is_some() || reading.is_some() || updating.is_some() || unreadable.is_some())
            .then(|| {
                v_flex()
                    .w_full()
                    .min_w_0()
                    .flex_shrink_0()
                    .gap(rem(8.))
                    .children(feedback)
                    .children(unreadable)
                    .children(updating)
                    .children(reading)
            })
    }

    /// The unfinished task for the open note, which will replace it when it finishes.
    fn replacing_task(
        &self,
        preview: &notes::Preview,
        cx: &App,
    ) -> Option<(String, workspace::TaskState)> {
        let manifest = preview.course.manifest.as_ref()?;
        let library = self.course_location(&preview.course, cx)?;
        let tasks = &self.workspace(cx)?.state.tasks;
        desktop_store::library::note_unfinished_task(tasks, &library.id, Some(&manifest.source_id))
            .filter(|task| !task.exports_only())
            .map(|task| (task.id.clone(), task.state))
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
                    ConnectedButtonGroup::new("reader-tabs", course2md::tr!("阅读视图"))
                        .tone(ButtonTone::Neutral)
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
                        .icon("note", symbols::description())
                        .icon("images", symbols::image())
                        .selected(if self.opened.result_tab == 0 {
                            "note"
                        } else {
                            "images"
                        })
                        .on_change(cx.listener(|this, selected: &SharedString, _, cx| {
                            this.show_reader_view(usize::from(selected.as_ref() == "images"), cx);
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
        if self.opened.result_tab != 0 || self.outline.is_empty() || metrics.toc_beside {
            return None;
        }
        let label = course2md::tr!("阅读目录");
        Some(
            ui::tip_button(
                "note-contents",
                symbols::format_list_bulleted(),
                label,
                |button| {
                    button
                        .toggle(true)
                        .selected(self.toc_open)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.toc_open = !this.toc_open;
                            cx.notify();
                        }))
                },
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
        let note = &mut self.note;
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
                .rounded(RADIUS_HERO)
                .overflow_hidden()
                .bg(color(SURFACE))
                // Copying from the note leaves its figures behind.
                .capture_action(|_: &CopyText, window, cx| {
                    let text = TextSelection::selected_text(window, cx);
                    let text = copied_text(&text);
                    if !text.is_empty() {
                        crate::clipboard::write(ClipboardItem::new_string(text), cx);
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
                        .text_size(rem(17.))
                        .line_height(relative(1.8))
                        .size_full()
                        .px(metrics.inset),
                );
        let current = {
            let top = note.list.logical_scroll_top().item_ix;
            note.document
                .block_at(top)
                .and_then(|block| outline_current(&self.outline, block))
        };
        if self.outline.is_empty() {
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
                .when(self.toc_open, |body| {
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
        if current == self.toc_revealed {
            return;
        }
        let outline = &self.outline;
        let position_of =
            |block: Option<usize>| outline.iter().position(|(index, ..)| Some(*index) == block);
        let previous = position_of(self.toc_revealed);
        if let Some(position) = position_of(current) {
            let ahead = previous.is_none_or(|previous| position > previous);
            self.toc_scroll
                .scroll_to_item(position + if ahead { 2 } else { 1 });
        }
        self.toc_revealed = current;
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
            .track_scroll(&self.toc_scroll)
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
                                    this.toc_open = false;
                                    cx.notify();
                                })),
                        )
                    }),
            )
            .gap(TILE_GAP)
            .children(self.outline.iter().map(|(index, label, seconds)| {
                let index = *index;
                let time = seconds
                    .filter(|seconds| *label != course2md::render::fmt_ts(*seconds))
                    .map(course2md::render::fmt_ts);
                // Flat pills on whatever the contents sit on (R9): the page
                // ground beside the note, the sheet's surface over it.
                ListItem::new(("toc-item", index), label.clone())
                    .density(ListDensity::Compact)
                    .container(if beside {
                        ColorRole::SURFACE_CONTAINER
                    } else {
                        ColorRole::SURFACE
                    })
                    .pill(true)
                    .selected(current == Some(index))
                    .when_some(time, |item, time| item.trailing_text(time))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.toc_open = false;
                        this.jump_reader_block(index, cx);
                    }))
            }))
    }

    /// A paragraph of the note that the reader draws itself.
    fn note_block(
        &mut self,
        address: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let preview = self.opened.preview.clone();
        if let (Some(preview), Some(block)) = (
            &preview,
            address
                .strip_prefix(FIGURE_BLOCK)
                .and_then(|block| block.parse::<usize>().ok()),
        ) {
            return self.note_figure(preview, block, cx);
        }
        match &preview {
            Some(preview) if address == HEAD_BLOCK => self
                .reader_head(preview, true, window, cx)
                .into_any_element(),
            // As far below the last block as the note's opening is below the card's top.
            _ if address == END_BLOCK => div().h(rem(24.)).into_any_element(),
            // With the paragraph gap after it, the title starts the inset down.
            _ if address == START_BLOCK => div().h(rem(8.)).into_any_element(),
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
        let frame = self.frame_by_path.get(path).copied();
        // A file that is missing or whose header cannot be read gets the same
        // figure as one that fails to decode.
        let loaded = if frame.is_none() && !self.data_loading {
            Loaded::Failed
        } else {
            let size = self.note.figure_size;
            self.images.get(path, size, cx)
        };
        let label = frame.map_or(course2md::tr!("截图").to_owned(), |frame| {
            frame_label(&self.frames[frame])
        });
        let enlarge = course2md::tr!("放大{}", label);
        // The frame's time at one corner and the action that enlarges it at the other.
        let copyable = !matches!(loaded, Loaded::Failed);
        let slot = screenshot_slot(("note-figure-slot", block), Some(loaded)).when_some(
            frame,
            |slot, frame| {
                let seconds = self.frames[frame].seconds;
                slot.child(
                    media_label(
                        ("note-figure-time", block),
                        course2md::render::fmt_ts(seconds),
                    )
                    .absolute()
                    .left(rem(12.))
                    .bottom(rem(12.)),
                )
                .child(
                    div().absolute().right(rem(12.)).bottom(rem(12.)).child(
                        MaterialTooltip::plain(
                            ("note-figure-enlarge-tip", block),
                            course2md::tr!("放大"),
                        )
                        .trigger(
                            IconButton::new(
                                ("note-figure-enlarge", block),
                                symbols::open_in_full(),
                                enlarge.clone(),
                            )
                            .variant(IconButtonVariant::Tonal)
                            .tone(ButtonTone::Neutral)
                            .on_click(cx.listener(
                                move |this, _, window, cx| {
                                    // The figure under the button opens the viewer too.
                                    cx.stop_propagation();
                                    this.open_reader_image(frame, window, cx)
                                },
                            )),
                        ),
                    ),
                )
            },
        );
        let menu = frame
            .filter(|frame| self.frames[*frame].image.is_some())
            .map(|_| {
                self.image_context_menu(("note-figure-context", block), path.clone(), copyable, cx)
            });
        let card = MaterialCard::new(("note-figure", block), CardVariant::Filled)
            .rounded(RADIUS_LARGE_INCREASED)
            .accessibility_label(enlarge)
            .w_full()
            .relative()
            .child(slot);
        let card =
            match frame {
                Some(frame) => card
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_reader_image(frame, window, cx)
                    }))
                    .into_any_element(),
                None => card.into_any_element(),
            };
        match menu {
            Some(menu) => with_context_menu(("note-figure-scope", block), menu, card)
                .w_full()
                .into_any_element(),
            None => card,
        }
    }

    /// What opens the note and heads the screenshots: its facts, obstacles to
    /// watching it, its details, and what needs attention.
    fn reader_head(
        &mut self,
        preview: &Rc<notes::Preview>,
        facts_shown: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let meta = &preview.document.meta;
        let platform = if meta.is_local() {
            Some(course2md::tr!("本地视频"))
        } else {
            source::video_site_name(&meta.webpage_url)
        };
        // Where it comes from and who made it are one fact, marked by the platform's icon.
        let origin: Vec<&str> = platform
            .into_iter()
            .chain(Some(meta.uploader.trim()).filter(|uploader| !uploader.is_empty()))
            .collect();
        let origin = (!origin.is_empty()).then(|| {
            status_chip(StatusKind::Neutral)
                .child(
                    Icon::from(if meta.is_local() {
                        symbols::movie()
                    } else {
                        symbols::tv()
                    })
                    .size(rem(16.)),
                )
                .child(origin.join(" · "))
        });
        let mut facts: Vec<String> = Vec::new();
        if meta.duration > 0. {
            facts.push(course2md::render::fmt_ts(meta.duration));
        }
        // How many screenshots it has is the 截图 view's own count.
        v_flex()
            .w_full()
            .min_w_0()
            .gap(rem(12.))
            .when(facts_shown, |head| {
                head.child(
                    h_flex()
                        .w_full()
                        .min_w_0()
                        .flex_wrap()
                        .items_center()
                        .gap_x(rem(12.))
                        .gap_y(rem(8.))
                        .children(origin)
                        .children(
                            facts
                                .into_iter()
                                .map(|fact| status_chip(StatusKind::Neutral).child(fact)),
                        ),
                )
            })
            .children(self.reader_obstacles(cx))
            .children(self.reader_details(preview, cx))
            .children(self.reader_notices(preview, cx))
            // Where the views have no line of their own, the page has none to spare either.
            .when(
                ReaderMetrics::new(window, !self.outline.is_empty()).short,
                |head| head.children(self.reader_attention(preview, cx)),
            )
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
        let offline = matches!(self.offline_video, OfflineVideo::Available(_));
        if self.reader_source_missing() && !offline {
            obstacles.push(obstacle(
                "missing-original-video",
                course2md::tr!("原视频不可用"),
                self.relocate_source_button("relocate-reader-source", cx),
            ));
        }
        if self.offline_video == OfflineVideo::Missing {
            let opening = self.offline_opening;
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
        let loading = self.source_loading;
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

    /// The note's details in place: when it was made, source video, subtitle and model facts.
    fn reader_details(
        &mut self,
        preview: &Rc<notes::Preview>,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.details_open {
            return None;
        }
        let mut details: Vec<(&str, SymbolIcon, String)> = Vec::new();
        let local_source = match &self.source {
            Some(nav::SourceTarget::Local(path)) => Some(path.clone()),
            _ => None,
        };
        if let Some(manifest) = &preview.course.manifest {
            details.push((
                course2md::tr!("生成日期"),
                symbols::schedule(),
                // The app's one date format.
                i64::try_from(manifest.created_at_ms)
                    .ok()
                    .and_then(chrono::DateTime::from_timestamp_millis)
                    .map(|instant| {
                        crate::i18n::calendar_date(
                            instant.with_timezone(&chrono::Local).date_naive(),
                        )
                    })
                    .unwrap_or_else(|| nav::date_local(manifest.created_at_ms)),
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
            .and_then(|manifest| self.workspace(cx)?.state.task(&manifest.task_id));
        if let Some(subtitle) = task.and_then(|task| task.plan.source.selected_subtitle.as_ref()) {
            details.push((
                course2md::tr!("所选字幕"),
                symbols::subtitles(),
                subtitle.label.clone(),
            ));
        }
        // What the note was made with: the settings its task's run used.
        if let Some(config) = task
            .and_then(|task| task.ran_with.as_ref())
            .map(|ran_with| &ran_with.config)
        {
            let defaults = &config.defaults;
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
            if config.llm.uses_service() {
                details.push((
                    course2md::tr!("AI 模型"),
                    symbols::auto_fix_high(),
                    config.llm.model.clone(),
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
            .filter(|_| self.source_available)
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
                                this.details_open = false;
                                cx.notify();
                            })),
                    ),
                ),
        ))
    }

    /// What is unfinished about this note: it concerns the note and its screenshots
    /// alike, so it heads the page above the views.
    fn reader_attention(
        &mut self,
        preview: &Rc<notes::Preview>,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let notice = processing_notice(&preview.processing_issues)?;
        Some(self.reader_processing_notice(preview, notice, cx))
    }

    /// The file-reload notice.
    fn reader_notices(
        &mut self,
        preview: &Rc<notes::Preview>,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let mut notices = Vec::new();
        if files_need_reload(preview, &self.issues, &self.frames) {
            let course = preview.course.clone();
            let issues: Vec<&str> = preview
                .issues
                .iter()
                .chain(self.issues.iter())
                .map(String::as_str)
                .collect();
            let text = if issues.is_empty() {
                course2md::tr!("部分截图无法读取").to_owned()
            } else {
                issues.join("\n")
            };
            let reading = self.opened.reading;
            notices.push(motion::enter(
                "reader-file-notice",
                notice_banner("reader-file-notice-text", NoticeKind::Danger, text).child(
                    material_button("reload-reader-files", ButtonVariant::Outlined)
                        .tone(ButtonTone::Accent)
                        .icon(symbols::refresh())
                        .child(course2md::tr!("重新读取"))
                        .loading(reading, course2md::tr!("重新读取"))
                        .disabled(reading)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.reload_note(course.clone(), Reload::Reread, cx)
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
            .and_then(|manifest| self.workspace(cx)?.state.note_task(&manifest.task_id));
        let requires_repair =
            task.is_some_and(desktop_tasks::task_view::task_requires_service_repair);
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
                notes::ProcessingStage::Translation if !requires_repair => {
                    Some("translation".to_owned())
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
                let label =
                    desktop_tasks::task_view::task_retry_components_label(task, &components);
                actions = actions.child(
                    material_button("reader-retry-incomplete", ButtonVariant::Outlined)
                        .tone(ButtonTone::Accent)
                        .icon(symbols::refresh())
                        .child(label)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.save_reading_position(cx);
                            cx.emit(ReaderEvent::Reprocess {
                                id: retry.clone(),
                                components: components
                                    .iter()
                                    .filter_map(|component| {
                                        course2md::execution::Component::parse(component)
                                    })
                                    .collect(),
                            });
                        })),
                );
            }
            if can_retry && needs_service {
                let repair = id.clone();
                actions = actions.child(
                    material_button("reader-repair-ai", ButtonVariant::Outlined)
                        .tone(ButtonTone::Accent)
                        .icon(symbols::settings())
                        .child(course2md::tr!("检查 AI 服务后重试"))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.save_reading_position(cx);
                            cx.emit(ReaderEvent::RepairService(repair.clone()));
                        })),
                );
            }
            actions = actions.child(
                material_button("reader-view-incomplete-task", ButtonVariant::Text)
                    .tone(ButtonTone::Neutral)
                    .icon(symbols::task())
                    .child(course2md::tr!("查看任务"))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.save_reading_position(cx);
                        cx.emit(ReaderEvent::OpenTask(id.clone()));
                        cx.notify();
                    })),
            );
        }
        let open = self.processing_details_open;
        let details = preview
            .processing_issues
            .iter()
            .enumerate()
            .filter_map(|(index, issue)| {
                let detail = issue.detail()?;
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
                    .tone(ButtonTone::Neutral)
                    .icon(symbols::info())
                    .child(if open {
                        course2md::tr!("收起处理详情")
                    } else {
                        course2md::tr!("查看处理详情")
                    })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.processing_details_open = !this.processing_details_open;
                        cx.notify();
                    })),
            );
        }
        motion::enter(
            "reader-processing-notice",
            notice_banner("reader-incomplete-processing", NoticeKind::Warning, notice)
                .flex_shrink_0()
                .rounded(RADIUS_HERO)
                .px(rem(20.))
                .py(rem(16.))
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
        let reading_note = self.opened.result_tab == 0;
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
                (symbols::description(), course2md::tr!("笔记"))
            };
            ui::tip_button("reader-view-toggle", icon, label, |button| {
                button
                    .variant(IconButtonVariant::Tonal)
                    .tone(ButtonTone::Neutral)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.select_reader_view(usize::from(reading_note), window, cx)
                    }))
            })
        });
        let toc_toggle = if metrics.short {
            self.reader_toc_toggle(metrics, cx)
        } else {
            None
        };
        // Search stays on the bar in both views: the note's find, or the screenshots'
        // own search field.
        let find_label = if reading_note {
            find_label
        } else {
            course2md::tr!("搜索截图对应文字")
        };
        let find = Some(()).map(|_| {
            MaterialTooltip::plain("reader-open-find-tip", format!("{find_label} {shortcut}"))
                .trigger(
                    // A tool on the reader's bar sits in the neutral circle the back button has.
                    IconButton::new("reader-open-find", symbols::search(), find_label)
                        .variant(IconButtonVariant::Tonal)
                        .tone(ButtonTone::Neutral)
                        .toggle(true)
                        .selected(reading_note && self.find.open)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            if !reading_note {
                                let input = this.gallery.find.clone();
                                input.update(cx, |state, cx| state.focus(window, cx));
                            } else if this.find.open {
                                this.close_reader_find(window, cx);
                            } else {
                                this.open_reader_find(window, cx);
                            }
                        })),
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
        matches!(self.offline_video, OfflineVideo::Available(_))
            || (self.source.is_some() && !self.reader_source_missing())
    }

    /// Whether the note's local video is no longer where it was.
    fn reader_source_missing(&self) -> bool {
        matches!(self.source, Some(nav::SourceTarget::Local(_))) && !self.source_available
    }

    /// Watches the video, the kept offline copy first.
    fn play_reader_video(&mut self, cx: &mut Context<Self>) {
        if matches!(self.offline_video, OfflineVideo::Available(_)) {
            self.play_reader_offline_video(cx);
        } else {
            self.open_reader_source(cx);
        }
    }

    /// The way to watch the video.
    fn reader_play_button(&self, icon_only: bool, cx: &mut Context<Self>) -> Option<AnyElement> {
        let label = course2md::tr!("播放原视频");
        let opening = self.offline_opening;
        if !self.reader_can_play() {
            return None;
        }
        let play = cx.listener(|this: &mut Self, _: &ClickEvent, _, cx| this.play_reader_video(cx));
        Some(if icon_only {
            // Folded to its icon it is an outlined icon button, as it is outlined
            // with its label.
            ui::tip_button("reader-play", symbols::play_arrow(), label, |button| {
                button
                    .variant(IconButtonVariant::Outlined)
                    .disabled(opening)
                    .on_click(play)
            })
            .into_any_element()
        } else {
            material_button("reader-play", ButtonVariant::Outlined)
                .tone(ButtonTone::Accent)
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
        let exporting = self.reader_exporting(cx);
        let current = self.exports.read(cx).exporting_note(&preview.course.dir);
        let note = preview.course.dir.clone();
        let mut items = Vec::new();
        if let Some(folder) = self.export_folder.clone() {
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
                    .on_click(cx.listener(move |_, _, _, cx| {
                        let text = if markdown {
                            &source.markdown_text
                        } else {
                            &source.plain_text
                        };
                        crate::clipboard::write(ClipboardItem::new_string(text.clone()), cx);
                        cx.emit(ReaderEvent::Message(copied.into()));
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
            let note = note.clone();
            items.push(
                MaterialMenuItem::new(label)
                    .leading(Icon::from(icon))
                    .disabled(exporting)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if let Some(course) = this.note_at(&note, cx) {
                            this.exports.update(cx, |exports, cx| {
                                exports.export(course, format, window, cx)
                            });
                        }
                    })),
            );
        }
        let label = course2md::tr!("复制与导出");
        let trigger: AnyElement = if icon_only {
            ui::tip_button("export-note", symbols::content_copy(), label, |button| {
                button
                    .variant(IconButtonVariant::Tonal)
                    .tone(ButtonTone::Neutral)
                    .disabled(exporting)
            })
            .into_any_element()
        } else {
            material_button("export-note", ButtonVariant::Tonal)
                .icon(symbols::content_copy())
                .child(if current {
                    course2md::tr!("正在导出…")
                } else {
                    label
                })
                // It opens a menu, as its chevron says.
                .child(Icon::from(symbols::keyboard_arrow_down()).size(rem(18.)))
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
        let source = self.source.clone();
        let missing_local = self.reader_source_missing();
        // A web source opens on its site; the title bar may be playing the kept copy.
        let source_label = match &source {
            _ if missing_local => course2md::tr!("找到原视频…").to_owned(),
            Some(nav::SourceTarget::Web(url)) => match source::video_site_name(url) {
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
                    this.details_open = !this.details_open;
                    // They open at the top of the view.
                    if this.details_open {
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
        if let Some(path) = self.exports.read(cx).exported(&preview.course.dir) {
            items.push(
                MaterialMenuItem::new(course2md::tr!("显示上次导出的文件"))
                    .leading(Icon::from(symbols::folder_open()))
                    .on_click(move |_, _, cx| cx.reveal_path(&path)),
            );
        }
        let label = course2md::tr!("更多笔记操作");
        ui::menu(
            "reader-more-menu",
            ui::tip_button("reader-more", symbols::more_vert(), label, |button| button),
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
        let reading_note = self.opened.result_tab == 0;
        if (reading_note && !self.find.open) || (!reading_note && self.frames.is_empty()) {
            return None;
        }
        let input = self.reader_find_input();
        let searching = !input.read(cx).value().trim().is_empty();
        let count = if reading_note {
            self.find.matches.len()
        } else {
            self.gallery.matches.len()
        };
        // A screenshot search that finds nothing says so once, in the gallery's own
        // card with its way out; the note's search has no such card.
        let status = (searching && (reading_note || count > 0)).then(|| {
            accessible_text(
                "find-status",
                if reading_note && count > 0 {
                    format!("{}/{count}", self.find.current + 1)
                } else if count == 0 {
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
            ui::tip_button(id, icon, label, |button| {
                button
                    .disabled(count == 0)
                    .on_click(cx.listener(move |this, _, _, cx| this.move_reader_match(delta, cx)))
            })
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
                        // The screenshots' search spans their grid; the note's find
                        // stays a bar's width.
                        .when(reading_note, |field| field.max_w(SEARCH_MAX_WIDTH))
                        .child(
                            SearchField::new(&input)
                                .field_style(SearchFieldStyle::Tonal)
                                .leading(Icon::from(symbols::search()))
                                // The note's search closes with its bar; the screenshots'
                                // stays, so it clears in place.
                                .when(!reading_note && searching, |field| {
                                    field.trailing(
                                        IconButton::new(
                                            "gallery-search-clear",
                                            symbols::close(),
                                            course2md::tr!("清除搜索"),
                                        )
                                        .on_click(
                                            cx.listener(|this, _, window, cx| {
                                                this.gallery.find.update(cx, |state, cx| {
                                                    state.set_value("", window, cx)
                                                });
                                                this.gallery_search_changed(cx);
                                            }),
                                        ),
                                    )
                                }),
                        ),
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
                    bar.child(ui::tip_button(
                        "close-find",
                        symbols::close(),
                        label,
                        |button| {
                            button.on_click(
                                cx.listener(|this, _, window, cx| {
                                    this.close_reader_find(window, cx)
                                }),
                            )
                        },
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
        let width = metrics.view_width(rem_size);
        // Fewer screenshots than columns take the row between them, each at most
        // twice the narrowest column, rather than leaving its end empty (V9).
        let shown = self.gallery.matches.len().max(1) as f32;
        let columns = ((width + gap) / (column + gap))
            .floor()
            .clamp(1., 6.)
            .min(shown) as usize;
        // The note's width, as the search above it is, so the views share their edges.
        if columns != self.gallery.columns {
            self.set_gallery_items(columns);
        }
        // Cards draw a thumbnail decoded at their own device width.
        let card_width = (width - gap * (columns as f32 - 1.)) / columns as f32;
        self.gallery
            .line_width
            .store(card_width as u32, Ordering::Relaxed);
        let thumbnail = Size::thumbnail(card_width * metrics.scale_factor);
        let items = self.gallery.items.clone();
        let matches = self.gallery.matches.clone();
        let preview = preview.clone();
        let desktop = cx.weak_entity();
        let gallery = list(self.gallery.list.clone(), move |index, window, cx| {
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
                            .w(px(width))
                            .max_w_full()
                            .min_w_0()
                            .gap(GALLERY_GAP)
                            .children(
                                matches[row.clone()]
                                    .iter()
                                    .map(|&frame| this.gallery_card(frame, thumbnail, cx)),
                            )
                            .into_any_element(),
                    };
                    // List measurements include padding, not margins. The list
                    // reaches past the column by the focus ring's room, and each
                    // item pads back into it.
                    div()
                        .w_full()
                        .min_w_0()
                        .flex_shrink_0()
                        .px(ui::FOCUS_RING_ROOM)
                        .pb(if last {
                            ui::FOCUS_RING_ROOM
                        } else {
                            GALLERY_GAP
                        })
                        .child(content)
                        .into_any_element()
                })
                .unwrap_or_else(|_| div().into_any_element())
        });
        div()
            .id("note-reader")
            .role(Role::Document)
            .relative()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .w_full()
            // Past the column on either side, so a focused card's ring is not cut;
            // its items pad back to the column.
            .child(
                gallery
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(-ui::FOCUS_RING_ROOM)
                    .right(-ui::FOCUS_RING_ROOM),
            )
            // The scrollbar sits in the gutter beside the column, clear of a notice's
            // trailing action.
            .child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .right(rem(-16.))
                    .w(rem(16.))
                    .child(Scrollbar::vertical(&self.gallery.list).viewport_from_layout()),
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
        // The screenshots repeat none of the note's facts; what needs attention stays.
        let head = self.reader_head(preview, false, window, cx);
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
        let state = if self.data_loading && self.frames.is_empty() {
            let label = course2md::tr!("正在读取截图…");
            Some(
                panel()
                    .child(spinner("reader-images-spinner", label, cx))
                    .child(accessible_text("loading-images", label).role(Role::Status)),
            )
        } else if self.frames.is_empty() {
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
                            .tone(ButtonTone::Accent)
                            .icon(symbols::description())
                            .child(course2md::tr!("阅读笔记"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.select_reader_view(0, window, cx)
                            })),
                    ),
            )
        } else if self.gallery.matches.is_empty() {
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
                                this.gallery
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
        let frames = self.frames.clone();
        let frame = &frames[index];
        let loaded = frame
            .image
            .as_ref()
            .map(|file| self.images.get(&file.path, thumbnail, cx));
        let openable = matches!(loaded, Some(Loaded::Pending | Loaded::Ready(..)));
        let copyable = !matches!(loaded, Some(Loaded::Failed));
        let excerpt = self
            .gallery
            .excerpts
            .get(&index)
            .cloned()
            .unwrap_or_default();
        let to_note = course2md::tr!("在笔记中查看");
        let menu = frame.image.as_ref().map(|image| {
            self.image_context_menu(
                ("screenshot-context", index),
                image.path.clone(),
                copyable,
                cx,
            )
        });
        let card = MaterialCard::new(
            SharedString::from(frame.anchor.clone()),
            CardVariant::Surface,
        )
        .rounded(TILE_RADIUS)
        .accessibility_label(course2md::tr!("放大{}", frame_label(frame)))
        .w_full()
        .h_full()
        .min_w_0()
        .relative()
        .child(
            div()
                .w_full()
                .p(rem(10.))
                .pb_0()
                .child(screenshot_slot(("screenshot", index), loaded).rounded(RADIUS_LARGE)),
        )
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
                            row.child(ui::tip_button(
                                ("frame-to-body", index),
                                symbols::description(),
                                to_note,
                                |button| {
                                    button.button_size(ButtonSize::ExtraSmall).on_click(
                                        cx.listener(move |this, _, _, cx| {
                                            // The card under the button opens the viewer.
                                            cx.stop_propagation();
                                            this.jump_reader_block(block, cx)
                                        }),
                                    )
                                },
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
                                .markdown_extensions(self.gallery.extensions.clone())
                                .style(note_text::note_style(cx))
                                // The excerpt is already bounded and ends in an
                                // ellipsis when cut; clamping lines would cut it
                                // again mid-sentence with none.
                                .selectable(false),
                        ),
                ),
        );
        let card = if openable {
            card.on_click(
                cx.listener(move |this, _, window, cx| this.open_reader_image(index, window, cx)),
            )
            .into_any_element()
        } else {
            card.into_any_element()
        };
        match menu {
            Some(menu) => with_context_menu(("screenshot-scope", index), menu, card)
                .size_full()
                .min_w_0()
                .into_any_element(),
            None => card,
        }
    }

    /// The viewer's screenshot commands, `id` naming one of its two places: the
    /// first child of the `relative` body, for a right press on the image or its
    /// text, and the ⋮ button's, which Shift+F10, the Menu key and the button open
    /// beside it. None for a frame without a file.
    fn reader_viewer_menu(
        &self,
        id: &'static str,
        cx: &mut Context<Self>,
    ) -> Option<MaterialContextMenu> {
        let viewer = self.viewer.as_ref()?;
        let image = viewer.frames[viewer.index].image.as_ref()?;
        // A file that exists but cannot be decoded keeps its path and location.
        let readable = !self.images.failed(&image.path, Size::Original);
        Some(self.image_context_menu(id, image.path.clone(), readable, cx))
    }

    /// What a right press on a screenshot offers: the picture itself, where a
    /// clipboard can hold one, its file's path and its file in the file manager.
    /// Shown by [`with_context_menu`], or as the first child of the `relative`
    /// element it serves.
    fn image_context_menu(
        &self,
        id: impl Into<ElementId>,
        path: PathBuf,
        copyable: bool,
        cx: &mut Context<Self>,
    ) -> MaterialContextMenu {
        command_context_menu(id, None, self.image_commands(path, copyable, cx))
    }

    /// The commands for the screenshot at `path`. A file that could not be
    /// decoded (`copyable` false) keeps its path and location but not 复制图片.
    fn image_commands(
        &self,
        path: PathBuf,
        copyable: bool,
        cx: &mut Context<Self>,
    ) -> Vec<Command> {
        let reader = cx.weak_entity();
        let say = |reader: &WeakEntity<Self>, message: &'static str, cx: &mut App| {
            let _ = reader.update(cx, |_, cx| cx.emit(ReaderEvent::Message(message.into())));
        };
        let mut commands = Vec::new();
        if crate::clipboard::IMAGES {
            let (reader, path) = (reader.clone(), path.clone());
            commands.push(
                Command::global(
                    course2md::tr!("复制图片"),
                    symbols::content_copy(),
                    move |_, cx| {
                        let _ =
                            reader.update(cx, |this, cx| this.copy_reader_image(path.clone(), cx));
                    },
                )
                .disabled(!copyable),
            );
        }
        commands.push({
            let (reader, path) = (reader.clone(), path.clone());
            Command::global(
                course2md::tr!("复制图片路径"),
                symbols::link(),
                move |_, cx| {
                    let text = path.display().to_string();
                    crate::clipboard::write(ClipboardItem::new_string(text), cx);
                    say(&reader, course2md::tr!("已复制图片路径"), cx);
                },
            )
        });
        let reveal = if cfg!(target_os = "macos") {
            course2md::tr!("在访达中显示")
        } else if cfg!(target_os = "windows") {
            course2md::tr!("在文件资源管理器中显示")
        } else {
            course2md::tr!("在文件管理器中显示")
        };
        commands.push(Command::global(
            reveal,
            symbols::folder_open(),
            move |_, cx| {
                // A file moved or deleted since the note opened has nothing to show.
                if !path.exists() {
                    say(&reader, course2md::tr!("找不到这张截图的文件"), cx);
                } else if !crate::youtube_ui::stubbed(&format!("reveal {}", path.display())) {
                    cx.reveal_path(&path);
                }
            },
        ));
        commands
    }

    /// A later copy, or leaving the note, supersedes one still encoding.
    fn copy_reader_image(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let copy = self.image_copy.begin();
        let encode = cx.background_spawn({
            let path = path.clone();
            async move { crate::clipboard::png_image(&path) }
        });
        cx.spawn(async move |this, cx| {
            let result = encode.await;
            let _ = this.update(cx, |this, cx| {
                if !this.image_copy.is_current(copy) {
                    return;
                }
                let message = match result {
                    Ok(item) => {
                        crate::clipboard::write(item, cx);
                        course2md::tr!("已复制图片").to_owned()
                    }
                    Err(error) => {
                        // The person is told it failed; the reason is for a report.
                        eprintln!("course2md: could not copy {}: {error:#}", path.display());
                        course2md::tr!("无法复制图片").to_owned()
                    }
                };
                cx.emit(ReaderEvent::Message(message));
            });
        })
        .detach();
    }

    pub(crate) fn open_reader_image(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.opened.preview.is_none()
            || self.reader_viewer_open()
            || self
                .frames
                .get(index)
                .is_none_or(|frame| frame.image.is_none())
        {
            return;
        }
        let filtered =
            self.opened.result_tab == 1 && !self.gallery.find.read(cx).value().trim().is_empty();
        let visible: Vec<usize> = if self.opened.result_tab == 1 {
            self.gallery.matches.to_vec()
        } else {
            (0..self.frames.len()).collect()
        };
        let Some(viewer_index) = visible.iter().position(|candidate| *candidate == index) else {
            return;
        };
        cx.emit(ReaderEvent::ViewerOpened);
        self.viewer = Some(ImageViewer {
            open: true,
            focused: false,
            text_scroll: ScrollHandle::new(),
            viewport_scroll: ScrollHandle::new(),
            viewport_size: size(px(0.), px(0.)),
            body_size: size(px(0.), px(0.)),
            frames: visible
                .iter()
                .map(|index| self.frames[*index].clone())
                .collect(),
            filtered,
            index: viewer_index,
            zoom: None,
            drag: None,
            text_open: None,
            focus: cx.focus_handle(),
            menu: ContextMenuHandle::default(),
            actions_clearance: Rc::default(),
        });
        let _ = window;
        cx.notify();
    }
    /// A screenshot finished decoding: repaint what draws screenshots.
    pub(crate) fn reader_image_loaded(&mut self, cx: &mut Context<Self>) {
        if self.visible {
            cx.notify();
        }
    }
    /// Leaving the reader releases its decoded screenshots.
    pub(crate) fn release_reader_images(&mut self, cx: &mut Context<Self>) {
        self.viewer = None;
        self.image_copy.begin();
        self.images.clear(cx);
    }
    pub(crate) fn close_reader_image(&mut self, cx: &mut Context<Self>) {
        if let Some(viewer) = &mut self.viewer {
            viewer.open = false;
        }
        cx.notify();
    }
    fn move_reader_image(&mut self, delta: isize, cx: &mut Context<Self>) {
        if let Some(viewer) = &mut self.viewer {
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
        let Some(viewer) = &mut self.viewer else {
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
        if let Some(viewer) = &mut self.viewer
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
    /// How far the viewer keeps its header and content from the window's top edge:
    /// the title bar's row, where the macOS traffic lights or the Windows and Linux
    /// caption buttons (drawn above every dialog) stay. The viewer's surface still
    /// fills the row behind them.
    fn viewer_safe_top(window: &Window) -> Pixels {
        TITLE_BAR_HEIGHT.to_pixels(window.rem_size())
    }
    /// The height the viewer has below the title bar's row.
    fn viewer_height(window: &Window) -> Pixels {
        window.bounds().size.height - Self::viewer_safe_top(window)
    }
    /// Whether the text goes in a column beside the image: only where the image
    /// is drawn at least as large there as above the text.
    fn viewer_text_beside(viewer: &ImageViewer, window: &Window) -> bool {
        let Some(image) = &viewer.frames[viewer.index].image else {
            return false;
        };
        let rem_size = window.rem_size();
        let to_f32 = |rems: Rems| f32::from(rems.to_pixels(rem_size));
        // Until the viewer has been laid out once, the window less the dialog's
        // header, padding and action row.
        let body = if viewer.body_size.width > px(0.) && viewer.body_size.height > px(0.) {
            viewer.body_size
        } else {
            size(
                window.bounds().size.width - rem(56.).to_pixels(rem_size),
                Self::viewer_height(window) - rem(130.).to_pixels(rem_size),
            )
        };
        text_beside(
            (f32::from(body.width), f32::from(body.height)),
            (image.width as f32, image.height as f32),
            to_f32(VIEWER_TEXT_COLUMN) + to_f32(VIEWER_BESIDE_GAP),
            to_f32(VIEWER_BELOW_GAP),
        )
    }
    fn viewer_is_short(window: &Window) -> bool {
        Self::viewer_height(window) < rem(420.).to_pixels(window.rem_size())
    }
    /// Text is shown by default when the window has room for it.
    fn viewer_text_open(viewer: &ImageViewer, window: &Window) -> bool {
        viewer
            .text_open
            .unwrap_or(Self::viewer_height(window) >= rem(448.).to_pixels(window.rem_size()))
    }
    fn zoom_reader_image(&mut self, factor: Option<f32>, window: &Window, cx: &mut Context<Self>) {
        let Some(viewer) = &mut self.viewer else {
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
        let Some(viewer) = &self.viewer else {
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
        let viewer = self.viewer.as_ref()?;
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
        let shows_text = text_only || (has_text && text_open);
        let placement = if text_only {
            TextPlacement::Only
        } else if Self::viewer_text_beside(viewer, window) {
            TextPlacement::Beside
        } else {
            TextPlacement::Below
        };
        let media = (!text_only).then(|| self.reader_viewer_media(label.clone(), cx));
        let text = shows_text.then(|| self.reader_viewer_text(placement, window));
        let menu = self.reader_viewer_menu("enlarged-image-context", cx);
        // Where the image stays as large, the text gets a column of its own beside
        // it, the viewer's whole height to read in; elsewhere it stays below.
        let owner = cx.weak_entity();
        let measure = canvas(
            move |bounds, window, cx| {
                let size = bounds.size;
                window.defer(cx, move |_, cx| {
                    let _ = owner.update(cx, |this, cx| {
                        if let Some(viewer) = &mut this.viewer
                            && viewer.body_size != size
                        {
                            viewer.body_size = size;
                            cx.notify();
                        }
                    });
                });
            },
            |_, _, _, _| {},
        )
        .absolute()
        .inset_0();
        let body = match (media, text) {
            (Some(media), Some(text)) if placement == TextPlacement::Beside => h_flex()
                .id("reader-image-body")
                .relative()
                .flex_1()
                .min_h_0()
                .w_full()
                .items_stretch()
                .gap(VIEWER_BESIDE_GAP)
                .children(menu)
                .child(measure)
                .child(v_flex().flex_1().min_w_0().h_full().child(media))
                .child(text)
                .into_any_element(),
            (media, text) => v_flex()
                .id("reader-image-body")
                .relative()
                .flex_1()
                .min_h_0()
                .w_full()
                .gap(VIEWER_BELOW_GAP)
                .children(menu)
                .child(measure)
                .children(media)
                .children(text)
                .into_any_element(),
        };
        let controls = self.reader_viewer_controls(window, cx);
        let viewer = self.viewer.as_mut()?;
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
            .child(body)
            .child(controls);
        let dismiss = cx.weak_entity();
        Some(
            MaterialDialog::new("reader-image-viewer")
                .container(ColorRole::SURFACE)
                .full_screen(course2md::tr!("关闭截图"), None)
                .safe_area(Edges {
                    top: Self::viewer_safe_top(window),
                    ..Edges::default()
                })
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
    fn reader_viewer_text(&self, placement: TextPlacement, window: &Window) -> Stateful<Div> {
        let reader = self;
        let viewer = reader.viewer.as_ref().expect("the viewer draws its text");
        let column = VIEWER_TEXT_COLUMN.to_pixels(window.rem_size());
        // Formulas wrap to the line the text actually has: inside the scrollbar's gutter.
        let gutter = VIEWER_TEXT_GUTTER.to_pixels(window.rem_size());
        reader.viewer_line.store(
            f32::from(match placement {
                TextPlacement::Beside => column - gutter,
                _ => window.bounds().size.width,
            }) as u32,
            Ordering::Relaxed,
        );
        let frame = &viewer.frames[viewer.index];
        div()
            .id("image-details")
            .relative()
            .min_w_0()
            .min_h_0()
            .map(|view| match placement {
                TextPlacement::Only => view.w_full().flex_1(),
                TextPlacement::Beside => view.flex_shrink_0().w(column).h_full(),
                TextPlacement::Below => view
                    .w_full()
                    .flex_shrink_0()
                    .max_h(relative(VIEWER_BELOW_TEXT)),
            })
            .pr(VIEWER_TEXT_GUTTER)
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
        let viewer = self.viewer.as_ref().expect("the viewer draws its controls");
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
            .is_some_and(|image| !self.images.failed(&image.path, Size::Original));
        let number = if viewer.filtered {
            course2md::tr!("匹配截图 {} / {}", index + 1, count)
        } else {
            format!("{} / {count}", index + 1)
        };
        let body_index = frame.body_index;
        let source = self.source.clone();
        let source_available = self.source_available;
        let source_link = source
            .as_ref()
            .filter(|source| source_available || matches!(source, nav::SourceTarget::Web(_)))
            .and_then(|source| nav::watch_url(source, frame.seconds));
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
                symbols::description(),
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
                .on_click(cx.listener(move |this, _, _, cx| this.open_watch_link(&url, cx))),
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
                // What was said at this picture, apart from the note's own icon (V10).
                if text_only {
                    symbols::image()
                } else {
                    symbols::subtitles()
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
                if let Some(viewer) = &mut this.viewer {
                    viewer.text_open = Some(!text_open);
                    cx.notify();
                }
            }))
        });
        // The right-click menu, opened (or closed again) where the button is: the
        // note's and the gallery's screenshots open here with Enter.
        // From the keyboard the menu opens beside this button, not beside the
        // whole body, where it would cover the title.
        let more = self
            .reader_viewer_menu("image-commands-menu", cx)
            .map(|context| {
                let menu = viewer.menu.clone();
                div()
                    .relative()
                    .flex_shrink_0()
                    .child(context.trigger(&viewer.focus).handle(&viewer.menu))
                    .child(ui::tip_button(
                        "image-commands",
                        symbols::more_vert(),
                        course2md::tr!("截图操作"),
                        |button| {
                            button.on_click(move |event: &ClickEvent, _, cx| {
                                menu.toggle_at(event, cx);
                            })
                        },
                    ))
            });
        let clearance = viewer.actions_clearance.clone();
        let measure = canvas(
            move |bounds, window, cx| {
                let measured = Some(window.viewport_size().height - bounds.top());
                // The window's snackbar reads it on the next frame; draw one when it moves.
                if clearance.replace(measured) != measured {
                    window.defer(cx, |window, _| window.refresh());
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .inset_0();
        h_flex()
            .id("reader-image-actions")
            .relative()
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
            .children(more)
            .child(measure)
    }

    /// The zoomable image, or a notice when its file cannot be read.
    fn reader_viewer_media(&mut self, label: String, cx: &mut Context<Self>) -> AnyElement {
        let unreadable = || {
            unreadable_screenshot("image-unreadable")
                .flex_1()
                .min_h(rem(112.))
                .w_full()
                .rounded(RADIUS_LARGE)
                .into_any_element()
        };
        let Some(viewer) = &self.viewer else {
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
            self.images.get(path, Size::Original, cx);
        }
        let picture = match self.images.get(&image.path, Size::Original, cx) {
            Loaded::Ready(original, _) => Some(original),
            // The gallery's thumbnail stands in while the original decodes.
            Loaded::Pending => self.images.any_ready(&image.path),
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
        let Some(viewer) = &self.viewer else {
            return div().into_any_element();
        };
        let viewport = div()
            .on_children_prepainted(move |_, window, cx| {
                let size = viewport_scroll.bounds().size;
                let owner = owner.clone();
                window.defer(cx, move |_, cx| {
                    let _ = owner.update(cx, |this, cx| {
                        if let Some(viewer) = &mut this.viewer
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
            .size_full()
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
                    if let Some(viewer) = &mut this.viewer {
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
            );
        // The kit's outlined card surface frames the image: its outline and corners.
        CardSurface::new(CardVariant::Outlined)
            .rounded(RADIUS_LARGE)
            .flex_1()
            .min_h(rem(112.))
            .w_full()
            .overflow_hidden()
            .relative()
            .child(viewport)
            .into_any_element()
    }

    /// Where the reader returns to, named for its crumb in the title bar.
    pub(crate) fn reader_origin_label(&self) -> &'static str {
        match self.opened.result_origin {
            Page::New => course2md::tr!("主页"),
            Page::Task => course2md::tr!("任务"),
            Page::Settings => course2md::tr!("设置"),
            _ => course2md::tr!("笔记库"),
        }
    }

    /// A note is read where it was opened: a tonal return to that place, the breadcrumb
    /// over the note title, then the reading tools.
    /// The title bar's content while a note shows: the way back, where the note lives,
    /// its title and the reading tools. Without a note, `bar` as it is.
    pub(crate) fn title_bar(
        &mut self,
        bar: Div,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let Some(preview) = self.opened.preview.clone() else {
            return bar;
        };
        let metrics = ReaderMetrics::new(window, !self.outline.is_empty());
        let tools = self.reader_tools(&preview, metrics, window, cx);
        let title = preview.course.title.clone();
        let back = course2md::tr!("返回「{}」", self.reader_origin_label());
        // The crumb names where the note lives, however it was reached; the
        // return button names the origin.
        let library = course2md::tr!("笔记库");
        let crumb = match &self.opened.preview {
            Some(preview) => {
                let folder = self
                    .records
                    .read(cx)
                    .folder_context(FolderPlace::Note(&preview.course.dir));
                match folder.load_error {
                    None => format!("{library} · {}", folder.label),
                    Some(_) => library.to_owned(),
                }
            }
            None => library.to_owned(),
        };
        // So short a window keeps the bar for its tools; the note opens with its title.
        let short = ReaderMetrics::new(window, false).short;
        // The title is said once: while the note's own headline is in view, the bar
        // names only where the note lives; scrolled past it, or on the screenshots,
        // the bar carries the title.
        let headline_in_view = self.opened.result_tab == 0
            && self.viewer.is_none()
            // The document's first item is the inset above the title.
            && self.note.list.logical_scroll_top().item_ix <= 1;
        bar.gap(rem(12.))
            .child(title_bar_control(div().flex_shrink_0().child(
                ui::tip_button("reader-back", symbols::arrow_back(), back, |button| {
                    button
                        .variant(IconButtonVariant::Tonal)
                        .tone(ButtonTone::Neutral)
                        .on_click(cx.listener(|this, _, _, cx| {
                            cx.emit(ReaderEvent::Navigate(this.opened.result_origin))
                        }))
                }),
            )))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .when(short, |title| title.invisible())
                    .child(
                        accessible_text("reader-crumb", crumb)
                            .truncate()
                            .text_size(TEXT_AUX)
                            .line_height(TypeRole::BodySmall.scale().line_height)
                            .text_color(color(ON_SURFACE_VARIANT)),
                    )
                    .when(!headline_in_view, |bar| {
                        bar.child(
                            // Both lines on their roles' line heights, so the pair centres
                            // on the back button as its ink does (U20).
                            accessible_text("reader-crumb-title", title)
                                .role(Role::Heading)
                                .truncate()
                                .text_size(TEXT_TITLE)
                                .line_height(TypeRole::TitleLarge.scale().line_height)
                                .text_color(color(ON_SURFACE)),
                        )
                    }),
            )
            .child(tools)
    }
}

/// The reader's part of the title bar: the way back, where the note is, its title and
/// the reading tools. A view of its own, so the shell around it is not built again when
/// the reader changes.
pub(crate) struct ReaderBar {
    reader: Entity<ReaderPage>,
}

impl ReaderBar {
    pub(crate) fn new(reader: Entity<ReaderPage>) -> Self {
        Self { reader }
    }
}

impl Render for ReaderBar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(feature = "capture")]
        crate::capture::count_render(crate::capture::Counted::ReaderBar);
        // Fills the cached box the shell lays out for it.
        let bar = h_flex().size_full().min_w_0().gap(px(4.)).items_center();
        self.reader
            .update(cx, |reader, cx| reader.title_bar(bar, window, cx))
    }
}

impl Render for ReaderPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(feature = "capture")]
        crate::capture::count_render(crate::capture::Counted::Reader);
        // The column the page sat in before it was a view of its own, so it centres
        // and fills the same way inside its cached box.
        v_flex()
            .size_full()
            .min_h_0()
            .child(self.reader_page(window, cx))
    }
}

fn find_placeholder() -> &'static str {
    course2md::tr!("查找笔记内容")
}

fn gallery_find_placeholder() -> &'static str {
    course2md::tr!("搜索截图对应文字")
}

impl ReaderPage {
    /// The reader's searches say what they do in the interface language.
    pub(crate) fn relocalize_reader_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let state = &*self;
        for (input, placeholder) in [
            (&state.find.input, find_placeholder()),
            (&state.gallery.find, gallery_find_placeholder()),
        ] {
            input.update(cx, |input, cx| {
                input.set_placeholder(placeholder, window, cx)
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        GalleryItem, NoteDocument, PreviewBlock, gallery_items, gallery_row_of, gallery_top_frame,
    };
    use gpui::{AppContext as _, ListAlignment, ListOffset, ListState, TestAppContext, px};
    use std::collections::HashMap;

    #[test]
    fn the_screenshot_viewer_header_starts_below_the_window_buttons() {
        use gpui_kit_material::components::traffic_light_position;
        // At the smallest text scale the row the viewer keeps clear is still the
        // whole title bar the traffic lights are centred on, and taller than the
        // Windows and Linux caption buttons (32 px).
        let row =
            super::TITLE_BAR_HEIGHT.to_pixels(px(gpui_kit_material::tokens::typography::BASE_PX));
        let lights = traffic_light_position(crate::views::APP_BAR_HEIGHT);
        assert!(row >= lights.y * 2. + px(14.), "{row:?}");
        assert!(row >= px(32.));
    }

    #[test]
    fn the_text_goes_beside_the_image_only_where_the_image_stays_as_large() {
        use super::text_beside;
        // The column and its gap, and the gap below, at 100% text.
        let (column, gap) = (360. + 24., 8.);
        let slide = (1280., 720.);
        // Bodies measured in the native viewer (window less header, padding and
        // action row), and what each layout draws a 16:9 slide at.
        // 1280×620: beside 62%, below 37% — the column wins.
        assert!(text_beside((1224., 446.), slide, column, gap));
        // 880×760: beside 34%, below 49% — the image would shrink by a third.
        assert!(!text_beside((824., 586.), slide, column, gap));
        // 1024×768: beside 46%, below 48% — still below.
        assert!(!text_beside((968., 594.), slide, column, gap));
        // 1440×900: beside 78%, below 59%.
        assert!(text_beside((1384., 726.), slide, column, gap));
        // 1920×1080 shows the slide at full size either way: beside.
        assert!(text_beside((1864., 906.), slide, column, gap));
        // A tall portrait image is limited by height, so the column costs nothing.
        assert!(text_beside((824., 586.), (720., 1280.), column, gap));
        // Nothing measured, or nothing to fit: below.
        assert!(!text_beside((0., 0.), slide, column, gap));
        assert!(!text_beside((1224., 446.), (0., 0.), column, gap));
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
                .starts_with("![](reader:start)\n\n# 标题 *\n\n![](reader:head)")
        );
        assert!(
            document
                .markdown
                .contains("## 问题由来\n\n### [▶ 01:00](https://video.example/?t=60)")
        );
        // The summary is one panel, which names it; its text stays the note's.
        assert!(
            document
                .markdown
                .contains("> [!summary]\n> 总览 \\*不是强调\\*\n>\n> - 第一点\n> - 第二点 $x$\n\n")
        );
        assert!(!document.markdown.contains("## 摘要"));
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
                None,
                Some(0),
                Some(4),
                Some(4),
                Some(6),
                Some(6),
                Some(6),
                Some(6),
                Some(7),
                Some(5),
                Some(8),
                None
            ]
        );
        // The card's inset below the last block scrolls with the note.
        assert!(document.markdown.ends_with("![](reader:end)"));
        // The overview and every key point are in the summary's panel.
        assert_eq!(document.item_of(1), Some(3));
        assert_eq!(document.item_of(3), Some(3));
        assert_eq!(document.block_at(0), Some(0));
        assert_eq!(document.block_at(11), Some(5));
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
        let open: crate::note_text::OpenLink = std::sync::Arc::new(|_, _, _| {});
        let extensions = crate::note_text::note_extensions(Default::default(), render, open);
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
}
