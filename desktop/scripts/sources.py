#!/usr/bin/env python3
"""Prepare GPUI main sources; --freeze records tested revisions for release.

The component repository publishes GPUI under gpui-pre package names. A manifest
overlay connects its source to Zed's original workspace; no source is vendored
in the project. A macro lookup compatibility patch supports the original GPUI package name. Developer checkouts remain untouched except for fast-forward pulls.
"""
import argparse
import json
from pathlib import Path
import re
import subprocess

ROOT = Path(__file__).resolve().parents[1]
REPOS = {"zed": "zed-industries/zed", "component": "longbridge/gpui-component"}


def git(path, *args, capture=False):
    result = subprocess.run(["git", "-C", str(path), *args], check=True,
                            text=True, stdout=subprocess.PIPE if capture else None)
    return result.stdout.strip() if capture else None


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--no-pull", action="store_true", help="Reuse already updated local main sources")
    mode.add_argument("--freeze", action="store_true", help="Record currently prepared and tested source revisions")
    mode.add_argument("--locked", action="store_true", help="Use release source revisions without updating main")
    parser.add_argument("--developer-dir", type=Path, default=Path.home() / "Developer")
    args = parser.parse_args()
    lock_path = ROOT / "sources.lock.json"
    if args.freeze:
        revisions = {name: git(ROOT / ".deps" / name, "rev-parse", "HEAD", capture=True) for name in REPOS}
        lock_path.write_text(json.dumps(revisions, indent=2) + "\n")
        print(f"Recorded tested revisions in {lock_path}")
        return
    locked = json.loads(lock_path.read_text()) if args.locked else {}
    if args.locked and (set(locked) != set(REPOS) or
                        any(not re.fullmatch(r"[0-9a-f]{40}", value) for value in locked.values())):
        raise SystemExit("Release source lock must contain a full commit hash for each dependency")
    (ROOT / ".deps").mkdir(exist_ok=True)
    for name, remote in REPOS.items():
        source = args.developer_dir / ("gpui-component" if name == "component" else "zed")
        if not source.exists():
            source.parent.mkdir(parents=True, exist_ok=True)
            subprocess.run(["git", "clone", f"https://github.com/{remote}.git", str(source)], check=True)
        if not args.locked and not args.no_pull:
            if git(source, "status", "--porcelain", capture=True):
                raise SystemExit(f"Commit or stash changes in {source} before pulling main")
            if git(source, "branch", "--show-current", capture=True) != "main":
                raise SystemExit(f"{source} must be on main")
            git(source, "pull", "--ff-only", f"https://github.com/{remote}.git", "main")
        revision = locked.get(name) or git(source, "rev-parse", "HEAD", capture=True)
        dest = ROOT / ".deps" / name
        if not dest.exists():
            git(source, "worktree", "add", "--detach", str(dest), revision)
        else:
            # Only our known generated manifest overlay may be discarded.
            changed = git(dest, "diff", "HEAD", "--name-only", capture=True).splitlines()
            allowed = ["Cargo.toml", "crates/component-macros/src/crate_path.rs"] if name == "component" else []
            if any(path not in allowed for path in changed):
                raise SystemExit(f"Unexpected source edits in {dest}: {changed}")
            if name == "component":
                git(dest, "restore", "--source=HEAD", "--staged", "--worktree", *allowed)
            git(dest, "checkout", "--detach", revision)
    manifest = ROOT / ".deps/component/Cargo.toml"
    text = manifest.read_text()
    for alias, crate in [("gpui", "gpui"), ("gpui_platform", "gpui_platform"),
                         ("gpui_macros", "gpui_macros"), ("sum-tree", "sum_tree"),
                         ("reqwest_client", "reqwest_client"), ("gpui_web", "gpui_web")]:
        pattern = rf'^{re.escape(alias)} = \{{ package = "gpui-pre[^\"]*",[^\n]*\}}$'
        features = ', features = ["font-kit", "x11", "wayland", "runtime_shaders"]' if alias == "gpui_platform" else ''
        package = f', package = "{crate}"' if alias != crate else ''
        replacement = f'{alias} = {{ path = "../zed/crates/{crate}"{package}{features} }}'
        text, count = re.subn(pattern, replacement, text, count=1, flags=re.M)
        if count != 1:
            raise SystemExit(f"Upstream changed the {alias} dependency; review the manifest overlay")
    manifest.write_text(text)
    macro = ROOT / ".deps/component/crates/component-macros/src/crate_path.rs"
    code = macro.read_text()
    old = 'Err(kit_error) => crate_name("gpui-pre")'
    if old not in code:
        raise SystemExit("Upstream macro lookup changed; review the compatibility patch")
    macro.write_text(code.replace(old, old + '.or_else(|_| crate_name("gpui"))'))
    print("Prepared GPUI and GPUI Component sources")


if __name__ == "__main__":
    main()
