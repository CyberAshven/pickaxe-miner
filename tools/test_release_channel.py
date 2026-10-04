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
    def test_portable_archives_require_the_same_source_commit(self):
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
                    ('web', 'pickaxe-web-experimental.tar.gz', 'web/SOURCE_COMMIT.txt'),
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
                    self.assertTrue((root / 'dist' / f'{tag}-web.tar.gz').is_file())

    def test_download_replacement_never_creates_a_release(self):
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

    def test_publication_and_promotion_channels(self):
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
            git(remote, 'checkout', 'master')
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
