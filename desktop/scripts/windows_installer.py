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


def programs(source):
    """The package's executables, relative with backslashes: setup and the
    uninstaller stop exactly these when they run from the install."""
    return sorted("\\".join(path.relative_to(source).parts)
                  for path in Path(source).rglob("*.exe"))


def compile_arguments(iscc, version, source, output_dir, sign=None):
    """The ISCC command line that builds the setup program for `source`.

    `sign`, a SignTool command (sign_command), signs the setup program and
    the uninstaller it embeds."""
    defines = {
        "AppId": APP_ID, "AppVersion": version, "FileVersion": file_version(version),
        "SourceDir": str(source), "OutputDir": str(output_dir), "OutputName": OUTPUT_NAME,
        "IconFile": str(ICON), "Programs": "|".join(programs(source)),
    }
    signing = [f"/S{SIGN_TOOL}={sign}", "/DSign"] if sign else []
    return [str(iscc), "/Qp", *(f"/D{name}={value}" for name, value in defines.items()),
            *signing, str(SCRIPT)]


SIGN_TOOL = "course2md"
TIMESTAMP_URL = "http://timestamp.digicert.com"


def sign_command(signtool, pfx, password):
    """Inno Setup's SignTool command for Authenticode with a .pfx: in it $q is a
    quote, $$ a dollar sign and $f the quoted file to sign."""
    def quoted(text):
        return "$q" + str(text).replace("$", "$$") + "$q"
    return (f"{quoted(signtool)} sign /f {quoted(pfx)} /p {quoted(password)} /fd sha256 "
            f"/tr {TIMESTAMP_URL} /td sha256 $f")


def find_signtool(environ=os.environ):
    """The newest Windows SDK's x64 signtool.exe."""
    base = environ.get("ProgramFiles(x86)")
    found = sorted(Path(base).glob("Windows Kits/10/bin/*/x64/signtool.exe")) if base else []
    return found[-1] if found else None


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


def build(source, version, output_dir, required, sign=None):
    """Compile the setup program; a release build stops without Inno Setup."""
    iscc = find_iscc()
    if iscc is None:
        message = ("Inno Setup 6.3 or later is needed for the Windows installer "
                   "(winget install JRSoftware.InnoSetup, or set ISCC)")
        if required:
            raise SystemExit(message)
        print(f"warning: {message}; only the portable ZIP was built")
        return None
    if "course2md-desktop.exe" not in programs(source):
        raise SystemExit(f"{source} holds no course2md-desktop.exe")
    subprocess.run(compile_arguments(iscc, version, source, output_dir, sign), check=True)
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


# Each setup or uninstaller run gets this long, well inside the CI step's
# timeout, so a hang ends here with its log instead of killing the step.
SETUP_TIMEOUT = 300


def _log_tail(log):
    if Path(log).is_file():
        print(f"--- last lines of {log} ---")
        print(Path(log).read_text(encoding="utf-8", errors="replace")[-6000:])


def _run_bounded(command, log, what):
    """Run setup or the uninstaller; on a hang print its log and end its process tree."""
    process = subprocess.Popen(command)
    try:
        returncode = process.wait(timeout=SETUP_TIMEOUT)
    except subprocess.TimeoutExpired:
        _log_tail(log)
        # Setup runs its wizard as a child process (and the uninstaller a copy of itself).
        subprocess.run(["taskkill", "/F", "/T", "/PID", str(process.pid)], check=False)
        process.wait(timeout=60)
        raise SystemExit(f"installer smoke test: {what} did not finish in {SETUP_TIMEOUT} s")
    if returncode:
        _log_tail(log)
    return returncode


def _run_setup(installer, log, *extra):
    what = f"setup ({' '.join(extra) or 'upgrade'})"
    returncode = _run_bounded([str(installer), "/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART",
                               f"/LOG={log}", *extra], log, what)
    _check(returncode == 0, f"{what} exited with 0 (exit code {returncode})")


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
                            env=env, timeout=180)
    print(doctor.stdout + doctor.stderr)
    problems = doctor_problems(doctor.stdout, app)
    _check(not problems, f"the engine runs the bundled tools from the install ({problems})")


def _start_installed(app):
    """The installed app and a bundled tool that keeps running, as during a task."""
    tool = [str(app / "ffmpeg.exe"), "-hide_banner", "-nostdin", "-loglevel", "error", "-re",
            "-f", "lavfi", "-i", "anullsrc=r=8000", "-t", "1800", "-f", "null", "-"]
    quiet = {"stdin": subprocess.DEVNULL, "stdout": subprocess.DEVNULL,
             "stderr": subprocess.DEVNULL}
    running = [("course2md-desktop.exe",
                subprocess.Popen([str(app / "course2md-desktop.exe")], cwd=app.parent, **quiet)),
               ("ffmpeg.exe", subprocess.Popen(tool, cwd=app.parent, **quiet))]
    time.sleep(15)
    for name, process in running:
        _check(process.poll() is None, f"{name} is running from the install before the upgrade")
    return running


