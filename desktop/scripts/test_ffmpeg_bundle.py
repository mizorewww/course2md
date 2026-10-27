import hashlib
from pathlib import Path
import tempfile
import unittest
from unittest import mock

import ffmpeg_bundle


class PinTests(unittest.TestCase):
    def test_every_source_is_pinned_to_a_sha256(self):
        for url, digest in [ffmpeg_bundle.FFMPEG_SOURCE, ffmpeg_bundle.DAV1D_SOURCE,
                            (ffmpeg_bundle.WINDOWS_URL, ffmpeg_bundle.WINDOWS_SHA256)]:
            self.assertTrue(url.startswith("https://"), url)
            self.assertRegex(digest, "^[0-9a-f]{64}$")

    def test_windows_build_is_the_lgpl_shared_variant_of_the_same_release(self):
        self.assertIn("-lgpl-shared-", ffmpeg_bundle.WINDOWS_BUILD)
        self.assertIn(f"n{ffmpeg_bundle.FFMPEG_VERSION}-", ffmpeg_bundle.WINDOWS_BUILD)
        # Monthly autobuilds are kept for two years; daily ones only for two weeks.
        self.assertRegex(ffmpeg_bundle.WINDOWS_RELEASE, r"^autobuild-\d{4}-\d{2}-(28|29|30|31)-")

    def test_bundled_platforms(self):
        for system, machine in [("Darwin", "arm64"), ("Linux", "x86_64"), ("Windows", "AMD64")]:
            self.assertTrue(ffmpeg_bundle.supported(system, machine))
        for system, machine in [("Darwin", "x86_64"), ("Linux", "aarch64"), ("Windows", "ARM64")]:
            self.assertFalse(ffmpeg_bundle.supported(system, machine))


class ConfigureTests(unittest.TestCase):
    def test_build_machine_libraries_cannot_leak_in(self):
        for system in ("Darwin", "Linux"):
            flags = ffmpeg_bundle.configure_flags(system)
            self.assertIn("--disable-autodetect", flags)
            self.assertNotIn("--enable-gpl", flags)
            self.assertNotIn("--enable-nonfree", flags)

    def test_macos_targets_the_app_minimum(self):
        flags = ffmpeg_bundle.configure_flags("Darwin")
        self.assertIn("--extra-cflags=-mmacosx-version-min=15.0", flags)

    def test_the_engine_test_tone_input_stays_available(self):
        self.assertIn("--enable-indev=lavfi", ffmpeg_bundle.configure_flags("Linux"))

    def test_cache_key_follows_the_recipe(self):
        key = ffmpeg_bundle.cache_key("Darwin", "arm64")
        with mock.patch.object(ffmpeg_bundle, "configure_flags", return_value=["--other"]):
            self.assertNotEqual(ffmpeg_bundle.cache_key("Darwin", "arm64"), key)


class CacheTests(unittest.TestCase):
    def test_a_cached_build_needs_no_build_tools_and_is_reused(self):
        with tempfile.TemporaryDirectory() as tmp:
            cache = Path(tmp)
            out = cache / ffmpeg_bundle.cache_key("Linux", "x86_64")
            (out / "bin").mkdir(parents=True)
            (out / "licenses").mkdir()
            (out / "bin/ffmpeg").write_text("")
            with mock.patch("shutil.which", return_value=None):
                self.assertEqual(ffmpeg_bundle.missing_build_tools(cache, "Linux", "x86_64"), [])
                self.assertEqual(ffmpeg_bundle.prepare(cache, "Linux", "x86_64"), out)
            self.assertIn("built unmodified from https://ffmpeg.org/",
                          (out / "licenses/ffmpeg-build.txt").read_text())

    def test_a_cold_cache_names_missing_build_tools(self):
        with tempfile.TemporaryDirectory() as tmp, mock.patch("shutil.which", return_value=None):
            self.assertEqual(ffmpeg_bundle.missing_build_tools(Path(tmp), "Linux", "x86_64"),
                             ["meson", "ninja", "pkg-config", "make", "nasm"])
            self.assertEqual(ffmpeg_bundle.missing_build_tools(Path(tmp), "Windows", "AMD64"), [])

    def test_download_rejects_a_wrong_checksum(self):
        with tempfile.TemporaryDirectory() as tmp:
            source = Path(tmp) / "source"
            source.write_bytes(b"payload")
            dest = Path(tmp) / "dest"
            with self.assertRaises(OSError):
                ffmpeg_bundle.download(source.as_uri(), "0" * 64, dest)
            self.assertFalse(dest.exists())
            digest = hashlib.sha256(b"payload").hexdigest()
            self.assertEqual(ffmpeg_bundle.download(source.as_uri(), digest, dest), dest)


class BundleTests(unittest.TestCase):
    def test_development_builds_fall_back_to_path(self):
        with tempfile.TemporaryDirectory() as tmp, \
                mock.patch.object(ffmpeg_bundle, "prepare", side_effect=OSError("offline")):
            self.assertIsNone(ffmpeg_bundle.bundle(Path(tmp), Path(tmp), required=False))
            with self.assertRaises(SystemExit):
                ffmpeg_bundle.bundle(Path(tmp), Path(tmp), required=True)


if __name__ == "__main__":
    unittest.main()
