"""ffmpeg and ffprobe for the desktop archives.

Desktop packages carry their own LGPL-2.1 ffmpeg, so a downloaded app converts
videos without a separate install. Every platform builds the same pinned
upstream release from source with only FFmpeg's built-in codecs plus dav1d
(AV1) and the system's TLS: Secure Transport on macOS, Schannel on Windows
and GnuTLS (linked dynamically, LGPL-2.1) on Linux. No reputable LGPL arm64
macOS build exists, and the general-purpose LGPL builds for Windows link
dozens of further libraries into a 90 MB libavcodec; this build is about
25 MB per program and needs no third-party notices beyond dav1d's.

Windows binaries are cross-compiled with the pinned llvm-mingw toolchain on
Linux or macOS (`python3 ffmpeg_bundle.py --target Windows`); the Windows
packaging job takes them from the cache that a Linux job fills.

To update, take each source's sha256 from its upstream checksum (cross-check
the ffmpeg tarball with Homebrew's ffmpeg formula) and llvm-mingw's from its
GitHub release (`gh release view <tag> -R mstorsjo/llvm-mingw --json assets`).
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import tarfile
import urllib.request

FFMPEG_VERSION = "9.0.2"
FFMPEG_SOURCE = (f"https://ffmpeg.org/releases/ffmpeg-{FFMPEG_VERSION}.tar.xz",
                 "8c3850283eb25fa026482078a04051e0be17347b09ef81a0849bec15a96e002e")
DAV1D_VERSION = "1.5.4"
DAV1D_SOURCE = (f"https://downloads.videolan.org/pub/videolan/dav1d/{DAV1D_VERSION}/"
                f"dav1d-{DAV1D_VERSION}.tar.xz",
                "686616b7c69eb88d44459391ab25cac13b6647a3b288835c5784e71c1514a5c5")
LLVM_MINGW_VERSION = "20261020"
_LLVM_MINGW = (f"https://github.com/mstorsjo/llvm-mingw/releases/download/{LLVM_MINGW_VERSION}/"
               f"llvm-mingw-{LLVM_MINGW_VERSION}-ucrt")
# The cross toolchain for each build host.
LLVM_MINGW = {
    ("Linux", "x86_64"): (f"{_LLVM_MINGW}-ubuntu-22.04-x86_64.tar.xz",
                          "55064b60b991a1e1e8090b8c2f84e82b22cfe5903d12c159d65064cf5e8ae335"),
    ("Darwin", "arm64"): (f"{_LLVM_MINGW}-macos-universal.tar.xz",
                          "e9c13e4ede38e715003ffc674d4d00a1088df8235439d1f0c16188ff0704c17e"),
}
WINDOWS_PREFIX = "x86_64-w64-mingw32-"
# Windows has no system zlib to link against (hash cross-checked with Homebrew's formula).
ZLIB_VERSION = "1.3.2"
ZLIB_SOURCE = (f"https://zlib.net/fossils/zlib-{ZLIB_VERSION}.tar.gz",
               "bb329a0a2cd0274d05519d61c667c062e06990d72e125ee2dfa8de64f0119d16")
MACOS_DEPLOYMENT_TARGET = "15.0"
BUILD_REVISION = 3
BUILD_ENVIRONMENT = {"PATH", "HOME", "TMPDIR", "TEMP", "TMP", "LANG", "LC_ALL", "DEVELOPER_DIR",
                     "SDKROOT", "SYSTEMROOT"}
# Which platform each build targets, by the packaging host.
TARGETS = {("Darwin", "arm64"), ("Linux", "x86_64"), ("Windows", "AMD64")}


def configure_flags(system):
    """ffmpeg configure options. Autodetection is off so a build machine's own
    libraries (Homebrew, apt) never leak into the archive; every external
    dependency is named here and is LGPL-2.1-compatible."""
    flags = [
        "--disable-autodetect", "--disable-shared", "--enable-static",
        "--disable-debug", "--disable-doc", "--disable-ffplay",
        "--enable-libdav1d", "--enable-zlib",
        # The engine generates test tones with -f lavfi; no capture devices.
        "--disable-indevs", "--enable-indev=lavfi", "--disable-outdevs",
        "--pkg-config-flags=--static",
    ]
    if system == "Darwin":
        target = f"-mmacosx-version-min={MACOS_DEPLOYMENT_TARGET}"
        flags += ["--cc=clang", "--enable-securetransport", f"--extra-cflags={target}",
                  f"--extra-ldflags={target}"]
    elif system == "Linux":
        # yt-dlp hands some HLS/DASH downloads to ffmpeg over https.
        flags += ["--enable-gnutls"]
    elif system == "Windows":
        flags += ["--enable-cross-compile", "--target-os=mingw32", "--arch=x86_64",
                  f"--cross-prefix={WINDOWS_PREFIX}", "--enable-schannel", "--enable-w32threads",
                  # The toolchain's own runtime goes into the programs.
                  "--extra-ldflags=-static"]
    return flags


def supported(system, machine):
    return (system, machine) in TARGETS


def sha256_file(path):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def download(url, sha256, dest):
    """Fetch url to dest once, verifying its sha256; a cached file is re-verified."""
    if dest.is_file() and sha256_file(dest) == sha256:
        return dest
    dest.parent.mkdir(parents=True, exist_ok=True)
    tmp = dest.with_name(dest.name + ".download")
    try:
        with urllib.request.urlopen(url, timeout=300) as response, tmp.open("wb") as out:
            shutil.copyfileobj(response, out)
        digest = sha256_file(tmp)
        if digest != sha256:
            raise OSError(f"{url} checksum mismatch: {digest}")
        tmp.replace(dest)
    finally:
        tmp.unlink(missing_ok=True)
    return dest


def cache_key(system, machine):
    """Names a cached build by everything that shapes it; raise BUILD_REVISION
    when the build steps change without a new pin or option."""
    recipe = [str(BUILD_REVISION), *FFMPEG_SOURCE, *DAV1D_SOURCE, *configure_flags(system)]
    if system == "Windows":
        recipe += [LLVM_MINGW_VERSION, *ZLIB_SOURCE]
    digest = hashlib.sha256("\n".join(recipe).encode()).hexdigest()[:12]
    return f"ffmpeg-{FFMPEG_VERSION}-{system.lower()}-{machine}-{digest}"


def build_notice(system):
    notice = (f"ffmpeg {FFMPEG_VERSION} (LGPL-2.1-or-later), built unmodified from "
              f"{FFMPEG_SOURCE[0]}\nsha256: {FFMPEG_SOURCE[1]}\n"
              f"dav1d {DAV1D_VERSION} (BSD-2-Clause), built unmodified from {DAV1D_SOURCE[0]}\n"
              f"sha256: {DAV1D_SOURCE[1]}\n"
              f"ffmpeg configure options: {' '.join(configure_flags(system))}\n"
              "dav1d is linked statically into ffmpeg and ffprobe")
    if system == "Linux":
        notice += "; GnuTLS and zlib are the system's shared libraries"
    elif system == "Windows":
        url, digest = LLVM_MINGW[("Linux", "x86_64")]
        notice += (f".\nCross-compiled with llvm-mingw {LLVM_MINGW_VERSION} ({url}, sha256 "
                   f"{digest}); its compiler runtime is linked in (LICENSE-llvm-mingw), and "
                   f"zlib {ZLIB_VERSION} (zlib license, {ZLIB_SOURCE[0]}) is linked statically")
    return notice + (".\nThe build recipe is desktop/scripts/ffmpeg_bundle.py in "
                     "https://github.com/mizorewww/course2md. Replace these programs with any "
                     "ffmpeg build of your own: the app uses whichever ffmpeg is beside it.\n")


def _extract(archive, dest):
    dest.mkdir(parents=True, exist_ok=True)
    with tarfile.open(archive) as tar:
        tar.extractall(dest, filter="data")
    (top,) = [path for path in dest.iterdir() if path.is_dir()]
    return top


def _tool(*names):
    for name in names:
        if found := shutil.which(name):
            return found
    raise OSError(f"{' or '.join(names)} is required to build ffmpeg "
                  "(pip install -r desktop/scripts/requirements-packaging.txt)")


def _windows_zlib(work, prefix, env, downloads):
    """zlib for the cross build; Windows has no system copy to link against."""
    source = _extract(download(*ZLIB_SOURCE, downloads / Path(ZLIB_SOURCE[0]).name), work / "zlib")
    make = ["make", "-f", "win32/Makefile.gcc", f"PREFIX={WINDOWS_PREFIX}",
            f"BINARY_PATH={prefix}/bin", f"INCLUDE_PATH={prefix}/include",
            f"LIBRARY_PATH={prefix}/lib"]
    subprocess.run([*make, "libz.a"], cwd=source, check=True, env=env)
    subprocess.run([*make, "install"], cwd=source, check=True, env=env)
    # Found through pkg-config, so its paths stay out of the configure line
    # that `ffmpeg -version` prints.
    (prefix / "lib/pkgconfig").mkdir(parents=True, exist_ok=True)
    (prefix / "lib/pkgconfig/zlib.pc").write_text(
        f"prefix={prefix}\nName: zlib\nDescription: zlib\nVersion: {ZLIB_VERSION}\n"
        "Libs: -L${prefix}/lib -lz\nCflags: -I${prefix}/include\n")
    return source / "LICENSE"


def _build(system, out, downloads):
    work = out.with_name(out.name + ".build")
    shutil.rmtree(work, ignore_errors=True)
    work.mkdir(parents=True)
    prefix = work / "prefix"
    # Only the variables a toolchain needs: a developer's CFLAGS/LDFLAGS or
    # PKG_CONFIG_PATH would otherwise point the build at their own libraries.
    env = {key: value for key, value in os.environ.items() if key in BUILD_ENVIRONMENT}
    if system == "Darwin":
        env["MACOSX_DEPLOYMENT_TARGET"] = MACOS_DEPLOYMENT_TARGET
    env["PKG_CONFIG_PATH"] = str(prefix / "lib/pkgconfig")
    jobs = str(os.cpu_count() or 2)
    host = (platform.system(), platform.machine())
    if system == "Windows" and host not in LLVM_MINGW:
        raise OSError("Windows ffmpeg is cross-compiled on Linux x86_64 or macOS arm64: "
                      "python3 desktop/scripts/ffmpeg_bundle.py --target Windows")
    meson, ninja = _tool("meson"), _tool("ninja")
    pkg_config = _tool("pkg-config", "pkgconf")
    licenses = {}
    cross = []
    if system == "Windows":
        url, digest = LLVM_MINGW[host]
        toolchain = _extract(download(url, digest, downloads / Path(url).name), work / "toolchain")
        env["PATH"] = f"{toolchain / 'bin'}{os.pathsep}{env.get('PATH', '')}"
        # The cross pkg-config must only see the libraries built here.
        env["PKG_CONFIG_LIBDIR"] = env.pop("PKG_CONFIG_PATH")
        licenses["LICENSE-llvm-mingw"] = toolchain / "LICENSE.TXT"
        licenses["LICENSE-zlib"] = _windows_zlib(work, prefix, env, downloads)
        cross_file = work / "cross.ini"
        cross_file.write_text(
            "[binaries]\n"
            f"c = '{WINDOWS_PREFIX}gcc'\nar = '{WINDOWS_PREFIX}ar'\n"
            f"strip = '{WINDOWS_PREFIX}strip'\nwindres = '{WINDOWS_PREFIX}windres'\n"
            f"pkg-config = '{Path(pkg_config).name}'\n"
            "[host_machine]\nsystem = 'windows'\ncpu_family = 'x86_64'\ncpu = 'x86_64'\n"
            "endian = 'little'\n")
        cross = [f"--cross-file={cross_file}"]
    dav1d = _extract(download(*DAV1D_SOURCE, downloads / Path(DAV1D_SOURCE[0]).name),
                     work / "dav1d")
    subprocess.run([meson, "setup", str(work / "dav1d-build"), str(dav1d), *cross,
                    f"--prefix={prefix}", "--libdir=lib", "--buildtype=release",
                    "--default-library=static", "-Denable_tools=false",
                    "-Denable_examples=false", "-Denable_tests=false", "-Denable_docs=false"],
                   check=True, env=env)
    subprocess.run([ninja, "-C", str(work / "dav1d-build"), "install"], check=True, env=env)
    source = _extract(download(*FFMPEG_SOURCE, downloads / Path(FFMPEG_SOURCE[0]).name),
                      work / "ffmpeg")
    # `ffmpeg -version` prints these options; name pkg-config by its PATH
    # name so build-machine paths stay out of the shipped binary.
    subprocess.run(["./configure", f"--pkg-config={Path(pkg_config).name}",
                    *configure_flags(system)], cwd=source, check=True, env=env)
    suffix = ".exe" if system == "Windows" else ""
    programs = [f"ffmpeg{suffix}", f"ffprobe{suffix}"]
    subprocess.run(["make", f"-j{jobs}", *programs], cwd=source, check=True, env=env)
    staged = out.with_name(out.name + ".staged")
    shutil.rmtree(staged, ignore_errors=True)
    (staged / "bin").mkdir(parents=True)
    (staged / "licenses").mkdir()
    for name in programs:
        # The build leaves stripped programs here (ffmpeg_g keeps the symbols).
        shutil.copy2(source / name, staged / "bin" / name)
    shutil.copy2(source / "COPYING.LGPLv2.1", staged / "licenses/LICENSE-ffmpeg")
    shutil.copy2(dav1d / "COPYING", staged / "licenses/LICENSE-dav1d")
    for name, path in licenses.items():
        shutil.copy2(path, staged / "licenses" / name)
    _write_manifest(staged)
    shutil.rmtree(out, ignore_errors=True)
    staged.replace(out)
    shutil.rmtree(work, ignore_errors=True)


MANIFEST = "manifest.json"


def _write_manifest(directory):
    files = {path.relative_to(directory).as_posix(): sha256_file(path)
             for path in sorted(directory.rglob("*")) if path.is_file() and path.name != MANIFEST}
    (directory / MANIFEST).write_text(json.dumps(files, indent=1, sort_keys=True) + "\n")


def _verified(directory):
    """A cached build is reused only while every file matches the hashes taken when it was built."""
    try:
        files = json.loads((directory / MANIFEST).read_text())
    except (OSError, ValueError):
        return False
    return bool(files) and all((directory / name).is_file()
                               and sha256_file(directory / name) == digest
                               for name, digest in files.items())


def missing_build_tools(cache, system=None, machine=None):
    """Build tools a source build still needs, so packaging can stop before compiling."""
    system = system or platform.system()
    machine = machine or platform.machine()
    if _verified(cache / cache_key(system, machine)):
        return []
    if platform.system() == "Windows":
        return ["a Windows ffmpeg cross-built on Linux or macOS (ffmpeg_bundle.py --target Windows)"]
    needed = [("meson",), ("ninja",), ("pkg-config", "pkgconf"), ("make",)]
    if system == "Windows" or machine in ("x86_64", "AMD64"):
        needed.append(("nasm",))
    return [names[0] for names in needed if not any(shutil.which(name) for name in names)]


def prepare(cache, system=None, machine=None):
    """A directory with bin/ (the files to place beside the app) and licenses/.

    Builds are cached under `cache` by their recipe, so a second package run
    and a CI cache hit skip the build; a cached build that no longer matches
    its manifest is rebuilt.
    """
    system = system or platform.system()
    machine = machine or platform.machine()
    if not supported(system, machine):
        raise OSError(f"no bundled ffmpeg for {system} {machine}")
    out = cache / cache_key(system, machine)
    if not _verified(out):
        _build(system, out, cache / "downloads")
    (out / "licenses/ffmpeg-build.txt").write_text(build_notice(system))
    return out


def bundle(binaries, cache, required):
    """Copy ffmpeg and ffprobe into binaries.

    Returns (placed files, license files), or None when not bundled; release
    builds stop instead.
    """
    try:
        prepared = prepare(cache)
        placed = []
        for path in sorted((prepared / "bin").iterdir()):
            shutil.copy2(path, binaries / path.name)
            (binaries / path.name).chmod(0o755)
            placed.append(binaries / path.name)
        return placed, sorted((prepared / "licenses").iterdir())
    except (OSError, subprocess.CalledProcessError, tarfile.TarError) as error:
        if required:
            raise SystemExit(f"Release builds must bundle ffmpeg: {error}") from error
        print(f"warning: ffmpeg not bundled ({error}); the app will use ffmpeg from PATH")
        return None


def main(argv=None):
    parser = argparse.ArgumentParser(description="Build the bundled ffmpeg into the tool cache.")
    parser.add_argument("--target", choices=["Darwin", "Linux", "Windows"],
                        default=platform.system(), help="platform to build for (default: this one)")
    parser.add_argument("--cache", type=Path,
                        default=Path(__file__).resolve().parents[1] / ".cache/tools")
    args = parser.parse_args(argv)
    machine = {"Windows": "AMD64", "Linux": "x86_64", "Darwin": "arm64"}[args.target]
    print(prepare(args.cache, args.target, machine))


if __name__ == "__main__":
    main()
