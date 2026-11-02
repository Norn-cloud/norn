#!/usr/bin/env python3
"""PROOF-BIND-1: named scenarios and generated projection properties."""
import copy
import json
import random
import subprocess
import tempfile
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
            self.assertIsNone(matrix.checked_result(Path("old-result.json"), {}, {"commit": "frozen"}))
        run.assert_called_once()
        self.assertIn("validate_result.py", run.call_args.args[0][1])

    def test_proof_changed_input_during_validation_is_unknown(self):
        with patch.object(matrix.subprocess, "run") as run, patch.object(Path, "read_bytes", return_value=b"{}"), \
                patch.object(matrix, "inputs", return_value=({}, {}, {"new": "digest"})):
            run.return_value.returncode = 0
            self.assertIsNone(matrix.checked_result(Path("result.json"), {"old": "digest"}, {"commit": "frozen"}))

    def test_guarantees_committed_inputs_are_immutable_during_workspace_edits(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            coverage = {"subsystems": [{"id": "core", "obligations": ["LAW"]}]}
            directory = root / "verification/coverage-v1/manifest"
            matrix.coverage_manifest.write(coverage, directory)
            files = {**{name: {"obligations": []} for name in matrix.REGISTRIES},
                     "scripts/guarantees_matrix.py": {"snapshot": True}}
            for name, value in files.items():
                path = root / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(json.dumps(value))
            for command in [["git", "init", "-q"], ["git", "add", "."],
                            ["git", "-c", "user.name=fixture", "-c", "user.email=fixture@example.invalid", "commit", "-qm", "fixture"]]:
                subprocess.run(command, cwd=root, check=True, capture_output=True)
            commit = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=root, text=True).strip()
            with patch.object(matrix, "ROOT", root), patch.object(matrix, "__file__", str(root / "scripts/guarantees_matrix.py")):
                before = matrix.inputs(commit)
                self.assertEqual(before[0], coverage)
                entry = next((directory / "subsystems/core/obligations").glob("*.json"))
                self.assertIn(entry.relative_to(root).as_posix(), before[2])
                entry.write_text("forged")
                self.assertEqual(before, matrix.inputs(commit))

    def test_guarantees_workspace_change_during_projection_discards_evidence(self):
        coverage, statements, pins, checked = fixture()
        with patch.object(matrix, "inputs", return_value=(coverage, statements, pins)), \
                patch.object(matrix, "checked_result", return_value=checked), \
                patch.object(matrix, "collect_git_identity", side_effect=[checked["git"], {"commit": "changed"}]):
            result = matrix.generate(Path("result.json"))
        self.assertEqual(result["rows"][0]["level"], "unknown")
        self.assertEqual(result["evidenceStatus"], "unknown_or_stale")

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

    def test_guarantees_edge_goal_without_a_law_index_is_retained(self):
        rows = matrix.challenge_rows({"lawStatements": [], "obligations": [
            {"goal": {"kind": "mapping_totality"}, "statement": {"edge": "mapping"}}]})
        self.assertIsNone(rows[0]["law"])
        self.assertEqual(rows[0]["level"], "unknown")

    def test_guarantees_partial_tests_retain_unknown_overall_scope(self):
        coverage, statements, pins, checked = fixture()
        coverage["subsystems"][0]["obligationClaims"][0]["runtime"] = "partial"
        row = matrix.project(coverage, statements, pins, checked)["rows"][0]
        self.assertEqual(row["level"], "unknown")
        self.assertEqual(row["tests"], {"level": "tested", "scope": "partial"})

    def test_guarantees_statement_digest_binds_the_entire_challenge(self):
        challenge = {"lawStatements": [{"artifactId": "core.law", "sha256": "a" * 64}],
            "obligations": [{"law": 0, "goal": {"kind": "initialization"}, "statement": {"law": 0}}]}
        before = matrix.challenge_rows(challenge)[0]["statementDigest"]
        challenge["lawStatements"][0]["sha256"] = "b" * 64
        self.assertNotEqual(before, matrix.challenge_rows(challenge)[0]["statementDigest"])

    def test_guarantees_malformed_challenges_fail_before_projection(self):
        for invalid in [[], {}, {"lawStatements": [], "obligations": {}},
                        {"lawStatements": [], "obligations": [None]},
                        {"lawStatements": [], "obligations": [{}]},
                        {"lawStatements": [], "obligations": [{"statement": {}}]}]:
            with self.subTest(invalid=invalid), self.assertRaisesRegex(ValueError, "invalid challenge"):
                matrix.challenge_rows(invalid)

    def test_guarantees_input_identity_changes_with_checked_evidence(self):
        coverage, statements, pins, checked = fixture()
        self.assertNotEqual(matrix.project(coverage, statements, pins, checked)["inputDigest"],
                            matrix.project(coverage, statements, pins, None)["inputDigest"])

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
