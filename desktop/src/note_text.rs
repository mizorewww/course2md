//! Note text in the window's text selection, with typeset formulas.
//!
//! A paragraph is laid out as GPUI text. An inline formula is held in the
//! text by a run of no-break spaces as wide as the typeset formula, which is
//! painted over them, so prose wraps around it like a word. A display
//! formula, or an inline one wider than the line, takes a row of its own
//! that scrolls sideways instead of being cut off.
//!
//! The whole paragraph is one selection participant: dragging, double- and
//! triple-clicking select as in plain text, find highlights and scroll
//! targets apply to the source text, and copying yields the selected span
//! of the source, so a formula comes back as its LaTeX with its `$…$` or
//! `$$…$$` delimiters. A label shown before the text is not copied, a list
//! item copies as `- item`, and each paragraph ends with a blank line so a
//! copied passage reads as Markdown.
use gpui::{
    AnyElement, App, Bounds, Corners, Edges, ElementId, GlobalElementId, HighlightStyle, Hitbox,
    HitboxBehavior, Hsla, InspectorElementId, LayoutId, MouseDownEvent, PaintQuad, Pixels, Point,
    SharedString, StyledText, Subscription, TextLayout, TransformationMatrix, Window, div,
    prelude::*, px, rems, transparent_black,
};
use gpui_base::{
    TextSelectionEvent, TextSelectionHandle, TextSelectionRegistration, TextSelectionRun,
    TextSelectionSnapshot,
};
use std::{cell::RefCell, ops::Range, rc::Rc};

/// Holds an inline formula's place in the text: wraps neither inside nor
/// next to it, and is measured to the formula's width.
const PLACEHOLDER: char = '\u{00A0}';
/// Allows a line break on either side of an inline formula.
const BREAK: char = '\u{200B}';
/// The widest one formula image is drawn, in device pixels: GPUI rasterises an
/// SVG at twice that size and draws none wider than 8192 pixels, so a wider
/// formula is drawn as several tiles side by side.
const MAX_FORMULA_PIXELS: f32 = 4000.;
/// The smallest a preview shrinks a formula to fit its line's width, as a
/// share of its line-height size.
const PREVIEW_MIN_SCALE: f32 = 0.5;
/// How tall an inline formula may be, in lines, before it is scaled down.
const INLINE_ROOM: f32 = 1.4;

/// How the paragraph copies.
#[derive(Clone, Default)]
pub enum Copy {
    #[default]
    Plain,
    /// A list item, copied as `- item`.
    ListItem,
    /// Shown before the text but not copied, such as a caption.
    Labeled(SharedString),
}

type SourcePosition = Box<dyn FnOnce(Point<Pixels>, Pixels)>;

/// Selectable note text; see the module documentation.
pub struct NoteText {
    id: ElementId,
    source: SharedString,
    copy: Copy,
    document_order: u64,
    highlights: Vec<(Range<usize>, HighlightStyle)>,
    source_position: Option<(usize, SourcePosition)>,
    selection_color: Hsla,
    surface: Hsla,
    group: Option<SharedString>,
    preview: bool,
    child: Option<AnyElement>,
}

impl NoteText {
    pub fn new(id: impl Into<ElementId>, source: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            source: source.into(),
            copy: Copy::Plain,
            document_order: 0,
            highlights: Vec::new(),
            source_position: None,
            selection_color: transparent_black(),
            surface: crate::theme::color(crate::theme::SURFACE).into(),
            group: None,
            preview: false,
            child: None,
        }
    }

    pub fn copy(mut self, copy: Copy) -> Self {
        self.copy = copy;
        self
    }

    /// Places the text in reading order among the note's other text.
    pub fn document_order(mut self, order: u64) -> Self {
        self.document_order = order;
        self
    }

    /// Find matches, as byte ranges of the source text.
    pub fn highlights(mut self, highlights: Vec<(Range<usize>, HighlightStyle)>) -> Self {
        self.highlights = highlights;
        self
    }

    /// Reports where the source byte `byte` was laid out (its line's top left
    /// in window coordinates) and the line height, once known.
    pub fn on_source_position(
        mut self,
        byte: usize,
        callback: impl FnOnce(Point<Pixels>, Pixels) + 'static,
    ) -> Self {
        self.source_position = Some((byte, Box::new(callback)));
        self
    }

    pub fn selection_color(mut self, color: Hsla) -> Self {
        self.selection_color = color;
        self
    }

    /// The surface behind the text, which a wide formula row fades into.
    pub fn surface(mut self, color: Hsla) -> Self {
        self.surface = color;
        self
    }

    /// A clamped preview, such as a screenshot card's excerpt: every formula
    /// stays in the line, scaled to the line's height (and its width, down to
    /// half that size, beyond which it is cut), so the text can end in an
    /// ellipsis; it is not selectable.
    pub fn preview(mut self) -> Self {
        self.preview = true;
        self
    }

    /// Paragraphs laid out at one width, such as the article column: a new
    /// one assumes the group's width until it has its own.
    pub fn width_group(mut self, group: impl Into<SharedString>) -> Self {
        self.group = Some(group.into());
        self
    }
}

/// One laid-out line of selectable text: its display text, the source span
/// each part shows, and the formulas painted over it.
struct Block {
    display: SharedString,
    segments: Vec<Segment>,
    layout: TextLayout,
}

/// What a range of display text shows of the source.
#[derive(Clone, Debug, PartialEq)]
pub struct Segment {
    pub display: Range<usize>,
    pub source: Range<usize>,
    /// Shown as a whole (a formula or a label): any part selects all of it.
    pub atomic: bool,
}

#[derive(Clone, Debug, PartialEq)]
struct PlacedFormula {
    /// The no-break spaces holding its place.
    display: Range<usize>,
    path: SharedString,
    width: Pixels,
    height: Pixels,
    /// Distance from its top to its baseline (inline formulas only).
    ascent: Option<Pixels>,
}

