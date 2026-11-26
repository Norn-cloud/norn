#!/usr/bin/env python3
"""Unit and refutation tests for scripts/ci/check_test_wiring.py."""

from __future__ import annotations

import unittest
from pathlib import Path


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


ctw = _sibling("check_test_wiring")


class TestCheckTestWiring(unittest.TestCase):
    def test_detects_unwired_python_test_file(self) -> None:
        errors = ctw.check_py_test_wiring(
            ["scripts/test_alpha.py", "scripts/test_orphan.py"],
            "python3 scripts/test_alpha.py\n",
        )
        self.assertEqual(len(errors), 1)
        self.assertIn("scripts/test_orphan.py", errors[0])

    def test_accepts_unittest_discover_directory(self) -> None:
        errors = ctw.check_py_test_wiring(
            ["tools/norn-py/tests/test_norn_py.py"],
            'PYTHONPATH="$PWD/tools/norn-py" python3 -m unittest discover -s tools/norn-py/tests -t tools/norn-py/tests\n',
        )
        self.assertEqual(errors, [])

    def test_detects_unwired_js_package_and_standalone_mjs(self) -> None:
        errors = ctw.check_js_test_wiring(
            [
                "apps/sidekick-bridge/relay.test.ts",
                "apps/norn-cell-do/wake.test.mjs",
                "apps/norn-cell-do/orphan.test.mjs",
                "packages/new-pkg/foo.test.ts",
            ],
            "(cd apps/sidekick-bridge && bun test)\n(cd apps/norn-cell-do && bun test ./wake.test.mjs)\n",
        )
        self.assertEqual(len(errors), 2)
        self.assertTrue(any("packages/new-pkg" in e for e in errors))
        self.assertTrue(any("apps/norn-cell-do/orphan.test.mjs" in e for e in errors))

    def test_detects_fail_open_head_fallback(self) -> None:
        errors = ctw.check_no_head_base_fallback(
            {
                "scripts/ci/core.sh": '# comment ${NORN_CI_BASE:-HEAD}\nbaseline-shrink-only.sh "${NORN_CI_BASE:-HEAD}"\n',
                "crates/norn-skeleton-guard/scripts/ok.sh": 'baseline-shrink-only.sh "${NORN_CI_BASE:-origin/main}"\n',
            }
        )
        self.assertEqual(len(errors), 1)
        self.assertIn("scripts/ci/core.sh:2", errors[0])

    def test_live_repository_passes_all_invariants(self) -> None:
        self.assertEqual(ctw.check_repo(Path(ctw.REPO)), [])


if __name__ == "__main__":
    unittest.main()
