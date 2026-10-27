"""ffmpeg and ffprobe for the desktop archives.

Desktop packages carry their own LGPL ffmpeg, so a downloaded app converts
videos without a separate install. macOS and Linux build the pinned upstream
release from source with only built-in codecs plus dav1d (AV1); no reputable
LGPL arm64 macOS build exists, and a minimal build is about a tenth of the
size of a general-purpose static one. Windows uses BtbN's LGPL shared build
from a monthly autobuild, which that project keeps for two years.

To update, take each source's sha256 from its upstream checksum (cross-check
the ffmpeg tarball with Homebrew's ffmpeg formula) and the Windows zip's from
the release's checksums.sha256.
"""
import hashlib
import os
from pathlib import Path
import platform
import shutil
import subprocess
import tarfile
import urllib.request
import zipfile

FFMPEG_VERSION = "9.0.2"
FFMPEG_SOURCE = (f"https://ffmpeg.org/releases/ffmpeg-{FFMPEG_VERSION}.tar.xz",
                 "8c3850283eb25fa026482078a04051e0be17347b09ef81a0849bec15a96e002e")
DAV1D_VERSION = "1.5.4"
DAV1D_SOURCE = (f"https://downloads.videolan.org/pub/videolan/dav1d/{DAV1D_VERSION}/"
                f"dav1d-{DAV1D_VERSION}.tar.xz",
                "686616b7c69eb88d44459391ab25cac13b6647a3b288835c5784e71c1514a5c5")
WINDOWS_RELEASE = "autobuild-2026-09-30-13-08"
WINDOWS_BUILD = "ffmpeg-n9.0.2-17-g2a571b6068-win64-lgpl-shared-9.0"
WINDOWS_SHA256 = "7157177b8a6cb2174c1650ba8c71b363f2c78cba5330f88c4c02cf5b2b880646"
WINDOWS_URL = (f"https://github.com/BtbN/FFmpeg-Builds/releases/download/{WINDOWS_RELEASE}/"
               f"{WINDOWS_BUILD}.zip")
# Runtime libraries of the shared Windows build; ffmpeg.exe and ffprobe.exe
# load them from their own directory.
WINDOWS_LIBRARIES = ("avcodec-63.dll", "avdevice-63.dll", "avfilter-12.dll", "avformat-63.dll",
                     "avutil-61.dll", "swresample-7.dll", "swscale-10.dll")
MACOS_DEPLOYMENT_TARGET = "15.0"
BUILD_REVISION = 1
BUILD_ENVIRONMENT = {"PATH", "HOME", "TMPDIR", "TEMP", "TMP", "LANG", "LC_ALL", "DEVELOPER_DIR",
                     "SDKROOT", "SYSTEMROOT"}


def configure_flags(system):
    """ffmpeg configure options. Autodetection is off so a build machine's own
    libraries (Homebrew, apt) never leak into the archive; every external
    dependency is named here and is LGPL-compatible."""
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
    return flags


def supported(system, machine):
    return (system, machine) in {("Darwin", "arm64"), ("Linux", "x86_64"), ("Windows", "AMD64")}


def download(url, sha256, dest):
    """Fetch url to dest once, verifying its sha256; a verified file is reused."""
    if dest.is_file() and hashlib.sha256(dest.read_bytes()).hexdigest() == sha256:
        return dest
    dest.parent.mkdir(parents=True, exist_ok=True)
    tmp = dest.with_name(dest.name + ".download")
    try:
        with urllib.request.urlopen(url, timeout=300) as response, tmp.open("wb") as out:
            shutil.copyfileobj(response, out)
        digest = hashlib.sha256(tmp.read_bytes()).hexdigest()
        if digest != sha256:
            raise OSError(f"{url} checksum mismatch: {digest}")
        tmp.replace(dest)
    finally:
        tmp.unlink(missing_ok=True)
    return dest


def cache_key(system, machine):
    """Names a cached build by everything that shapes it; raise BUILD_REVISION
    when the build steps change without a new pin or option."""
    recipe = "\n".join([str(BUILD_REVISION), *FFMPEG_SOURCE, *DAV1D_SOURCE, WINDOWS_SHA256,
                        *configure_flags(system)])
    digest = hashlib.sha256(recipe.encode()).hexdigest()[:12]
    return f"ffmpeg-{FFMPEG_VERSION}-{system.lower()}-{machine}-{digest}"


def build_notice(system):
    if system == "Windows":
        return (f"ffmpeg {FFMPEG_VERSION} (LGPL shared build {WINDOWS_BUILD})\n"
                f"Binary: {WINDOWS_URL}\nsha256: {WINDOWS_SHA256}\n"
                "Source and build scripts: https://github.com/BtbN/FFmpeg-Builds "
                f"(release {WINDOWS_RELEASE}); FFmpeg source: https://ffmpeg.org/download.html\n")
    return (f"ffmpeg {FFMPEG_VERSION}, built unmodified from {FFMPEG_SOURCE[0]}\n"
            f"sha256: {FFMPEG_SOURCE[1]}\n"
            f"dav1d {DAV1D_VERSION}, built unmodified from {DAV1D_SOURCE[0]}\n"
            f"sha256: {DAV1D_SOURCE[1]}\n"
            f"ffmpeg configure options: {' '.join(configure_flags(system))}\n"
            "Both are linked statically into ffmpeg and ffprobe. Replace them with any "
            "ffmpeg build of your own: the app uses whichever ffmpeg is beside it.\n")


