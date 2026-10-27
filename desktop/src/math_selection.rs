//! Prose with typeset formulas in the window's text selection.
//!
//! A formula paragraph is laid out as wrapping tokens (words, CJK characters
//! and formula images), so it cannot be one `SelectableText`. This element
//! makes the whole paragraph one selection participant: each token records
//! its laid-out bounds and the byte range of the note text it shows, a drag
//! selects the tokens it sweeps over, and copying yields that span of the
//! original text, so a formula copies as its LaTeX source with its `$…$` or
//! `$$…$$` delimiters.
use gpui::{
    AnyElement, App, Bounds, ElementId, GlobalElementId, Hitbox, HitboxBehavior, Hsla,
    InspectorElementId, LayoutId, PaintQuad, Pixels, Point, SharedString, Window, prelude::*,
    transparent_black,
};
use gpui_base::{TextSelectionHandle, TextSelectionRegistration};
use std::{cell::RefCell, ops::Range, rc::Rc};

/// The tokens of one paragraph: the source range each one shows, and the
/// bounds it was laid out at in the current frame.
#[derive(Clone, Default)]
pub struct Tokens(Rc<RefCell<Vec<Token>>>);

struct Token {
    source: Range<usize>,
    bounds: Option<Bounds<Pixels>>,
}

impl Tokens {
    /// Wraps a token element that shows `source` of the paragraph's text.
    pub fn token(&self, source: Range<usize>, element: impl IntoElement) -> AnyElement {
        let mut tokens = self.0.borrow_mut();
        let index = tokens.len();
        tokens.push(Token {
            source,
            bounds: None,
        });
        Measured {
            child: element.into_any_element(),
            index,
            tokens: self.clone(),
        }
        .into_any_element()
    }
}

/// Lays out its child unchanged and records where it landed.
struct Measured {
    child: AnyElement,
    index: usize,
    tokens: Tokens,
}

impl IntoElement for Measured {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for Measured {
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
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        (self.child.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        if let Some(token) = self.tokens.0.borrow_mut().get_mut(self.index) {
            token.bounds = Some(bounds);
        }
        self.child.prepaint(window, cx);
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.child.paint(window, cx);
    }
}

/// A paragraph of tokens that takes part in the window's text selection.
pub struct SelectableTokens {
    id: ElementId,
    text: SharedString,
    tokens: Tokens,
    child: AnyElement,
    document_order: u64,
    selection_color: Hsla,
}

impl SelectableTokens {
    /// `child` lays out the tokens made with `tokens` for `text`.
    pub fn new(
        id: impl Into<ElementId>,
        text: SharedString,
        tokens: Tokens,
        child: impl IntoElement,
    ) -> Self {
        Self {
            id: id.into(),
            text,
            tokens,
            child: child.into_any_element(),
            document_order: 0,
            selection_color: transparent_black(),
        }
    }

    /// Places the paragraph in reading order among the note's other text.
    pub fn document_order(mut self, order: u64) -> Self {
        self.document_order = order;
        self
    }