/// Shared between the element, its selection subscription and its copy.
#[derive(Default)]
struct Shared {
    source: SharedString,
    copy: Copy,
    snapshot: Option<TextSelectionSnapshot>,
    runs: Vec<(SharedString, TextLayout, Vec<Segment>)>,
    /// Selected display ranges per block, as projected for painting.
    painted: Vec<Option<Range<usize>>>,
    /// A triple click's whole paragraph, when it spans formula rows.
    paragraph: Paragraph,
}

/// Window text selection selects the line under a triple click within one
/// run of text; a paragraph split by a formula row spans several, so the
/// element selects the paragraph itself. Armed by the click, it takes over
/// the selection that click makes and ends with the next one.
#[derive(Clone, Debug, Default, PartialEq)]
enum Paragraph {
    #[default]
    None,
    Armed(Range<usize>),
    Selected(Range<usize>),
}

impl Shared {
    fn selection_event(&mut self, event: &TextSelectionEvent) {
        match event {
            TextSelectionEvent::SelectionChanged(snapshot) => {
                self.snapshot = *snapshot;
                self.paragraph = match (std::mem::take(&mut self.paragraph), snapshot) {
                    (Paragraph::Armed(range), Some(_)) => Paragraph::Selected(range),
                    (Paragraph::Selected(_), None) => Paragraph::None,
                    (paragraph, _) => paragraph,
                };
            }
            TextSelectionEvent::Cleared => {
                if let Paragraph::Selected(_) = self.paragraph {
                    self.paragraph = Paragraph::None;
                }
            }
            TextSelectionEvent::AutoScroll(_) => {}
        }
    }

    /// The source paragraph (text between blank lines) under `position`,
    /// when it spans more than one of the laid-out blocks.
    fn paragraph_at(&self, position: Point<Pixels>) -> Option<Range<usize>> {
        let byte = self.runs.iter().find_map(|(_, layout, segments)| {
            if !layout.bounds().contains(&position) {
                return None;
            }
            let index = match layout.index_for_position(position) {
                Ok(index) | Err(index) => index,
            };
            source_range(segments, index..index + 1).map(|range| range.start)
        })?;
        let range = paragraph_range(&self.source, byte);
        let blocks = self
            .runs
            .iter()
            .filter(|(_, _, segments)| !display_ranges(segments, &range).is_empty())
            .count();
        (blocks > 1).then_some(range)
    }

    /// The copied text for the current selection, from the current content.
    fn copied(&self) -> String {
        if let Paragraph::Selected(range) = &self.paragraph {
            return copied_text(&self.source, &self.copy, std::slice::from_ref(range));
        }
        let Some(points) = self.snapshot.and_then(|snapshot| snapshot.window_points()) else {
            return String::new();
        };
        let selected: Vec<Range<usize>> = self
            .runs
            .iter()
            .filter_map(|(text, layout, segments)| {
                let range = selected_range(text, layout, points.anchor(), points.cursor())?;
                source_range(segments, range)
            })
            .collect();
        copied_text(&self.source, &self.copy, &selected)
    }
}

struct Retained {
    handle: TextSelectionHandle,
    shared: Rc<RefCell<Shared>>,
    width: Option<Pixels>,
    /// Scroll positions of the formula rows, in order.
    rows: Vec<gpui::ScrollHandle>,
    _subscription: Subscription,
}

impl Retained {
    fn new(source: SharedString, cx: &mut App) -> Self {
        let handle = TextSelectionHandle::new(source, cx);
        let shared = Rc::new(RefCell::new(Shared::default()));
        let selection = shared.clone();
        let subscription = handle.subscribe(
            move |event, _| {
                selection.borrow_mut().selection_event(event);
            },
            cx,
        );
        let copied = shared.clone();
        handle.copy_with(move |_| copied.borrow().copied(), cx);
        Self {
            handle,
            shared,
            width: None,
            rows: Vec::new(),
            _subscription: subscription,
        }
    }
}

/// Typeset sizes of one formula at the paragraph's text size.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FormulaSize {
    pub width: f32,
    pub height: f32,
    pub ascent: f32,
}

/// How a formula is placed: in the line with `n` placeholders, or on its own row.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Placement {
    Inline {
        placeholders: usize,
        size: FormulaSize,
    },
    Row {
        placeholders: usize,
        size: FormulaSize,
    },
}

/// One planned block: its display text and segments, and where formulas go.
#[derive(Debug, PartialEq)]
pub struct PlannedBlock {
    pub display: String,
    pub segments: Vec<Segment>,
    pub row: bool,
    /// Display range of each formula's placeholders, with its LaTeX and display flag.
    pub formulas: Vec<(Range<usize>, String, bool, FormulaSize)>,
}

