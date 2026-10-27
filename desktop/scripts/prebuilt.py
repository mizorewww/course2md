"""Official prebuilt tools that Windows and Linux desktop archives bundle.

- uv runs the Intel NPU worker (it fetches OpenVINO as wheels on first use);
  without it NPU transcription needs a Python with those packages installed.
- llama.cpp's llama-server runs GPU and CPU transcription. The Vulkan build
  carries every CPU backend too, so one build serves NVIDIA, AMD and Intel
  GPUs through their standard drivers and falls back to the CPU without a
  Vulkan driver. CUDA, ROCm and SYCL builds are 150-400 MB each and stay a
  separate install. macOS uses the Apple speech module instead.

Each archive is pinned by sha256 from its GitHub release (`gh release view
<tag> --json assets`), and so is each license fetched from the tagged source.
"""
from dataclasses import dataclass, field
import fnmatch
import shutil
import tarfile
import zipfile

from ffmpeg_bundle import download

UV_VERSION = "0.13.5"
LLAMA_VERSION = "b11925"
_UV = f"https://github.com/astral-sh/uv/releases/download/{UV_VERSION}"
_UV_SOURCE = f"https://raw.githubusercontent.com/astral-sh/uv/{UV_VERSION}"
_LLAMA = f"https://github.com/ggml-org/llama.cpp/releases/download/{LLAMA_VERSION}"
_LLAMA_LICENSE = (f"https://raw.githubusercontent.com/ggml-org/llama.cpp/{LLAMA_VERSION}/LICENSE",
                  "94f29bbed6a22c35b992c5c6ebf0e7c92f13b836b90f36f461c9cf2f0f1d010d")
_UV_LICENSES = {
    "LICENSE-uv-MIT": (f"{_UV_SOURCE}/LICENSE-MIT",
                       "860e3d7a86b84e6a7012c7a635fc64df475cebc6cce34dfeb73a5982ec58176c"),
    "LICENSE-uv-APACHE": (f"{_UV_SOURCE}/LICENSE-APACHE",
                          "c71d239df91726fc519c6eb72d318ec65820627232b2f796219e87dcf35d0ab4"),
}


def _LLAMA_SERVER_LIBRARIES(pattern):
    """llama-server's own libraries and the ggml backends it loads (every CPU
    variant and Vulkan); the archives' other tools and the RPC backend stay out."""
    return tuple(pattern.format(name) for name in (
        "llama-server-impl", "llama-common", "llama", "mtmd", "ggml", "ggml-base", "ggml-cpu-*",
        "ggml-vulkan"))


@dataclass(frozen=True)
class Prebuilt:
    name: str
    url: str
    sha256: str
    # Archive members to keep, as fnmatch patterns on their file names.
    keep: tuple
    # Directory under the app's binaries; "" places them beside the engine.
    subdir: str = ""
    # License file name -> archive member name, or (url, sha256) of a source file.
    licenses: dict = field(default_factory=dict)