    pub fn selection_color(mut self, color: Hsla) -> Self {
        self.selection_color = color;
        self
    }
}

impl IntoElement for SelectableTokens {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for SelectableTokens {
    type RequestLayoutState = TextSelectionHandle;
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
    ) -> (LayoutId, TextSelectionHandle) {
        let handle = window.with_element_state(
            global_id.expect("a selectable paragraph has a stable element id"),
            |retained: Option<TextSelectionHandle>, _| {
                let handle =
                    retained.unwrap_or_else(|| TextSelectionHandle::new(self.text.clone(), cx));
                (handle.clone(), handle)
            },
        );
        (self.child.request_layout(window, cx), handle)
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        handle: &mut TextSelectionHandle,
        window: &mut Window,
        cx: &mut App,
    ) -> Hitbox {
        self.child.prepaint(window, cx);
        let text_bounds = self
            .tokens
            .0
            .borrow()
            .iter()
            .filter_map(|token| token.bounds)
            .collect();
        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
        handle.register(
            TextSelectionRegistration::new(hitbox.clone(), bounds)
                .with_document_order(self.document_order)
                .with_text_bounds(text_bounds),
            window,
            cx,
        );
        hitbox
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        handle: &mut TextSelectionHandle,
        _: &mut Hitbox,
        window: &mut Window,
        cx: &mut App,
    ) {
        let points = handle
            .snapshot(cx)
            .and_then(|snapshot| snapshot.window_points());
        if let Some(points) = points {
            let tokens = self.tokens.0.borrow();
            let laid_out: Vec<(Range<usize>, Bounds<Pixels>)> = tokens
                .iter()
                .filter_map(|token| Some((token.source.clone(), token.bounds?)))
                .collect();
            let selected = selected_tokens(
                laid_out.iter().map(|(_, bounds)| *bounds),
                points.anchor(),
                points.cursor(),
            );
            let highlights = line_highlights(laid_out.iter().map(|(_, bounds)| *bounds));
            for &index in &selected {
                window.paint_quad(PaintQuad {
                    bounds: highlights[index],
                    background: self.selection_color.into(),
                    corner_radii: Default::default(),
                    border_widths: Default::default(),
                    border_color: transparent_black(),
                    border_style: Default::default(),
                });
            }
            let ranges: Vec<Range<usize>> = selected
                .iter()
                .map(|&index| laid_out[index].0.clone())
                .collect();
            handle.set_fallback_copy_text(copied_text(&self.text, &ranges), cx);
        } else {
            // Select-all and other participant-wide selections copy all of it.
            handle.set_fallback_copy_text(self.text.to_string(), cx);
        }
        self.child.paint(window, cx);
    }
}

/// The indices of the tokens a selection from `anchor` to `cursor` covers,
/// in reading order: whole lines between the endpoints, and from or up to
/// the endpoint on their own lines (as text selection treats characters).
pub fn selected_tokens(
    bounds: impl IntoIterator<Item = Bounds<Pixels>>,
    anchor: Point<Pixels>,
    cursor: Point<Pixels>,
) -> Vec<usize> {
    let (top, bottom) = if (anchor.y, anchor.x) <= (cursor.y, cursor.x) {
        (anchor, cursor)
    } else {
        (cursor, anchor)
    };
    bounds
        .into_iter()
        .enumerate()
        .filter(|(_, bounds)| {
            let on_line = |point: Point<Pixels>| {
                point.y >= bounds.origin.y && point.y < bounds.origin.y + bounds.size.height
            };
            let middle = bounds.origin.x + bounds.size.width / 2.;
            if bounds.origin.y + bounds.size.height <= top.y.min(bottom.y)
                || bounds.origin.y > top.y.max(bottom.y)
            {
                return false;
            }
            match (on_line(top), on_line(bottom)) {
                (true, true) => middle >= top.x.min(bottom.x) && middle <= top.x.max(bottom.x),
                (true, false) => middle >= top.x,
                (false, true) => middle <= bottom.x,
                (false, false) => true,
            }
        })
        .map(|(index, _)| index)
        .collect()
}

/// Where each token's selection is painted: its own width, across the full
/// height of its line, so words and formulas on one line highlight as one band.
pub fn line_highlights(bounds: impl IntoIterator<Item = Bounds<Pixels>>) -> Vec<Bounds<Pixels>> {
    let bounds: Vec<Bounds<Pixels>> = bounds.into_iter().collect();
    let mut highlights = Vec::with_capacity(bounds.len());
    let mut line_start = 0;
    for index in 0..=bounds.len() {
        // Tokens flow left to right; a token further left (or below the
        // line) starts the next line.
        let wraps = index == bounds.len()
            || (index > line_start
                && (bounds[index].origin.x < bounds[index - 1].origin.x
                    || bounds[index].origin.y >= bounds[index - 1].bottom()));
        if wraps {
            let line = &bounds[line_start..index];
            let top = line
                .iter()
                .map(|b| b.origin.y)
                .fold(Pixels::MAX, Pixels::min);
            let bottom = line
                .iter()
                .map(|b| b.bottom())
                .fold(Pixels::MIN, Pixels::max);
            highlights.extend(line.iter().map(|b| {
                Bounds::from_corners(Point::new(b.origin.x, top), Point::new(b.right(), bottom))
            }));
            line_start = index;
        }
    }
    highlights
}

/// The note text behind the selected tokens: everything from the first
/// selected token's source to the last one's, so formulas keep their
/// delimiters and the spaces and line breaks between tokens are kept.
pub fn copied_text(text: &str, ranges: &[Range<usize>]) -> String {
    let start = ranges.iter().map(|range| range.start).min();
    let end = ranges.iter().map(|range| range.end).max();
    match (start, end) {
        (Some(start), Some(end)) if start < end => {
            text.get(start..end).map(str::to_owned).unwrap_or_default()
        }
        _ => String::new(),
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{point, px, size};

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
        let sources = piece_sources(text);
        assert_eq!(
            source_of(text, &sources),
            vec![
                "收益率 ",
                "$r = \\frac{P_1}{P_0}$",
                "，以及\n",
                "$$\nE = mc^2\n$$",
                "\n结束"
            ]
        );
    }

    #[test]
    fn adjacent_formulas_each_keep_their_own_source() {
        let text = "$a$$$b$$";
        let sources = piece_sources(text);
        assert_eq!(source_of(text, &sources), vec!["$a$", "$$b$$"]);
        let text = "$x$ and $y$";
        assert_eq!(
            source_of(text, &piece_sources(text)),
            vec!["$x$", " and ", "$y$"]
        );
    }

    #[test]
    fn copying_a_span_with_a_formula_yields_its_latex_source() {
        let text = "设 $\\sigma_p^2 = w^T \\Sigma w$ 为方差";
        let sources = piece_sources(text);
        // "设 ", the formula, " 为" selected: the span between them is kept.
        let ranges = [sources[0].0.clone(), sources[1].0.clone()];
        assert_eq!(
            copied_text(text, &ranges),
            "设 $\\sigma_p^2 = w^T \\Sigma w$"
        );
        assert_eq!(copied_text(text, &[]), "");
        let all: Vec<_> = sources.iter().map(|(range, _)| range.clone()).collect();
        assert_eq!(copied_text(text, &all), text);
    }

    #[test]
    fn a_line_highlights_as_one_band() {
        let token = |x: f32, y: f32, h: f32| Bounds::new(point(px(x), px(y)), size(px(10.), px(h)));
        // A word, a taller formula a little higher, then a word on the next line.
        let bands = line_highlights([token(0., 4., 20.), token(10., 0., 30.), token(0., 30., 20.)]);
        assert_eq!(
            bands[0],
            Bounds::from_corners(point(px(0.), px(0.)), point(px(10.), px(30.)))
        );
        assert_eq!(
            bands[1],
            Bounds::from_corners(point(px(10.), px(0.)), point(px(20.), px(30.)))
        );
        assert_eq!(bands[2], token(0., 30., 20.));
    }

    #[test]
    fn a_drag_covers_tokens_like_characters_across_lines() {
        let token = |x: f32, y: f32| Bounds::new(point(px(x), px(y)), size(px(10.), px(20.)));
        // Two lines of three tokens.
        let tokens = [
            token(0., 0.),
            token(10., 0.),
            token(20., 0.),
            token(0., 20.),
            token(10., 20.),
            token(20., 20.),
        ];
        // Within one line: only the tokens between the endpoints.
        assert_eq!(
            selected_tokens(tokens, point(px(8.), px(5.)), point(px(22.), px(5.))),
            vec![1]
        );
        // From the second token of line one to the first token of line two,
        // dragged backwards.
        assert_eq!(
            selected_tokens(tokens, point(px(12.), px(25.)), point(px(8.), px(5.))),
            vec![1, 2, 3]
        );
        // Endpoints above and below the paragraph cover all of it.
        assert_eq!(
            selected_tokens(tokens, point(px(0.), px(-10.)), point(px(0.), px(60.))),
            vec![0, 1, 2, 3, 4, 5]
        );
    }
}
