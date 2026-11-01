#!/usr/bin/env python3
"""Conformance tests for GUAR-* in verification/guarantees-v1/spec.md."""
import copy
import json
import unittest

from guarantees_document import ROOT, OUTPUT, project, read, render

LAW = {"id": "law", "statement": "A named law.", "lawLayer": "workflow"}
LINK = {"artifact": "proof.lean", "command": "lean proof.lean", "selectors": ["theorem:law"]}


def registry(claim):
    return {"subsystems": [{"id": "demo", "obligationClaims": [claim],
                            "limitations": ["Reference scope only."]}]}


class GuaranteesConformance(unittest.TestCase):
    def row(self, claim):
        return project(registry(claim), [LAW])["guarantees"][0]

    def test_unknown_and_no_runtime_promotion(self):
        for claim in ({"id": "law"}, {"id": "other"},
                      {"id": "law", "runtime": "tested", "model": "bounded_model_checked",
                       "proof": "partial", "evidence": {"runtime": [LINK], "proof": [LINK]}}):
            row = self.row(claim)
            self.assertTrue(all(x == {"level": "unknown", "links": []}
                                for x in row["evidence"].values()))
            self.assertEqual(row["installation"], "unknown")

    def test_independent_evidence_and_removal(self):
        for key, claim, label in (
            ("model", {"proof": "reference_proved", "evidence": {"proof": [LINK]}}, "proved on the model"),
            ("execution", {"execution": {"status": "enforced", "evidence": [LINK]}}, "enforced in execution"),
            ("parity", {"parity": {"status": "tested", "evidence": [LINK]}}, "tested for parity"),
        ):
            row = self.row(dict(id="law", **claim))
            self.assertEqual(row["evidence"][key], {"level": label, "links": [LINK]})
            self.assertEqual(sum(x["level"] != "unknown" for x in row["evidence"].values()), 1)
            for field in LINK:
                broken = copy.deepcopy(claim)
                evidence = broken["evidence"]["proof"] if key == "model" else broken[key]["evidence"]
                del evidence[0][field]
                self.assertEqual(self.row(dict(id="law", **broken))["evidence"][key]["level"], "unknown")

    def test_duplicates_and_scope(self):
        with self.assertRaisesRegex(ValueError, "duplicate"):
            project(registry({"id": "law"}), [LAW, LAW])
        repeated = registry({"id": "law"})
        repeated["subsystems"] *= 2
        with self.assertRaisesRegex(ValueError, "duplicate"):
            project(repeated, [LAW])
        self.assertEqual(self.row({"id": "law", "residual": "Unbound."})["recordedCoverage"]["limitations"],
                         ["Reference scope only."])

    def test_document_drift(self):
        payload = render(ROOT)
        self.assertEqual((ROOT / OUTPUT).read_bytes(), payload.encode("utf-8"))
        enrolled = {c["id"] for s in read(ROOT, "verification/coverage-v1/coverage.json")["subsystems"]
                    for c in s["obligationClaims"]}
        rows = json.loads(payload)["guarantees"]
        self.assertTrue(enrolled <= {row["id"] for row in rows})
        self.assertTrue({"POST-ATOMIC", "POST-VALUE", "POST-IDENTITY", "POST-ORDER", "POST-AUTHORITY"}
                        <= {row["id"] for row in rows})


if __name__ == "__main__":
    unittest.main()
