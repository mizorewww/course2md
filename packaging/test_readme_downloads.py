"""The READMEs' download links point at the workspace version's release assets.

GitHub's releases/latest/download only reaches the newest stable release, so
the READMEs link the current version's assets by tag. A release PR that bumps
the workspace version must move the links with it, or this test fails (the
release workflow's validate job runs it too).
"""
from pathlib import Path
import re
import tomllib
import unittest

from versioning import PRERELEASE_CHANNELS, parse_release

ROOT = Path(__file__).resolve().parents[1]
READMES = {"readme.md": "## Download", "readme.zh.md": "## 下载"}
INSTALL = {"readme.md": "## Install", "readme.zh.md": "## 安装"}
CASK = re.compile(r"mizorewww/tap/(course2md-gui(?:@[a-z]+)?)(?![\w@-])")
RELEASES = "https://github.com/mizorewww/course2md/releases"
DOWNLOAD = re.compile(re.escape(RELEASES) + r"/(?:download/([^/\s)]+)|latest/download)/([^\s)\"']+)")
BADGE = re.compile(r"\[!\[[^\]]+\]\((https://img\.shields\.io/[^)\s]+)\)\]\(([^)\s]+)\)")
MACOS, LINUX = "course2md-gui-macos-arm64.dmg", "course2md-desktop-linux-x86_64.tar.gz"
WINDOWS_ZIP = "course2md-desktop-windows-AMD64.zip"
WINDOWS_SETUP = "course2md-desktop-windows-x64-setup.exe"
GUI_ASSETS = {MACOS, LINUX, WINDOWS_ZIP, WINDOWS_SETUP}
# The first release that publishes the Windows installer. From it on, the
# Windows download is the installer and the ZIP stays the portable option.
INSTALLER_SINCE = "3.0.0-alpha.5"


def workspace_version():
    manifest = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))
    return manifest["workspace"]["package"]["version"]


def release_key(version):
    """Sort key of a release version: prereleases before their stable release."""
    channel, number = parse_release(version)
    core = tuple(int(part) for part in version.split("-")[0].split("."))
    rank = len(PRERELEASE_CHANNELS) if channel == "stable" else PRERELEASE_CHANNELS.index(channel)
    return core, rank, int(number or 0)


def windows_asset(version):
    return WINDOWS_SETUP if release_key(version) >= release_key(INSTALLER_SINCE) else WINDOWS_ZIP


def cask(version):
    channel, _ = parse_release(version)
    return "course2md-gui" if channel == "stable" else f"course2md-gui@{channel}"


def section(text, heading):
    """The text from `heading` to the next second-level heading."""
    headings = [match.start() for match in re.finditer(r"^## ", text, re.MULTILINE)]
    start = text.find(heading + "\n")
    assert start in headings, f"missing {heading}"
    following = [position for position in headings if position > start]
    return text[start:following[0] if following else len(text)]


def download_section(text, heading):
    """The download section, which must be the README's first second-level heading."""
    first = re.search(r"^## ", text, re.MULTILINE)
    assert first and text.startswith(heading + "\n", first.start()), f"{heading} must be the first section"
    return section(text, heading)


class ReadmeDownloadTests(unittest.TestCase):
    def setUp(self):
        self.version = workspace_version()
        self.readmes = {name: (ROOT / name).read_text(encoding="utf-8") for name in READMES}

    def test_every_gui_download_link_is_the_workspace_version(self):
        for name, text in self.readmes.items():
            for match in DOWNLOAD.finditer(text):
                tag, asset = match.groups()
                if asset in GUI_ASSETS:
                    with self.subTest(readme=name, link=match.group(0)):
                        self.assertEqual(tag, f"v{self.version}", "link the versioned asset; "
                                         "releases/latest only reaches the stable release")

    def test_download_section_has_a_badge_per_platform_and_all_releases(self):
        expected = [MACOS, windows_asset(self.version), LINUX]
        for name, text in self.readmes.items():
            with self.subTest(readme=name):
                section = download_section(text, READMES[name])
                badges = BADGE.findall(section)
                targets = [target for _, target in badges]
                self.assertEqual(targets, [f"{RELEASES}/download/v{self.version}/{asset}"
                                           for asset in expected])
                for image, _ in badges:
                    # Badge images stay the same across versions; only targets move.
                    self.assertNotIn(self.version, image)
                # The plain-text line repeats each download for readers without images.
                for target in targets:
                    self.assertEqual(section.count(f"]({target})"), 2, target)
                self.assertIn(f"]({RELEASES})", section)
                self.assertIn(self.version, section.replace(f"v{self.version}/", ""))

    def test_homebrew_cask_matches_the_release_channel(self):
        expected = cask(self.version)
        for name, text in self.readmes.items():
            with self.subTest(readme=name):
                self.assertEqual(CASK.findall(download_section(text, READMES[name])), [expected])
                self.assertEqual(CASK.findall(section(text, INSTALL[name])), [expected])
                # Nowhere else may a README offer another channel's cask.
                self.assertEqual(set(CASK.findall(text)), {expected})

    def test_release_order_and_channels(self):
        self.assertLess(release_key("3.0.0-alpha.4"), release_key("3.0.0-alpha.10"))
        self.assertLess(release_key("3.0.0-rc.1"), release_key("3.0.0"))
        self.assertEqual(windows_asset("3.0.0-alpha.4"), WINDOWS_ZIP)
        self.assertEqual(windows_asset("3.0.0-beta.1"), WINDOWS_SETUP)
        self.assertEqual((cask("3.0.0-alpha.5"), cask("3.0.0")),
                         ("course2md-gui@alpha", "course2md-gui"))


if __name__ == "__main__":
    unittest.main()
