#!/usr/bin/env python3
"""Migration invariants: preserve disjoint intent and refuse ambiguous edits.

Run: python3 scripts/test_migrate_coverage.py
"""
import unittest

from migrate_coverage import MISSING, merge, parse


class MigrationTests(unittest.TestCase):
    def test_disjoint_enrollment_survives_split(self):
        base = {'subsystems': [{'id': 'core', 'sourceFiles': ['old.rs']}]}
        own = {'subsystems': [{'id': 'core', 'sourceFiles': ['old.rs', 'pr.rs']}]}
        current = {'subsystems': [{'id': 'core', 'sourceFiles': ['old.rs', 'main.rs']},
                                  {'id': 'new', 'sourceFiles': ['new.rs']}]}
        result = merge(base, own, current, 'coverage')
        self.assertEqual(result['subsystems'], [
            {'id': 'core', 'sourceFiles': ['old.rs', 'main.rs', 'pr.rs']},
            {'id': 'new', 'sourceFiles': ['new.rs']}])

    def test_deletions_survive(self):
        self.assertIs(merge({'a': 1}, MISSING, {'a': 1}, 'record'), MISSING)
        self.assertEqual(merge(['a', 'b'], ['b'], ['a', 'b', 'c'], 'files'), ['b', 'c'])

    def test_concurrent_scalar_edit_refused(self):
        with self.assertRaisesRegex(ValueError, 'ambiguous concurrent change'):
            merge({'license': 'A'}, {'license': 'B'}, {'license': 'C'}, 'record')

    def test_unknown_list_identity_refused(self):
        with self.assertRaisesRegex(ValueError, 'ambiguous concurrent change'):
            merge([{'name': 'a'}], [{'name': 'b'}], [{'name': 'c'}], 'evidence')

    def test_concurrent_reorder_refused(self):
        with self.assertRaisesRegex(ValueError, 'concurrent list reorder'):
            merge(['a', 'b'], ['b', 'a'], ['a', 'b', 'c'], 'files')

    def test_duplicate_json_key_refused(self):
        with self.assertRaisesRegex(ValueError, 'duplicate JSON key'):
            parse('{"id": "a", "id": "b"}')

    def test_duplicate_identity_refused(self):
        with self.assertRaisesRegex(ValueError, 'duplicate ids'):
            merge([{'id': 'a'}], [{'id': 'a'}, {'id': 'a'}],
                  [{'id': 'a'}, {'id': 'b'}], 'subsystems')


if __name__ == '__main__':
    unittest.main()
