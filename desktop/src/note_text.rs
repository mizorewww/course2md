//! A note as Markdown for Kit's `TextView`: prose escaped so it stays as written,
//! formulas where `math::pieces` finds them, typeset by two small plugins, and
//! figures as image paragraphs a third plugin hands to the reader.
//!
//! One document holds a whole note, so selection, find and scrolling cross its
//! paragraphs inside one virtualized list. Copying yields its source: a formula
//! with its delimiters, a list item as `- item`, paragraphs a blank line apart.
use crate::math::{self, Formula, Piece};
use gpui::{prelude::*, *};
use gpui_base::text::markdown_ast::Node;
use gpui_base::{
    h_flex,
    text::{
        InlineElement, InlineRenderContext, MarkdownExtensions, MarkdownNode, MarkdownParseContext,
        MarkdownPlugin, TextViewStyle,
    },
};
use gpui_kit::component::scroll::{Scrollbar, ScrollbarMode};
use gpui_kit_material::{color::ColorRole, prelude::*};
use std::{
    ops::Range,
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
};

/// A link with this text is the control that plays the source from a section's time.
pub const SOURCE_LINK_LABEL: &str = "▶";

/// The widest one formula image is drawn, in device pixels: GPUI rasterises an SVG
/// at twice that size and draws none wider than 8192 pixels, so a wider formula is
/// drawn as several tiles side by side.
const MAX_FORMULA_PIXELS: f32 = 4000.;

/// Closing punctuation stays in the sentence: a display formula it follows is laid
/// out inline, so a line never starts with it.
fn closes_line(c: char) -> bool {
    "，。、；：！？）」』”’》〉】,.;:!?)]}".contains(c)
}

/// Text as it reads once Markdown has parsed it. `dollars` escapes `$` too, for text
/// that also holds a formula; elsewhere a stray pair stays literal through [`formula_of`].
/// Only what could start markup is escaped, because whole paragraphs copy as their source.
fn escape(text: &str, dollars: bool, out: &mut String) {
    let mut lines = text.split('\n').peekable();
    while let Some(line) = lines.next() {
        let line = line.trim();
        // What opens a block at a line's start. A numbered list's delimiter is after its digits.
        let digits = line.bytes().take_while(u8::is_ascii_digit).count();
        let (number, rest) = line.split_at(digits);
        out.push_str(number);
        if rest.starts_with(['#', '>', '=', '|'])
            || rest.starts_with("- ")
            || rest.starts_with("+ ")
            || rest.starts_with("---")
            || (digits > 0 && rest.starts_with(['.', ')']))
        {
            out.push('\\');
        }
        escape_inline(rest, dollars, out);
        // Two spaces hold the source's line break.
        if lines.peek().is_some() {
            out.push_str("  \n");
        }
    }
}

fn escape_inline(line: &str, dollars: bool, out: &mut String) {
    let mut chars = line.chars().peekable();
    let mut before = None;
    while let Some(c) = chars.next() {
        let next = chars.peek().copied();
        let markup = match c {
            // `\$` already is a literal dollar, in the source and in Markdown.
            '\\' => next != Some('$'),
            '`' | '[' | ']' | '~' => true,
            '$' => dollars && before != Some('\\'),
            '*' => {
                !(next.is_none_or(char::is_whitespace) && before.is_none_or(char::is_whitespace))
            }
            '_' => {
                !(next.is_some_and(char::is_alphanumeric)
                    && before.is_some_and(char::is_alphanumeric))
            }
            '<' => next.is_some_and(|next| next.is_alphabetic() || "/!?".contains(next)),
            '&' => next.is_some_and(|next| next.is_alphanumeric() || next == '#'),
            _ => false,
        };
        if markup {
            out.push('\\');
        }
        out.push(c);
        before = Some(c);
    }
}

/// `$…$`. Inside it a backslash is no escape, so a `\$` would end it: then `$$…$$`.
fn inline_formula(latex: &str, display: bool, out: &mut String) {
    let fence = if display || latex.contains('$') {
        "$$"
    } else {
        "$"
    };
    out.push_str(fence);
    out.push_str(latex);
    out.push_str(fence);
}

