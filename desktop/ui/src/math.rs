//! LaTeX formulas in notes, typeset by RaTeX into monochrome SVG. The SVG is served
//! through the app's asset source and drawn as a tinted mask, so formulas follow the
//! theme and stay sharp at every text scale.
use gpui::*;
use std::{
    borrow::Cow,
    collections::HashMap,
    sync::{Arc, LazyLock, Mutex},
};

pub use c2m_artifact::math::{Piece, piece_sources, pieces};

/// A typeset formula: its asset path and its box in em, with the baseline
/// `height` below the top.
#[derive(Clone, Debug)]
pub struct Formula {
    pub path: SharedString,
    pub width: f32,
    pub height: f32,
    pub depth: f32,
}

const ASSET_PREFIX: &str = "math/";

/// Formulas kept typeset; notes rarely hold more than a few hundred.
const CACHE_LIMIT: usize = 4096;

struct Cache {
    formulas: HashMap<(String, bool), Option<Formula>>,
    svgs: HashMap<SharedString, Arc<[u8]>>,
}

impl Cache {
    /// Starts over rather than track use. A path names its formula, so one still on
    /// screen is typeset again from its path when next drawn.
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
    })
});

/// `math/{d|i}{hex of the LaTeX}.svg`: the path is the formula, so it can never name
/// another one and can always be typeset again.
fn path_of(latex: &str, display: bool) -> SharedString {
    let mut path = format!("{ASSET_PREFIX}{}", if display { 'd' } else { 'i' });
    for byte in latex.bytes() {
        path.push_str(&format!("{byte:02x}"));
    }
    path.push_str(".svg");
    path.into()
}

/// What a formula's asset path names.
struct Source {
    latex: String,
    display: bool,
    /// A tile's slice: index and count.
    slice: Option<(usize, usize)>,
}

fn source_of(path: &str) -> Option<Source> {
    let name = path.strip_prefix(ASSET_PREFIX)?.strip_suffix(".svg")?;
    let (name, slice) = match name.split_once('-') {
        Some((name, slice)) => {
            let (index, count) = slice.split_once("of")?;
            (name, Some((index.parse().ok()?, count.parse().ok()?)))
        }
        None => (name, None),
    };
    let display = match name.as_bytes().first()? {
        b'd' => true,
        b'i' => false,
        _ => return None,
    };
    let hex = &name[1..];
    let bytes = (0..hex.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(hex.get(at..at + 2)?, 16).ok())
        .collect::<Option<Vec<_>>>()?;
    Some(Source {
        latex: String::from_utf8(bytes).ok()?,
        display,
        slice,
    })
}

/// The typeset formula, or `None` when RaTeX cannot parse it; the note then
/// shows the source. Results are cached. The note's parser asks off the UI thread.
pub fn formula(latex: &str, display: bool) -> Option<Formula> {
    let key = (latex.to_owned(), display);
    if let Some(found) = CACHE.lock().ok()?.formulas.get(&key) {
        return found.clone();
    }
    let typeset = c2m_artifact::math::typeset(latex, display);
    let mut cache = CACHE.lock().ok()?;
    cache.reset_if_full();
    let formula = typeset.map(|typeset| {
        let path = path_of(latex, display);
        cache
            .svgs
            .insert(path.clone(), typeset.svg.into_bytes().into());
        Formula {
            path,
            width: typeset.width,
            height: typeset.height,
            depth: typeset.depth,
        }
    });
    cache.formulas.insert(key, formula.clone());
    formula
}

/// The asset path of slice `index` of `count` equal vertical slices of the
/// formula at `path`, so a formula too wide for one image can be drawn as
/// several. The slice is cut when first drawn.
pub fn tile(path: &str, index: usize, count: usize) -> SharedString {
    if count <= 1 {
        return SharedString::from(path.to_owned());
    }
    SharedString::from(format!(
        "{}-{index}of{count}.svg",
        path.trim_end_matches(".svg")
    ))
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
    if let Some(svg) = CACHE.lock().ok()?.svgs.get(path) {
        return Some(Cow::Owned(svg.to_vec()));
    }
    let Source {
        latex,
        display,
        slice,
    } = source_of(path)?;
    let svg = c2m_artifact::math::typeset(&latex, display)?.svg;
    let svg = match slice {
        Some((index, count)) if index < count => slice_svg(&svg, index, count)?,
        Some(_) => return None,
        None => svg,
    };
    let mut cache = CACHE.lock().ok()?;
    cache.reset_if_full();
    cache
        .svgs
        .insert(SharedString::from(path.to_owned()), svg.as_bytes().into());
    Some(Cow::Owned(svg.into_bytes()))
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
        assert_eq!(tile(&typeset.path, 0, 1), typeset.path);
        let second = tile(&typeset.path, 1, 2);
        assert!(asset(&second).is_some());
        assert!(asset(&tile(&typeset.path, 2, 2)).is_none());
    }

    /// The tests below fill the one shared cache, which resets it for the others.
    static FILLING: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn slices_count_against_the_cache_limit() {
        let _filling = FILLING.lock().unwrap();
        for index in 0..=CACHE_LIMIT {
            let typeset = formula("x^2+y^2", false).unwrap();
            assert!(asset(&tile(&typeset.path, index, CACHE_LIMIT * 2)).is_some());
            assert!(CACHE.lock().unwrap().svgs.len() <= CACHE_LIMIT);
        }
    }

    /// A note keeps the formula it parsed; a reset must not blank it on screen.
    #[test]
    fn a_formula_parsed_before_a_reset_still_draws_after_it() {
        let _filling = FILLING.lock().unwrap();
        let first = formula("a_{0} + \\frac{1}{2}", true).unwrap();
        let expected = asset(&first.path).unwrap().into_owned();
        for n in 1..=CACHE_LIMIT {
            formula(&format!("a_{{{n}}}"), false);
        }
        {
            let cache = CACHE.lock().unwrap();
            assert!(cache.formulas.len() <= CACHE_LIMIT);
            assert!(cache.svgs.len() <= CACHE_LIMIT);
            assert!(!cache.svgs.contains_key(&first.path));
        }
        assert_eq!(asset(&first.path).unwrap().into_owned(), expected);
        assert!(asset(&tile(&first.path, 1, 3)).is_some());
        assert_eq!(
            formula("a_{0} + \\frac{1}{2}", true).unwrap().path,
            first.path
        );
        assert!(asset("math/zz.svg").is_none());
    }
}