def _replaced(log, path):
    """Whether setup's log records installing `path` over the existing file."""
    entries = log.split("-- File entry --")
    target = f"Dest filename: {path}".casefold()
    return any(target in entry.casefold() and "Successfully installed the file." in entry
               for entry in entries)


def _uninstall(app, log):
    return _run_bounded([str(app / "unins000.exe"), "/VERYSILENT", "/SUPPRESSMSGBOXES",
                         "/NORESTART", f"/LOG={log}"], log, "the uninstaller")


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

    # Whatever fails after setup starts, the runner is left without the install.
    running, uninstalled = [], False
    try:
        _run_setup(installer, work / "install.log", f"/DIR={app}")
        _installed(package, app, version, start_menu)

        # Upgrade in place while the app and a long-running bundled tool run from
        # the install: no /DIR, so setup must find the install through the AppId,
        # close both, and replace their files; a stale file in a directory the
        # package owns must go.
        stale = app / "llama.cpp/stale-from-older-version.dll"
        stale.parent.mkdir(exist_ok=True)
        stale.write_bytes(b"")
        running.extend(_start_installed(app))
        _run_setup(installer, work / "upgrade.log")
        for name, process in running:
            try:
                process.wait(timeout=60)
            except subprocess.TimeoutExpired:
                pass
            _check(process.poll() is not None, f"the upgrade closed the running {name}")
        log = (work / "upgrade.log").read_text(encoding="utf-8", errors="replace")
        for name, _ in running:
            _check(_replaced(log, app / name), f"the upgrade replaced the running {name}")
        _check(not stale.exists(), "an upgrade replaces llama.cpp/ instead of mixing versions")
        _installed(package, app, version, start_menu)

        uninstalled = True
        returncode = _uninstall(app, work / "uninstall.log")
        _check(returncode == 0, f"the uninstaller exited with 0 (exit code {returncode})")
        # The uninstaller finishes from a copy of itself in TEMP.
        deadline = time.monotonic() + 120
        while (app.exists() or _uninstall_entry()) and time.monotonic() < deadline:
            time.sleep(2)
        leftover = package_files(app) if app.exists() else []
        _check(not app.exists(), f"the install directory is removed (left: {leftover[:10]})")
        _check(_uninstall_entry() is None, "Windows no longer lists the program")
        _check(not start_menu.exists(), "the Start menu shortcut is removed")
    finally:
        for _, process in running:
            if process.poll() is None:
                process.kill()
        if not uninstalled and (app / "unins000.exe").is_file():
            print("installer smoke test: uninstalling after a failure")
            try:
                _uninstall(app, work / "cleanup-uninstall.log")
            except (OSError, SystemExit, subprocess.SubprocessError) as error:
                print(f"installer smoke test: cleanup failed: {error}")
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
    build_parser.add_argument("--sign-pfx", type=Path, metavar="PATH",
                              help="sign the setup program and its uninstaller with this "
                                   "certificate; its password comes from WINDOWS_CERT_PASS")
    smoke_parser = commands.add_parser("smoke", help="install, upgrade and uninstall a setup program")
    smoke_parser.add_argument("installer", type=Path)
    smoke_parser.add_argument("package", type=Path, help="the package directory it was built from")
    smoke_parser.add_argument("--work", type=Path,
                              default=Path(os.environ.get("RUNNER_TEMP") or ".") / "installer-smoke")
    args = parser.parse_args(argv)
    if args.command == "build":
        package = args.package.resolve()
        sign = signtool = None
        if args.sign_pfx:
            signtool = find_signtool()
            if signtool is None:
                raise SystemExit("Signing needs signtool.exe from the Windows SDK")
            sign = sign_command(signtool, args.sign_pfx.resolve(),
                                os.environ.get("WINDOWS_CERT_PASS", ""))
        installer = build(package, workspace_version(), args.output or package.parent,
                          required=True, sign=sign)
        if signtool:
            subprocess.run([str(signtool), "verify", "/pa", str(installer)], check=True)
        print(installer)
    else:
        if sys.platform != "win32":
            raise SystemExit("The installer smoke test runs on Windows")
        # doctor's output has check marks the runner's code page lacks.
        sys.stdout.reconfigure(encoding="utf-8", errors="replace")
        smoke(args.installer, args.package, args.work, workspace_version())


if __name__ == "__main__":
    main()
