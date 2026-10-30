//! LaTeX formulas in notes: `$…$` inside a sentence and `$$…$$` on a line of
//! its own, typeset by RaTeX into monochrome SVG. The SVG is served through the
//! app's asset source and drawn as a tinted mask, so formulas follow the theme
//! and stay sharp at every text scale.
use gpui::*;
use std::{
    borrow::Cow,
    collections::HashMap,
    sync::{Arc, LazyLock, Mutex},
};

pub use course2md::math::{Piece, pieces};

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

impl Cache {
    /// Starts over rather than track use: a formula still on screen is
    /// typeset again under a new path the next time it is drawn.
    fn reset_if_full(&mut self) {
        if self.formulas.len() >= CACHE_LIMIT || self.svgs.len() >= CACHE_LIMIT {
            self.formulas.clear();
            self.svgs.clear();
        }
    }
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
    cache.reset_if_full();
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

/// The asset path of slice `index` of `count` equal vertical slices of the
/// formula at `path`, so a formula too wide for one image can be drawn as
/// several. `None` once the cache has been reset; the next frame typesets
/// the formula again.
pub fn tile(path: &str, index: usize, count: usize) -> Option<SharedString> {
    if count <= 1 {
        return Some(SharedString::from(path.to_owned()));
    }
    let tile = SharedString::from(format!(
        "{}-{index}of{count}.svg",
        path.trim_end_matches(".svg")
    ));
    let mut cache = CACHE.lock().ok()?;
    if !cache.svgs.contains_key(&tile) {
        let svg = std::str::from_utf8(cache.svgs.get(path)?).ok()?;
        let sliced = slice_svg(svg, index, count)?;
        cache.reset_if_full();
        cache.svgs.insert(tile.clone(), sliced.into_bytes().into());
    }
    Some(tile)
}

/// The same drawing seen through slice `index` of `count` of its viewBox,
/// with its width narrowed to match.
fn slice_svg(svg: &str, index: usize, count: usize) -> Option<String> {
    let header_end = svg.find('>')?;
    let header = &svg[..header_end];
    let attribute = |name: &str| {
        let start = header.find(&format!(" {name}=\""))? + name.len() + 3;
        let end = start + header[start..].find('"')?;
        Some(start..end)
    };
    let view_box = attribute("viewBox")?;
    let numbers: Vec<f64> = header[view_box.clone()]
        .split_whitespace()
        .map(|number| number.parse().ok())
        .collect::<Option<_>>()?;
    let [x, y, width, height] = numbers[..] else {
        return None;
    };
    let slice = width / count as f64;
    let mut header = header.to_owned();
    // Replace the later attribute first, so the earlier range stays valid.
    let mut edits = vec![(
        view_box,
        format!("{} {y} {slice} {height}", x + slice * index as f64),
    )];
    if let Some(width_range) = attribute("width") {
        let unit: String = header[width_range.clone()]
            .chars()
            .skip_while(|c| c.is_ascii_digit() || *c == '.')
            .collect();
        edits.push((width_range, format!("{slice}{unit}")));
    }
    edits.sort_by_key(|(range, _)| std::cmp::Reverse(range.start));
    for (range, value) in edits {
        header.replace_range(range, &value);
    }
    Some(format!("{header}{}", &svg[header_end..]))
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
    /// The asset path its SVG is served under (see `note_text`, which paints it).
    pub fn path(&self) -> SharedString {
        self.path.clone()
    }

    /// Its width in em.
    pub fn width(&self) -> f32 {
        self.width
    }

    /// From its top to its baseline, in em.
    pub fn height(&self) -> f32 {
        self.height
    }

    /// Below its baseline, in em.
    pub fn depth(&self) -> f32 {
        self.depth
    }
}

#[cfg(test)]
mod tests {
    use super::{CACHE, CACHE_LIMIT, asset, formula, slice_svg, tile};

    #[test]
    fn a_wide_formula_is_sliced_into_equal_views_of_one_drawing() {
        let _filling = FILLING.lock().unwrap();
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 300 40" width="300pt" height="40pt"><path d="M0 0"/></svg>"#;
        assert_eq!(
            slice_svg(svg, 1, 3).unwrap(),
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="100 0 100 40" width="100pt" height="40pt"><path d="M0 0"/></svg>"#
        );
        let typeset = formula("a+b+c", false).unwrap();
        assert_eq!(tile(&typeset.path(), 0, 1).unwrap(), typeset.path());
        let second = tile(&typeset.path(), 1, 2).unwrap();
        assert!(asset(&second).is_some());
    }

    /// The tests below fill the one shared cache, which resets it for the others.
    static FILLING: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn slices_count_against_the_cache_limit() {
        let _filling = FILLING.lock().unwrap();
        for index in 0..=CACHE_LIMIT {
            let typeset = formula("x^2+y^2", false).unwrap();
            assert!(tile(&typeset.path(), index, CACHE_LIMIT * 2).is_some());
            assert!(CACHE.lock().unwrap().svgs.len() <= CACHE_LIMIT);
        }
    }

    #[test]
    fn the_formula_cache_is_bounded_and_never_reuses_a_path() {
        let _filling = FILLING.lock().unwrap();
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