/// Splits `source` into blocks: prose with inline formulas, and rows for
/// display formulas and inline ones `place` puts on a row. Unreadable LaTeX
/// stays in the prose as written. Prose next to a row is trimmed.
pub fn plan(
    source: &str,
    label: Option<&str>,
    mut place: impl FnMut(&str, bool) -> Option<Placement>,
) -> Vec<PlannedBlock> {
    let mut blocks: Vec<PlannedBlock> = Vec::new();
    let mut current = PlannedBlock {
        display: String::new(),
        segments: Vec::new(),
        row: false,
        formulas: Vec::new(),
    };
    if let Some(label) = label.filter(|label| !label.is_empty()) {
        current.display.push_str(label);
        current.segments.push(Segment {
            display: 0..label.len(),
            source: 0..0,
            atomic: true,
        });
    }
    let push_prose = |block: &mut PlannedBlock, source_range: Range<usize>| {
        let mut range = source_range;
        if block.display.is_empty() {
            // A block never begins with the space or line break after a row.
            let text = &source[range.clone()];
            range.start += text.len() - text.trim_start().len();
        }
        if range.is_empty() {
            return;
        }
        let start = block.display.len();
        block.display.push_str(&source[range.clone()]);
        block.segments.push(Segment {
            display: start..block.display.len(),
            source: range,
            atomic: false,
        });
    };
    let finish = |block: &mut PlannedBlock, blocks: &mut Vec<PlannedBlock>| {
        // Nor ends with the space or line break before one.
        let trimmed = block.display.trim_end().len();
        block.display.truncate(trimmed);
        block.segments.retain_mut(|segment| {
            if segment.display.start >= trimmed {
                return false;
            }
            if segment.display.end > trimmed {
                let cut = segment.display.end - trimmed;
                segment.display.end = trimmed;
                if !segment.atomic {
                    segment.source.end -= cut;
                }
            }
            true
        });
        if !block.display.is_empty() {
            blocks.push(std::mem::replace(
                block,
                PlannedBlock {
                    display: String::new(),
                    segments: Vec::new(),
                    row: false,
                    formulas: Vec::new(),
                },
            ));
        }
    };
    for (range, piece) in piece_sources(source) {
        match piece {
            crate::math::Piece::Text(_) => {
                let mut range = range;
                // Closing punctuation never starts a line: after a formula
                // row it stays on the row, following the formula.
                if current.display.is_empty()
                    && let Some(row) = blocks.last_mut().filter(|block| block.row)
                {
                    let text = &source[range.clone()];
                    let start = range.start + text.len() - text.trim_start().len();
                    let end = start
                        + source[start..range.end]
                            .chars()
                            .take_while(|c| closes_line(*c))
                            .map(char::len_utf8)
                            .sum::<usize>();
                    if end > start {
                        let from = row.display.len();
                        row.display.push_str(&source[start..end]);
                        row.segments.push(Segment {
                            display: from..row.display.len(),
                            source: start..end,
                            atomic: false,
                        });
                        range.start = end;
                    }
                }
                push_prose(&mut current, range)
            }
            crate::math::Piece::Math { latex, display } => match place(latex, display) {
                Some(Placement::Inline { placeholders, size }) => {
                    let start = current.display.len();
                    current.display.push(BREAK);
                    let holder = current.display.len();
                    current
                        .display
                        .extend(std::iter::repeat_n(PLACEHOLDER, placeholders.max(1)));
                    let holder = holder..current.display.len();
                    current.display.push(BREAK);
                    current.segments.push(Segment {
                        display: start..current.display.len(),
                        source: range,
                        atomic: true,
                    });
                    current
                        .formulas
                        .push((holder, latex.to_owned(), false, size));
                }
                Some(Placement::Row { placeholders, size }) => {
                    finish(&mut current, &mut blocks);
                    let display: String =
                        std::iter::repeat_n(PLACEHOLDER, placeholders.max(1)).collect();
                    let len = display.len();
                    blocks.push(PlannedBlock {
                        display,
                        segments: vec![Segment {
                            display: 0..len,
                            source: range,
                            atomic: true,
                        }],
                        row: true,
                        formulas: vec![(0..len, latex.to_owned(), true, size)],
                    });
                }
                None => push_prose(&mut current, range),
            },
        }
    }
    finish(&mut current, &mut blocks);
    blocks
}

/// Punctuation that may not begin a line (CJK and Latin closing marks).
fn closes_line(c: char) -> bool {
    "，。、；：！？）」』”’》〉】,.;:!?)]}".contains(c)
}

/// The byte ranges `pieces` cover in `text`: prose as written, and a formula
/// together with its delimiters (and any space inside them).
pub fn piece_sources(text: &str) -> Vec<(Range<usize>, crate::math::Piece<'_>)> {
    let offset = |slice: &str| slice.as_ptr() as usize - text.as_ptr() as usize;
    let mut position = 0;
    crate::math::pieces(text)
        .into_iter()
        .map(|piece| {
            let end = match &piece {
                crate::math::Piece::Text(prose) => offset(prose) + prose.len(),
                // `pieces` trims the LaTeX inside its delimiters; the source
                // runs on through that space and the closing delimiter.
                crate::math::Piece::Math { latex, display } => {
                    let inner_end = offset(latex) + latex.len();
                    let rest = &text[inner_end..];
                    let space = rest.len() - rest.trim_start().len();
                    (inner_end + space + if *display { 2 } else { 1 }).min(text.len())
                }
            };
            let source = position..end;
            position = end;
            (source, piece)
        })
        .collect()
}

/// The source span a selected display range shows; a formula or label
/// counts whole once any part of it is selected.
pub fn source_range(segments: &[Segment], display: Range<usize>) -> Option<Range<usize>> {
    let mut found: Option<Range<usize>> = None;
    for segment in segments {
        if segment.display.end <= display.start || display.end <= segment.display.start {
            continue;
        }
        let part = if segment.atomic {
            segment.source.clone()
        } else {
            let from = display.start.max(segment.display.start) - segment.display.start;
            let to = display.end.min(segment.display.end) - segment.display.start;
            segment.source.start + from..segment.source.start + to
        };
        found = Some(match found {
            Some(range) => range.start.min(part.start)..range.end.max(part.end),
            None => part,
        });
    }
    found
}

/// The display ranges that show a source range.
pub fn display_ranges(segments: &[Segment], source: &Range<usize>) -> Vec<Range<usize>> {
    segments
        .iter()
        .filter(|segment| {
            !segment.source.is_empty()
                && segment.source.start < source.end
                && source.start < segment.source.end
        })
        .map(|segment| {
            if segment.atomic {
                segment.display.clone()
            } else {
                let from = source.start.max(segment.source.start) - segment.source.start;
                let to = source.end.min(segment.source.end) - segment.source.start;
                segment.display.start + from..segment.display.start + to
            }
        })
        .collect()
}

/// A selected display range grown to cover every formula (or other whole
/// segment) it touches, so it is painted as it is copied.
fn widened(segments: &[Segment], range: Range<usize>) -> Range<usize> {
    let selected = range.clone();
    segments
        .iter()
        .filter(|segment| {
            segment.atomic
                && !segment.source.is_empty()
                && segment.display.start < selected.end
                && selected.start < segment.display.end
        })
        .fold(range, |range, segment| {
            range.start.min(segment.display.start)..range.end.max(segment.display.end)
        })
}

