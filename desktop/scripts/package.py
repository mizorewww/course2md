#!/usr/bin/env python3
"""Build and package the native app and its matching CLI on the current platform."""
import argparse
import json
import os
from pathlib import Path
import platform
import plistlib
import shutil
import subprocess
import tomllib

ROOT = Path(__file__).resolve().parents[1]
PROJECT = ROOT.parent


def run(*args):
    subprocess.run(args, cwd=PROJECT, check=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--debug", action="store_true")
    parser.add_argument("--no-build", action="store_true")
    args = parser.parse_args()
    profile = "debug" if args.debug else "release"
    flags = [] if args.debug else ["--release", "--locked"]
    revisions = {
        name: subprocess.check_output(
            ["git", "-C", str(ROOT / ".deps" / name), "rev-parse", "HEAD"], text=True
        ).strip()
        for name in ("zed", "component")
    }
    if not args.debug:
        if not (ROOT / "sources.lock.json").is_file():
            raise SystemExit("Freeze the tested source revisions with scripts/sources.py --freeze before release")
        expected = json.loads((ROOT / "sources.lock.json").read_text())
        if revisions != expected:
            raise SystemExit("Prepared sources differ from the tested release revisions; prepare sources with --locked")
    if not args.no_build:
        run("cargo", "build", *flags)
        run("cargo", "build", "--manifest-path", str(ROOT / "Cargo.toml"), *flags)
    version = tomllib.loads((PROJECT / "Cargo.toml").read_text())["package"]["version"]
    system = platform.system()
    suffix = ".exe" if system == "Windows" else ""
    package_name = f"course2md-desktop-{system.lower()}-{platform.machine()}"
    base = ROOT / "target" / "packages" / package_name
    if base.exists():
        shutil.rmtree(base)
    base.mkdir(parents=True, exist_ok=True)
    if system == "Darwin":
        bundle = base / "course2md.app"
        binaries = bundle / "Contents/MacOS"
        resources = bundle / "Contents/Resources"
        binaries.mkdir(parents=True, exist_ok=True)
        resources.mkdir(parents=True, exist_ok=True)
        with (bundle / "Contents/Info.plist").open("wb") as stream:
            plistlib.dump({
                "CFBundleName": "course2md", "CFBundleDisplayName": "course2md",
                "CFBundleIdentifier": "dev.course2md.desktop",
                "CFBundleExecutable": "course2md-desktop", "CFBundlePackageType": "APPL",
                "CFBundleVersion": version, "CFBundleShortVersionString": version,
                "NSHighResolutionCapable": True, "NSPrincipalClass": "NSApplication",
                "LSMinimumSystemVersion": "14.0", "CFBundleIconFile": "course2md.icns",
            }, stream)
        shutil.copy2(ROOT / "assets/icon.icns", resources / "course2md.icns")
        # MLX first searches beside the executable for mlx.metallib. Keeping
        # that layout avoids changing the CLI's working directory and breaking
        # user-supplied relative source, output, and model paths.
        metal = PROJECT / "target" / profile / "mlx.metallib"
        if metal.is_file():
            shutil.copy2(metal, resources / "mlx.metallib")
            (binaries / "mlx.metallib").symlink_to("../Resources/mlx.metallib")
        elif platform.machine() == "arm64" and not os.environ.get("COURSE2MD_NO_APPLE"):
            raise SystemExit("Apple speech library was built without mlx.metallib; inspect the core build output")
    else:
        binaries = base
        if system == "Linux":
            (base / "course2md.desktop").write_text(
                "[Desktop Entry]\nType=Application\nName=course2md\nComment=Turn courses into illustrated notes\n"
                "Exec=course2md-desktop\nIcon=course2md\nTerminal=false\nCategories=Education;AudioVideo;\n")
            shutil.copy2(ROOT / "assets/icon.png", base / "course2md.png")
    shutil.copy2(PROJECT / "target" / profile / f"course2md{suffix}", binaries / f"course2md{suffix}")
    shutil.copy2(ROOT / "target" / profile / f"course2md-desktop{suffix}", binaries / f"course2md-desktop{suffix}")
    shutil.copy2(PROJECT / "LICENSE", base / "LICENSE")
    shutil.copy2(ROOT / "README.md", base / "README.md")
    # A development archive follows main and must not claim the previous release
    # revisions. Release builds have already checked this snapshot against the lock.
    (base / "sources.lock.json").write_text(json.dumps(revisions, indent=2) + "\n")
    if system == "Darwin":
        identity = os.environ.get("APPLE_SIGNING_IDENTITY", "-")
        signing = ["--force", "--sign", identity]
        if identity != "-":
            signing.extend(["--options", "runtime", "--timestamp"])
        run("codesign", *signing, str(binaries / "course2md"))
        run("codesign", *signing, str(binaries / "course2md-desktop"))
        run("codesign", *signing, str(bundle))
        run("codesign", "--verify", "--deep", "--strict", str(bundle))
        notarization = [os.environ.get(key) for key in ("APPLE_API_KEY_PATH", "APPLE_API_KEY_ID", "APPLE_API_ISSUER")]
        if identity != "-" and all(notarization):
            upload = base.parent / "notarization.zip"
            run("ditto", "-c", "-k", "--keepParent", str(bundle), str(upload))
            run("xcrun", "notarytool", "submit", str(upload), "--key", notarization[0],
                "--key-id", notarization[1], "--issuer", notarization[2], "--wait")
            run("xcrun", "stapler", "staple", str(bundle))
            upload.unlink()
        dmg = base.parent / f"course2md-gui-macos-{platform.machine()}.dmg"
        if dmg.exists():
            dmg.unlink()
        applications = base / "Applications"
        applications.symlink_to("/Applications")
        # Size the volume from logical file bytes, not host allocation/cloning.
        # Reserve 64 MiB for filesystem metadata and block rounding. Empty space
        # compresses in UDZO, so this does not inflate the download by 64 MiB.
        payload_bytes = sum(path.stat().st_size for path in base.rglob("*")
                            if not path.is_symlink() and path.is_file())
        image_mib = (payload_bytes + 1024 * 1024 - 1) // (1024 * 1024) + 64
        print(f"Creating {image_mib} MiB HFS+ image for {payload_bytes} bytes of files", flush=True)
        run("hdiutil", "create", "-volname", "course2md", "-srcfolder", str(base),
            "-fs", "HFS+", "-size", f"{image_mib}m", "-ov", "-format", "UDZO", str(dmg))
        applications.unlink()
        if identity != "-":
            run("codesign", "--force", "--sign", identity, "--timestamp", str(dmg))
        if identity != "-" and all(notarization):
            run("xcrun", "notarytool", "submit", str(dmg), "--key", notarization[0],
                "--key-id", notarization[1], "--issuer", notarization[2], "--wait")
            run("xcrun", "stapler", "staple", str(dmg))
        print(dmg)
    if system == "Darwin":
        archive = str(base) + ".zip"
        # ditto preserves the signed app's symlinks and resource metadata.
        run("ditto", "-c", "-k", "--keepParent", str(base), archive)
    else:
        archive = shutil.make_archive(str(base), "zip" if system == "Windows" else "gztar", base.parent, base.name)
    print(archive)


if __name__ == "__main__":
    main()
