#!/usr/bin/env python3
"""Build and package the native app and its matching CLI on the current platform."""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import platform
import plistlib
import shutil
import subprocess
import tomllib
import urllib.request
import zipfile

from dmg import build_install_dmg
import ffmpeg_bundle
import prebuilt

ROOT = Path(__file__).resolve().parents[1]
PROJECT = ROOT.parent
# Bundled tools that are built or downloaded once and reused by later runs.
TOOL_CACHE = ROOT / ".cache/tools"


def _load_versioning():
    """packaging/versioning.py: the release version grammar shared by every channel."""
    spec = importlib.util.spec_from_file_location("versioning", PROJECT / "packaging/versioning.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


RELEASE = _load_versioning().RELEASE


def macos_versions(version):
    """Keep the product SemVer while using Apple's bundle version syntax."""
    match = RELEASE.fullmatch(version)
    if not match:
        raise ValueError(f"Unsupported macOS release version: {version}")
    short, channel, number = match.groups()
    build = short
    if channel:
        if int(number) > 255:
            raise ValueError("Apple prerelease build numbers must be in 1..255")
        build += {"alpha": "a", "beta": "b", "rc": "fc"}[channel] + number
    return {"CFBundleShortVersionString": short, "CFBundleVersion": build,
            "Course2mdVersion": version}


def run(*args, env=None):
    subprocess.run(args, cwd=PROJECT, check=True, env=env)


def copy_licenses(dest, tools=()):
    """The app's licenses, plus those of bundled tools (`tools`: license files)."""
    for path in tools:
        shutil.copy2(path, dest / path.name)
    shutil.copy2(PROJECT / "LICENSE", dest / "LICENSE")
    shutil.copy2(ROOT / "assets/material/LICENSE", dest / "LICENSE-material-icons")
    shutil.copy2(ROOT / "assets/outline/LICENSE", dest / "LICENSE-lucide-icons")
    shutil.copytree(ROOT / "assets/themes/licenses", dest / "design-licenses", dirs_exist_ok=True)


def notarize(path, key_path, key_id, issuer):
    run("xcrun", "notarytool", "submit", str(path), "--key", key_path,
        "--key-id", key_id, "--issuer", issuer, "--wait")


# Bundled yt-dlp is pinned so release archives are reproducible. To update, take
# the version and hashes from that release's SHA2-256SUMS.
YTDLP_VERSION = "2026.08.19"
YTDLP_SHA256 = {
    "yt-dlp_macos": "0f192b7ec147ab6288885d6351d9ab67367640029b4377576ef46dd79cf7b202",
    "yt-dlp.exe": "66674953fe251b89f4d08c5f0e35e0728679bd67ab3d7d05c0562af101dd3e7a",
    "yt-dlp_arm64.exe": "05b438997bafc3affdfda9d041353c9d73e04dc842207254b655b0887c4445b0",
    "yt-dlp_linux": "58162f9bfdc27458ea47bfcb311cf47028f17d8154a8bf7d689861d46399230a",
    "yt-dlp_linux_aarch64": "b16e4dab368a816cd05d477d698a605a6ae87ccee1c8ffd38fa21d7254141fcc",
}


def ytdlp_asset(system, machine):
    """Standalone binary asset name in yt-dlp's GitHub releases, or None."""
    if system == "Darwin":
        return "yt-dlp_macos"
    if system == "Windows":
        return {"AMD64": "yt-dlp.exe", "ARM64": "yt-dlp_arm64.exe"}.get(machine)
    if system == "Linux":
        return {"x86_64": "yt-dlp_linux", "aarch64": "yt-dlp_linux_aarch64"}.get(machine)
    return None


def bundle_ytdlp(dest, required, local=None):
    """Place the pinned standalone yt-dlp beside the app binaries.

    `local` is a yt-dlp build to bundle instead of downloading (--ytdlp).
    Release builds fail without it; development builds fall back to PATH.
    """
    try:
        if local:
            shutil.copy2(local, dest)
        else:
            asset = ytdlp_asset(platform.system(), platform.machine())
            if not asset:
                return None
            url = f"https://github.com/yt-dlp/yt-dlp/releases/download/{YTDLP_VERSION}/{asset}"
            tmp = dest.parent / (dest.name + ".download")
            try:
                with urllib.request.urlopen(url, timeout=300) as response, tmp.open("wb") as out:
                    shutil.copyfileobj(response, out)
                digest = hashlib.sha256(tmp.read_bytes()).hexdigest()
                if digest != YTDLP_SHA256[asset]:
                    raise OSError(f"{asset} {YTDLP_VERSION} checksum mismatch: {digest}")
                tmp.replace(dest)
            finally:
                tmp.unlink(missing_ok=True)
        dest.chmod(0o755)
        return dest
    except OSError as error:
        if required:
            raise SystemExit(f"Release builds must bundle yt-dlp: {error}") from error
        print(f"warning: yt-dlp not bundled ({error}); the app will use yt-dlp from PATH")
        return None


# aria2 downloads speech models over several connections. Package managers
# supply it on macOS and Linux; Windows archives bundle the official static
# build. It is GPL-2.0 and ships beside the app with its license.
ARIA2_VERSION = "1.37.0"
ARIA2_WINDOWS = "aria2-1.37.0-win-64bit-build1"
ARIA2_WINDOWS_SHA256 = "67d015301eef0b612191212d564c5bb0a14b5b9c4796b76454276a4d28d9b288"


def bundle_aria2(binaries, licenses, required):
    """Place aria2c.exe beside the Windows binaries; the engine finds it there."""
    url = (f"https://github.com/aria2/aria2/releases/download/release-{ARIA2_VERSION}/"
           f"{ARIA2_WINDOWS}.zip")
    tmp = binaries / "aria2.zip.download"
    try:
        with urllib.request.urlopen(url, timeout=300) as response, tmp.open("wb") as out:
            shutil.copyfileobj(response, out)
        digest = hashlib.sha256(tmp.read_bytes()).hexdigest()
        if digest != ARIA2_WINDOWS_SHA256:
            raise OSError(f"{ARIA2_WINDOWS} checksum mismatch: {digest}")
        with zipfile.ZipFile(tmp) as archive:
            (binaries / "aria2c.exe").write_bytes(archive.read(f"{ARIA2_WINDOWS}/aria2c.exe"))
            (licenses / "LICENSE-aria2").write_bytes(archive.read(f"{ARIA2_WINDOWS}/COPYING"))
        return binaries / "aria2c.exe"
    except (OSError, KeyError, zipfile.BadZipFile) as error:
        if required:
            raise SystemExit(f"Release builds must bundle aria2: {error}") from error
        print(f"warning: aria2 not bundled ({error}); models download without it")
        return None
    finally:
        tmp.unlink(missing_ok=True)


def verify_tools(binaries, system, skip=()):
    """Run each bundled tool once, so a missing library or an ffmpeg without
    TLS fails the package build rather than a user's first conversion."""
    suffix = ".exe" if system == "Windows" else ""
    checks = []
    if "ffmpeg" not in skip and ffmpeg_bundle.supported(system, platform.machine()):
        checks += [(["ffmpeg", "-hide_banner", "-protocols"], "https"),
                   (["ffprobe", "-hide_banner", "-version"], "ffprobe version")]
    names = {tool.name for tool in prebuilt.PREBUILT.get((system, platform.machine()), [])}
    if "uv" in names and "uv" not in skip:
        checks.append((["uv", "--version"], "uv "))
    if "llama.cpp" in names and "llama.cpp" not in skip:
        checks.append((["llama.cpp/llama-server", "--version"], "version"))
    if system == "Linux":
        # Shared libraries the archive expects from the system: printed for the
        # log, and a missing one fails here.
        for program in ["ffmpeg", "llama.cpp/llama-server"]:
            if (binaries / program).is_file():
                linked = subprocess.run(["ldd", str(binaries / program)], capture_output=True,
                                        text=True).stdout
                print(f"{program} links:\n{linked}")
                if "not found" in linked:
                    raise SystemExit(f"Bundled {program} misses a shared library:\n{linked}")
    for (program, *arguments), expected in checks:
        path = program if isinstance(program, Path) else binaries / f"{program}{suffix}"
        try:
            result = subprocess.run([str(path), *arguments], capture_output=True, text=True,
                                    timeout=120, stdin=subprocess.DEVNULL)
        except (OSError, subprocess.TimeoutExpired) as error:
            raise SystemExit(f"Bundled {program} does not run: {error}") from error
        output = result.stdout + result.stderr
        if result.returncode != 0 or expected not in output:
            raise SystemExit(f"Bundled {program} failed its check (exit {result.returncode}, "
                             f"expected {expected!r}):\n{output[-2000:]}")
        print(f"checked {program}")


def parse_args(argv=None, environ=os.environ):
    """Command-line options; each environment variable named in its help is the default."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--debug", action="store_true")
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument("--app-only", action="store_true",
                        help="macOS: assemble and ad-hoc sign the release .app for local use, "
                             "without the source lock, notarization or archives")
    flag = lambda name: bool(environ.get(name))
    parser.add_argument("--ytdlp", default=environ.get("COURSE2MD_YTDLP") or None, metavar="PATH",
                        help="bundle this yt-dlp instead of downloading the pinned release "
                             "(COURSE2MD_YTDLP)")
    parser.add_argument("--no-ytdlp", action="store_true", default=flag("COURSE2MD_NO_YTDLP"),
                        help="do not bundle yt-dlp (COURSE2MD_NO_YTDLP)")
    parser.add_argument("--require-tools", action="store_true",
                        default=flag("COURSE2MD_REQUIRE_TOOLS"),
                        help="stop if a bundled tool is missing or fails its check, as release "
                             "builds do; for development packages in CI (COURSE2MD_REQUIRE_TOOLS)")
    parser.add_argument("--no-ffmpeg", action="store_true", default=flag("COURSE2MD_NO_FFMPEG"),
                        help="do not bundle ffmpeg and ffprobe (COURSE2MD_NO_FFMPEG)")
    parser.add_argument("--no-uv", action="store_true", default=flag("COURSE2MD_NO_UV"),
                        help="Windows/Linux: do not bundle uv (COURSE2MD_NO_UV)")
    parser.add_argument("--no-llama", action="store_true", default=flag("COURSE2MD_NO_LLAMA"),
                        help="Windows/Linux: do not bundle llama-server (COURSE2MD_NO_LLAMA)")
    parser.add_argument("--no-aria2", action="store_true", default=flag("COURSE2MD_NO_ARIA2"),
                        help="Windows: do not bundle aria2 (COURSE2MD_NO_ARIA2)")
    parser.add_argument("--no-apple", action="store_true", default=flag("COURSE2MD_NO_APPLE"),
                        help="build without the Apple speech module and mlx.metallib "
                             "(COURSE2MD_NO_APPLE)")
    parser.add_argument("--signing-identity", default=environ.get("APPLE_SIGNING_IDENTITY") or "-",
                        help="macOS codesign identity; '-' signs ad hoc (APPLE_SIGNING_IDENTITY)")
    parser.add_argument("--notary-key", default=environ.get("APPLE_API_KEY_PATH"), metavar="PATH",
                        help="App Store Connect API key file for notarization (APPLE_API_KEY_PATH)")
    parser.add_argument("--notary-key-id", default=environ.get("APPLE_API_KEY_ID"),
                        help="(APPLE_API_KEY_ID)")
    parser.add_argument("--notary-issuer", default=environ.get("APPLE_API_ISSUER"),
                        help="(APPLE_API_ISSUER)")
    return parser.parse_args(argv)


def main():
    args = parse_args()
    system = platform.system()
    if args.app_only and system != "Darwin":
        raise SystemExit("--app-only builds a macOS .app")
    distribution = not args.debug and not args.app_only
    # Release builds, and CI's development packages, stop if a tool is missing.
    tools_required = distribution or args.require_tools
    # Fail before expensive builds if the platform's packaging tools are absent.
    try:
        if system == "Darwin" and not args.app_only:
            import dmgbuild  # noqa: F401
        elif system == "Windows":
            from verify_windows_icon import verify_windows_icon
    except ImportError as error:
        raise SystemExit("Install packaging tools: python -m pip install -r "
                         "desktop/scripts/requirements-packaging.txt") from error
    if tools_required and not args.no_ffmpeg and ffmpeg_bundle.supported(system, platform.machine()):
        if missing := ffmpeg_bundle.missing_build_tools(TOOL_CACHE):
            raise SystemExit(f"Building the bundled ffmpeg needs {', '.join(missing)}: python -m pip "
                             "install -r desktop/scripts/requirements-packaging.txt (nasm: your "
                             "system package manager)")
    profile = "debug" if args.debug else "release"
    flags = [] if args.debug else ["--release", *(["--locked"] if distribution else [])]
    revisions = {
        name: subprocess.check_output(
            ["git", "-C", str(ROOT / ".deps" / name), "rev-parse", "HEAD"], text=True
        ).strip()
        for name in ("zed", "component")
    }
    if distribution:
        if not (ROOT / "sources.lock.json").is_file():
            raise SystemExit("Freeze the tested source revisions with scripts/sources.py --freeze before release")
        expected = json.loads((ROOT / "sources.lock.json").read_text())
        if revisions != expected:
            raise SystemExit("Prepared sources differ from the tested release revisions; prepare sources with --locked")
    if not args.no_build:
        # build.rs reads COURSE2MD_NO_APPLE; pass the flag on to it.
        env = {**os.environ, "COURSE2MD_NO_APPLE": "1"} if args.no_apple else None
        run("cargo", "build", *flags, env=env)
        run("cargo", "build", "--manifest-path", str(ROOT / "Cargo.toml"), *flags, env=env)
    version = tomllib.loads((PROJECT / "Cargo.toml").read_text(encoding="utf-8"))["package"]["version"]
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
                **macos_versions(version),
                "NSHighResolutionCapable": True, "NSPrincipalClass": "NSApplication",
                "LSMinimumSystemVersion": "15.0", "CFBundleIconFile": "course2md.icns",
            }, stream)
        shutil.copy2(ROOT / "assets/icon.icns", resources / "course2md.icns")
        # MLX first searches beside the executable for mlx.metallib. Keeping
        # that layout avoids changing the CLI's working directory and breaking
        # user-supplied relative source, output, and model paths.
        # build.rs copies it beside the CLI executable; see the note there.
        metal = PROJECT / "target" / profile / "mlx.metallib"
        if metal.is_file():
            shutil.copy2(metal, resources / "mlx.metallib")
            (binaries / "mlx.metallib").symlink_to("../Resources/mlx.metallib")
        elif platform.machine() == "arm64" and not args.no_apple:
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
    # The runtime searches beside the executable for yt-dlp; --no-ytdlp skips bundling.
    if not args.no_ytdlp:
        bundled = bundle_ytdlp(binaries / f"yt-dlp{suffix}", required=tools_required,
                               local=args.ytdlp)
        if bundled:
            print(f"bundled {bundled.name}")
    # ffmpeg and ffprobe sit beside the engine too; it runs those before PATH.
    tool_licenses = []
    if not args.no_ffmpeg:
        bundled = ffmpeg_bundle.bundle(binaries, TOOL_CACHE, required=tools_required)
        if bundled:
            placed, tool_licenses = bundled
            print("bundled " + ", ".join(path.name for path in placed))
    # Windows/Linux: uv beside the engine (NPU worker) and llama-server in
    # llama.cpp/ (GPU/CPU transcription); the app searches both before PATH.
    skip = [name for name, off in (("uv", args.no_uv), ("llama.cpp", args.no_llama)) if off]
    for name in prebuilt.bundle(system, platform.machine(), binaries, base,
                                TOOL_CACHE / "downloads", required=tools_required, skip=skip):
        print(f"bundled {name}")
    if system == "Windows" and not args.no_aria2:
        if bundle_aria2(binaries, base, required=tools_required):
            print("bundled aria2c.exe")
    if tools_required:
        verify_tools(binaries, system, skip=[*skip, *(["ffmpeg"] if args.no_ffmpeg else [])])
    if system == "Windows":
        verify_windows_icon(binaries / "course2md-desktop.exe")
    copy_licenses(base, tool_licenses)
    shutil.copy2(PROJECT / "readme.md", base / "README.md")
    # A development archive follows main and must not claim the previous release
    # revisions. Release builds have already checked this snapshot against the lock.
    (base / "sources.lock.json").write_text(json.dumps(revisions, indent=2) + "\n")
    if system == "Darwin":
        copy_licenses(resources, tool_licenses)
        shutil.copy2(base / "sources.lock.json", resources / "sources.lock.json")
        identity = "-" if args.app_only else args.signing_identity
        signed = identity != "-"
        signing = ["--force", "--sign", identity]
        if signed:
            signing.extend(["--options", "runtime", "--timestamp"])
        run("codesign", *signing, str(binaries / "course2md"))
        run("codesign", *signing, str(binaries / "course2md-desktop"))
        # Bundled ffmpeg/ffprobe are nested code: hardened runtime, no entitlements.
        for tool in ("ffmpeg", "ffprobe"):
            if (binaries / tool).is_file():
                run("codesign", *signing, str(binaries / tool))
        # Sign the bundled yt-dlp as nested code; --deep verification and notarization require it.
        # Its PyInstaller payload loads a Python framework with a different Team ID, so
        # library validation must be disabled for this nested executable.
        ytdlp = binaries / "yt-dlp"
        if ytdlp.is_file():
            run(
                "codesign",
                *signing,
                "--entitlements",
                str(ROOT / "assets/yt-dlp.entitlements"),
                str(ytdlp),
            )
        run("codesign", *signing, str(bundle))
        run("codesign", "--verify", "--deep", "--strict", str(bundle))
        if args.app_only:
            print(bundle)
            return
        notarization = [args.notary_key, args.notary_key_id, args.notary_issuer]
        if signed and not all(notarization):
            raise SystemExit(
                "Signing identity is set but notarization credentials are incomplete "
                "(--notary-key/--notary-key-id/--notary-issuer); the DMG would be "
                "blocked by Gatekeeper. Provide them or sign ad hoc with --signing-identity -."
            )
        if signed:
            upload = base.parent / "notarization.zip"
            run("ditto", "-c", "-k", "--keepParent", str(bundle), str(upload))
            notarize(upload, *notarization)
            run("xcrun", "stapler", "staple", str(bundle))
            upload.unlink()
        dmg = base.parent / f"course2md-gui-macos-{platform.machine()}.dmg"
        if dmg.exists():
            dmg.unlink()
        build_install_dmg(bundle, dmg)
        if signed:
            run("codesign", "--force", "--sign", identity, "--timestamp", str(dmg))
            notarize(dmg, *notarization)
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
