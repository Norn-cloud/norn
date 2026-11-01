#!/usr/bin/env python3
"""Merge refusal and preservation scenarios, exercised in real Git merges."""
import hashlib
import json
from pathlib import Path
import subprocess
import tempfile
import unittest

from merge_main import manifest

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

    def scenario(self, path, base, ours, theirs, success, source_conflict=False):
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
            if source_conflict:
                self.put('source.txt', 'our source')
            self.commit()
            self.run_git('checkout', 'incoming')
            self.put(path, theirs)
            if source_conflict:
                self.put('source.txt', 'their source')
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

    def test_one_sided_metadata_change_is_preserved(self):
        values = [dict(sha256=s, parents=[]) for s in ('base', 'ours', 'theirs')]
        values[2]['parents'] = ['new-parent']
        self.assertEqual(self.scenario(RECORD, *values, True)['parents'], ['new-parent'])

    def test_unknown_conflict_aborts(self):
        self.scenario('unknown.json', {'x': 0}, {'x': 1}, {'x': 2}, False)

    def test_unresolved_source_aborts(self):
        values = [dict(sha256=s, origin='authored') for s in ('base', 'ours', 'theirs')]
        self.scenario(RECORD, *values, False, source_conflict=True)

    def test_field_conflict_aborts(self):
        values = [dict(sha256=s, origin=s) for s in ('base', 'ours', 'theirs')]
        self.scenario(RECORD, *values, False)

    def test_manifest_union(self):
        values = [dict(sourceFiles=['base', s]) for s in ('base', 'ours', 'theirs')]
        self.assertEqual(self.scenario(COVERAGE, *values, True), dict(sourceFiles=['base', 'ours', 'theirs']))

    def test_union_retains_one_sided_deletions(self):
        self.assertEqual(
            manifest(['retained'], ['new'], ['retained']),
            ['new', 'retained'],
        )

    def test_same_entry_conflict_aborts(self):
        values = [dict(obligationClaims=[dict(id='law', runtime=s)]) for s in ('base', 'ours', 'theirs')]
        self.scenario(COVERAGE, *values, False)


if __name__ == '__main__':
    unittest.main()
