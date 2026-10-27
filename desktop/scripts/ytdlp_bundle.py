"""yt-dlp for the desktop archives: the official PyInstaller "onedir" builds.

The one-file builds unpack a whole Python into a temporary directory on every
run (8-14 s per call on macOS, which every online-video preview paid); the
onedir builds start in well under a second. Each archive is a `yt-dlp`
executable beside its `_internal` directory, placed in a `yt-dlp` directory:
beside the engine on Windows and Linux, in `Contents/Resources` of the macOS
app (Contents/MacOS may hold only code). macOS builds are universal; the app is
arm64-only, so their Mach-O files are thinned to arm64, and the Python
framework links the release zip flattened into copies are restored.

The version and hashes come from the release's SHA2-256SUMS.
"""
from pathlib import Path, PurePosixPath
import platform
import shutil
import subprocess
import zipfile

from ffmpeg_bundle import download

YTDLP_VERSION = "2026.08.19"
YTDLP_ZIPS = {
    ("Darwin", "arm64"): ("yt-dlp_macos.zip", "yt-dlp_macos",
                          "07e54b0865303c864006925913bce2604f8ee8cc6f18699bac9c309f9328a6d8"),
    ("Windows", "AMD64"): ("yt-dlp_win.zip", "yt-dlp.exe",
                           "30b4c14aafab6082becff7881e41b76df46dc43ea7633479410a91e29da492bf"),
    ("Windows", "ARM64"): ("yt-dlp_win_arm64.zip", "yt-dlp.exe",
                           "2ad49db7429a6204721f711a44199a8c9fa8d5df2183dd7fedc4ef6c9e2e50d3"),
    ("Linux", "x86_64"): ("yt-dlp_linux.zip", "yt-dlp_linux",
                          "32e72032766bef9199d99d15beb69fd52e46df8f8b06f0d8745db59e04d339e9"),
    ("Linux", "aarch64"): ("yt-dlp_linux_aarch64.zip", "yt-dlp_linux_aarch64",
                           "4e27ad43f3a34bacffd078694eb3edbb4e3b378e7da44edab2be02e98555516e"),
}
DIRECTORY = "yt-dlp"
MACHO_MAGICS = {b"\xcf\xfa\xed\xfe", b"\xce\xfa\xed\xfe", b"\xca\xfe\xba\xbe", b"\xbe\xba\xfe\xca"}


def is_macho(path):
    if path.is_symlink() or not path.is_file():
        return False
    with path.open("rb") as stream:
        return stream.read(4) in MACHO_MAGICS


def _extract(archive, dest, executable, name):
    with zipfile.ZipFile(archive) as zipped:
        for info in zipped.infolist():
            member = PurePosixPath(info.filename)
            if member.is_absolute() or ".." in member.parts:
                raise OSError(f"{archive.name}: unsafe member {info.filename}")
            target = dest.joinpath(*member.parts)
            if info.is_dir():
                target.mkdir(parents=True, exist_ok=True)
                continue
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(zipped.read(info))
            mode = (info.external_attr >> 16) & 0o777
            target.chmod(mode or 0o644)
    exe = dest / executable
    if not exe.is_file() or not (dest / "_internal").is_dir():
        raise OSError(f"{archive.name} is not a onedir build ({executable} and _internal/)")
    exe.rename(dest / name)
    (dest / name).chmod(0o755)


def _same(a, b):
    if a.is_dir() and b.is_dir():
        left = sorted(p.relative_to(a) for p in a.rglob("*"))
        right = sorted(p.relative_to(b) for p in b.rglob("*"))
        return left == right and all(_same(a / p, b / p) for p in left if (a / p).is_file())
    return a.is_file() and b.is_file() and a.read_bytes() == b.read_bytes()


def _link(path, target):
    """Replace path with a relative symlink to target when both hold the same bytes."""
    if path.is_symlink() or not path.exists() or not _same(path, path.parent / target):
        return
    if path.is_dir():
        shutil.rmtree(path)
    else:
        path.unlink()
    path.symlink_to(target)


def _restore_framework(internal):
    """Turn the copies the zip holds back into the framework's links."""
    for framework in internal.glob("*.framework"):
        versions = [v for v in (framework / "Versions").iterdir()
                    if v.name != "Current" and v.is_dir() and not v.is_symlink()]
        if len(versions) != 1:
            continue
        version = versions[0].name
        name = framework.stem
        _link(framework / "Versions/Current", version)
        _link(framework / name, f"Versions/Current/{name}")
        _link(framework / "Resources", "Versions/Current/Resources")
        _link(internal / name, f"{framework.name}/Versions/Current/{name}")


def _thin(directory, arch):
    for path in sorted(directory.rglob("*")):
        if not is_macho(path):
            continue
        archs = subprocess.run(["lipo", "-archs", str(path)], capture_output=True,
                               text=True).stdout.split()
        if len(archs) > 1 and arch in archs:
            subprocess.run(["lipo", str(path), "-thin", arch, "-output", str(path)], check=True)


def stage(dest_parent, downloads, system=None, machine=None):
    """Place yt-dlp/{yt-dlp, _internal} under dest_parent; returns the executable."""
    system = system or platform.system()
    machine = machine or platform.machine()
    if (system, machine) not in YTDLP_ZIPS:
        raise OSError(f"no bundled yt-dlp for {system} {machine}")
    asset, executable, digest = YTDLP_ZIPS[(system, machine)]
    url = f"https://github.com/yt-dlp/yt-dlp/releases/download/{YTDLP_VERSION}/{asset}"
    archive = download(url, digest, downloads / f"{YTDLP_VERSION}-{asset}")
    dest = dest_parent / DIRECTORY
    shutil.rmtree(dest, ignore_errors=True)
    dest.mkdir(parents=True)
    name = "yt-dlp.exe" if system == "Windows" else "yt-dlp"
    _extract(archive, dest, executable, name)
    if system == "Darwin":
        _restore_framework(dest / "_internal")
        _thin(dest, machine)
    return dest / name


def signing_order(directory):
    """Mach-O files and frameworks under directory, innermost first, as codesign wants them.

    A framework is signed as a whole (its Versions/<v> directory) after the
    files inside it; the executable comes last."""
    frameworks = sorted({p for p in directory.rglob("*.framework") if p.is_dir()})
    inside = lambda path: any(f in path.parents for f in frameworks)
    files = [p for p in sorted(directory.rglob("*")) if is_macho(p) and not inside(p)]
    executable = directory / "yt-dlp"
    files = [p for p in files if p != executable]
    versions = [f / "Versions" / "Current" for f in frameworks
                if (f / "Versions" / "Current").exists()]
    return sorted(files, key=lambda p: -len(p.parts)) + [v.resolve() for v in versions] + [executable]

