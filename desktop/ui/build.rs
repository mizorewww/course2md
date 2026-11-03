//! Generates `symbols::<name>()` from `assets/symbols.txt`.

/// The stroke weight of the app's Material Symbols.
const ICON_WEIGHT: f32 = 300.;

const LIST: &str = "../assets/symbols.txt";

fn main() {
    render_symbols();
}

/// Renders the symbols `assets/symbols.txt` lists and generates `symbols::<name>()`
/// for each: the fonts behind every symbol stay in this build script, out of the app.
fn render_symbols() {
    use gpui_material_symbols::{Axes, Style, Symbol};
    // A fine, round-ended line is the app's icon character.
    let axes = Axes::new(0., ICON_WEIGHT, 0., 24.).expect("valid symbol axes");
    use std::fmt::Write as _;
    println!("cargo:rerun-if-changed={LIST}");
    let list = std::fs::read_to_string(LIST).expect("assets/symbols.txt");
    let mut assets = std::collections::BTreeMap::new();
    let mut functions = String::new();
    for line in list.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (name, fill) = match line.split_once(' ') {
            Some((name, "fill")) => (name, true),
            None => (line, false),
            Some(_) => panic!("assets/symbols.txt: {line:?} is `name` or `name fill`"),
        };
        let symbol = Symbol::named(name)
            .unwrap_or_else(|| panic!("assets/symbols.txt: {name} is not a Material Symbol"));
        let icon = symbol.icon().style(Style::Rounded).axes(axes);
        // The kit's components ask for their own glyphs at the catalog's default weight.
        let kit = symbol.icon().style(Style::Rounded);
        for icon in [
            Some(icon),
            Some(kit),
            fill.then(|| icon.filled(true)),
            fill.then(|| kit.filled(true)),
            // A kit glyph drawn filled in the catalog's own style, such as a selected
            // card's check.
            fill.then(|| symbol.icon().filled(true)),
        ]
        .into_iter()
        .flatten()
        {
            let svg = icon
                .to_svg()
                .unwrap_or_else(|error| panic!("{name}: {error}"));
            assets.insert(icon.asset_path(), svg);
        }
        writeln!(
            functions,
            "#[allow(dead_code)]\npub fn {name}() -> SymbolIcon {{\n    \
             gpui_kit_material::icons::Symbol::named({name:?})\
             .expect(\"a symbol of the pinned catalog\").icon().style(Style::Rounded)\
             .axes(Axes::new(0., {ICON_WEIGHT:?}, 0., 24.).expect(\"valid symbol axes\"))\n}}"
        )
        .unwrap();
    }
    let mut code = format!("const SYMBOLS: [(&str, &str); {}] = [\n", assets.len());
    for (path, svg) in &assets {
        writeln!(code, "    ({path:?}, {svg:?}),").unwrap();
    }
    code.push_str("];\n");
    code.push_str(&functions);
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    std::fs::write(out.join("symbols.rs"), code).expect("write the generated symbols");
}
