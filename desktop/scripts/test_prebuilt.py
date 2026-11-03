import hashlib
import io
from pathlib import Path
import tarfile
import tempfile
import unittest
import zipfile

import prebuilt


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


class PinTests(unittest.TestCase):
    def test_every_archive_and_license_is_pinned(self):
        for tools in prebuilt.PREBUILT.values():
            for tool in tools:
                self.assertTrue(tool.url.startswith("https://github.com/"), tool.url)
                self.assertRegex(tool.sha256, "^[0-9a-f]{64}$")
                self.assertTrue(tool.licenses, tool.name)
                for source in tool.licenses.values():
                    if not isinstance(source, str):
                        self.assertRegex(source[1], "^[0-9a-f]{64}$")

    def test_windows_and_linux_bundle_uv_and_the_vulkan_llama_server(self):
        for platform in [("Windows", "AMD64"), ("Linux", "x86_64")]:
            tools = {tool.name: tool for tool in prebuilt.PREBUILT[platform]}
            self.assertLessEqual({"uv", "llama.cpp"}, set(tools))
            self.assertIn("vulkan", tools["llama.cpp"].url)
            self.assertEqual(tools["llama.cpp"].subdir, "llama.cpp")
            self.assertEqual(tools["uv"].subdir, "")
        self.assertNotIn(("Darwin", "arm64"), prebuilt.PREBUILT)

    def test_only_windows_bundles_aria2(self):
        names = {platform: {tool.name for tool in tools} for platform, tools in prebuilt.PREBUILT.items()}
        self.assertIn("aria2", names[("Windows", "AMD64")])
        self.assertNotIn("aria2", names[("Linux", "x86_64")])

    def test_only_llama_servers_libraries_are_kept(self):
        import fnmatch
        names = {
            ("Windows", "AMD64"): (["llama-server.exe", "llama-server-impl.dll", "ggml-cpu-zen4.dll",
                                    "ggml-vulkan.dll", "libomp.dll", "mtmd.dll"],
                                   ["llama-cli.exe", "llama-cli-impl.dll", "llama-bench-impl.dll",
                                    "ggml-rpc.dll", "ggml-rpc-server.exe"]),
            ("Linux", "x86_64"): (["llama-server", "libllama.so.0", "libllama.so.0.7.0",
                                   "libggml-cpu-haswell.so", "libggml-vulkan.so"],
                                  ["libllama-cli-impl.so", "libllama-quantize-impl.so",
                                   "libggml-rpc.so", "ggml-rpc-server"]),
        }
        for platform, (kept, dropped) in names.items():
            (llama,) = [tool for tool in prebuilt.PREBUILT[platform] if tool.name == "llama.cpp"]
            keeps = lambda name: any(fnmatch.fnmatch(name, p) for p in llama.keep)
            self.assertTrue(all(map(keeps, kept)), platform)
            self.assertFalse(any(map(keeps, dropped)), platform)


class StageTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        self.binaries = self.root / "app"
        self.binaries.mkdir()
        self.downloads = self.root / "downloads"

    def tearDown(self):
        self.tmp.cleanup()

    def test_zip_keeps_only_the_server_and_libraries(self):
        archive = self.root / "src" / "llama.zip"
        archive.parent.mkdir()
        with zipfile.ZipFile(archive, "w") as zipped:
            for name in ["llama-server.exe", "llama-cli.exe", "ggml.dll", "LICENSE-LLVM-OpenMP"]:
                zipped.writestr(name, name)
        tool = prebuilt.Prebuilt("llama.cpp", archive.as_uri(), sha(archive),
                                 keep=("llama-server.exe", "*.dll"), subdir="llama.cpp",
                                 licenses={"LICENSE-llvm-openmp": "LICENSE-LLVM-OpenMP"})
        prebuilt.stage(tool, self.binaries, self.binaries, self.downloads)
        self.assertEqual(sorted(p.name for p in (self.binaries / "llama.cpp").iterdir()),
                         ["ggml.dll", "llama-server.exe"])
        self.assertEqual((self.binaries / "LICENSE-llvm-openmp").read_text(), "LICENSE-LLVM-OpenMP")

    def test_tar_keeps_library_links(self):
        archive = self.root / "src" / "llama.tar.gz"
        archive.parent.mkdir()
        with tarfile.open(archive, "w:gz") as tar:
            for name, data in [("llama-server", b"elf"), ("libllama.so.0.7.0", b"lib"),
                               ("LICENSE", b"MIT")]:
                info = tarfile.TarInfo(f"llama-b1/{name}")
                info.size, info.mode = len(data), 0o755
                tar.addfile(info, io.BytesIO(data))
            link = tarfile.TarInfo("llama-b1/libllama.so.0")
            link.type, link.linkname = tarfile.SYMTYPE, "libllama.so.0.7.0"
            tar.addfile(link)
        tool = prebuilt.Prebuilt("llama.cpp", archive.as_uri(), sha(archive),
                                 keep=("llama-server", "lib*.so", "lib*.so.*"), subdir="llama.cpp",
                                 licenses={"LICENSE-llama.cpp": "LICENSE"})
        prebuilt.stage(tool, self.binaries, self.binaries, self.downloads)
        dest = self.binaries / "llama.cpp"
        self.assertTrue((dest / "libllama.so.0").is_symlink())
        self.assertEqual((dest / "libllama.so.0").read_bytes(), b"lib")
        self.assertTrue((dest / "llama-server").stat().st_mode & 0o111)

    def test_a_link_leaving_the_directory_is_refused(self):
        archive = self.root / "src" / "bad.tar.gz"
        archive.parent.mkdir()
        with tarfile.open(archive, "w:gz") as tar:
            link = tarfile.TarInfo("x/libevil.so")
            link.type, link.linkname = tarfile.SYMTYPE, "../../etc/passwd"
            tar.addfile(link)
        tool = prebuilt.Prebuilt("evil", archive.as_uri(), sha(archive), keep=("lib*.so",))
        with self.assertRaises(OSError):
            prebuilt.stage(tool, self.binaries, self.binaries, self.downloads)

    def test_development_builds_continue_without_a_tool(self):
        missing = prebuilt.Prebuilt("uv", (self.root / "absent.zip").as_uri(), "0" * 64,
                                    keep=("uv.exe",))
        original = prebuilt.PREBUILT
        prebuilt.PREBUILT = {("Windows", "AMD64"): [missing]}
        try:
            self.assertEqual(prebuilt.bundle("Windows", "AMD64", self.binaries, self.binaries,
                                             self.downloads, required=False), [])
            with self.assertRaises(SystemExit):
                prebuilt.bundle("Windows", "AMD64", self.binaries, self.binaries,
                                self.downloads, required=True)
            self.assertEqual(prebuilt.bundle("Windows", "AMD64", self.binaries, self.binaries,
                                             self.downloads, required=True, skip=["uv"]), [])
        finally:
            prebuilt.PREBUILT = original


if __name__ == "__main__":
    unittest.main()
