#!/usr/bin/env python3
"""The Guarantees page drift check bites, and every claim keeps its recorded evidence."""

import json
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from guarantees_page import PAGE, REGISTRY, ROOT, headline, render

SCRIPT = ROOT / "scripts/guarantees_page.py"


class GuaranteesPageTests(unittest.TestCase):
    def setUp(self):
        self.registry = json.loads((ROOT / REGISTRY).read_text())

    def test_committed_page_is_current(self):
        self.assertEqual((ROOT / PAGE).read_text(), render(self.registry))

    def test_every_claim_appears_with_its_layers(self):
        page = render(self.registry)
        for subsystem in self.registry["subsystems"]:
            for claim in subsystem["obligationClaims"]:
                row = f"| `{claim['id']}` | {headline(claim)} | {claim['runtime']} | {claim['model']} | {claim['proof']} |"
                self.assertIn(row, page)

    def test_headline_never_exceeds_the_record(self):
        self.assertEqual(headline({"runtime": "tested", "model": "partial", "proof": "partial"}), "tested")
        self.assertEqual(headline({"runtime": "missing", "model": "missing", "proof": "missing"}), "no evidence")
        self.assertEqual(headline({"runtime": "missing", "model": "bounded_model_checked", "proof": "reference_proved"}),
                         "proved (reference model)")

    def test_stale_page_fails_the_check(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / REGISTRY).parent.mkdir(parents=True)
            (root / PAGE).parent.mkdir(parents=True)
            shutil.copy(ROOT / REGISTRY, root / REGISTRY)
            shutil.copy(ROOT / PAGE, root / PAGE)
            ok = subprocess.run([sys.executable, str(SCRIPT), "--check", "--root", str(root)], capture_output=True)
            self.assertEqual(ok.returncode, 0)
            registry = json.loads((root / REGISTRY).read_text())
            registry["subsystems"][0]["obligationClaims"][0]["proof"] = "implementation_proved"
            (root / REGISTRY).write_text(json.dumps(registry))
            stale = subprocess.run([sys.executable, str(SCRIPT), "--check", "--root", str(root)], capture_output=True, text=True)
            self.assertEqual(stale.returncode, 1)
            self.assertIn("is stale", stale.stderr)


if __name__ == "__main__":
    unittest.main()
