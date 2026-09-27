"""Exercise the actual workflow gate against disposable Git branch histories."""
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import textwrap
import unittest


class ReleaseChannelTest(unittest.TestCase):
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


if __name__ == '__main__':
    unittest.main()
