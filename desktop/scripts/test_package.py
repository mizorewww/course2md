import os
from pathlib import Path
import tempfile
import unittest
from unittest import mock

from package import YTDLP_SHA256, macos_versions, parse_args, verify_tools, ytdlp_asset


class MacOSVersionTests(unittest.TestCase):
    def test_prerelease_bundle_identifies_stage_without_invalid_short_version(self):
        for channel, suffix in [('alpha', 'a'), ('beta', 'b'), ('rc', 'fc')]:
            version = f'2.0.0-{channel}.1'
            self.assertEqual(macos_versions(version), {
                'CFBundleShortVersionString': '2.0.0',
                'CFBundleVersion': f'2.0.0{suffix}1',
                'Course2mdVersion': version,
            })

    def test_stable_removes_prerelease_suffix(self):
        self.assertEqual(macos_versions('2.0.0'), {
            'CFBundleShortVersionString': '2.0.0', 'CFBundleVersion': '2.0.0',
            'Course2mdVersion': '2.0.0',
        })

    def test_invalid_apple_build_suffix_fails(self):
        for version in ['2.0.0-alpha.0', '2.0.0-alpha.256', '2.0.0-alpha.01', '2.0.0-preview.1']:
            with self.assertRaises(ValueError):
                macos_versions(version)



class YtDlpPinTests(unittest.TestCase):
    def test_every_bundled_platform_has_a_pinned_checksum(self):
        for system, machine in [('Darwin', 'arm64'), ('Windows', 'AMD64'), ('Windows', 'ARM64'),
                                ('Linux', 'x86_64'), ('Linux', 'aarch64')]:
            self.assertRegex(YTDLP_SHA256[ytdlp_asset(system, machine)], '^[0-9a-f]{64}$')


class OptionTests(unittest.TestCase):
    def test_environment_supplies_defaults_and_flags_override_them(self):
        environ = {'COURSE2MD_NO_YTDLP': '1', 'APPLE_SIGNING_IDENTITY': 'Developer ID',
                   'APPLE_API_KEY_PATH': '/k.p8', 'COURSE2MD_YTDLP': '/bin/yt'}
        args = parse_args([], environ)
        self.assertTrue(args.no_ytdlp)
        self.assertFalse(args.no_aria2)
        self.assertEqual((args.signing_identity, args.notary_key, args.ytdlp),
                         ('Developer ID', '/k.p8', '/bin/yt'))
        args = parse_args(['--signing-identity', '-', '--no-apple'], environ)
        self.assertEqual(args.signing_identity, '-')
        self.assertTrue(args.no_apple)

    def test_without_environment_builds_bundle_everything_and_sign_ad_hoc(self):
        args = parse_args([], {})
        self.assertEqual(args.signing_identity, '-')
        self.assertFalse(args.no_ytdlp or args.no_aria2 or args.no_apple)
        self.assertIsNone(args.ytdlp)

    def test_require_tools_comes_from_the_environment(self):
        self.assertFalse(parse_args([], {}).require_tools)
        self.assertTrue(parse_args([], {'COURSE2MD_REQUIRE_TOOLS': '1'}).require_tools)


@unittest.skipIf(os.name == 'nt', 'shell scripts stand in for the bundled tools')
class VerifyToolsTests(unittest.TestCase):
    def tool(self, directory, name, output):
        path = Path(directory) / name
        path.write_text(f'#!/bin/sh\necho "{output}"\n')
        path.chmod(0o755)

    def test_an_ffmpeg_without_https_fails_the_package(self):
        with tempfile.TemporaryDirectory() as tmp, \
                mock.patch('platform.machine', return_value='arm64'):
            self.tool(tmp, 'ffprobe', 'ffprobe version 9')
            self.tool(tmp, 'ffmpeg', 'Supported file protocols: file http')
            with self.assertRaises(SystemExit):
                verify_tools(Path(tmp), 'Darwin')
            self.tool(tmp, 'ffmpeg', 'Supported file protocols: file http https tls')
            verify_tools(Path(tmp), 'Darwin')

    def test_a_missing_tool_fails_the_package(self):
        with tempfile.TemporaryDirectory() as tmp, \
                mock.patch('platform.machine', return_value='arm64'):
            with self.assertRaises(SystemExit):
                verify_tools(Path(tmp), 'Darwin')
            verify_tools(Path(tmp), 'Darwin', skip=['ffmpeg'])


if __name__ == '__main__':
    unittest.main()