/// `text` as Markdown blocks, each one top-level block: its paragraphs and display
/// formulas. A formula Markdown could not hold stays prose.
pub fn blocks(text: &str) -> Vec<String> {
    let pieces = math::pieces(text);
    let has_formula = pieces
        .iter()
        .any(|piece| matches!(piece, Piece::Math { .. }));
    let mut blocks = Vec::new();
    let mut paragraph = String::new();
    let mut pieces = pieces.into_iter().peekable();
    while let Some(piece) = pieces.next() {
        match piece {
            Piece::Text(prose) => {
                let mut parts = prose.split("\n\n");
                if let Some(first) = parts.next() {
                    escape(first, has_formula, &mut paragraph);
                }
                for part in parts {
                    flush(&mut paragraph, &mut blocks);
                    escape(part.trim_start(), has_formula, &mut paragraph);
                }
            }
            Piece::Math { latex, .. } if latex.contains("\n\n") => {
                escape(latex, true, &mut paragraph);
            }
            Piece::Math { latex, display } => {
                let in_sentence = matches!(
                    pieces.peek(),
                    Some(Piece::Text(next)) if next.trim_start().starts_with(closes_line)
                );
                if display && !in_sentence {
                    flush(&mut paragraph, &mut blocks);
                    blocks.push(format!("$$\n{latex}\n$$"));
                } else {
                    inline_formula(latex, display, &mut paragraph);
                }
            }
        }
    }
    flush(&mut paragraph, &mut blocks);
    blocks
}

fn flush(paragraph: &mut String, blocks: &mut Vec<String>) {
    let block = paragraph.trim_end();
    if !block.is_empty() {
        blocks.push(block.to_owned());
    }
    paragraph.clear();
}

/// `text` as one Markdown document.
pub fn markdown(text: &str) -> String {
    blocks(text).join("\n\n")
}

/// `text` on one line, for a list item or a part of an excerpt: its formulas stay in it.
pub fn inline_markdown(text: &str) -> String {
    let flat = text.replace('\n', " ");
    let pieces = math::pieces(&flat);
    let has_formula = pieces
        .iter()
        .any(|piece| matches!(piece, Piece::Math { .. }));
    let mut out = String::new();
    for piece in pieces {
        match piece {
            Piece::Text(prose) => escape_inline(prose, has_formula, &mut out),
            Piece::Math { latex, .. } => inline_formula(latex, false, &mut out),
        }
    }
    out
}

/// A heading or title: never a formula, always the text as written.
pub fn plain_inline_markdown(text: &str) -> String {
    let mut out = String::new();
    escape_inline(&text.replace('\n', " "), true, &mut out);
    out
}

/// The formula a math node's source really is, by the note's own rule: Markdown pairs
/// any two dollars (`$5 and $10`), `math::pieces` does not.
fn formula_of(source: &str, display: bool) -> Option<Formula> {
    let [Piece::Math { latex, .. }] = math::pieces(source)[..] else {
        return None;
    };
    math::formula(latex, display)
}

/// A formula at `em`, drawn in `ink`: one SVG, or tiles when too wide for one.
fn formula_image(formula: &Formula, em: Pixels, ink: Hsla, window: &Window) -> AnyElement {
    let width = em * formula.width;
    let height = em * (formula.height + formula.depth);
    let tiles =
        ((f32::from(width) * window.scale_factor() / MAX_FORMULA_PIXELS).ceil() as usize).max(1);
    h_flex()
        .flex_none()
        .items_start()
        .children((0..tiles).filter_map(|tile| {
            let path = math::tile(&formula.path, tile, tiles)?;
            Some(
                svg()
                    .path(path)
                    .flex_none()
                    .w(width / tiles as f32)
                    .h(height)
                    .text_color(ink),
            )
        }))
        .into_any_element()
}

/// A row as wide as its parent that scrolls a wider formula sideways, its scrollbar
/// shown so it never looks complete when it is not.
fn scrolling_row(
    id: ElementId,
    image: AnyElement,
    overflows: bool,
    window: &mut Window,
    cx: &mut App,
) -> Div {
    let scroll = window
        .use_keyed_state(
            ElementId::NamedChild(Arc::new(id.clone()), "scroll".into()),
            cx,
            |_, _| ScrollHandle::new(),
        )
        .read(cx)
        .clone();
    div()
        .relative()
        .w_full()
        .min_w_0()
        .child(
            div()
                .id(id)
                .w_full()
                .flex()
                .overflow_x_scroll()
                // A vertical wheel keeps scrolling the note around it.
                .restrict_scroll_to_axis()
                .track_scroll(&scroll)
                .when(overflows, |row| row.pb(px(12.)))
                .child(div().flex_none().mx_auto().child(image)),
        )
        .when(overflows, |row| {
            row.child(Scrollbar::horizontal(&scroll).mode(ScrollbarMode::Always))
        })
}

