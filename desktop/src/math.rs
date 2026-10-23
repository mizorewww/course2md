//! LaTeX formulas in notes: `$…$` inside a sentence and `$$…$$` on a line of
//! its own, typeset by RaTeX into monochrome SVG. The SVG is served through the
//! app's asset source and drawn as a tinted mask, so formulas follow the theme
//! and stay sharp at every text scale.
use gpui::{prelude::*, *};
use std::{
    borrow::Cow,
    collections::HashMap,
    sync::{Arc, LazyLock, Mutex},
};

pub use course2md::math::{Piece, has_math, pieces};

/// A typeset formula: its asset path and its box in em, with the baseline
/// `height` below the top.
#[derive(Clone, Debug)]
pub struct Formula {
    path: SharedString,
    width: f32,
    height: f32,
    depth: f32,
}

const ASSET_PREFIX: &str = "math/";

/// Formulas kept typeset; notes rarely hold more than a few hundred.
const CACHE_LIMIT: usize = 4096;

struct Cache {
    formulas: HashMap<(String, bool), Option<Formula>>,
    svgs: HashMap<SharedString, Arc<[u8]>>,
    /// Never reused, so a path cannot name another formula after a reset.
    next_path: u64,
}

static CACHE: LazyLock<Mutex<Cache>> = LazyLock::new(|| {
    Mutex::new(Cache {
        formulas: HashMap::new(),
        svgs: HashMap::new(),
        next_path: 0,
    })
});

/// The typeset formula, or `None` when RaTeX cannot parse it; the note then
/// shows the source. Results are cached; `prepare` warms the cache off the UI thread.
pub fn formula(latex: &str, display: bool) -> Option<Formula> {
    let key = (latex.to_owned(), display);
    if let Some(found) = CACHE.lock().ok()?.formulas.get(&key) {
        return found.clone();
    }
    let typeset = typeset(latex, display);
    let mut cache = CACHE.lock().ok()?;
    if cache.formulas.len() >= CACHE_LIMIT {
        // Start over rather than track use: a formula still on screen is
        // typeset again under a new path the next time it is drawn.
        cache.formulas.clear();
        cache.svgs.clear();
    }
    let formula = typeset.map(|(svg, width, height, depth)| {
        let path = SharedString::from(format!("{ASSET_PREFIX}{}.svg", cache.next_path));
        cache.next_path += 1;
        cache.svgs.insert(path.clone(), svg.into_bytes().into());
        Formula {
            path,
            width,
            height,
            depth,
        }
    });
    cache.formulas.insert(key, formula.clone());
    formula
}

/// Typesets every formula in `texts`, so opening a note does not do it while drawing.
pub fn prepare<'a>(texts: impl IntoIterator<Item = &'a str>) {
    for text in texts {
        for piece in pieces(text) {
            if let Piece::Math { latex, display } = piece {
                formula(latex, display);
            }
        }
    }
}

fn typeset(latex: &str, display: bool) -> Option<(String, f32, f32, f32)> {
    let formula = course2md::math::typeset(latex, display)?;
    Some((formula.svg, formula.width, formula.height, formula.depth))
}

/// The SVG behind a formula's asset path.
pub fn asset(path: &str) -> Option<Cow<'static, [u8]>> {
    if !path.starts_with(ASSET_PREFIX) {
        return None;
    }
    let cache = CACHE.lock().ok()?;
    cache.svgs.get(path).map(|svg| Cow::Owned(svg.to_vec()))
}

impl Formula {
    /// The formula at `em` text size in `ink`. GPUI paints an SVG only in its
    /// own text color, which it does not inherit from the paragraph.
    /// Inline formulas sit on the text baseline: a line's content box centers
    /// near 0.35 em above the baseline, the formula's own center at
    /// (height − depth) / 2.
    pub fn element(&self, em: Rems, display: bool, ink: Hsla) -> AnyElement {
        let svg = svg()
            .path(self.path.clone())
            .text_color(ink)
            .flex_shrink_0()
            .w(em * self.width)
            .h(em * (self.height + self.depth));
        if display {
            svg.into_any_element()
        } else {
            svg.relative()
                .top(em * (0.35 - (self.height - self.depth) / 2.))
                .into_any_element()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{CACHE, CACHE_LIMIT, asset, formula};

    #[test]
    fn the_formula_cache_is_bounded_and_never_reuses_a_path() {
        let first = formula("a_{0}", false).unwrap();
        for n in 1..=CACHE_LIMIT {
            formula(&format!("a_{{{n}}}"), false);
        }
        {
            let cache = CACHE.lock().unwrap();
            assert!(cache.formulas.len() <= CACHE_LIMIT);
            assert!(cache.svgs.len() <= CACHE_LIMIT);
        }
        // CACHE_LIMIT later insertions reset the cache at least once.
        assert!(asset(&first.path).is_none());
        let again = formula("a_{0}", false).unwrap();
        assert_ne!(again.path, first.path);
        assert!(asset(&again.path).is_some());
    }
}