PREBUILT = {
    ("Windows", "AMD64"): [
        Prebuilt("uv", f"{_UV}/uv-x86_64-pc-windows-msvc.zip",
                 "a38b8d1902b7cfbc76b423ba1bb456f623f42064cdc72628b6075b89afd1019a",
                 keep=("uv.exe",), licenses=_UV_LICENSES),
        Prebuilt("llama.cpp", f"{_LLAMA}/llama-{LLAMA_VERSION}-bin-win-vulkan-x64.zip",
                 "cea8a40a2b3e5996df0e0c1e598f65106dd60adc19da9d234f6ef9773b95349b",
                 keep=("llama-server.exe", *_LLAMA_SERVER_LIBRARIES("{}.dll"), "libomp.dll"),
                 subdir="llama.cpp",
                 licenses={"LICENSE-llama.cpp": _LLAMA_LICENSE,
                           "LICENSE-llvm-openmp": "LICENSE-LLVM-OpenMP"}),
    ],
    ("Linux", "x86_64"): [
        Prebuilt("uv", f"{_UV}/uv-x86_64-unknown-linux-gnu.tar.gz",
                 "f68d291c75783122dad58ab77def0e13f3233f358731db5d6d7e8715603b928f",
                 keep=("uv",), licenses=_UV_LICENSES),
        # RUNPATH is $ORIGIN, so the libraries load from the same directory.
        Prebuilt("llama.cpp", f"{_LLAMA}/llama-{LLAMA_VERSION}-bin-ubuntu-vulkan-x64.tar.gz",
                 "fd88a932b46ec418e9eff979c35ba3cbbb87f8b6ad308f2d02f095ee141d1b73",
                 keep=("llama-server", *_LLAMA_SERVER_LIBRARIES("lib{}.so"),
                       *_LLAMA_SERVER_LIBRARIES("lib{}.so.*")),
                 subdir="llama.cpp",
                 licenses={"LICENSE-llama.cpp": "LICENSE"}),
    ],
}


def _members(archive):
    """(file name, bytes or symlink target, is_link, mode) for every regular file or link."""
    if archive.suffix == ".zip":
        with zipfile.ZipFile(archive) as zipped:
            for info in zipped.infolist():
                if not info.is_dir():
                    yield info.filename.rsplit("/", 1)[-1], zipped.read(info), False, 0o755
        return
    with tarfile.open(archive) as tar:
        for info in tar.getmembers():
            name = info.name.rsplit("/", 1)[-1]
            if info.issym():
                if "/" in info.linkname or info.linkname in ("", ".", ".."):
                    raise OSError(f"{archive.name}: unexpected link {info.name} -> {info.linkname}")
                yield name, info.linkname, True, 0
            elif info.isfile():
                yield name, tar.extractfile(info).read(), False, info.mode | 0o644


def stage(tool, binaries, licenses, downloads):
    """Place `tool`'s kept files under binaries (/subdir) and its licenses in licenses."""
    archive = download(tool.url, tool.sha256, downloads / tool.url.rsplit("/", 1)[-1])
    dest = binaries / tool.subdir if tool.subdir else binaries
    dest.mkdir(parents=True, exist_ok=True)
    wanted = {member for member in tool.licenses.values() if isinstance(member, str)}
    found, placed = {}, []
    for name, payload, is_link, mode in _members(archive):
        if name in wanted:
            found[name] = payload
        if not any(fnmatch.fnmatch(name, pattern) for pattern in tool.keep):
            continue
        target = dest / name
        target.unlink(missing_ok=True)
        if is_link:
            target.symlink_to(payload)
        else:
            target.write_bytes(payload)
            target.chmod(mode & 0o777)
        placed.append(target)
    if not (dest / tool.keep[0]).is_file():
        raise OSError(f"{archive.name} has no {tool.keep[0]}")
    for license_name, source in tool.licenses.items():
        if isinstance(source, str):
            if source not in found:
                raise OSError(f"{archive.name} has no {source}")
            (licenses / license_name).write_bytes(found[source])
        else:
            url, digest = source
            shutil.copy2(download(url, digest, downloads / f"{tool.name}-{license_name}"),
                         licenses / license_name)
    return placed


def bundle(system, machine, binaries, licenses, downloads, required, skip=()):
    """Stage every prebuilt tool for this platform except those named in skip."""
    bundled = []
    for tool in PREBUILT.get((system, machine), []):
        if tool.name in skip:
            continue
        try:
            stage(tool, binaries, licenses, downloads)
            bundled.append(tool.name)
        except (OSError, KeyError, zipfile.BadZipFile, tarfile.TarError) as error:
            if required:
                raise SystemExit(f"Release builds must bundle {tool.name}: {error}") from error
            print(f"warning: {tool.name} not bundled ({error}); the app will look for it on PATH")
    return bundled
