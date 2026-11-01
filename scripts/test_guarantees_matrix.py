#!/usr/bin/env python3
"""PROOF-BIND-1: named scenarios and generated projection properties."""
import copy
import random
import unittest
from pathlib import Path
from unittest.mock import patch

import guarantees_matrix as matrix


def fixture():
    entries = {layer: [{"artifact": f"{layer}.source", "command": f"check {layer}",
                        "selectors": ["theorem:conserved" if layer == "proof" else "test:conserved"]}]
               for layer in ("proof", "runtime")}
    coverage = {"subsystems": [{"id": "core", "obligations": ["LAW"], "limitations": ["scope"],
        "obligationClaims": [{"id": "LAW", "proof": "reference_proved", "runtime": "tested",
                              "evidence": entries, "residual": "Not an implementation proof"}]}]}
    checked = {"git": {"commit": "frozen"}, "tools": {"checker": "pinned"}, "executedGates": [{"command": f"check {layer}", "result": "pass",
        "role": "claim-evidence", "classifications": [{"subsystem": "core", "layer": layer}]}
        for layer in ("proof", "runtime")],
        "evidence": [{"path": f"{layer}.source", "sha256": "a" * 64} for layer in ("proof", "runtime")]}
    statements = {"LAW": {"law": "core.conservation", "statement": "Conservation", "assumptions": ["exact"]}}
    return coverage, statements, {"source": "b" * 64}, checked


class GuaranteesScenarios(unittest.TestCase):
    def test_guarantees_missing_result_never_promotes_declared_claims(self):
        coverage, statements, pins, _ = fixture()
        result = matrix.project(coverage, statements, pins, None)
        self.assertEqual(result["rows"][0]["level"], "unknown")
        self.assertEqual(result["rows"][0]["proof"]["level"], "unknown")
        self.assertEqual(result["rows"][0]["evidence"], {"proof": [], "runtime": []})
        self.assertEqual(result["authority"], "projection_only")

    def test_guarantees_checked_reference_proof_keeps_its_scope(self):
        row = matrix.project(*fixture())["rows"][0]
        self.assertEqual(row["level"], "tested")
        self.assertEqual(row["proof"], {"level": "proved", "scope": "reference_proved"})
        self.assertEqual(row["residual"], "Not an implementation proof")
        self.assertEqual(row["assumptions"], ["exact"])

    def test_guarantees_checked_implementation_proof(self):
        coverage, statements, pins, checked = fixture()
        coverage["subsystems"][0]["obligationClaims"][0]["proof"] = "implementation_proved"
        self.assertEqual(matrix.project(coverage, statements, pins, checked)["rows"][0]["level"], "proved")

    def test_proof_stale_validator_refusal_discards_all_evidence(self):
        with patch.object(matrix.subprocess, "run") as run, patch.object(Path, "read_bytes", return_value=b"{}"):
            run.return_value.returncode = 1
            self.assertIsNone(matrix.checked_result(Path("old-result.json"), {}))
        run.assert_called_once()
        self.assertIn("validate_result.py", run.call_args.args[0][1])

    def test_proof_changed_input_during_validation_is_unknown(self):
        with patch.object(matrix.subprocess, "run") as run, patch.object(Path, "read_bytes", return_value=b"{}"), \
                patch.object(matrix, "inputs", return_value=({}, {}, {"new": "digest"})):
            run.return_value.returncode = 0
            self.assertIsNone(matrix.checked_result(Path("result.json"), {"old": "digest"}))

    def test_guarantees_unclaimed_obligation_remains_visible_and_unknown(self):
        coverage, statements, pins, checked = fixture()
        coverage["subsystems"][0]["obligations"].append("NEW")
        rows = matrix.project(coverage, statements, pins, checked)["rows"]
        self.assertEqual(rows[1]["obligation"], "NEW")
        self.assertEqual(rows[1]["level"], "unknown")

    def test_guarantees_generated_frame_assessment_does_not_prove_the_law(self):
        challenge = {"lawStatements": [{"artifactId": "core.law", "sha256": "a" * 64}],
            "obligations": [{"law": 0, "goal": {"kind": "preservation"},
                "statement": {"rule": "preservation"}, "assessment": "transition_frame"}]}
        row = matrix.challenge_rows(challenge)[0]
        self.assertEqual(row["level"], "unknown")
        self.assertEqual(row["law"], challenge["lawStatements"][0])

    def test_generated_evidence_removal_never_increases_assurance(self):
        rng = random.Random(213)
        rank = {"unknown": 0, "tested": 1, "proved": 2}
        for _ in range(256):
            coverage, statements, pins, checked = fixture()
            coverage["subsystems"][0]["obligationClaims"][0]["proof"] = rng.choice(
                ["missing", "partial", "reference_proved", "implementation_proved"])
            before = matrix.project(coverage, statements, pins, checked)["rows"][0]
            reduced = copy.deepcopy(checked)
            reduced[rng.choice(["executedGates", "evidence"])].clear()
            after = matrix.project(coverage, statements, pins, reduced)["rows"][0]
            self.assertLessEqual(rank[after["level"]], rank[before["level"]])
            self.assertEqual(after["proof"]["level"], "unknown")


if __name__ == "__main__":
    unittest.main()