def _extract(archive, dest):
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


def _build_from_source(system, out, downloads):
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
    meson, ninja = _tool("meson"), _tool("ninja")
    pkg_config = _tool("pkg-config", "pkgconf")
    (work / "dav1d").mkdir()
    dav1d = _extract(download(*DAV1D_SOURCE, downloads / Path(DAV1D_SOURCE[0]).name), work / "dav1d")
    subprocess.run([meson, "setup", str(work / "dav1d-build"), str(dav1d), f"--prefix={prefix}",
                    "--libdir=lib", "--buildtype=release", "--default-library=static",
                    "-Denable_tools=false", "-Denable_examples=false", "-Denable_tests=false",
                    "-Denable_docs=false"], check=True, env=env)
    subprocess.run([ninja, "-C", str(work / "dav1d-build"), "install"], check=True, env=env)
    (work / "ffmpeg").mkdir()
    source = _extract(download(*FFMPEG_SOURCE, downloads / Path(FFMPEG_SOURCE[0]).name),
                      work / "ffmpeg")
    # `ffmpeg -version` prints these options; name pkg-config by its PATH
    # name so build-machine paths stay out of the shipped binary.
    subprocess.run(["./configure", f"--pkg-config={Path(pkg_config).name}",
                    *configure_flags(system)], cwd=source, check=True, env=env)
    subprocess.run(["make", f"-j{jobs}", "ffmpeg", "ffprobe"], cwd=source, check=True, env=env)
    staged = out.with_name(out.name + ".staged")
    shutil.rmtree(staged, ignore_errors=True)
    (staged / "bin").mkdir(parents=True)
    (staged / "licenses").mkdir()
    for name in ("ffmpeg", "ffprobe"):
        # The build leaves stripped programs here (ffmpeg_g keeps the symbols).
        shutil.copy2(source / name, staged / "bin" / name)
    shutil.copy2(source / "COPYING.LGPLv2.1", staged / "licenses/LICENSE-ffmpeg")
    shutil.copy2(dav1d / "COPYING", staged / "licenses/LICENSE-dav1d")
    staged.replace(out)
    shutil.rmtree(work, ignore_errors=True)


def _unpack_windows(out, downloads):
    archive = download(WINDOWS_URL, WINDOWS_SHA256, downloads / f"{WINDOWS_BUILD}.zip")
    staged = out.with_name(out.name + ".staged")
    shutil.rmtree(staged, ignore_errors=True)
    (staged / "bin").mkdir(parents=True)
    (staged / "licenses").mkdir()
    with zipfile.ZipFile(archive) as zipped:
        for name in ("ffmpeg.exe", "ffprobe.exe", *WINDOWS_LIBRARIES):
            (staged / "bin" / name).write_bytes(zipped.read(f"{WINDOWS_BUILD}/bin/{name}"))
        (staged / "licenses/LICENSE-ffmpeg").write_bytes(zipped.read(f"{WINDOWS_BUILD}/LICENSE.txt"))
    staged.replace(out)


def missing_build_tools(cache, system=None, machine=None):
    """Build tools a source build still needs, so packaging can stop before compiling."""
    system = system or platform.system()
    machine = machine or platform.machine()
    if system == "Windows" or (cache / cache_key(system, machine) / "bin").is_dir():
        return []
    needed = [("meson",), ("ninja",), ("pkg-config", "pkgconf"), ("make",)]
    if machine in ("x86_64", "AMD64"):
        needed.append(("nasm",))
    return [names[0] for names in needed if not any(shutil.which(name) for name in names)]


def prepare(cache, system=None, machine=None):
    """A directory with bin/ (the files to place beside the app) and licenses/.

    Builds are cached under `cache` by their recipe, so a second package run
    and a CI cache hit skip the build.
    """
    system = system or platform.system()
    machine = machine or platform.machine()
    if not supported(system, machine):
        raise OSError(f"no bundled ffmpeg for {system} {machine}")
    out = cache / cache_key(system, machine)
    if not (out / "bin").is_dir():
        downloads = cache / "downloads"
        if system == "Windows":
            _unpack_windows(out, downloads)
        else:
            _build_from_source(system, out, downloads)
    (out / "licenses/ffmpeg-build.txt").write_text(build_notice(system))
    return out


def bundle(binaries, cache, required):
    """Copy ffmpeg, ffprobe (and on Windows their libraries) into binaries.

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
    except (OSError, subprocess.CalledProcessError, KeyError, zipfile.BadZipFile,
            tarfile.TarError) as error:
        if required:
            raise SystemExit(f"Release builds must bundle ffmpeg: {error}") from error
        print(f"warning: ffmpeg not bundled ({error}); the app will use ffmpeg from PATH")
        return None
