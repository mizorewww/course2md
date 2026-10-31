#!/usr/bin/env python3
"""Production Rust lines: every .rs under the repo's source roots, minus a trailing test module."""
import pathlib, re, sys

root = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else ".")
skip = {"target", ".git", ".deps", ".cache", "tests", "benches", "examples"}
total = {}
for path in root.rglob("*.rs"):
    rel = path.relative_to(root)
    if skip & set(rel.parts) or rel.name in {"build.rs", "tests.rs"} or "build_support" in rel.parts:
        continue
    text = path.read_text(errors="replace")
    match = re.search(r"^#\[cfg\(test\)\]\s*\n(pub(\(\w+\))? )?mod \w+ *\{", text, re.M)
    if match:
        text = text[: match.start()]
    if "test_" in rel.name:
        continue
    key = rel.parts[0] if rel.parts[0] != "crates" else "/".join(rel.parts[:2])
    total[key] = total.get(key, 0) + text.count("\n")
for key, lines in sorted(total.items()):
    print(f"{lines:7d} {key}")
print(f"{sum(total.values()):7d} total")
