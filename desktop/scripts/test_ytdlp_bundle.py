import hashlib
from pathlib import Path
import tempfile
import unittest
import zipfile

import ytdlp_bundle

MACHO = b"\xcf\xfa\xed\xfe" + b"\0" * 28


class PinTests(unittest.TestCase):
    def test_every_bundled_platform_has_a_pinned_onedir_zip(self):
        for system, machine in [("Darwin", "arm64"), ("Windows", "AMD64"), ("Windows", "ARM64"),
                                ("Linux", "x86_64"), ("Linux", "aarch64")]:
            asset, executable, digest = ytdlp_bundle.YTDLP_ZIPS[(system, machine)]
            self.assertTrue(asset.endswith(".zip"), asset)
            self.assertTrue(executable.startswith("yt-dlp"), executable)
            self.assertRegex(digest, "^[0-9a-f]{64}$")


class StageTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)

    def tearDown(self):
        self.tmp.cleanup()

    def archive(self, members):
        path = self.root / "yt-dlp_linux.zip"
        with zipfile.ZipFile(path, "w") as zipped:
            for name, data in members.items():
                info = zipfile.ZipInfo(name)
                info.external_attr = 0o755 << 16
                zipped.writestr(info, data)
        return path

    def stage(self, archive, system="Linux", machine="x86_64"):
        digest = hashlib.sha256(archive.read_bytes()).hexdigest()
        asset, executable, _ = ytdlp_bundle.YTDLP_ZIPS[(system, machine)]
        downloads = self.root / "downloads"
        downloads.mkdir(exist_ok=True)
        archive.replace(downloads / f"{ytdlp_bundle.YTDLP_VERSION}-{asset}")
        original = ytdlp_bundle.YTDLP_ZIPS[(system, machine)]
        ytdlp_bundle.YTDLP_ZIPS[(system, machine)] = (asset, executable, digest)
        try:
            return ytdlp_bundle.stage(self.root / "app", downloads, system, machine)
        finally:
            ytdlp_bundle.YTDLP_ZIPS[(system, machine)] = original

    def test_the_onedir_build_lands_in_a_yt_dlp_directory(self):
        exe = self.stage(self.archive({"yt-dlp_linux": b"elf", "_internal/libpython.so": b"lib"}))
        self.assertEqual(exe, self.root / "app/yt-dlp/yt-dlp")
        self.assertTrue(exe.stat().st_mode & 0o111)
        self.assertTrue((exe.parent / "_internal/libpython.so").is_file())

    def test_a_one_file_build_is_refused(self):
        with self.assertRaises(OSError):
            self.stage(self.archive({"yt-dlp_linux": b"elf"}))

    def test_members_outside_the_directory_are_refused(self):
        with self.assertRaises(OSError):
            self.stage(self.archive({"yt-dlp_linux": b"elf", "_internal/x": b"",
                                     "../escape": b""}))


class FrameworkTests(unittest.TestCase):
    def test_flattened_framework_copies_become_links_again(self):
        with tempfile.TemporaryDirectory() as tmp:
            internal = Path(tmp)
            framework = internal / "Python.framework"
            for version in ("3.14", "Current"):
                (framework / "Versions" / version / "Resources").mkdir(parents=True)
                (framework / "Versions" / version / "Python").write_bytes(MACHO)
                (framework / "Versions" / version / "Resources/Info.plist").write_text("plist")
            (framework / "Resources").mkdir()
            (framework / "Resources/Info.plist").write_text("plist")
            (framework / "Python").write_bytes(MACHO)
            (internal / "Python").write_bytes(MACHO)
            ytdlp_bundle._restore_framework(internal)
            self.assertEqual(str((framework / "Versions/Current").readlink()), "3.14")
            self.assertEqual(str((framework / "Python").readlink()), "Versions/Current/Python")
            self.assertEqual(str((framework / "Resources").readlink()), "Versions/Current/Resources")
            self.assertEqual(str((internal / "Python").readlink()),
                             "Python.framework/Versions/Current/Python")
            self.assertEqual((internal / "Python").read_bytes(), MACHO)

    def test_differing_copies_are_left_alone(self):
        with tempfile.TemporaryDirectory() as tmp:
            internal = Path(tmp)
            framework = internal / "Python.framework"
            for version, data in (("3.14", MACHO), ("Current", MACHO + b"x")):
                (framework / "Versions" / version).mkdir(parents=True)
                (framework / "Versions" / version / "Python").write_bytes(data)
            ytdlp_bundle._restore_framework(internal)
            self.assertFalse((framework / "Versions/Current").is_symlink())

    def test_signing_goes_from_the_innermost_code_to_the_executable(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "_internal/lib-dynload").mkdir(parents=True)
            (root / "_internal/lib-dynload/_ssl.so").write_bytes(MACHO)
            (root / "_internal/libssl.3.dylib").write_bytes(MACHO)
            (root / "_internal/base_library.zip").write_bytes(b"PK")
            version = root / "_internal/Python.framework/Versions/3.14"
            version.mkdir(parents=True)
            (version / "Python").write_bytes(MACHO)
            (root / "_internal/Python.framework/Versions/Current").symlink_to("3.14")
            (root / "yt-dlp").write_bytes(MACHO)
            order = ytdlp_bundle.signing_order(root)
            self.assertEqual(order[0], root / "_internal/lib-dynload/_ssl.so")
            self.assertIn(version.resolve(), order)
            self.assertEqual(order[-1], root / "yt-dlp")
            self.assertNotIn(root / "_internal/base_library.zip", order)
            self.assertLess(order.index(root / "_internal/libssl.3.dylib"), order.index(root / "yt-dlp"))


if __name__ == "__main__":
    unittest.main()