/// Where a source byte is shown, if this block shows it.
fn display_index(segments: &[Segment], byte: usize) -> Option<usize> {
    segments
        .iter()
        .find(|segment| segment.source.start <= byte && byte < segment.source.end)
        .map(|segment| {
            if segment.atomic {
                segment.display.start
            } else {
                segment.display.start + byte - segment.source.start
            }
        })
}

/// The paragraph around `byte`: the text between the blank lines before and
/// after it.
fn paragraph_range(source: &str, byte: usize) -> Range<usize> {
    let byte = byte.min(source.len());
    let start = source[..byte].rfind("\n\n").map_or(0, |found| found + 2);
    let end = source[byte..]
        .find("\n\n")
        .map_or(source.len(), |found| byte + found);
    start..end
}

/// The copied text for the selected source spans.
pub fn copied_text(source: &str, copy: &Copy, selected: &[Range<usize>]) -> String {
    let start = selected.iter().map(|range| range.start).min();
    let end = selected.iter().map(|range| range.end).max();
    let (Some(start), Some(end)) = (start, end) else {
        return String::new();
    };
    let Some(text) = source
        .get(start..end)
        .filter(|text| !text.trim().is_empty())
    else {
        return String::new();
    };
    let item = matches!(copy, Copy::ListItem) && start == 0;
    // The blank line separates it from the next paragraph; the copy as a
    // whole is trimmed.
    format!("{}{text}\n", if item { "- " } else { "" })
}

/// The selected byte range of laid-out text between two window points,
/// character by character as window text selection projects it.
fn selected_range(
    text: &str,
    layout: &TextLayout,
    anchor: Point<Pixels>,
    cursor: Point<Pixels>,
) -> Option<Range<usize>> {
    if text.len() != layout.len() {
        return None;
    }
    let line_height = layout.line_height();
    let mut range: Option<Range<usize>> = None;
    for (offset, character) in text.char_indices() {
        let next = offset + character.len_utf8();
        let Some(position) = layout.position_for_index(offset) else {
            continue;
        };
        let width = layout
            .position_for_index(next)
            .filter(|next| next.y == position.y)
            .map_or(line_height / 2., |next| next.x - position.x);
        if in_selection_band(position, width, anchor, cursor, line_height) {
            range.get_or_insert(offset..offset).end = next;
        }
    }
    range
}

fn in_selection_band(
    position: Point<Pixels>,
    width: Pixels,
    start: Point<Pixels>,
    end: Point<Pixels>,
    line_height: Pixels,
) -> bool {
    let on_line =
        |point: Point<Pixels>| point.y >= position.y && point.y < position.y + line_height;
    let top = start.y.min(end.y);
    let bottom = start.y.max(end.y);
    let x = position.x + width / 2.;
    if position.y + line_height <= top || position.y > bottom {
        return false;
    }
    if on_line(start) && on_line(end) {
        return x >= start.x.min(end.x) && x <= start.x.max(end.x);
    }
    let (upper, lower) = if start.y < end.y {
        (start, end)
    } else {
        (end, start)
    };
    if on_line(upper) {
        x >= upper.x
    } else if on_line(lower) {
        x <= lower.x
    } else {
        true
    }
}

/// The quads that paint a selection from `start` to `end` in `bounds`.
fn selection_quads(
    start: Point<Pixels>,
    end: Point<Pixels>,
    bounds: Bounds<Pixels>,
    line_height: Pixels,
) -> Vec<Bounds<Pixels>> {
    if start.y == end.y {
        return vec![Bounds::from_corners(
            start,
            Point::new(end.x, end.y + line_height),
        )];
    }
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
}

/// Merges overlapping highlight ranges (several matches in one formula),
/// keeping them in order as styled text requires.
fn merged(
    mut highlights: Vec<(Range<usize>, HighlightStyle)>,
) -> Vec<(Range<usize>, HighlightStyle)> {
    highlights.sort_by_key(|(range, _)| range.start);
    let mut result: Vec<(Range<usize>, HighlightStyle)> = Vec::new();
    for (range, style) in highlights {
        match result.last_mut() {
            Some((last, _)) if range.start < last.end => last.end = last.end.max(range.end),
            _ => result.push((range, style)),
        }
    }
    result
}

