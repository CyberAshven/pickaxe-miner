"""Exercise the actual workflow gate against disposable Git branch histories."""
import os
import hashlib
import io
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import tarfile
import textwrap
import unittest


class ReleaseChannelTest(unittest.TestCase):
    """Run workflow release gates against isolated Git histories and artifacts."""

    def test_release_mode_skips_existing_versions_and_fails_on_lookup_errors(self):
        """Require an explicit missing tag before a branch push can release."""
        workflow = (Path(__file__).resolve().parents[1] /
                    '.github/workflows/release.yml').read_text(encoding='utf-8')
        block = re.search(
            r'      - name: Resolve release tag\n.*?        run: \|\n'
            r'((?:          [^\n]*\n|\n)+)', workflow, re.S)
        self.assertIsNotNone(block)
        script = textwrap.dedent(block[1])
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            subprocess.run(['git', 'init', '-b', 'master', str(root)],
                           check=True, capture_output=True)
            subprocess.run(
                ['git', '-c', 'user.name=Release Test',
                 '-c', 'user.email=release-test@example.invalid',
                 '-c', 'commit.gpgsign=false', 'commit', '--allow-empty', '-m', 'test'],
                cwd=root, check=True, capture_output=True)
            subprocess.run(['git', 'tag', 'pickaxe-miner-v0.0.3'],
                           cwd=root, check=True, capture_output=True)
            output = root / 'outputs'
            cases = [
                ('push', 'branch', '0.0.3', str(root), 'skip'),
                ('push', 'branch', '0.0.4', str(root), 'create'),
                ('push', 'branch', '0.0.3', str(root / 'missing-remote'), None),
                ('pull_request', 'branch', '0.0.3', str(root), 'dry-run'),
                ('push', 'tag', '0.0.3', str(root), 'publish'),
                ('workflow_dispatch', 'branch', '0.0.3', str(root), 'publish'),
            ]
            for event, ref_type, version, remote, expected_mode in cases:
                with self.subTest(event=event, ref_type=ref_type,
                                  version=version, remote=remote):
                    (root / 'Cargo.toml').write_text(
                        f'[package]\nversion = "{version}"\n', encoding='utf-8')
                    output.write_text('', encoding='utf-8')
                    tag = f'pickaxe-miner-v{version}'
                    result = subprocess.run(
                        [shutil.which('bash'), '--noprofile', '--norc', '-c', script],
                        cwd=root, capture_output=True, text=True,
                        env=dict(os.environ, GITHUB_EVENT_NAME=event,
                                 PUSH_REF_TYPE=ref_type, PUSH_TAG=tag, MANUAL_TAG=tag,
                                 REPO_URL=remote, GITHUB_OUTPUT=str(output)))
                    self.assertEqual(result.returncode == 0, expected_mode is not None,
                                     result.stdout + result.stderr)
                    values = dict(line.split('=', 1) for line in
                                  output.read_text(encoding='utf-8').splitlines())
                    if expected_mode is None:
                        self.assertNotIn('mode', values)
                        self.assertIn('refusing to release', result.stderr)
                    else:
                        self.assertEqual(values['mode'], expected_mode)
                        self.assertEqual(values['tag'], tag)

    def test_replace_mode_needs_a_push_that_changes_the_replace_file(self):
        """Move an existing tag only when the push itself names it in the file."""
        workflow = (Path(__file__).resolve().parents[1] /
                    '.github/workflows/release.yml').read_text(encoding='utf-8')
        block = re.search(
            r'      - name: Resolve release tag\n.*?        run: \|\n'
            r'((?:          [^\n]*\n|\n)+)', workflow, re.S)
        self.assertIsNotNone(block)
        script = textwrap.dedent(block[1])
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)

            def git(*args):
                """Run Git in the disposable repository and return its output."""
                return subprocess.check_output(
                    ['git', '-c', 'user.name=Release Test',
                     '-c', 'user.email=release-test@example.invalid',
                     '-c', 'commit.gpgsign=false', *args],
                    cwd=root, text=True, stderr=subprocess.STDOUT).strip()

            def commit(message, files):
                """Commit `files` (path -> text) and return the commit id."""
                for name, text in files.items():
                    path = root / name
                    path.parent.mkdir(parents=True, exist_ok=True)
                    path.write_text(text, encoding='utf-8')
                git('add', '-A')
                git('commit', '-m', message)
                return git('rev-parse', 'HEAD')

            git('init', '-b', 'master', str(root))
            released = commit('release', {'Cargo.toml': '[package]\nversion = "0.0.3"\n'})
            git('tag', 'pickaxe-miner-v0.0.3')
            named = commit('replace', {'.github/release-replace': 'pickaxe-miner-v0.0.3\n'})
            unrelated = commit('later', {'README.md': 'later\n'})
            other = commit('other tag', {'.github/release-replace': 'pickaxe-miner-v0.0.2\n'})
            output = root / 'outputs'
            cases = [
                (named, released, 'replace'),
                (named, named, 'skip'),
                (named, '0' * 40, 'skip'),
                (named, '', 'skip'),
                (named, 'f' * 40, 'skip'),
                (unrelated, named, 'skip'),
                (other, unrelated, 'skip'),
            ]
            for head, before, expected_mode in cases:
                with self.subTest(head=head, before=before):
                    git('checkout', '-q', head)
                    output.write_text('', encoding='utf-8')
                    result = subprocess.run(
                        [shutil.which('bash'), '--noprofile', '--norc', '-c', script],
                        cwd=root, capture_output=True, text=True,
                        env=dict(os.environ, GITHUB_EVENT_NAME='push', PUSH_REF_TYPE='branch',
                                 PUSH_TAG='master', MANUAL_TAG='', PUSH_BEFORE=before,
                                 REPO_URL=str(root), GITHUB_OUTPUT=str(output)))
                    self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                    values = dict(line.split('=', 1) for line in
                                  output.read_text(encoding='utf-8').splitlines())
                    self.assertEqual(values['mode'], expected_mode)
                    self.assertEqual(values['tag'], 'pickaxe-miner-v0.0.3')

    def test_portable_archives_require_the_same_source_commit(self):
        """Reject a portable archive whose recorded source differs from the release."""
        workflow = (Path(__file__).resolve().parents[1] /
                    '.github/workflows/release.yml').read_text(encoding='utf-8')
        block = re.search(
            r'      - name: Verify portable package provenance\n.*?        run: \|\n'
            r'((?:          [^\n]*\n|\n)+)', workflow, re.S)
        self.assertIsNotNone(block)
        script = textwrap.dedent(block[1])
        tag, sha = 'pickaxe-miner-v0.0.3', 'a' * 40
        for wrong_platform in (None, 'mac', 'web'):
            with self.subTest(wrong_platform=wrong_platform), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                (root / 'dist').mkdir()
                for platform, filename, member in [
                    ('mac', f'{tag}-macos-arm64.tar.gz', f'{tag}-macos-arm64/SOURCE_COMMIT.txt'),
                    ('web', 'pickaxe-browser.tar.gz', 'web/SOURCE_COMMIT.txt'),
                ]:
                    destination = root / 'portable' / platform
                    destination.mkdir(parents=True)
                    archive = destination / filename
                    content = ((('b' * 40) if platform == wrong_platform else sha) + '\n').encode()
                    with tarfile.open(archive, 'w:gz') as output:
                        info = tarfile.TarInfo(member)
                        info.size = len(content)
                        output.addfile(info, io.BytesIO(content))
                    (destination / 'SHA256SUMS.txt').write_bytes(
                        (hashlib.sha256(archive.read_bytes()).hexdigest() + f'  {filename}\n').encode())
                result = subprocess.run(
                    [shutil.which('bash'), '--noprofile', '--norc', '-c', script],
                    cwd=root, env=dict(os.environ, TAG=tag, SHA=sha),
                    capture_output=True, text=True)
                self.assertEqual(result.returncode == 0, wrong_platform is None,
                                 result.stdout + result.stderr)
                if wrong_platform is None:
                    self.assertTrue((root / 'dist' / f'{tag}-macos-arm64.tar.gz').is_file())
                    self.assertTrue((root / 'dist' / f'{tag}-browser.tar.gz').is_file())

    def test_download_replacement_never_creates_a_release(self):
        """Keep explicit download replacement confined to an existing release."""
        workflow = (Path(__file__).resolve().parents[1] /
                    '.github/workflows/release.yml').read_text(encoding='utf-8')
        block = re.search(
            r'      - name: Publish release assets\n.*?        run: \|\n'
            r'((?:          [^\n]*\n|\n)+)', workflow, re.S)
        self.assertIsNotNone(block)
        script = textwrap.dedent(block[1])
        mock_gh = '''gh() {
          printf '%s\\n' "$*" >> gh.calls
          if [[ "$1 $2" == "release view" ]]; then
            return "$RELEASE_MISSING"
          fi
        }
        '''
        with tempfile.TemporaryDirectory() as directory:
            calls = Path(directory) / 'gh.calls'
            for rebuild, missing, allowed in [(True, False, True),
                                               (True, True, False),
                                               (False, True, True)]:
                with self.subTest(rebuild=rebuild, missing=missing):
                    calls.unlink(missing_ok=True)
                    env = dict(os.environ, MODE='publish',
                               TAG='pickaxe-miner-v0.0.3', SHA='a' * 40,
                               GH_REPO='example/miner',
                               REBUILD_SHA='a' * 40 if rebuild else '',
                               RELEASE_MISSING=str(int(missing)))
                    result = subprocess.run(
                        [shutil.which('bash'), '--noprofile', '--norc', '-c', mock_gh + script],
                        cwd=directory, env=env, capture_output=True, text=True)
                    self.assertEqual(result.returncode == 0, allowed,
                                     result.stdout + result.stderr)
                    commands = calls.read_text(encoding='utf-8')
                    self.assertEqual('release create ' in commands, not rebuild)
                    self.assertEqual('release upload ' in commands, not missing)
                    self.assertEqual('release edit ' in commands, not missing)
                    self.assertNotIn('git/refs/tags', commands)
            for missing in (False, True):
                with self.subTest(replace=True, missing=missing):
                    calls.unlink(missing_ok=True)
                    env = dict(os.environ, MODE='replace',
                               TAG='pickaxe-miner-v0.0.3', SHA='a' * 40,
                               GH_REPO='example/miner', REBUILD_SHA='',
                               RELEASE_MISSING=str(int(missing)))
                    result = subprocess.run(
                        [shutil.which('bash'), '--noprofile', '--norc', '-c', mock_gh + script],
                        cwd=directory, env=env, capture_output=True, text=True)
                    self.assertEqual(result.returncode == 0, not missing,
                                     result.stdout + result.stderr)
                    commands = calls.read_text(encoding='utf-8')
                    self.assertNotIn('release create ', commands)
                    self.assertEqual(
                        'api -X PATCH repos/example/miner/git/refs/tags/pickaxe-miner-v0.0.3 '
                        f'-f sha={"a" * 40} -F force=true' in commands, not missing)
                    self.assertEqual('release upload ' in commands, not missing)

    def test_publication_and_promotion_channels(self):
        """Enforce stable and prerelease ancestry plus replacement provenance."""
        workflow = (Path(__file__).resolve().parents[1] /
                    '.github/workflows/release.yml').read_text(encoding='utf-8')
        block = re.search(
            r'      - name: Verify release channel\n.*?        run: \|\n'
            r'((?:          [^\n]*\n|\n)+)', workflow, re.S)
        self.assertIsNotNone(block)
        script = textwrap.dedent(block[1])
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            remote = root / 'remote'
            clone = root / 'clone'

            def git(cwd, *args):
                """Run Git in a disposable repository and return its output."""
                return subprocess.check_output(
                    ['git', *args], cwd=cwd, text=True,
                    stderr=subprocess.STDOUT).strip()

            git(root, 'init', '-b', 'master', str(remote))
            git(remote, 'config', 'user.name', 'Release Test')
            git(remote, 'config', 'user.email', 'release-test@example.invalid')
            git(remote, '-c', 'commit.gpgsign=false', 'commit', '--allow-empty', '-m', 'stable')
            master = git(remote, 'rev-parse', 'HEAD')
            git(remote, 'tag', 'pickaxe-miner-v0.0.2')
            git(remote, 'checkout', '-b', 'dev')
            git(remote, '-c', 'commit.gpgsign=false', 'commit', '--allow-empty', '-m', 'alpha')
            dev = git(remote, 'rev-parse', 'HEAD')
            git(remote, 'checkout', '-b', 'feature')
            git(remote, '-c', 'commit.gpgsign=false', 'commit', '--allow-empty', '-m', 'unreviewed')
            feature = git(remote, 'rev-parse', 'HEAD')
            git(remote, 'tag', 'pickaxe-miner-v0.0.3', dev)
            git(remote, 'checkout', 'master')
            git(remote, '-c', 'commit.gpgsign=false', 'commit', '--allow-empty', '-m', 'fix')
            later = git(remote, 'rev-parse', 'HEAD')
            git(root, 'clone', str(remote), str(clone))

            cases = [
                ('publish', 'tag', '0.0.2', master, True),
                ('create', 'master', '0.0.2', master, True),
                ('publish', 'tag', '0.0.2-alpha.1', dev, True),
                ('publish', 'master', '0.0.2-alpha.1', dev, True),
                ('publish', 'tag', '0.0.2', dev, False),
                ('publish', 'tag', '0.0.2-alpha.1', feature, False),
                ('dry-run', 'master', '0.0.2-alpha.1', dev, False),
                ('create', 'master', '0.0.2-alpha.1', dev, False),
                ('dry-run', 'dev', '0.0.2-alpha.1', feature, True),
                ('dry-run', 'master', '0.0.2', feature, True),
                ('skip', 'master', '0.0.1', master, True),
                ('unknown', 'dev', '0.0.2', master, False),
                ('replace', 'master', '0.0.2', later, True),
                ('replace', 'master', '0.0.2', master, True),
                ('replace', 'master', '0.0.2', dev, False),
                ('replace', 'master', '0.0.3', later, False),
                ('replace', 'master', '0.0.2-alpha.1', later, False),
            ]
            for mode, branch, version, sha, allowed in cases:
                with self.subTest(mode=mode, branch=branch, version=version, sha=sha):
                    env = dict(os.environ, MODE=mode, BRANCH=branch,
                               TAG=f'pickaxe-miner-v{version}', SHA=sha)
                    result = subprocess.run(
                        [shutil.which('bash'), '--noprofile', '--norc', '-c', script],
                        cwd=clone, env=env, capture_output=True, text=True)
                    self.assertEqual(result.returncode == 0, allowed,
                                     result.stdout + result.stderr)
            for rebuild, workflow_sha, tag, allowed in [
                (master, master, 'pickaxe-miner-v0.0.2', True),
                (dev, master, 'pickaxe-miner-v0.0.2', False),
                (master, dev, 'pickaxe-miner-v0.0.2', False),
                ('master', master, 'pickaxe-miner-v0.0.2', False),
                (master, master, 'pickaxe-miner-v0.0.3', False),
            ]:
                with self.subTest(rebuild=rebuild, workflow_sha=workflow_sha, tag=tag):
                    env = dict(os.environ, MODE='publish', BRANCH='master', TAG=tag,
                               SHA=master, REBUILD_SHA=rebuild, WORKFLOW_SHA=workflow_sha)
                    result = subprocess.run(
                        [shutil.which('bash'), '--noprofile', '--norc', '-c', script],
                        cwd=clone, env=env, capture_output=True, text=True)
                    self.assertEqual(result.returncode == 0, allowed,
                                     result.stdout + result.stderr)


if __name__ == '__main__':
    unittest.main()
