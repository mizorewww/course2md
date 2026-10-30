import importlib.util
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('release_version', Path(__file__).with_name('release-version.py'))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class ReleaseVersionTests(unittest.TestCase):
    def resolve(self, env, releases=None, draft=False, prerelease=False, jobs=None):
        calls = []
        def api(path):
            calls.append(path)
            if path.startswith('actions/runs/'):
                return {'jobs': jobs or []}
            if path.startswith('releases?'):
                return releases or []
            return {'draft': draft, 'prerelease': prerelease}
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / 'output'
            with patch.dict(os.environ, {**env, 'GITHUB_OUTPUT': str(output)}, clear=True), patch.object(module, 'api', api):
                module.main()
            return output.read_text(), calls

    def test_manual_and_tag(self):
        for env in [{'REQUESTED_VERSION': '1.7.0'}, {'RELEASE_BRANCH': 'v1.7.0'}]:
            output, calls = self.resolve(env)
            self.assertEqual(output, 'version=1.7.0\nchannel=stable\npublish=true\n')
            self.assertEqual(calls, ['releases/tags/v1.7.0'])

    def test_workflow_dispatch_resolves_commit_not_latest(self):
        releases = [
            {'target_commitish': sha, 'tag_name': tag, 'draft': False, 'prerelease': False}
            for sha, tag in [('newer', 'v1.8.0'), ('tested', 'v1.7.0')]
        ]
        output, _ = self.resolve({'RELEASE_BRANCH': 'main', 'RELEASE_SHA': 'tested'}, releases)
        self.assertEqual(output, 'version=1.7.0\nchannel=stable\npublish=true\n')

    def test_a_dry_run_publishes_nothing_even_if_the_commit_has_a_release(self):
        release = {'target_commitish': 'tested', 'tag_name': 'v1.7.0', 'draft': False, 'prerelease': False}
        skipped = [{'name': 'build', 'conclusion': 'success'}, {'name': 'release', 'conclusion': 'skipped'}]
        env = {'RELEASE_BRANCH': 'main', 'RELEASE_SHA': 'tested', 'RELEASE_RUN_ID': '42'}
        output, calls = self.resolve(env, [release], jobs=skipped)
        self.assertEqual(output, 'publish=false\n')
        self.assertEqual(calls, ['actions/runs/42/jobs?per_page=100'])
        # A dry run of a commit with no release is not a failure either.
        output, _ = self.resolve({**env, 'RELEASE_SHA': 'untagged'}, [], jobs=skipped)
        self.assertEqual(output, 'publish=false\n')

    def test_a_real_release_run_still_publishes(self):
        release = {'target_commitish': 'tested', 'tag_name': 'v1.7.0', 'draft': False, 'prerelease': False}
        done = [{'name': 'release', 'conclusion': 'success'}]
        env = {'RELEASE_BRANCH': 'main', 'RELEASE_SHA': 'tested', 'RELEASE_RUN_ID': '42'}
        output, _ = self.resolve(env, [release], jobs=done)
        self.assertEqual(output, 'version=1.7.0\nchannel=stable\npublish=true\n')

    def test_a_manual_channel_run_is_not_gated_on_a_run(self):
        output, calls = self.resolve({'REQUESTED_VERSION': '1.7.0', 'RELEASE_RUN_ID': ''})
        self.assertIn('publish=true', output)
        self.assertEqual(calls, ['releases/tags/v1.7.0'])

    def test_prerelease_only_updates_explicitly_enabled_channels(self):
        for source in ['REQUESTED_VERSION', 'RELEASE_BRANCH']:
            for enabled in [False, True]:
                env = {source: 'v2.0.0-alpha.1', 'ALLOW_PRERELEASE': str(enabled).lower()}
                output, calls = self.resolve(env, prerelease=True)
                self.assertEqual(output, f'version=2.0.0-alpha.1\nchannel=alpha\npublish={str(enabled).lower()}\n')
                self.assertEqual(calls, ['releases/tags/v2.0.0-alpha.1'])

    def test_manual_workflow_prerelease_resolves_triggering_commit(self):
        releases = [
            {'target_commitish': 'tested', 'tag_name': 'v2.0.0-alpha.1', 'draft': False, 'prerelease': True},
            {'target_commitish': 'older', 'tag_name': 'v1.7.0', 'draft': False, 'prerelease': False},
        ]
        output, _ = self.resolve({'RELEASE_BRANCH': 'main', 'RELEASE_SHA': 'tested'}, releases, prerelease=True)
        self.assertIn('publish=false', output)
        self.assertIn('version=2.0.0-alpha.1', output)

    def test_mislabelled_release_cannot_enter_a_package_channel(self):
        for version, prerelease in [('1.7.0', True), ('2.0.0-alpha.1', False)]:
            with self.assertRaises(SystemExit):
                self.resolve({'REQUESTED_VERSION': version, 'ALLOW_PRERELEASE': 'true'}, prerelease=prerelease)

    def test_missing_or_ambiguous_release_fails(self):
        record = {'target_commitish': 'tested', 'tag_name': 'v1.7.0', 'draft': False, 'prerelease': False}
        for releases in [[], [record, record]]:
            with self.assertRaises(SystemExit):
                self.resolve({'RELEASE_BRANCH': 'main', 'RELEASE_SHA': 'tested'}, releases)

    def test_invalid_input_and_draft_fail(self):
        for version in ['1.7.0;echo bad', '../../main', '1.7.0-rc1', '2.0.0-alpha.0', '2.0.0-alpha.01', '02.0.0']:
            with self.assertRaises(SystemExit):
                self.resolve({'REQUESTED_VERSION': version})
        with self.assertRaises(SystemExit):
            self.resolve({'REQUESTED_VERSION': '1.7.0'}, draft=True)


if __name__ == '__main__':
    unittest.main()
