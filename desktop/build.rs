#[path = "../build_support/git.rs"]
mod git;

fn main() {
    emit_commit();
    render_symbols();
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rerun-if-changed=assets/windows.rc");
        println!("cargo:rerun-if-changed=assets/icon.ico");
        // GPUI loads icon resource 1 for the window class. Embedding the same
        // resource also gives Explorer and shortcuts the application icon.
        embed_resource::compile_for(
            "assets/windows.rc",
            ["course2md-desktop"],
            embed_resource::ParamsIncludeDirs(["assets"]),
        )
        .manifest_required()
        .expect("Windows application icon must be embedded");
    }
    // The shared core can link the native Apple speech module. A library's
    // rustc-link-arg is not inherited by its consumers, so each final executable
    // must supply the system Swift runtime search path.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
    }
}

fn emit_commit() {
    let desktop = std::path::PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let commit = git::git_commit(desktop.parent().unwrap());
    println!(
        "cargo:rustc-env=COURSE2MD_DESKTOP_COMMIT={}",
        commit.as_deref().unwrap_or("")
    );
}

/// Renders the symbols `assets/symbols.txt` lists and generates `symbols::<name>()`
/// for each: the fonts behind every symbol stay in this build script, out of the app.
fn render_symbols() {
    use gpui_material_symbols::{Style, Symbol};
    use std::fmt::Write as _;
    println!("cargo:rerun-if-changed=assets/symbols.txt");
    let list = std::fs::read_to_string("assets/symbols.txt").expect("assets/symbols.txt");
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
        let icon = symbol.icon().style(Style::Rounded);
        for icon in [Some(icon), fill.then(|| icon.filled(true))].into_iter().flatten() {
            let svg = icon.to_svg().unwrap_or_else(|error| panic!("{name}: {error}"));
            assets.insert(icon.asset_path(), svg);
        }
        writeln!(
            functions,
            "#[allow(dead_code)]\npub fn {name}() -> SymbolIcon {{\n    \
             gpui_kit_material::icons::Symbol::named({name:?})\
             .expect(\"a symbol of the pinned catalog\").icon().style(Style::Rounded)\n}}"
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
