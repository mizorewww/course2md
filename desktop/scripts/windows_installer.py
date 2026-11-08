#!/usr/bin/env python3
"""The Windows setup program: a per-user Inno Setup installer of the desktop package.

package.py builds it on Windows from the same directory the portable ZIP holds
(desktop/installer/windows/course2md.iss). `smoke` installs a built setup
program, checks it, upgrades it in place and uninstalls it, as CI does.
"""
import argparse
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import time
import tomllib

ROOT = Path(__file__).resolve().parents[1]
PROJECT = ROOT.parent
SCRIPT = ROOT / "installer/windows/course2md.iss"
ICON = ROOT / "assets/icon.ico"
# Inno Setup's AppId. Windows keys the installed program by it: an upgrade
# finds the earlier install through it, so it must never change.
APP_ID = "F33C27B9-52FF-4FEC-9338-1A61D8C8605B"
OUTPUT_NAME = "course2md-desktop-windows-x64-setup"
UNINSTALL_KEY = rf"Software\Microsoft\Windows\CurrentVersion\Uninstall\{{{APP_ID}}}_is1"


def file_version(version):
    """The numeric x.y.z.0 Windows version resources need, from a SemVer release."""
    match = re.fullmatch(r"(\d+)\.(\d+)\.(\d+)(?:-[0-9A-Za-z.]+)?", version)
    if not match:
        raise ValueError(f"Unsupported Windows release version: {version}")
    return ".".join([*match.groups(), "0"])


def compile_arguments(iscc, version, source, output_dir):
    """The ISCC command line that builds the setup program for `source`."""
    defines = {
        "AppId": APP_ID, "AppVersion": version, "FileVersion": file_version(version),
        "SourceDir": str(source), "OutputDir": str(output_dir), "OutputName": OUTPUT_NAME,
        "IconFile": str(ICON),
    }
    return [str(iscc), "/Qp", *(f"/D{name}={value}" for name, value in defines.items()),
            str(SCRIPT)]


def find_iscc(environ=os.environ):
    """Inno Setup's compiler: ISCC names it, then PATH, then the default install folders."""
    if named := environ.get("ISCC"):
        return Path(named)
    if found := shutil.which("iscc"):
        return Path(found)
    for variable in ("ProgramFiles(x86)", "ProgramFiles", "LOCALAPPDATA"):
        base = environ.get(variable)
        if not base:
            continue
        roots = [Path(base)] + ([Path(base) / "Programs"] if variable == "LOCALAPPDATA" else [])
        for root in roots:
            candidates = sorted(root.glob("Inno Setup */ISCC.exe"), reverse=True)
            if candidates:
                return candidates[0]
    return None


def build(source, version, output_dir, required):
    """Compile the setup program; a release build stops without Inno Setup."""
    iscc = find_iscc()
    if iscc is None:
        message = ("Inno Setup 6.3 or later is needed for the Windows installer "
                   "(winget install JRSoftware.InnoSetup, or set ISCC)")
        if required:
            raise SystemExit(message)
        print(f"warning: {message}; only the portable ZIP was built")
        return None
    subprocess.run(compile_arguments(iscc, version, source, output_dir), check=True)
    return Path(output_dir) / f"{OUTPUT_NAME}.exe"


# --- Smoke test (Windows) ---------------------------------------------------

def package_files(directory):
    return sorted(path.relative_to(directory) for path in Path(directory).rglob("*")
                  if path.is_file())


def missing_files(package, installed):
    """Files of the package that the install directory lacks."""
    return [path for path in package_files(package) if not (Path(installed) / path).is_file()]


def doctor_problems(output, install_dir):
    """Bundled tools that `course2md doctor` did not find in the install directory."""
    text = output.casefold()
    expected = {"ffmpeg": ["ffmpeg.exe"], "ffprobe": ["ffprobe.exe"], "aria2c": ["aria2c.exe"],
                "yt-dlp": ["yt-dlp", "yt-dlp.exe"]}
    return [tool for tool, parts in expected.items()
            if f"included with the app · {Path(install_dir, *parts)}".casefold() not in text]


def _uninstall_entry():
    import winreg
    try:
        with winreg.OpenKey(winreg.HKEY_CURRENT_USER, UNINSTALL_KEY) as key:
            return {name: winreg.QueryValueEx(key, name)[0]
                    for name in ("DisplayVersion", "InstallLocation", "UninstallString")}
    except FileNotFoundError:
        return None


def _check(condition, message):
    if not condition:
        raise SystemExit(f"installer smoke test: {message}")
    print(f"ok: {message}")


def _run_setup(installer, log, *extra):
    result = subprocess.run([str(installer), "/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART",
                             f"/LOG={log}", *extra], timeout=900)
    if result.returncode and Path(log).is_file():
        print(Path(log).read_text(encoding="utf-8", errors="replace")[-4000:])
    _check(result.returncode == 0, f"setup exited with 0 ({' '.join(extra) or 'upgrade'})")