/// `$…$` in a sentence, or `$$…$$` where the sentence goes on after it.
struct InlineFormula {
    line_width: Arc<AtomicU32>,
}

impl MarkdownPlugin for InlineFormula {
    fn name(&self) -> &str {
        "formula"
    }

    fn parse(&self, node: &Node, cx: &MarkdownParseContext<'_>) -> Option<MarkdownNode> {
        let Node::InlineMath(math) = node else {
            return None;
        };
        let source = cx.node_source(node)?;
        let formula = formula_of(source, source.starts_with("$$"))?;
        Some(MarkdownNode::new("formula", formula).text(math.value.clone()))
    }

    fn render_inline(
        &self,
        node: &MarkdownNode,
        context: &InlineRenderContext,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<InlineElement> {
        let formula = node.data::<Formula>()?;
        let em = context.font_size();
        let image = formula_image(formula, em, context.text_style().color, window);
        let line = px(self.line_width.load(Ordering::Relaxed) as f32);
        // A box as tall as the line, so a selection over it has no notches and a formula
        // without depth keeps its baseline inside the box, as `TextView` requires. The
        // text's baseline sits about 0.35 em below the middle of the line.
        let ascent = em * formula.height;
        let above = (context.line_height() / 2. + em * 0.35 - ascent).max(px(0.));
        let height = (above + em * (formula.height + formula.depth)).max(context.line_height());
        let content = if line > px(0.) && em * formula.width > line {
            // Wider than any line: it keeps its size and scrolls in a row of its own.
            let id = (
                "formula-row",
                node.source_range().map_or(0, |range| range.start),
            );
            scrolling_row(id.into(), image, true, window, cx)
                .w(line)
                .into_any_element()
        } else {
            image
        };
        Some(
            InlineElement::new(
                div()
                    .h(px(f32::from(height).ceil()))
                    .pt(above)
                    .child(content),
            )
            .with_baseline(above + ascent),
        )
    }
}

/// `$$…$$` on its own.
struct DisplayFormula;

impl MarkdownPlugin for DisplayFormula {
    fn is_block(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "display-formula"
    }

    fn parse(&self, node: &Node, cx: &MarkdownParseContext<'_>) -> Option<MarkdownNode> {
        let Node::Math(math) = node else {
            return None;
        };
        let formula = math::formula(&math.value, true)?;
        let source = cx.node_source(node).unwrap_or_default();
        Some(
            MarkdownNode::new("display-formula", formula)
                .text(math.value.clone())
                .markdown(source.to_owned()),
        )
    }

    fn render(&self, node: &MarkdownNode, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let id = (
            "formula",
            node.source_range().map_or(0, |range| range.start),
        );
        let Some(formula) = node.data::<Formula>() else {
            return div().into_any_element();
        };
        let style = window.text_style();
        let em = style.font_size.to_pixels(window.rem_size());
        let image = formula_image(formula, em, style.color, window);
        let overflows = window
            .use_keyed_state(ElementId::from(id), cx, |_, _| ScrollHandle::new())
            .read(cx)
            .max_offset()
            .x
            > px(0.);
        scrolling_row(id.into(), image, overflows, window, cx)
            .py(rems(0.5))
            .into_any_element()
    }
}

/// What a reader paragraph draws, keyed by its image's address.
pub type RenderBlock = Arc<dyn Fn(&str, &mut Window, &mut App) -> AnyElement + Send + Sync>;

/// A paragraph that is one image: the reader draws it. Its source is real Markdown,
/// which `notes::without_image_references` removes from a copy.
struct ReaderBlock {
    render: RenderBlock,
}

impl MarkdownPlugin for ReaderBlock {
    fn is_block(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "reader-block"
    }

    fn parse(&self, node: &Node, _: &MarkdownParseContext<'_>) -> Option<MarkdownNode> {
        let Node::Paragraph(paragraph) = node else {
            return None;
        };
        let [Node::Image(image)] = &paragraph.children[..] else {
            return None;
        };
        Some(MarkdownNode::new("reader-block", image.url.clone()))
    }

    fn render(&self, node: &MarkdownNode, window: &mut Window, cx: &mut App) -> impl IntoElement {
        match node.data::<String>() {
            Some(address) => (self.render)(address, window, cx),
            None => div().into_any_element(),
        }
    }
}

/// `[▶](url)` after a section's time: the control that plays the source from there.
struct SourceLink;

impl MarkdownPlugin for SourceLink {
    fn name(&self) -> &str {
        "source-link"
    }

