#!/usr/bin/env python3
"""PROOF-BIND-1: only admitted declarations supply demo law identities."""
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent / "demo"))
from guarantees_drop import pack_rows  # noqa: E402


class PackInventoryScenarios(unittest.TestCase):
    def test_missing_declarations_are_unknown_without_invented_law_ids(self):
        row = pack_rows({"laws": []}, [])
        self.assertEqual(row["laws"], [])
        self.assertEqual(row["message"], "declaration missing → unknown")

    def test_admitted_law_retains_pin_and_unknown_evidence(self):
        pin = {"artifactId": "pack.law", "version": "1", "sha256": "a" * 64}
        inventory = {"laws": [{"pin": pin, "law": {"id": "declared.law", "subject": "pack.Thing"}}]}
        row = pack_rows(inventory, ["pack.law"])["laws"][0]
        self.assertEqual(row["pin"], pin)
        self.assertEqual(row["law"], "declared.law")
        self.assertEqual(row["level"], "unknown")
        self.assertEqual(pack_rows(inventory, ["DATA-INGEST"])["laws"], [])


if __name__ == "__main__":
    unittest.main()