impl IntoElement for NoteText {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

pub struct NoteTextLayout {
    handle: TextSelectionHandle,
    shared: Rc<RefCell<Shared>>,
    blocks: Vec<Block>,
    /// The width this layout's formula placement assumed.
    placed_for: Option<Pixels>,
    /// Inline formula widths and whether each fitted, to tell whether a new
    /// width moves any to or from a row.
    inline_widths: Vec<(Pixels, bool)>,
    margin: Pixels,
}

/// How many tiles a formula `width` pixels wide is drawn in.
fn tile_count(width: f32, scale_factor: f32) -> usize {
    ((width * scale_factor / MAX_FORMULA_PIXELS).ceil() as usize).max(1)
}

thread_local! {
    /// The width each group of paragraphs was last laid out at.
    static GROUP_WIDTHS: RefCell<std::collections::HashMap<SharedString, Pixels>> =
        RefCell::default();
}

fn group_width(group: &SharedString) -> Option<Pixels> {
    GROUP_WIDTHS.with(|widths| widths.borrow().get(group).copied())
}

/// Whether an inline formula fits a line `width` wide (unknown: it does).
fn fits_inline(formula: Pixels, width: Option<Pixels>, margin: Pixels) -> bool {
    width.is_none_or(|width| formula <= width - margin)
}

impl Element for NoteText {
    type RequestLayoutState = NoteTextLayout;
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
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, NoteTextLayout) {
        let (handle, shared, own_width) = window.with_element_state(
            global_id.expect("note text has a stable element id"),
            |retained: Option<Retained>, _| {
                let retained = retained.unwrap_or_else(|| Retained::new(self.source.clone(), cx));
                (
                    (
                        retained.handle.clone(),
                        retained.shared.clone(),
                        retained.width,
                    ),
                    retained,
                )
            },
        );
        // Before its first layout, a paragraph assumes the width last found
        // for its group (the article column, the viewer), so a formula too
        // wide for the line takes its row from the first frame.
        let width = own_width.or_else(|| self.group.as_ref().and_then(group_width));
        let rem = window.rem_size();
        let style = window.text_style();
        let font_size = style.font_size.to_pixels(rem);
        let line_height = style.line_height_in_pixels(rem);
        // Formulas are drawn in the text's own color.
        let ink = style.color;
        // The placeholder's width as the text will actually be shaped.
        const SAMPLE: usize = 64;
        let sample: SharedString = std::iter::repeat_n(PLACEHOLDER, SAMPLE)
            .collect::<String>()
            .into();
        let runs = [style.to_run(sample.len())];
        let space = (window
            .text_system()
            .shape_line(sample, font_size, &runs, None)
            .width
            / SAMPLE as f32)
            .max(px(1.));
        let margin = space * 2.;
        let em = f32::from(font_size);
        let placeholders = |width: f32| (width / f32::from(space)).ceil() as usize;
        let mut inline_widths = Vec::new();
        let label = match &self.copy {
            Copy::Labeled(label) => Some(label.to_string()),
            _ => None,
        };
        let planned = plan(&self.source, label.as_deref(), |latex, display| {
            let formula = crate::math::formula(latex, display)?;
            let full = FormulaSize {
                width: formula.width() * em,
                height: (formula.height() + formula.depth()) * em,
                ascent: formula.height() * em,
            };
            let row = Placement::Row {
                placeholders: placeholders(full.width),
                size: full,
            };
            if self.preview {
                // Scaled to the line, and to its width while it stays at
                // least half as tall; a longer one keeps that size and shows
                // its beginning, cut at the line's end.
                let height = (f32::from(line_height) / full.height).min(1.);
                let line = width.map(|width| f32::from(width - margin).max(0.));
                let across = line.map_or(1., |line| (line / full.width).min(1.));
                let scale = height.min(across.max(height * PREVIEW_MIN_SCALE));
                inline_widths.push((px(full.width), fits_inline(px(full.width), width, margin)));
                let size = FormulaSize {
                    width: full.width * scale,
                    height: full.height * scale,
                    ascent: full.ascent * scale,
                };
                let held = line.map_or(size.width, |line| size.width.min(line));
                return Some(Placement::Inline {
                    placeholders: placeholders(held),
                    size,
                });
            }
            if display {
                return Some(row);
            }
            // A tall inline formula, such as a fraction, may reach into the
            // space between lines (a line is 1.8 em, its glyphs about 1 em);
            // only a taller one is scaled down rather than pushing lines apart.
            let room = f32::from(line_height) * INLINE_ROOM;
            let scale = (room / full.height).min(1.);
            let inline = FormulaSize {
                width: full.width * scale,
                height: full.height * scale,
                ascent: full.ascent * scale,
            };
            let fits = fits_inline(px(inline.width), width, margin);
            inline_widths.push((px(inline.width), fits));
            Some(if fits {
                Placement::Inline {
                    placeholders: placeholders(inline.width),
                    size: inline,
                }
            } else {
                row
            })
        });
        let source_len = self.source.len();
        let display_highlights = |segments: &[Segment]| {
            merged(
                self.highlights
                    .iter()
                    .filter(|(range, _)| range.start < range.end && range.end <= source_len)
                    .flat_map(|(range, style)| {
                        display_ranges(segments, range)
                            .into_iter()
                            .map(move |range| (range, *style))
                    })
                    .collect(),
            )
        };
        let row_count = planned.iter().filter(|block| block.row).count();
        let rows = window.with_element_state(
            global_id.expect("note text has a stable element id"),
            |retained: Option<Retained>, _| {
                let mut retained = retained.expect("created above");
                retained
                    .rows
                    .resize_with(row_count, gpui::ScrollHandle::new);
                (retained.rows.clone(), retained)
            },
        );
        let mut rows = rows.into_iter();
        let mut blocks = Vec::with_capacity(planned.len());
        let mut children = Vec::with_capacity(planned.len());
        for (index, block) in planned.into_iter().enumerate() {
            let display = SharedString::from(block.display);
            let highlights = display_highlights(&block.segments);
            let text = if highlights.is_empty() {
                StyledText::new(display.clone())
            } else {
                StyledText::new(display.clone()).with_highlights(highlights)
            };
            let layout = text.layout().clone();
            let formulas = block
                .formulas
                .iter()
                .filter_map(|(range, latex, display_mode, size)| {
                    Some(PlacedFormula {
                        display: range.clone(),
                        path: crate::math::formula(latex, *display_mode)?.path(),
                        width: px(size.width),
                        height: px(size.height),
                        ascent: (!block.row).then(|| px(size.ascent)),
                    })
                })
                .collect::<Vec<_>>();
            let tallest = formulas
                .iter()
                .map(|formula| formula.height)
                .fold(line_height, Pixels::max);
            let row_width = formulas.first().map(|formula| formula.width);
            let line = Line {
                text,
                index,
                shared: shared.clone(),
                formulas,
                ink,
                selection_color: self.selection_color,
                font_size,
                fade: block.row.then_some(self.surface),
            };
            children.push(
                if let Some(scroll) = block.row.then(|| rows.next()).flatten() {
                    // A row scrolls sideways when the formula is wider than the
                    // paragraph, and centers it otherwise. An overflowing row
                    // keeps its scrollbar shown, and the formula fades at each
                    // edge it continues past (see `Line`), so it never looks
                    // complete when it is not.
                    let overflows = matches!(
                        (row_width, width),
                        (Some(formula), Some(width)) if formula > width
                    );
                    div()
                        .relative()
                        .w_full()
                        .child(
                            div()
                                .id(("note-formula-row", index))
                                .w_full()
                                .flex()
                                .overflow_x_scroll()
                                // A vertical wheel keeps scrolling the text around it.
                                .restrict_scroll_to_axis()
                                .track_scroll(&scroll)
                                .pt(rems(0.5))
                                // Room for the scrollbar under an overflowing formula.
                                .pb(if overflows { rems(1.) } else { rems(0.5) })
                                .child(
                                    div()
                                        .flex_none()
                                        .mx_auto()
                                        .h(tallest)
                                        .flex()
                                        .items_center()
                                        .whitespace_nowrap()
                                        .child(line),
                                ),
                        )
                        .child(crate::scroll::horizontal_scrollbar(&scroll, overflows))
                        .into_any_element()
                } else {
                    line.into_any_element()
                },
            );
            blocks.push(Block {
                display,
                segments: block.segments,
                layout,
            });
        }
        let mut child = div()
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(rems(0.25))
            .children(children)
            .into_any_element();
        let layout_id = child.request_layout(window, cx);
        self.child = Some(child);
        (
            layout_id,
            NoteTextLayout {
                handle,
                shared,
                blocks,
                placed_for: width,
                inline_widths,
                margin,
            },
        )
    }