def _installed(package, app, version, start_menu):
    missing = missing_files(package, app)
    _check(not missing, f"every package file is installed ({len(missing)} missing: {missing[:5]})")
    _check((app / "unins000.exe").is_file(), "the uninstaller is installed")
    _check(start_menu.is_file(), f"the Start menu shortcut exists ({start_menu})")
    entry = _uninstall_entry()
    _check(entry is not None and entry["DisplayVersion"] == version,
           f"Windows lists course2md {version} under the stable AppId ({entry})")
    _check(Path(entry["InstallLocation"]).resolve() == app.resolve(),
           "the listed install location is the install directory")
    engine = subprocess.run([str(app / "course2md.exe"), "--version"], capture_output=True,
                            text=True, encoding="utf-8", errors="replace", timeout=60)
    _check(engine.returncode == 0 and engine.stdout.startswith(f"course2md {version} "),
           f"the installed engine reports {version} ({engine.stdout.strip()})")
    # Only the system directories on PATH: every tool must come from the install.
    system = Path(os.environ["SystemRoot"])
    env = {**os.environ, "PATH": os.pathsep.join([str(system / "System32"), str(system)])}
    doctor = subprocess.run([str(app / "course2md.exe"), "--lang", "en", "doctor"],
                            capture_output=True, text=True, encoding="utf-8", errors="replace",
                            env=env, timeout=300)
    print(doctor.stdout + doctor.stderr)
    problems = doctor_problems(doctor.stdout, app)
    _check(not problems, f"the engine runs the bundled tools from the install ({problems})")


def smoke(installer, package, work, version):
    installer, package, work = Path(installer).resolve(), Path(package).resolve(), Path(work)
    work.mkdir(parents=True, exist_ok=True)
    app = (work / "course2md app").resolve()
    start_menu = (Path(os.environ["APPDATA"]) /
                  r"Microsoft\Windows\Start Menu\Programs\course2md.lnk")
    _check(_uninstall_entry() is None, "no earlier install is registered")
    # User data the app keeps outside its install directory.
    user_data = [Path(os.environ["APPDATA"]) / "course2md/installer-smoke.txt",
                 Path(os.environ["LOCALAPPDATA"]) / "course2md/models/installer-smoke.txt"]
    for path in user_data:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text("kept\n", encoding="utf-8")

    _run_setup(installer, work / "install.log", f"/DIR={app}")
    _installed(package, app, version, start_menu)

    # Upgrade in place: no /DIR, so setup must find the install through the
    # AppId; a stale file in a directory the package owns must go.
    stale = app / "llama.cpp/stale-from-older-version.dll"
    stale.parent.mkdir(exist_ok=True)
    stale.write_bytes(b"")
    _run_setup(installer, work / "upgrade.log")
    _check(not stale.exists(), "an upgrade replaces llama.cpp/ instead of mixing versions")
    _installed(package, app, version, start_menu)

    uninstaller = app / "unins000.exe"
    result = subprocess.run([str(uninstaller), "/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART"],
                            timeout=600)
    _check(result.returncode == 0, "the uninstaller exited with 0")
    # The uninstaller finishes from a copy of itself in TEMP.
    deadline = time.monotonic() + 120
    while (app.exists() or _uninstall_entry()) and time.monotonic() < deadline:
        time.sleep(2)
    leftover = package_files(app) if app.exists() else []
    _check(not app.exists(), f"the install directory is removed (left: {leftover[:10]})")
    _check(_uninstall_entry() is None, "Windows no longer lists the program")
    _check(not start_menu.exists(), "the Start menu shortcut is removed")
    for path in user_data:
        _check(path.is_file(), f"user data survives uninstall: {path}")
        path.unlink()


def workspace_version():
    manifest = tomllib.loads((PROJECT / "Cargo.toml").read_text(encoding="utf-8"))
    return manifest["workspace"]["package"]["version"]


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    build_parser = commands.add_parser("build", help="compile the setup program for a package")
    build_parser.add_argument("package", type=Path, help="the package directory (the ZIP's contents)")
    build_parser.add_argument("--output", type=Path, help="default: the package's parent")
    smoke_parser = commands.add_parser("smoke", help="install, upgrade and uninstall a setup program")
    smoke_parser.add_argument("installer", type=Path)
    smoke_parser.add_argument("package", type=Path, help="the package directory it was built from")
    smoke_parser.add_argument("--work", type=Path,
                              default=Path(os.environ.get("RUNNER_TEMP") or ".") / "installer-smoke")
    args = parser.parse_args(argv)
    if args.command == "build":
        package = args.package.resolve()
        print(build(package, workspace_version(), args.output or package.parent, required=True))
    else:
        if sys.platform != "win32":
            raise SystemExit("The installer smoke test runs on Windows")
        # doctor's output has check marks the runner's code page lacks.
        sys.stdout.reconfigure(encoding="utf-8", errors="replace")
        smoke(args.installer, args.package, args.work, workspace_version())


if __name__ == "__main__":
    main()