    fn parse(&self, node: &Node, _: &MarkdownParseContext<'_>) -> Option<MarkdownNode> {
        let Node::Link(link) = node else {
            return None;
        };
        let [Node::Text(text)] = &link.children[..] else {
            return None;
        };
        (text.value == SOURCE_LINK_LABEL)
            .then(|| MarkdownNode::new("source-link", link.url.clone()).text(SOURCE_LINK_LABEL))
    }

    fn render_inline(
        &self,
        node: &MarkdownNode,
        context: &InlineRenderContext,
        _: &mut Window,
        _: &mut App,
    ) -> Option<InlineElement> {
        let url = node.data::<String>()?.clone();
        let at = node.source_range().map_or(0, |range| range.start);
        let label = course2md::tr!("从此处观看");
        // Centred on the text beside it: a Latin x-height above the baseline.
        let middle =
            CONTROL_HEIGHT.to_pixels(context.rem_size()) * 0.4 + context.font_size() * 0.35;
        Some(
            InlineElement::new(
                MaterialTooltip::plain(("source-link-tip", at), label).trigger(
                    IconButton::new(("source-link", at), crate::symbols::play_arrow(), label)
                        .button_size(ButtonSize::ExtraSmall)
                        .on_click(move |_, _, cx| cx.open_url(&url)),
                ),
            )
            .with_baseline(middle),
        )
    }
}

/// The plugins of note text. `line_width` is the note's text column, in pixels.
pub fn extensions(line_width: Arc<AtomicU32>) -> MarkdownExtensions {
    MarkdownExtensions::default()
        .parser_revision(1)
        .plugin(InlineFormula { line_width })
        .plugin(DisplayFormula)
}

/// [`extensions`], plus what only a whole note holds.
pub fn note_extensions(line_width: Arc<AtomicU32>, render: RenderBlock) -> MarkdownExtensions {
    extensions(line_width)
        .parser_revision(2)
        .plugin(ReaderBlock { render })
        .plugin(SourceLink)
}

/// How note text reads: the kit's type roles on the note's surface.
pub fn note_style(cx: &App) -> TextViewStyle {
    TextViewStyle::from_theme(&gpui_base::Theme::global(cx))
        .with_foreground(color(ColorRole::ON_SURFACE).into())
        .with_muted_foreground(color(ColorRole::ON_SURFACE_VARIANT).into())
        .with_link(color(ColorRole::PRIMARY).into())
        .with_selection(color(ColorRole::READING_SELECTION).into())
        .with_border(color(ColorRole::OUTLINE_VARIANT).into())
        .with_paragraph_gap(rems(1.))
        .with_heading(|level| {
            let role = match level {
                1 => TypeRole::HeadlineMedium,
                2 => TypeRole::TitleLarge,
                _ => TypeRole::TitleSmall,
            };
            let scale = role.scale();
            StyleRefinement::default()
                .text_size(scale.font_size)
                .line_height(scale.line_height)
                .font_weight(FontWeight::MEDIUM)
        })
}

/// `text` as one paragraph of Markdown with `marks`, byte ranges of its prose, highlighted.
pub fn marked_markdown(text: &str, marks: &[Range<usize>], highlight: Rgba) -> String {
    let formulas: Vec<_> = piece_sources(text)
        .into_iter()
        .filter(|(_, piece)| matches!(piece, Piece::Math { .. }))
        .map(|(range, _)| range)
        .collect();
    let mut out = String::new();
    let mut end = 0;
    for mark in marks {
        // A mark inside a formula would end it.
        if !formulas
            .iter()
            .any(|formula| formula.start < mark.end && mark.start < formula.end)
        {
            out.push_str(&inline_markdown(&text[end..mark.start]));
            out.push_str(&format!(
                "<mark color=\"#{:06x}\">{}</mark>",
                u32::from(highlight) >> 8,
                inline_markdown(&text[mark.clone()]),
            ));
            end = mark.end;
        }
    }
    out.push_str(&inline_markdown(&text[end..]));
    out
}

/// What a selection of a note copies: its source, without the figures and the blank
/// lines they leave.
pub fn copied_text(selected: &str) -> String {
    let mut text = crate::notes::without_image_references(selected);
    while text.contains("\n\n\n") {
        text = text.replace("\n\n\n", "\n\n");
    }
    text.trim().to_owned()
}

/// The byte ranges `pieces` cover in `text`: prose as written, and a formula
/// together with its delimiters (and any space inside them).
pub fn piece_sources(text: &str) -> Vec<(Range<usize>, Piece<'_>)> {
    let offset = |slice: &str| slice.as_ptr() as usize - text.as_ptr() as usize;
    let mut position = 0;
    math::pieces(text)
        .into_iter()
        .map(|piece| {
            let end = match &piece {
                Piece::Text(prose) => offset(prose) + prose.len(),
                // `pieces` trims the LaTeX inside its delimiters; the source
                // runs on through that space and the closing delimiter.
                Piece::Math { latex, display } => {
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

    fn source_of<'a>(text: &'a str) -> Vec<&'a str> {
        piece_sources(text)
            .iter()
            .map(|(range, _)| &text[range.clone()])
            .collect()
    }

    #[test]
    fn formulas_keep_their_delimiters_in_the_source() {
        let text = "收益率 $r = \\frac{P_1}{P_0}$，以及\n$$\nE = mc^2\n$$\n结束";
        assert_eq!(
            source_of(text),
            vec![
                "收益率 ",
                "$r = \\frac{P_1}{P_0}$",
                "，以及\n",
                "$$\nE = mc^2\n$$",
                "\n结束"
            ]
        );
        assert_eq!(source_of("$a$$$b$$"), vec!["$a$", "$$b$$"]);
    }

    #[test]
    fn prose_stays_as_written_and_only_markup_is_escaped() {
        assert_eq!(
            markdown("snake_case and 2 * 3 * 4"),
            "snake_case and 2 * 3 * 4"
        );
        assert_eq!(
            markdown("*强调* [链接](x) `代码` <b>粗</b>"),
            "\\*强调\\* \\[链接\\](x) \\`代码\\` \\<b>粗\\</b>"
        );
        assert_eq!(markdown("# 不是标题"), "\\# 不是标题");
        assert_eq!(markdown("- 不是列表"), "\\- 不是列表");
        assert_eq!(markdown("1. 不是列表"), "1\\. 不是列表");
        assert_eq!(markdown("> 不是引用"), "\\> 不是引用");
        assert_eq!(markdown("a & b &amp; c"), "a & b \\&amp; c");
        // A line break stays one; a blank line starts a paragraph.
        assert_eq!(blocks("一\n二\n\n三"), ["一  \n二", "三"]);
    }

    #[test]
    fn dollars_that_are_no_formula_stay_prose() {
        // `math::pieces` says no formula: left as written, the plugin declines it.
        assert_eq!(markdown("$5 and $10"), "$5 and $10");
        assert!(formula_of("$5 and $", false).is_none());
        // Beside a real formula, Markdown must not pair them differently.
        assert_eq!(markdown("$5 and $10 with $x$"), "\\$5 and \\$10 with $x$");
        assert_eq!(markdown("\\$5 and $x$"), "\\$5 and $x$");
    }

    #[test]
    fn a_display_formula_is_a_block_unless_the_sentence_goes_on() {
        assert_eq!(
            blocks("如下：\n$$E = mc^2$$\n 和后文"),
            ["如下：", "$$\nE = mc^2\n$$", "和后文"]
        );
        // Closing punctuation never starts a line.
        assert_eq!(
            blocks("如下\n$$E$$\n，以及后文"),
            ["如下  \n$$E$$  \n，以及后文"]
        );
        // A formula holding a dollar cannot use one as its fence.
        assert_eq!(blocks("设 $a\\$b$ 为"), ["设 $$a\\$b$$ 为"]);
    }

    #[test]
    fn a_copy_leaves_figures_behind() {
        assert_eq!(
            copied_text("一段\n\n![](figure:5)\n\n### 01:00\n\n![](figure:9)"),
            "一段\n\n### 01:00"
        );
    }

    #[test]
    fn a_list_item_keeps_its_formula_inline() {
        assert_eq!(inline_markdown("要点 $x$\n和 $$y$$"), "要点 $x$ 和 $y$");
    }

    #[test]
    fn marks_highlight_prose_and_leave_formulas_whole() {
        let highlight = gpui::rgb(0xffe082);
        let text = "a match $match$ *b*";
        let marks = [2..7, 9..14];
        assert_eq!(
            marked_markdown(text, &marks, highlight),
            "a <mark color=\"#ffe082\">match</mark> $match$ \\*b\\*"
        );
    }
}
