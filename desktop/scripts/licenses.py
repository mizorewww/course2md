#!/usr/bin/env python3
"""Third-party notices: Rust crates through cargo-about, Swift packages from the
checkouts that Package.resolved pins. Which crates count and which licenses may
ship are set in packaging/licenses/about.toml."""
import argparse
import json
from pathlib import Path
import subprocess

PROJECT = Path(__file__).resolve().parents[2]
ABOUT = PROJECT / "packaging/licenses"
SWIFT_PACKAGE = PROJECT / "native/apple-asr"
FILE_NAME = "THIRD-PARTY-LICENSES.txt"
LICENSE_FILES = ("license", "licence", "copying", "notice")


def swift_notices(package=SWIFT_PACKAGE):
    """The license files of every pinned Swift package; a package without one is an error."""
    pins = json.loads((package / "Package.resolved").read_text(encoding="utf-8"))["pins"]
    sections, missing = [], []
    for pin in pins:
        name = pin["location"].rstrip("/").rsplit("/", 1)[-1].removesuffix(".git")
        files = sorted(path for path in (package / ".build/checkouts" / name).glob("*")
                       if path.is_file() and path.name.lower().startswith(LICENSE_FILES))
        if not files:
            missing.append(name)
        for path in files:
            text = path.read_text(encoding="utf-8", errors="replace")
            sections.append(f"{'=' * 80}\n{name} ({pin['location']}): {path.name}\n\n{text}\n")
    if missing:
        raise OSError(f"Swift packages without license files (is {package}/.build checked out?): "
                      + ", ".join(missing))
    return "Swift packages, linked into the Apple speech module:\n\n" + "\n".join(sections)


def generate(output, manifest, swift=False):
    """Write the notices for `manifest`'s locked dependencies to `output`."""
    # cargo-about reads each crate's own files, and a build only unpacks its host's.
    subprocess.run(["cargo", "fetch", "--locked", "--manifest-path", str(manifest)], cwd=PROJECT, check=True)
    subprocess.run(["cargo", "about", "generate", "--fail", "--locked", "--config", str(ABOUT / "about.toml"),
                    "--manifest-path", str(manifest), "--output-file", str(output),
                    str(ABOUT / "notices.hbs")], cwd=PROJECT, check=True)
    if swift:
        with Path(output).open("a", encoding="utf-8") as stream:
            stream.write(swift_notices())
    return output


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=Path)
    parser.add_argument("--manifest", type=Path, default=PROJECT / "Cargo.toml")
    parser.add_argument("--swift", action="store_true", help="add the Swift packages' licenses")
    args = parser.parse_args()
    generate(args.output, args.manifest, args.swift)
