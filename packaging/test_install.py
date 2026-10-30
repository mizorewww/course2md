"""install.sh installs nothing it cannot verify. Runs the real script with a fake curl."""
import hashlib
import os
from pathlib import Path
import stat
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parent.parent
BINARY = b'prebuilt course2md\n'

FAKE_CURL = """#!/bin/sh
# curl -fsSL [--max-time N] URL -o FILE: serves $FAKE_RELEASE/<basename of URL>.
while [ $# -gt 0 ]; do
  case "$1" in
    -o) out="$2"; shift 2 ;;
    --max-time) shift 2 ;;
    -*) shift ;;
    *) url="$1"; shift ;;
  esac
done
name="${url##*/}"
[ -f "$FAKE_RELEASE/$name" ] || exit 22
cp "$FAKE_RELEASE/$name" "$out"
"""


def tool(directory, name, body='#!/bin/sh\nexit 0\n'):
    path = directory / name
    path.write_text(body)
    path.chmod(path.stat().st_mode | stat.S_IXUSR)


class InstallChecksumTests(unittest.TestCase):
    def run_install(self, sums):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            release, fake_bin, bin_dir = root / 'release', root / 'path', root / 'bin'
            release.mkdir()
            fake_bin.mkdir()
            tool(fake_bin, 'curl', FAKE_CURL)
            for name in ['ffmpeg', 'ffprobe', 'yt-dlp', 'llama-server']:
                tool(fake_bin, name)
            for name in ['course2md-macos-arm64', 'course2md-macos-x86_64',
                         'course2md-linux-x86_64', 'course2md-linux-aarch64']:
                (release / name).write_bytes(BINARY)
            if sums is not None:
                (release / 'SHA256SUMS').write_text(sums)
            result = subprocess.run(
                ['bash', str(ROOT / 'install.sh')],
                capture_output=True, text=True,
                env={**os.environ,
                     'PATH': f"{fake_bin}{os.pathsep}{os.environ['PATH']}",
                     'FAKE_RELEASE': str(release),
                     'COURSE2MD_BIN_DIR': str(bin_dir)},
            )
            return result, (bin_dir / 'course2md').exists()

    @staticmethod
    def listing(data=BINARY):
        digest = hashlib.sha256(data).hexdigest()
        return ''.join(f'{digest}  {name}\n' for name in [
            'course2md-macos-arm64', 'course2md-macos-x86_64',
            'course2md-linux-x86_64', 'course2md-linux-aarch64'])

    def test_a_listed_matching_binary_installs(self):
        result, installed = self.run_install(self.listing())
        self.assertTrue(installed, result.stderr)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_missing_sums_stop_the_install(self):
        result, installed = self.run_install(None)
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(installed)
        self.assertIn('SHA256SUMS', result.stderr)

    def test_an_unlisted_binary_stops_the_install(self):
        result, installed = self.run_install('0' * 64 + '  something-else\n')
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(installed)
        self.assertIn('SHA256SUMS', result.stderr)

    def test_a_wrong_checksum_stops_the_install(self):
        result, installed = self.run_install(self.listing(b'tampered'))
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(installed)
        self.assertIn('Checksum mismatch', result.stderr)


if __name__ == '__main__':
    unittest.main()