    fn prepaint(
        &mut self,
        global_id: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        state: &mut NoteTextLayout,
        window: &mut Window,
        cx: &mut App,
    ) -> Hitbox {
        if let Some(child) = &mut self.child {
            child.prepaint(window, cx);
        }
        let text_bounds: Vec<_> = state
            .blocks
            .iter()
            .map(|block| block.layout.bounds())
            .collect();
        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
        if !self.preview {
            state.handle.register(
                TextSelectionRegistration::new(hitbox.clone(), bounds)
                    .with_document_order(self.document_order)
                    .with_text_bounds(text_bounds),
                window,
                cx,
            );
        }
        let width = bounds.size.width;
        if let Some(group) = &self.group {
            GROUP_WIDTHS.with(|widths| widths.borrow_mut().insert(group.clone(), width));
        }
        if state.placed_for != Some(width) {
            if let Some(global_id) = global_id {
                window.with_element_state(global_id, |retained: Option<Retained>, _| {
                    let mut retained = retained.expect("laid out this frame");
                    retained.width = Some(width);
                    ((), retained)
                });
            }
            // Another frame places the formulas for the width found.
            if state
                .inline_widths
                .iter()
                .any(|(formula, fits)| fits_inline(*formula, Some(width), state.margin) != *fits)
            {
                window.refresh();
            }
        }
        if let Some((byte, callback)) = self.source_position.take() {
            let found = state.blocks.iter().find_map(|block| {
                let index = display_index(&block.segments, byte)?;
                Some((
                    block.layout.position_for_index(index)?,
                    block.layout.line_height(),
                ))
            });
            if let Some((position, line_height)) = found {
                callback(position, line_height);
            }
        }
        hitbox
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        state: &mut NoteTextLayout,
        _: &mut Hitbox,
        window: &mut Window,
        cx: &mut App,
    ) {
        if self.preview {
            if let Some(child) = &mut self.child {
                child.paint(window, cx);
            }
            return;
        }
        let runs: Vec<TextSelectionRun> = state
            .blocks
            .iter()
            .enumerate()
            .map(|(index, block)| {
                TextSelectionRun::new(
                    block.display.clone(),
                    block.layout.clone(),
                    block.layout.bounds(),
                )
                .with_document_order(index as u64)
            })
            .collect();
        let projection = state.handle.update_runs(&runs, cx);
        {
            let mut shared = state.shared.borrow_mut();
            shared.source = self.source.clone();
            shared.copy = self.copy.clone();
            shared.runs = state
                .blocks
                .iter()
                .map(|block| {
                    (
                        block.display.clone(),
                        block.layout.clone(),
                        block.segments.clone(),
                    )
                })
                .collect();
            // A label is not copied, so it is not shown as selected either.
            let label = match &self.copy {
                Copy::Labeled(label) => label.len(),
                _ => 0,
            };
            shared.painted = projection
                .ranges()
                .iter()
                .enumerate()
                .map(|(index, range)| {
                    let range = range.clone()?;
                    let start = if index == 0 {
                        range.start.max(label)
                    } else {
                        range.start
                    };
                    // A formula is selected whole, as it is copied.
                    (start < range.end)
                        .then(|| widened(&state.blocks[index].segments, start..range.end))
                })
                .collect();
            if let Paragraph::Selected(paragraph) = &shared.paragraph {
                shared.painted = state
                    .blocks
                    .iter()
                    .map(|block| {
                        let ranges = display_ranges(&block.segments, paragraph);
                        let start = ranges.iter().map(|range| range.start).min()?;
                        let end = ranges.iter().map(|range| range.end).max()?;
                        Some(start..end)
                    })
                    .collect();
            }
        }
        if state.blocks.len() > 1 {
            let shared = state.shared.clone();
            window.on_mouse_event(move |event: &MouseDownEvent, phase, window, _| {
                if phase.bubble() && event.click_count >= 3 {
                    let mut shared = shared.borrow_mut();
                    if let Some(range) = shared.paragraph_at(event.position) {
                        shared.paragraph = Paragraph::Armed(range);
                        window.refresh();
                    }
                }
            });
        }
        if let Some(child) = &mut self.child {
            child.paint(window, cx);
        }
    }
}

/// One block's text: its selection, the text, then the formulas over it.
struct Line {
    text: StyledText,
    index: usize,
    shared: Rc<RefCell<Shared>>,
    formulas: Vec<PlacedFormula>,
    ink: Hsla,
    selection_color: Hsla,
    font_size: Pixels,
    /// On a formula row: the surface the formula fades into where it runs
    /// past the row's edge.
    fade: Option<Hsla>,
}

impl IntoElement for Line {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for Line {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
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
    ) -> (LayoutId, ()) {
        self.text
            .request_layout(global_id, inspector_id, window, cx)
    }

