//! LaTeX formulas in notes: `$…$` inside a sentence and `$$…$$` on a line of
//! its own. Parsing and typesetting (RaTeX, monochrome SVG) are shared by the
//! desktop reader and the HTML export.

/// A run of note text: prose, or one formula and whether it stands alone.
#[derive(Debug, PartialEq, Eq)]
pub enum Piece<'a> {
    Text(&'a str),
    Math { latex: &'a str, display: bool },
}

/// Splits text at `$$…$$` and `$…$`. `\$` is a literal dollar, and an inline
/// formula must hug its delimiters and not be followed by a digit, so prices
/// such as "$5 and $10" stay prose. Unclosed delimiters are prose too.
pub fn pieces(text: &str) -> Vec<Piece<'_>> {
    let bytes = text.as_bytes();
    let mut pieces = Vec::new();
    let mut prose = 0;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            i += 2;
            continue;
        }
        if bytes[i] != b'$' {
            i += 1;
            continue;
        }
        let display = bytes.get(i + 1) == Some(&b'$');
        let open = i + if display { 2 } else { 1 };
        let Some(close) = closing(text, open, display) else {
            i += 1;
            continue;
        };
        let latex = &text[open..close];
        let inline_ok = display
            || (!latex.starts_with(char::is_whitespace)
                && !latex.ends_with(char::is_whitespace)
                && !text[close + 1..].starts_with(|c: char| c.is_ascii_digit()));
        if latex.trim().is_empty() || !inline_ok {
            i = open;
            continue;
        }
        if prose < i {
            pieces.push(Piece::Text(&text[prose..i]));
        }
        pieces.push(Piece::Math {
            latex: latex.trim(),
            display,
        });
        i = close + if display { 2 } else { 1 };
        prose = i;
    }
    if prose < text.len() {
        pieces.push(Piece::Text(&text[prose..]));
    }
    pieces
}

/// The byte offset of the delimiter closing a formula that starts at `from`.
fn closing(text: &str, from: usize, display: bool) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut i = from;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b'$' if !display => return Some(i),
            b'$' if bytes.get(i + 1) == Some(&b'$') => return Some(i),
            _ => i += 1,
        }
    }
    None
}

/// A typeset formula as monochrome SVG, with its box in em: `height` above
/// the baseline, `depth` below.
pub struct Typeset {
    pub svg: String,
    pub width: f32,
    pub height: f32,
    pub depth: f32,
}

/// Typesets one formula; `None` when RaTeX cannot parse it.
pub fn typeset(latex: &str, display: bool) -> Option<Typeset> {
    use ratex_layout::{LayoutOptions, layout, to_display_list};
    use ratex_types::{color::Color, math_style::MathStyle};
    let ast = ratex_parser::parser::parse(latex).ok()?;
    let style = if display {
        MathStyle::Display
    } else {
        MathStyle::Text
    };
    let laid_out = layout(
        &ast,
        &LayoutOptions::default()
            .with_style(style)
            .with_color(Color::BLACK),
    );
    let svg = ratex_svg::render_to_svg(
        &to_display_list(&laid_out),
        &ratex_svg::SvgOptions {
            font_size: 32.,
            padding: 0.,
            embed_glyphs: true,
            ..Default::default()
        },
    );
    Some(Typeset {
        svg,
        width: laid_out.width as f32,
        height: laid_out.height as f32,
        depth: laid_out.depth as f32,
    })
}

/// Glyphs inherit the surrounding text color, so formulas follow the theme,
/// and the drawing fills the em-sized box it is placed in.
fn current_color(svg: &str) -> String {
    let Some(rest) = svg.strip_prefix("<svg ") else {
        return svg.to_owned();
    };
    let end = rest.find('>').unwrap_or(rest.len());
    let attributes: Vec<_> = rest[..end]
        .split(' ')
        .filter(|attribute| !attribute.starts_with("width=") && !attribute.starts_with("height="))
        .collect();
    // RaTeX paints every path and rule in the layout color, black here.
    format!(
        "<svg fill=\"currentColor\" width=\"100%\" height=\"100%\" {}{}",
        attributes.join(" "),
        &rest[end..]
    )
    .replace("\"rgba(0,0,0,1)\"", "\"currentColor\"")
}

/// Note text as HTML: prose escaped, formulas as inline SVG sized in em so
/// they scale with the text. An unparseable formula keeps its source.
pub fn html(text: &str, escape: impl Fn(&str) -> String) -> String {
    let mut out = String::new();
    for piece in pieces(text) {
        match piece {
            Piece::Text(prose) => out.push_str(&escape(prose)),
            Piece::Math { latex, display } => match typeset(latex, display) {
                Some(formula) => {
                    let class = if display { "math display" } else { "math" };
                    out.push_str(&format!(
                        "<span class=\"{class}\" role=\"img\" aria-label=\"{}\" style=\"width:{:.3}em;height:{:.3}em;vertical-align:{:.3}em\">{}</span>",
                        escape(latex),
                        formula.width,
                        formula.height + formula.depth,
                        -formula.depth,
                        current_color(&formula.svg)
                    ));
                }
                None => {
                    let fence = if display { "$$" } else { "$" };
                    out.push_str(&escape(&format!("{fence}{latex}{fence}")));
                }
            },
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{Piece, html, pieces};

    #[test]
    fn splits_inline_and_display_formulas_and_keeps_prices_as_prose() {
        assert_eq!(
            pieces("面积 $S=\\pi r^2$，即$$\\int_a^b f(x)\\,dx$$。"),
            vec![
                Piece::Text("面积 "),
                Piece::Math {
                    latex: "S=\\pi r^2",
                    display: false
                },
                Piece::Text("，即"),
                Piece::Math {
                    latex: "\\int_a^b f(x)\\,dx",
                    display: true
                },
                Piece::Text("。"),
            ]
        );
        assert_eq!(
            pieces("costs $5 and $10"),
            vec![Piece::Text("costs $5 and $10")]
        );
        assert_eq!(pieces("\\$ 不是公式"), vec![Piece::Text("\\$ 不是公式")]);
        assert_eq!(pieces("未闭合 $x"), vec![Piece::Text("未闭合 $x")]);
    }

    #[test]
    fn html_draws_formulas_as_themed_svg() {
        let escape = |s: &str| s.replace('<', "&lt;");
        let out = html("当 $x<1$ 时 $\\frac{1}{2}$", escape);
        assert!(out.starts_with("当 "));
        assert_eq!(out.matches("<svg").count(), 2);
        assert!(out.contains("currentColor"));
        assert!(!out.contains("pt\""));
        assert!(!out.contains("rgba(0,0,0,1)"));
        assert!(out.contains("aria-label=\"x&lt;1\""));
    }
}
