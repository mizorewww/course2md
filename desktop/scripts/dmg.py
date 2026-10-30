"""Create a Finder install window without requiring a GUI session in CI."""
from pathlib import Path
import subprocess
import tempfile

ASSETS = Path(__file__).resolve().parents[1] / "assets"
APP_ICON_AT = (196, 224)
APPLICATIONS_ICON_AT = (564, 224)


def build_install_dmg(bundle, destination):
    import dmgbuild

    bundle = Path(bundle).resolve()
    background = ASSETS / "dmg/background.tiff"
    # Logical size is reliable on both APFS clones and GitHub-hosted runners.
    # Reserve metadata space; empty blocks compress away in the final UDZO.
    payload_bytes = sum(path.stat().st_size for path in bundle.rglob("*")
                        if path.is_file() and not path.is_symlink())
    payload_bytes += background.stat().st_size
    image_mib = (payload_bytes + 1024 * 1024 - 1) // (1024 * 1024) + 64
    print(f"Creating {image_mib} MiB install image for {payload_bytes} bytes of files", flush=True)
    dmgbuild.build_dmg(str(destination), "course2md", settings={
        "format": "UDZO",
        "filesystem": "HFS+",
        "size": f"{image_mib}m",
        "files": [str(bundle)],
        "symlinks": {"Applications": "/Applications"},
        "icon": str(ASSETS / "icon.icns"),
        "background": str(background),
        # Leave room when Finder's global preferences force tabs/status bars on.
        "window_rect": ((200, 200), (760, 540)),
        "icon_locations": {bundle.name: APP_ICON_AT, "Applications": APPLICATIONS_ICON_AT},
        "icon_size": 128,
        "text_size": 13,
        "label_pos": "bottom",
        "default_view": "icon-view",
        "arrange_by": None,
        "grid_spacing": 80,
        "show_icon_preview": False,
        "show_status_bar": False,
        "show_tab_view": False,
        "show_toolbar": False,
        "show_pathbar": False,
        "show_sidebar": False,
        # Do not set the app's FinderInfo (including its extension-hidden bit)
        # after signing: strict codesign verification rejects that metadata.
    })
    verify_install_dmg(destination, bundle.name)


def verify_install_dmg(image, app_name):
    from ds_store import DSStore

    with tempfile.TemporaryDirectory(prefix="course2md-dmg-check-") as directory:
        mount = Path(directory) / "volume"
        mount.mkdir()
        subprocess.run(["hdiutil", "attach", "-readonly", "-nobrowse", "-noautoopen",
                        "-mountpoint", str(mount), str(image)], check=True, capture_output=True)
        try:
            visible = {path.name for path in mount.iterdir() if not path.name.startswith(".")}
            if visible != {app_name, "Applications"}:
                raise ValueError(f"Unexpected files in install window: {visible}")
            if (mount / "Applications").readlink() != Path("/Applications"):
                raise ValueError("Applications shortcut has the wrong destination")
            if (mount / ".background.tiff").read_bytes() != (ASSETS / "dmg/background.tiff").read_bytes():
                raise ValueError("DMG background differs from the checked-in artwork")
            with DSStore.open(str(mount / ".DS_Store"), "r") as store:
                if (store[app_name]["Iloc"] != APP_ICON_AT
                        or store["Applications"]["Iloc"] != APPLICATIONS_ICON_AT):
                    raise ValueError("DMG is missing its drag-to-install layout")
                if store["."]["icvp"]["backgroundType"] != 2:
                    raise ValueError("Finder background image is not configured")
            resources = mount / app_name / "Contents/Resources"
            if (resources / "course2md.icns").read_bytes() != (ASSETS / "icon.icns").read_bytes():
                raise ValueError("Packaged app has a stale icon")
            for name in ("LICENSE", "LICENSE-material-icons", "LICENSE-lucide-icons",
                         "design-licenses"):
                if not (resources / name).exists():
                    raise ValueError(f"App is missing {name}")
            subprocess.run(["codesign", "--verify", "--deep", "--strict", str(mount / app_name)], check=True)
        finally:
            subprocess.run(["hdiutil", "detach", str(mount)], check=True, capture_output=True)
    print("Verified DMG: Finder layout, Applications link, artwork, licenses, and app signature")
