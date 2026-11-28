#!/usr/bin/env python3
"""Isolated test runner for `tools/norn-py` (works under `python3 -I`)."""

from __future__ import annotations

import sys
import unittest
from pathlib import Path


def main() -> int:
    root = Path(__file__).resolve().parent
    root_str = str(root)
    if root_str not in sys.path:
        sys.path.insert(0, root_str)
    suite = unittest.defaultTestLoader.discover(start_dir=str(root / "tests"))
    if suite.countTestCases() == 0:
        print("norn-py: discovered 0 tests", file=sys.stderr)
        return 1
    result = unittest.TextTestRunner(verbosity=2).run(suite)
    return 0 if result.wasSuccessful() else 1


if __name__ == "__main__":
    raise SystemExit(main())