    fn prepaint(
        &mut self,
        global_id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.text
            .prepaint(global_id, inspector_id, bounds, &mut (), window, cx);
    }

    fn paint(
        &mut self,
        global_id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let layout = self.text.layout().clone();
        let line_height = layout.line_height();
        let selected = self
            .shared
            .borrow()
            .painted
            .get(self.index)
            .cloned()
            .flatten();
        let quads = selected
            .and_then(|range| {
                Some(selection_quads(
                    layout.position_for_index(range.start)?,
                    layout.position_for_index(range.end)?,
                    layout.bounds(),
                    line_height,
                ))
            })
            .unwrap_or_default();
        let selection_color = self.selection_color;
        let paint_selection = |window: &mut Window| {
            for quad in &quads {
                window.paint_quad(PaintQuad {
                    bounds: *quad,
                    background: selection_color.into(),
                    corner_radii: Corners::default(),
                    border_widths: Edges::default(),
                    border_color: transparent_black(),
                    border_style: Default::default(),
                });
            }
        };
        // Selection lies under prose, and over a formula row and its fades.
        if self.fade.is_none() {
            paint_selection(window);
        }
        self.text.paint(
            global_id,
            inspector_id,
            bounds,
            &mut (),
            &mut (),
            window,
            cx,
        );
        let visible = window.content_mask().bounds;
        let mut runs_before = false;
        let mut runs_after = false;
        for formula in &self.formulas {
            let Some(mut start) = layout.position_for_index(formula.display.start) else {
                continue;
            };
            // A line wrapped just before the formula reports its first
            // placeholder at the end of the previous line; the formula
            // starts the next one.
            let second = formula.display.start + PLACEHOLDER.len_utf8();
            if let Some(next) = layout.position_for_index(second)
                && next.y > start.y
            {
                start = Point::new(layout.bounds().left(), next.y);
            }
            let held = layout
                .position_for_index(formula.display.end)
                .filter(|end| end.y == start.y)
                .map_or(formula.width, |end| end.x - start.x);
            let x = start.x + ((held - formula.width) / 2.).max(px(0.));
            // Inline formulas share the text's baseline, which sits about
            // 0.35 em below the middle of the line; a row centers its own.
            let y = match formula.ascent {
                Some(ascent) => start.y + line_height / 2. + self.font_size * 0.35 - ascent,
                None => start.y + (line_height - formula.height) / 2.,
            };
            // A formula wider than one image is drawn as tiles, each the
            // same drawing seen through its own slice, and only the tiles
            // in view are drawn.
            runs_before |= x < visible.left() - px(0.5);
            runs_after |= x + formula.width > visible.right() + px(0.5);
            let tiles = tile_count(f32::from(formula.width), window.scale_factor());
            let tile_width = formula.width / tiles as f32;
            // A preview's formula longer than its place is cut at its end.
            let place = Bounds::new(
                Point::new(start.x, visible.top()),
                gpui::size(held.min(formula.width), visible.size.height),
            );
            let mask = gpui::ContentMask {
                bounds: if held < formula.width - px(0.5) {
                    place
                } else {
                    visible
                },
            };
            let visible = mask.bounds.intersect(&visible);
            window.with_content_mask(Some(mask), |window| {
                for tile in 0..tiles {
                    let bounds = Bounds::new(
                        Point::new(x + tile_width * tile as f32, y),
                        gpui::size(tile_width, formula.height),
                    );
                    if !bounds.intersects(&visible) {
                        continue;
                    }
                    let Some(path) = crate::math::tile(&formula.path, tile, tiles) else {
                        continue;
                    };
                    let _ = window.paint_svg(
                        bounds,
                        path,
                        None,
                        TransformationMatrix::unit(),
                        self.ink,
                        cx,
                    );
                }
            });
        }
        if let Some(surface) = self.fade {
            let width = window.rem_size() * 2.;
            let clear = Hsla { a: 0., ..surface };
            let mut fade = |left: Pixels, angle: f32| {
                window.paint_quad(PaintQuad {
                    bounds: Bounds::new(
                        Point::new(left, visible.top()),
                        gpui::size(width, visible.size.height),
                    ),
                    background: gpui::linear_gradient(
                        angle,
                        gpui::linear_color_stop(clear, 0.),
                        gpui::linear_color_stop(surface, 1.),
                    ),
                    corner_radii: Corners::default(),
                    border_widths: Edges::default(),
                    border_color: transparent_black(),
                    border_style: Default::default(),
                });
            };
            if runs_after {
                fade(visible.right() - width, 90.);
            }
            if runs_before {
                fade(visible.left(), 270.);
            }
            paint_selection(window);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn size() -> FormulaSize {
        FormulaSize {
            width: 40.,
            height: 20.,
            ascent: 14.,
        }
    }

    fn inline(_: &str, display: bool) -> Option<Placement> {
        Some(if display {
            Placement::Row {
                placeholders: 3,
                size: size(),
            }
        } else {
            Placement::Inline {
                placeholders: 2,
                size: size(),
            }
        })
    }

    fn source_of<'a>(
        text: &'a str,
        sources: &[(Range<usize>, crate::math::Piece<'_>)],
    ) -> Vec<&'a str> {
        sources
            .iter()
            .map(|(range, _)| &text[range.clone()])
            .collect()
    }

    #[test]
    fn formulas_keep_their_delimiters_in_the_source() {
        let text = "收益率 $r = \\frac{P_1}{P_0}$，以及\n$$\nE = mc^2\n$$\n结束";
        assert_eq!(
            source_of(text, &piece_sources(text)),
            vec![
                "收益率 ",
                "$r = \\frac{P_1}{P_0}$",
                "，以及\n",
                "$$\nE = mc^2\n$$",
                "\n结束"
            ]
        );
        let text = "$a$$$b$$";
        assert_eq!(source_of(text, &piece_sources(text)), vec!["$a$", "$$b$$"]);
    }

    #[test]
    fn an_inline_formula_holds_its_place_in_the_text() {
        let blocks = plan("设 $x$ 为", None, inline);
        assert_eq!(blocks.len(), 1);
        let block = &blocks[0];
        assert_eq!(block.display, "设 \u{200B}\u{A0}\u{A0}\u{200B} 为");
        assert_eq!(
            block.formulas[0].0,
            "设 \u{200B}".len().."设 \u{200B}\u{A0}\u{A0}".len()
        );
        // Selecting part of the placeholder copies the whole formula.
        let holder = block.formulas[0].0.clone();
        let source = source_range(&block.segments, holder.start..holder.start + 1).unwrap();
        assert_eq!(&"设 $x$ 为"[source], "$x$");
    }

    #[test]
    fn a_display_formula_takes_a_row_and_the_prose_around_it_is_trimmed() {
        let text = "如下：\n$$E = mc^2$$\n 和后文";
        let blocks = plan(text, None, inline);
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[0].display, "如下：");
        assert!(blocks[1].row);
        assert_eq!(blocks[2].display, "和后文");
        assert_eq!(&text[blocks[2].segments[0].source.clone()], "和后文");
        assert_eq!(&text[blocks[1].segments[0].source.clone()], "$$E = mc^2$$");
    }

    #[test]
    fn a_triple_click_selects_its_paragraph_until_the_next_selection() {
        assert_eq!(paragraph_range("一\n\n二 $x$ 三\n\n四", 9), 5..16);
        assert_eq!(paragraph_range("只有一段", 3), 0..12);
        let mut shared = Shared {
            paragraph: Paragraph::Armed(0..4),
            ..Shared::default()
        };
        // The click's own clearing comes first and leaves it armed; the
        // selection the click makes turns it into the selection.
        shared.selection_event(&TextSelectionEvent::Cleared);
        shared.selection_event(&TextSelectionEvent::SelectionChanged(None));
        assert_eq!(shared.paragraph, Paragraph::Armed(0..4));
        shared.paragraph = match std::mem::take(&mut shared.paragraph) {
            Paragraph::Armed(range) => Paragraph::Selected(range),
            other => other,
        };
        // The next click clears it.
        shared.selection_event(&TextSelectionEvent::Cleared);
        assert_eq!(shared.paragraph, Paragraph::None);
    }

    #[test]
    fn closing_punctuation_after_a_formula_row_stays_on_the_row() {
        let text = "如下\n$$E$$\n，以及后文";
        let blocks = plan(text, None, inline);
        assert_eq!(blocks.len(), 3);
        assert!(blocks[1].row);
        assert!(blocks[1].display.ends_with('，'));
        assert_eq!(blocks[2].display, "以及后文");
        // The row copies as the formula and its punctuation.
        let all = source_range(&blocks[1].segments, 0..blocks[1].display.len()).unwrap();
        assert_eq!(&text[all], "$$E$$\n，");
    }

    #[test]
    fn copying_spans_formulas_and_paragraphs_as_markdown() {
        let text = "设 $\\sigma^2$ 为方差\n\n第二段";
        let blocks = plan(text, Some("讲解："), inline);
        let block = &blocks[0];
        // From inside the label to inside "方差": the label is not copied.
        let display_end = block.display.find("方差").unwrap() + "方".len();
        let source = source_range(&block.segments, 1..display_end).unwrap();
        assert_eq!(
            copied_text(text, &Copy::Plain, &[source]),
            "设 $\\sigma^2$ 为方\n"
        );
        // Only the label: nothing.
        let source = source_range(&block.segments, 0..3);
        assert_eq!(
            copied_text(text, &Copy::Plain, &source.into_iter().collect::<Vec<_>>()),
            ""
        );
        // The whole text keeps its blank line.
        let all = source_range(&block.segments, 0..block.display.len()).unwrap();
        assert_eq!(copied_text(text, &Copy::Plain, &[all]), format!("{text}\n"));
        // A list item copies as one.
        let (whole, second) = (0..6, 3..6);
        assert_eq!(copied_text("要点", &Copy::ListItem, &[whole]), "- 要点\n");
        assert_eq!(copied_text("要点", &Copy::ListItem, &[second]), "点\n");
    }

    #[test]
    fn a_selection_touching_a_formula_paints_all_of_it() {
        let text = "a $x$ b";
        let blocks = plan(text, None, inline);
        let segments = &blocks[0].segments;
        let holder = blocks[0].formulas[0].0.clone();
        let widened = widened(segments, holder.start + 1..holder.start + 2);
        assert!(widened.start < holder.start && widened.end > holder.end);
        // Prose alone is unchanged.
        assert_eq!(super::widened(segments, 0..1), 0..1);
    }

    #[test]
    fn find_matches_map_onto_the_display_text() {
        let text = "a $x$ b";
        let blocks = plan(text, None, inline);
        let segments = &blocks[0].segments;
        // A match in the prose maps one to one; one in the formula covers its place.
        assert_eq!(
            display_ranges(segments, &(6..7)),
            vec![blocks[0].display.len() - 1..blocks[0].display.len()]
        );
        let holder = display_ranges(segments, &(3..4));
        assert_eq!(holder.len(), 1);
        assert_eq!(
            &blocks[0].display[holder[0].clone()],
            "\u{200B}\u{A0}\u{A0}\u{200B}"
        );
        assert_eq!(
            display_index(segments, 6),
            Some(blocks[0].display.len() - 1)
        );
    }

    #[test]
    fn unreadable_latex_stays_in_the_prose() {
        let blocks = plan("a $\\bad$ b", None, |_, _| None);
        assert_eq!(blocks[0].display, "a $\\bad$ b");
        assert!(blocks[0].formulas.is_empty());
    }

    #[test]
    fn a_wide_inline_formula_moves_to_its_own_row() {
        assert!(fits_inline(px(100.), None, px(4.)));
        assert!(fits_inline(px(100.), Some(px(200.)), px(4.)));
        assert!(!fits_inline(px(199.), Some(px(200.)), px(4.)));
    }
}
