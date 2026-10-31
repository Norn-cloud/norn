#!/usr/bin/env python3
"""Positive/negative enrollment regressions against the actual public tree."""

import copy
import importlib.util
import json
import tempfile
import unittest
from pathlib import Path

from proof_enrollment import ROOT, discover_sources, load_coverage, validate


class EnrollmentTests(unittest.TestCase):
    def setUp(self):
        self.coverage = json.loads((ROOT / "verification/coverage-v1/coverage.json").read_text())
        self.gates = json.loads((ROOT / "verification/coverage-v1/promoted-gates.json").read_text())

    def test_actual_absent_domain_tree(self):
        self.assertEqual(validate(ROOT, self.coverage, self.gates), set())

    def test_new_unregistered_subsystem(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = root / "crates/unregistered/src/lib.rs"
            path.parent.mkdir(parents=True)
            path.write_text("pub struct NewSubsystem;\n")
            with self.assertRaisesRegex(ValueError, "unregistered=.*unregistered/src/lib.rs"):
                validate(root, self.coverage, self.gates)

    def test_deleted_enrollment_cannot_delete_code(self):
        self.coverage["subsystems"] = [s for s in self.coverage["subsystems"] if s["id"] != "quantity-ledger"]
        with self.assertRaisesRegex(ValueError, "exact source enrollment mismatch"):
            validate(ROOT, self.coverage, self.gates)

    def test_unregistered_other_languages_and_extensionless_scripts(self):
        examples = (("new/main.go", "package main\n"),
                    ("new/worker.rb", "puts 'synthetic'\n"),
                    ("new/run", "#!/bin/sh\nexit 0\n"))
        for name, source in examples:
            with self.subTest(name=name), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                path = root / name
                path.parent.mkdir(parents=True)
                path.write_text(source)
                with self.assertRaisesRegex(ValueError, "exact source enrollment mismatch"):
                    validate(root, self.coverage, self.gates)

    def test_unregistered_executable_without_known_suffix_or_shebang(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = root / "program.bin"
            path.write_bytes(b"synthetic executable fixture")
            path.chmod(0o755)
            with self.assertRaisesRegex(ValueError, "exact source enrollment mismatch"):
                validate(root, self.coverage, self.gates)

    def test_reintroduced_domain_code_or_law_requires_full_enrollment(self):
        for name, source in (("crates/norn-checker/src/accounting.rs", "pub struct AccountingState;"), ("verification/renamed/authority.lean", "namespace AccountingAuthority"), ("verification/renamed/quantity.lean", "namespace ConservedQuantity"), ("crates/norn-checker/src/renamed.rs", "pub struct InventoryState;"), ("verification/renamed/stock.qnt", "module InventoryAuthority {}")):
            with self.subTest(name=name), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                path = root / name
                path.parent.mkdir(parents=True)
                path.write_text(source)
                candidate = copy.deepcopy(self.coverage)
                # Account for this independent source to exercise held-domain
                # checks rather than the generic missing-source rejection.
                candidate["subsystems"] = [{"id": "verification-integrity", "sourceFiles": sorted(discover_sources(root))}]
                with self.assertRaisesRegex(ValueError, "independently discovered authority/law presence"):
                    validate(root, candidate, self.gates)

    def test_missing_generic_obligation_rejected_by_actual_coverage_validator(self):
        spec = importlib.util.spec_from_file_location("coverage_validator", ROOT / "verification/coverage-v1/validate.py")
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        for subsystem in self.coverage["subsystems"]:
            if subsystem["id"] == "quantity-ledger":
                subsystem["obligations"].remove("INV-QL-CONSERVATION")
                subsystem["obligationClaims"] = [c for c in subsystem["obligationClaims"] if c["id"] != "INV-QL-CONSERVATION"]
        with tempfile.TemporaryDirectory() as directory:
            candidate = Path(directory) / "coverage.json"
            candidate.write_text(json.dumps(self.coverage))
            with self.assertRaisesRegex(module.InvalidCoverage, "unreferenced P0 obligations.*INV-QL-CONSERVATION"):
                module.validate(coverage_path=candidate)

    def test_law_artifact_cannot_hide_under_another_subsystem(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "crates/toy/src/lib.rs"
            source.parent.mkdir(parents=True)
            source.write_text("pub struct Toy;")
            law = root / "registry/renamed.json"
            law.parent.mkdir()
            law.write_text(json.dumps({"kind": "law", "artifactId": "renamed", "payload": {"domain": "accounting"}}))
            candidate = {"subsystems": [{"id": "verification-integrity", "sourceFiles": ["crates/toy/src/lib.rs"]}]}
            with self.assertRaisesRegex(ValueError, "independently discovered authority/law presence"):
                validate(root, candidate, self.gates)

    def test_partial_domain_enrollment_is_insufficient(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "crates/toy/src/accounting.rs"
            source.parent.mkdir(parents=True)
            source.write_text("pub struct AccountingState;")
            candidate = {"subsystems": [{"id": "accounting-pack", "sourceFiles": ["crates/toy/src/accounting.rs"], "obligations": ["INV-ACC-JOURNAL-BALANCED"]}]}
            with self.assertRaisesRegex(ValueError, "full domain runtime/Lean/Quint gate enrollment required"):
                validate(root, candidate, self.gates)

    def test_either_coverage_layout_never_both(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            monolith = root / "verification/coverage-v1/coverage.json"
            monolith.parent.mkdir(parents=True)
            monolith.write_text(json.dumps(self.coverage))
            self.assertEqual(load_coverage(root), self.coverage)
            split = root / "verification/coverage-v1/manifest/subsystems"
            split.mkdir(parents=True)
            (split.parent / "index.json").write_text("{}")
            for subsystem in self.coverage["subsystems"]:
                body = {key: value for key, value in subsystem.items() if key != "id"}
                (split / f"{subsystem['id']}.json").write_text(json.dumps(body))
            with self.assertRaisesRegex(ValueError, "exactly one of"):
                load_coverage(root)
            monolith.unlink()
            loaded = sorted(load_coverage(root)["subsystems"], key=lambda item: item["id"])
            self.assertEqual(loaded, sorted(self.coverage["subsystems"], key=lambda item: item["id"]))
            self.assertEqual(validate(ROOT, load_coverage(root), self.gates), set())
            (split / "notes.txt").write_text("synthetic")
            with self.assertRaisesRegex(ValueError, "not a subsystem file"):
                load_coverage(root)
            (split / "notes.txt").unlink()
            (split.parent / "index.json").unlink()
            with self.assertRaisesRegex(ValueError, "index.json is missing"):
                load_coverage(root)


if __name__ == "__main__":
    unittest.main()
