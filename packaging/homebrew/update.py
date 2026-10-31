#!/usr/bin/env python3
"""Render one Homebrew release channel, verifying every asset before changing files."""
import argparse
import hashlib
import re
from pathlib import Path
import subprocess
import sys
import urllib.request

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from versioning import PRERELEASE_CHANNELS, parse_release  # noqa: E402

TEMPLATES = Path(__file__).resolve().parent
CHANNELS = ("stable", *PRERELEASE_CHANNELS)
ASSETS = {
    "SHA_ARM": "course2md-macos-arm64",
    "SHA_INTEL": "course2md-macos-x86_64",
    "SHA_MLX": "mlx-macos-arm64.metallib",
    "SHA_NOTICES": "course2md-third-party-licenses.txt",
    "SHA_LINUX_X86": "course2md-linux-x86_64",
    "SHA_LINUX_ARM": "course2md-linux-aarch64",
    "SHA_DMG": "course2md-gui-macos-arm64.dmg",
}


def checksum(url):
    digest = hashlib.sha256()
    size = 0
    with urllib.request.urlopen(url, timeout=120) as response:
        for block in iter(lambda: response.read(1024 * 1024), b""):
            digest.update(block)
            size += len(block)
        expected = response.headers.get("Content-Length")
        if not size or (expected is not None and size != int(expected)):
            raise ValueError(f"Empty or incomplete release asset: {url}")
    return digest.hexdigest()


def render(tap, version, fetch_checksum=checksum):
    match = parse_release(version)
    if not match:
        raise ValueError("Expected a stable version or alpha/beta/rc.N prerelease")
    channel = match[0]
    suffix = "" if channel == "stable" else f"@{channel}"
    # Homebrew only translates numeric @ suffixes into valid Ruby class names.
    # Keep named prerelease channels as hyphenated formulae.
    formula_token = "course2md" if not suffix else f"course2md-{channel}"
    cask_token = f"course2md-gui{suffix}"
    conflicts = [f'"course2md-gui{("@" + other) if other != "stable" else ""}"'
                 for other in CHANNELS if other != channel]
    values = {
        "VERSION": version,
        "FORMULA_CLASS": "Course2md" + (channel.capitalize() if suffix else ""),
        "CASK_TOKEN": cask_token,
        # :versioned_formula may auto-link when no numeric sibling is found.
        "KEG_ONLY": f'\n  keg_only "it is the {channel} prerelease channel"\n' if suffix else "",
        "CONFLICTS": f'  conflicts_with cask: [{", ".join(conflicts)}]',
        "LIVECHECK": '\n  livecheck do\n    skip "Prerelease channel"\n  end\n' if suffix else "",
    }
    # Finish all downloads before touching either channel file. A missing asset
    # must never leave an apparently updated formula with stale checksums.
    for token, asset in ASSETS.items():
        values[token] = fetch_checksum(f"https://github.com/mizorewww/course2md/releases/download/v{version}/{asset}")
        if not re.fullmatch(r"[a-f0-9]{64}", values[token]):
            raise ValueError(f"Invalid checksum for {asset}")
    rendered = {}
    for template, relative in [
        ("course2md.rb.template", f"Formula/{formula_token}.rb"),
        ("course2md-gui.rb.template", f"Casks/{cask_token}.rb"),
    ]:
        source = (TEMPLATES / template).read_text()
        for key, value in values.items():
            source = source.replace(f"@{key}@", value)
        if re.search(r"@[A-Z_]+@", source):
            raise ValueError(f"Unresolved template placeholder in {template}")
        subprocess.run(["ruby", "-c"], input=source, text=True, check=True, capture_output=True)
        rendered[Path(tap) / relative] = source
    for path, source in rendered.items():
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(source)
        print(path)
    return list(rendered)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("tap", type=Path)
    parser.add_argument("--version", required=True)
    args = parser.parse_args()
    render(args.tap, args.version)
