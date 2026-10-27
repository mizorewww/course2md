import hashlib
import json
from pathlib import Path
import tempfile
import unittest
from unittest import mock

import ffmpeg_bundle


class PinTests(unittest.TestCase):
    def test_every_source_and_toolchain_is_pinned_to_a_sha256(self):
        for url, digest in [ffmpeg_bundle.FFMPEG_SOURCE, ffmpeg_bundle.DAV1D_SOURCE,
                            ffmpeg_bundle.ZLIB_SOURCE, *ffmpeg_bundle.LLVM_MINGW.values()]:
            self.assertTrue(url.startswith("https://"), url)
            self.assertRegex(digest, "^[0-9a-f]{64}$")

    def test_the_windows_toolchain_runs_where_ci_cross_compiles(self):
        self.assertIn(("Linux", "x86_64"), ffmpeg_bundle.LLVM_MINGW)
        for url, _ in ffmpeg_bundle.LLVM_MINGW.values():
            self.assertIn(f"/{ffmpeg_bundle.LLVM_MINGW_VERSION}/", url)

    def test_bundled_platforms(self):
        for system, machine in [("Darwin", "arm64"), ("Linux", "x86_64"), ("Windows", "AMD64")]:
            self.assertTrue(ffmpeg_bundle.supported(system, machine))
        for system, machine in [("Darwin", "x86_64"), ("Linux", "aarch64"), ("Windows", "ARM64")]:
            self.assertFalse(ffmpeg_bundle.supported(system, machine))


class ConfigureTests(unittest.TestCase):
    def test_build_machine_libraries_cannot_leak_in_and_the_license_stays_lgpl_2_1(self):
        for system in ("Darwin", "Linux", "Windows"):
            flags = ffmpeg_bundle.configure_flags(system)
            self.assertIn("--disable-autodetect", flags)
            for flag in ("--enable-gpl", "--enable-nonfree", "--enable-version3",
                         "--enable-openssl"):
                self.assertNotIn(flag, flags)

    def test_every_platform_reads_https(self):
        tls = {"Darwin": "--enable-securetransport", "Linux": "--enable-gnutls",
               "Windows": "--enable-schannel"}
        for system, flag in tls.items():
            self.assertIn(flag, ffmpeg_bundle.configure_flags(system))

    def test_windows_is_cross_compiled(self):
        flags = ffmpeg_bundle.configure_flags("Windows")
        self.assertIn("--target-os=mingw32", flags)
        self.assertIn(f"--cross-prefix={ffmpeg_bundle.WINDOWS_PREFIX}", flags)

    def test_macos_targets_the_app_minimum(self):
        flags = ffmpeg_bundle.configure_flags("Darwin")
        self.assertIn("--extra-cflags=-mmacosx-version-min=15.0", flags)

    def test_the_engine_test_tone_input_stays_available(self):
        self.assertIn("--enable-indev=lavfi", ffmpeg_bundle.configure_flags("Linux"))

    def test_cache_key_follows_the_recipe(self):
        key = ffmpeg_bundle.cache_key("Darwin", "arm64")
        with mock.patch.object(ffmpeg_bundle, "configure_flags", return_value=["--other"]):
            self.assertNotEqual(ffmpeg_bundle.cache_key("Darwin", "arm64"), key)
        self.assertNotEqual(ffmpeg_bundle.cache_key("Windows", "AMD64"),
                            ffmpeg_bundle.cache_key("Linux", "x86_64"))

    def test_the_notice_names_the_sources_and_options(self):
        for system in ("Darwin", "Linux", "Windows"):
            notice = ffmpeg_bundle.build_notice(system)
            self.assertIn(ffmpeg_bundle.FFMPEG_SOURCE[0], notice)
            self.assertIn(ffmpeg_bundle.FFMPEG_SOURCE[1], notice)
            self.assertIn(" ".join(ffmpeg_bundle.configure_flags(system)), notice)
        self.assertIn("llvm-mingw", ffmpeg_bundle.build_notice("Windows"))


class CacheTests(unittest.TestCase):
    def cached_build(self, cache, system, machine):
        out = cache / ffmpeg_bundle.cache_key(system, machine)
        (out / "bin").mkdir(parents=True)
        (out / "licenses").mkdir()
        (out / "bin/ffmpeg").write_text("ffmpeg")
        ffmpeg_bundle._write_manifest(out)
        return out

    def test_a_verified_build_needs_no_build_tools_and_is_reused(self):
        with tempfile.TemporaryDirectory() as tmp:
            cache = Path(tmp)
            out = self.cached_build(cache, "Linux", "x86_64")
            with mock.patch("shutil.which", return_value=None), \
                    mock.patch.object(ffmpeg_bundle, "_build") as build:
                self.assertEqual(ffmpeg_bundle.missing_build_tools(cache, "Linux", "x86_64"), [])
                self.assertEqual(ffmpeg_bundle.prepare(cache, "Linux", "x86_64"), out)
                build.assert_not_called()
            self.assertIn("built unmodified from https://ffmpeg.org/",
                          (out / "licenses/ffmpeg-build.txt").read_text())

    def test_a_changed_cached_build_is_rebuilt(self):
        with tempfile.TemporaryDirectory() as tmp:
            cache = Path(tmp)
            out = self.cached_build(cache, "Linux", "x86_64")
            (out / "bin/ffmpeg").write_text("tampered")
            with mock.patch.object(ffmpeg_bundle, "_build") as build:
                build.side_effect = lambda system, out, downloads: (
                    (out / "licenses").mkdir(parents=True, exist_ok=True))
                ffmpeg_bundle.prepare(cache, "Linux", "x86_64")
                build.assert_called_once()

    def test_a_cold_cache_names_missing_build_tools(self):
        with tempfile.TemporaryDirectory() as tmp, mock.patch("shutil.which", return_value=None), \
                mock.patch("platform.system", return_value="Linux"):
            self.assertEqual(ffmpeg_bundle.missing_build_tools(Path(tmp), "Linux", "x86_64"),
                             ["meson", "ninja", "pkg-config", "make", "nasm"])
            self.assertIn("nasm", ffmpeg_bundle.missing_build_tools(Path(tmp), "Windows", "AMD64"))

    def test_a_windows_host_cannot_build_and_says_where_to(self):
        with tempfile.TemporaryDirectory() as tmp, mock.patch("platform.system",
                                                              return_value="Windows"):
            (missing,) = ffmpeg_bundle.missing_build_tools(Path(tmp), "Windows", "AMD64")
            self.assertIn("--target Windows", missing)

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
            # A cached file that no longer matches is fetched again.
            dest.write_bytes(b"changed")
            self.assertEqual(ffmpeg_bundle.download(source.as_uri(), digest, dest).read_bytes(),
                             b"payload")

    def test_manifest_lists_every_file(self):
        with tempfile.TemporaryDirectory() as tmp:
            out = self.cached_build(Path(tmp), "Darwin", "arm64")
            files = json.loads((out / ffmpeg_bundle.MANIFEST).read_text())
            self.assertEqual(list(files), ["bin/ffmpeg"])


class BundleTests(unittest.TestCase):
    def test_development_builds_fall_back_to_path(self):
        with tempfile.TemporaryDirectory() as tmp, \
                mock.patch.object(ffmpeg_bundle, "prepare", side_effect=OSError("offline")):
            self.assertIsNone(ffmpeg_bundle.bundle(Path(tmp), Path(tmp), required=False))
            with self.assertRaises(SystemExit):
                ffmpeg_bundle.bundle(Path(tmp), Path(tmp), required=True)


if __name__ == "__main__":
    unittest.main()
