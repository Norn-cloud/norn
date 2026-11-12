#!/usr/bin/env python3
"""Regressions for missing, renamed and unexpectedly selected drop scenarios."""

import unittest


def _sibling(name, directory=None):
    """Load sibling module `name` from its source by explicit path: never through sys.path,
    never from a bytecode cache, never a same-named module loaded some other way (#513)."""
    import pathlib
    import sys
    import types

    path = (directory or pathlib.Path(__file__).resolve().parent) / f"{name}.py"
    loaded = sys.modules.get(name)
    if getattr(loaded, "__norn_sibling__", None) == str(path):
        return loaded
    module = types.ModuleType(name)
    module.__file__ = str(path)
    module.__norn_sibling__ = str(path)
    sys.modules[name] = module
    try:
        exec(compile(path.read_bytes(), str(path), "exec", dont_inherit=True), module.__dict__)
    except BaseException:
        del sys.modules[name]
        raise
    return module

_check_demo_scenarios = _sibling("check_demo_scenarios")
check_profile = _check_demo_scenarios.check_profile
required_scenarios = _check_demo_scenarios.required_scenarios


def listing(*names, ignored=False, excluded=()):
    cases = {name: {"ignored": ignored, "filter-match": {"status": "matches"}} for name in names}
    cases.update({name: {"ignored": False, "filter-match": {"status": "mismatch"}} for name in excluded})
    return {"rust-suites": {"engine::laws": {"testcases": cases}}}


class DropScenarioTests(unittest.TestCase):
    def setUp(self):
        self.required = {("engine::laws", "valid_commit"), ("engine::laws", "atomic_refusal")}

    def test_exact_selection_with_unrelated_exclusions_passes(self):
        check_profile("drop-test", self.required, listing("valid_commit", "atomic_refusal", excluded=("other",)))

    def test_renamed_scenario_fails_even_when_another_case_still_matches(self):
        # The old exact-name filter still matches valid_commit but excludes the renamed test.
        with self.assertRaisesRegex(ValueError, "missing .*atomic_refusal"):
            check_profile("drop-test", self.required, listing("valid_commit", excluded=("renamed_refusal",)))

    def test_broadened_filter_cannot_select_unlisted_cases(self):
        with self.assertRaisesRegex(ValueError, "unexpected .*unlisted_case"):
            check_profile("drop-test", self.required, listing("valid_commit", "atomic_refusal", "unlisted_case"))

    def test_ignored_required_case_is_not_qualified(self):
        with self.assertRaisesRegex(ValueError, "ignored scenario"):
            check_profile("drop-test", self.required, listing("valid_commit", "atomic_refusal", ignored=True))

    def test_new_profile_requires_its_own_nonempty_data(self):
        with self.assertRaisesRegex(ValueError, "every drop"):
            required_scenarios({"drop-old": {"engine::laws": ["case"]}}, {"drop-old", "drop-new"})
        with self.assertRaisesRegex(ValueError, "nonempty"):
            required_scenarios({"drop-empty": {}}, {"drop-empty"})

    def test_duplicate_required_names_refuse(self):
        with self.assertRaisesRegex(ValueError, "duplicate"):
            required_scenarios({"drop-test": {"engine::laws": ["case", "case"]}}, {"drop-test"})


if __name__ == "__main__":
    unittest.main()
