//! The shared catalog and the code that looks strings up in it stay in step:
//! every literal key has a row, and every row is still looked up somewhere.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

const MACROS: [&str; 5] = [
    "tr!",
    "println_tr!",
    "eprintln_tr!",
    "print_tr!",
    "eprint_tr!",
];

fn sources() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    let mut stack = vec![root.join("src"), root.join("desktop/src")];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                files.push(path);
            }
        }
    }
    files
}

/// Reads a source file the way rustc does: a Windows checkout's CRLF line
/// endings become LF, so multi-line literals match the strings at run time.
fn source(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap().replace("\r\n", "\n")
}

/// Parses a Rust string literal (plain or raw) at the start of `text`.
fn string_literal(text: &str) -> Option<String> {
    let text = text.trim_start();
    if let Some(rest) = text.strip_prefix('r') {
        let hashes = rest.chars().take_while(|c| *c == '#').count();
        let body = rest[hashes..].strip_prefix('"')?;
        let end = body.find(&format!("\"{}", "#".repeat(hashes)))?;
        return Some(body[..end].to_owned());
    }
    let mut chars = text.strip_prefix('"')?.chars();
    let mut out = String::new();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(out),
            '\\' => match chars.next()? {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                'r' => out.push('\r'),
                '0' => out.push('\0'),
                '\\' => out.push('\\'),
                '"' => out.push('"'),
                '\'' => out.push('\''),
                '\n' => {
                    // A line continuation skips the newline and leading spaces.
                    let rest: String = chars.clone().collect();
                    let skipped = rest.len() - rest.trim_start().len();
                    for _ in 0..rest[..skipped].chars().count() {
                        chars.next();
                    }
                }
                'u' => {
                    let rest: String = chars.clone().collect();
                    let close = rest.find('}')?;
                    let code = u32::from_str_radix(&rest[1..close], 16).ok()?;
                    out.push(char::from_u32(code)?);
                    for _ in 0..rest[..=close].chars().count() {
                        chars.next();
                    }
                }
                other => {
                    out.push('\\');
                    out.push(other);
                }
            },
            c => out.push(c),
        }
    }
    None
}

/// The code of a file with line comments (and doc examples) blanked out.
fn without_comments(text: &str) -> String {
    text.lines()
        .map(|line| {
            if line.trim_start().starts_with("//") {
                ""
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Literal keys passed to the tr! family, nested calls included.
fn literal_keys(text: &str, keys: &mut BTreeSet<String>) {
    let text = &without_comments(text);
    for name in MACROS {
        let mut from = 0;
        while let Some(found) = text[from..].find(name) {
            let at = from + found;
            from = at + name.len();
            // `println_tr!` also contains `tr!`; count each call once.
            let preceded = text[..at]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_alphanumeric() || c == '_');
            if preceded {
                continue;
            }
            let rest = text[from..].trim_start();
            let Some(args) = rest.strip_prefix('(') else {
                continue;
            };
            if let Some(key) = string_literal(args) {
                keys.insert(key);
            }
        }
    }
}

/// Every string literal in a file. Keys reached through a constant or a table
/// (`tr!(NO_SCREENSHOTS)`, `tr!(label)`) are literals there, not at the call.
fn all_literals(text: &str, out: &mut BTreeSet<String>) {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                i = text[i..].find('\n').map_or(bytes.len(), |n| i + n);
            }
            b'\'' => {
                // Skip a char literal ('"', '\'') but not a lifetime ('a).
                let rest = &text[i + 1..];
                let close = if rest.starts_with('\\') {
                    rest[2..].find('\'').map(|n| n + 2)
                } else {
                    rest.char_indices()
                        .nth(1)
                        .filter(|(_, c)| *c == '\'')
                        .map(|(n, _)| n)
                };
                i += close.map_or(1, |n| n + 2);
            }
            b'"' | b'r' => {
                let raw = bytes[i] == b'r'
                    && (i == 0 || !(bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_'))
                    && matches!(bytes.get(i + 1), Some(b'"' | b'#'));
                if (bytes[i] == b'"' || raw)
                    && let Some(value) = string_literal(&text[i..])
                {
                    let len = literal_len(&text[i..]);
                    out.insert(value);
                    i += len.max(1);
                    continue;
                }
                i += 1;
            }
            _ => i += 1,
        }
    }
}

/// Byte length of the literal at the start of `text` (which starts with it).
fn literal_len(text: &str) -> usize {
    if let Some(rest) = text.strip_prefix('r') {
        let hashes = rest.chars().take_while(|c| *c == '#').count();
        let closing = format!("\"{}", "#".repeat(hashes));
        let body = 1 + hashes + 1;
        return text[body..]
            .find(&closing)
            .map_or(text.len(), |n| body + n + closing.len());
    }
    let bytes = text.as_bytes();
    let mut i = 1;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b'"' => return i + 1,
            _ => i += 1,
        }
    }
    text.len()
}

/// clap help text: doc comments and `about`/`help_heading` strings in the CLI
/// definition are localized at runtime by cli::localize_command.
fn cli_texts(text: &str) -> String {
    let mut docs = String::new();
    for line in text.lines() {
        let line = line.trim();
        if let Some(doc) = line.strip_prefix("///") {
            docs.push_str(doc.trim());
            docs.push(' ');
        } else {
            docs.push('\n');
            for marker in ["about = ", "help_heading = ", "after_help = ", "help = "] {
                if let Some(index) = line.find(marker)
                    && let Some(value) = string_literal(&line[index + marker.len()..])
                {
                    docs.push_str(&value);
                    docs.push('\n');
                }
            }
        }
    }
    docs
}

fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn catalog() -> Vec<String> {
    let text =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("i18n/catalog.tsv"))
            .unwrap();
    text.lines()
        .skip(1)
        .filter(|line| !line.is_empty())
        .map(|line| {
            let key = line.split('\t').next().unwrap();
            key.replace("\\n", "\n")
                .replace("\\t", "\t")
                .replace("\\\\", "\\")
        })
        .collect()
}

/// A "中文 / English" key falls back to its halves without a row.
fn bilingual(key: &str) -> bool {
    key.contains(" / ")
}

#[test]
fn every_literal_key_has_a_catalog_row() {
    let rows: BTreeSet<String> = catalog().into_iter().collect();
    let mut keys = BTreeSet::new();
    for file in sources() {
        literal_keys(&source(&file), &mut keys);
    }
    let missing: Vec<_> = keys
        .iter()
        .filter(|key| !rows.contains(*key) && !bilingual(key))
        // Paths and markup-free punctuation are not messages.
        .filter(|key| !key.is_ascii())
        .collect();
    assert!(
        missing.is_empty(),
        "tr! keys without a catalog row: {missing:#?}"
    );
}

#[test]
fn every_catalog_row_is_looked_up() {
    let mut keys = BTreeSet::new();
    let mut cli = String::new();
    for file in sources() {
        let text = source(&file);
        all_literals(&text, &mut keys);
        if file.ends_with("src/cli.rs") {
            cli = collapse(&cli_texts(&text));
        }
    }
    let unused: Vec<_> = catalog()
        .into_iter()
        .filter(|key| !keys.contains(key))
        .filter(|key| !cli.contains(&collapse(key)))
        .collect();
    assert!(
        unused.is_empty(),
        "catalog rows nothing looks up ({}): {unused:#?}",
        unused.len()
    );
}
