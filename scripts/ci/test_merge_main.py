#!/usr/bin/env python3
"""Merge refusal and preservation scenarios, exercised in real Git merges."""
import hashlib
import json
from pathlib import Path
import subprocess
import tempfile
import unittest

TOOL = Path(__file__).with_name('merge_main.py').resolve()
RECORD = '.github/ci/provenance/artifacts/source.txt.json'
COVERAGE = 'verification/coverage-v1/manifest/subsystems/test.json'


class MergeTests(unittest.TestCase):
    def run_git(self, *args):
        return subprocess.run(['git', '-C', str(self.root), *args], check=True, capture_output=True)

    def put(self, path, value):
        target = self.root / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(json.dumps(value, indent=2) + '\n')

    def commit(self):
        self.run_git('add', '.')
        self.run_git('commit', '-m', 'fixture')

    def scenario(self, path, base, ours, theirs, success):
        with tempfile.TemporaryDirectory() as directory:
            self.root = Path(directory)
            self.run_git('init', '-b', 'topic')
            self.run_git('config', 'user.name', 'Synthetic')
            self.run_git('config', 'user.email', 'synthetic@example.invalid')
            self.put('source.txt', 'merged source')
            self.put(path, base)
            self.commit()
            self.run_git('branch', 'incoming')
            self.put(path, ours)
            self.commit()
            self.run_git('checkout', 'incoming')
            self.put(path, theirs)
            self.commit()
            self.run_git('update-ref', 'refs/remotes/origin/main', 'HEAD')
            self.run_git('checkout', 'topic')
            result = subprocess.run(['python3', str(TOOL)], cwd=self.root, capture_output=True)
            self.assertEqual(result.returncode, 0 if success else 1, result.stderr.decode())
            if success:
                return json.loads((self.root / path).read_text())
            self.assertTrue(self.run_git('ls-files', '-u').stdout)

    def test_sha_only(self):
        values = [dict(sha256=s, origin='authored', license='Apache-2.0') for s in ('base', 'ours', 'theirs')]
        merged = self.scenario(RECORD, *values, True)
        expected = hashlib.sha256((json.dumps('merged source', indent=2) + '\n').encode()).hexdigest()
        self.assertEqual(merged, {**values[0], 'sha256': expected})

    def test_field_conflict_aborts(self):
        values = [dict(sha256=s, origin=s) for s in ('base', 'ours', 'theirs')]
        self.scenario(RECORD, *values, False)

    def test_manifest_union(self):
        values = [dict(sourceFiles=['base', s]) for s in ('base', 'ours', 'theirs')]
        self.assertEqual(self.scenario(COVERAGE, *values, True), dict(sourceFiles=['base', 'ours', 'theirs']))

    def test_same_entry_conflict_aborts(self):
        values = [dict(obligationClaims=[dict(id='law', runtime=s)]) for s in ('base', 'ours', 'theirs')]
        self.scenario(COVERAGE, *values, False)


if __name__ == '__main__':
    unittest.main()
