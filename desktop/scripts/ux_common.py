"""Shared by the desktop UX validation scripts.

ux_fixture.py copies this module beside the ux_ytdlp.py and ux_ffmpeg.py
wrappers it installs, so they import it from the same directory.
"""
import os
from pathlib import Path
import shutil

# Where `ux_fixture.py prepare/serve/bundle` put the isolated library by default.
DEFAULT_FIXTURE_ROOT = Path("/tmp/course2md-ux-validation")


def fixture_root():
    """The isolated library the validation app was bundled with."""
    return Path(os.environ.get("COURSE2MD_UX_FIXTURE_ROOT", DEFAULT_FIXTURE_ROOT))


def real_tool(name):
    """The installed tool on PATH, skipping the fixture directory that holds the wrappers."""
    here = Path(__file__).resolve().parent
    path = os.pathsep.join(d for d in os.environ.get("PATH", "").split(os.pathsep)
                           if d and Path(d).resolve() != here)
    return shutil.which(name, path=path)
