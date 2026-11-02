#!/usr/bin/env python3
"""Positive/negative enrollment regressions against the actual public tree."""

import copy
import importlib.util
import json
import tempfile
import unittest
from pathlib import Path

import coverage_manifest
from test_migrate_registry_pr import MigrationTests
from proof_enrollment import ROOT, discover_sources, validate


class EnrollmentTests(unittest.TestCase):
    def setUp(self):
        self.coverage = coverage_manifest.load()
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

    def test_workflow_cleanup_contract_is_exact(self):
        spec = importlib.util.spec_from_file_location("coverage_validator", ROOT / "verification/coverage-v1/validate.py")
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        source = module.FORMAL_WORKFLOW.read_text()
        module.validate_workflow_integrity([])
        workflow = module.yaml.load(source, Loader=module.WorkflowLoader)
        for job in ("core", "test", "phase"):
            for mutation in ("extra", "reordered", "missing", "changed", "unconditional", "event-only", "runner-only"):
                with self.subTest(job=job, mutation=mutation), tempfile.TemporaryDirectory() as directory:
                    candidate = copy.deepcopy(workflow)
                    steps = candidate["jobs"][job]["steps"]
                    if mutation == "extra":
                        steps.insert(1, {"run": "echo unexpected"})
                    elif mutation == "reordered":
                        steps[0], steps[1] = steps[1], steps[0]
                    elif mutation == "missing":
                        steps.pop(0)
                    elif mutation == "unconditional":
                        steps[0].pop("if")
                    elif mutation == "event-only":
                        steps[0]["if"] = "${{ github.event_name != 'merge_group' }}"
                    elif mutation == "runner-only":
                        steps[0]["if"] = "${{ runner.environment != 'self-hosted' }}"
                    else:
                        steps[0]["run"] += "echo unexpected\n"
                    path = Path(directory) / "workflow.yml"
                    path.write_text(module.yaml.safe_dump(candidate, sort_keys=False))
                    with self.assertRaisesRegex(module.InvalidCoverage, "preparation actions and inputs must match the approved contract"):
                        module.validate_workflow_integrity([], formal_path=path)

    def test_missing_generic_obligation_rejected_by_actual_coverage_validator(self):
        spec = importlib.util.spec_from_file_location("coverage_validator", ROOT / "verification/coverage-v1/validate.py")
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        for subsystem in self.coverage["subsystems"]:
            if subsystem["id"] == "quantity-ledger":
                subsystem["obligations"].remove("INV-QL-CONSERVATION")
                subsystem["obligationClaims"] = [c for c in subsystem["obligationClaims"] if c["id"] != "INV-QL-CONSERVATION"]
        with tempfile.TemporaryDirectory() as directory:
            candidate = Path(directory) / "manifest"
            coverage_manifest.write(self.coverage, candidate)
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


class CoverageLayoutTests(unittest.TestCase):
    def test_both_layouts_preserve_every_claim_and_order(self):
        document = coverage_manifest.load()
        # Repeated artifact paths remain valid; occurrence IDs preserve them.
        subsystem = next(s for s in document["subsystems"] if s["runtime"]["artifacts"])
        subsystem["runtime"]["artifacts"] *= 2
        for split in (False, True):
            with self.subTest(split=split), tempfile.TemporaryDirectory() as temporary:
                target = Path(temporary) / "manifest"
                coverage_manifest.write(document, target, split_entries=split)
                # Git archives omit empty directories.
                for folder in sorted(target.rglob("*"), reverse=True):
                    if folder.is_dir() and not any(folder.iterdir()):
                        folder.rmdir()
                self.assertEqual(coverage_manifest.load(target), document)
                self.assertTrue(coverage_manifest.digest(target))

    def test_nested_evidence_additions_touch_independent_files(self):
        original = {"subsystems": [{"id": "toy", "obligationClaims": [{"id": "INV-X", "evidence": {"runtime": [{"artifact": "a.rs", "selectors": ["test:a"]}]}}]}]}
        with tempfile.TemporaryDirectory() as temporary:
            target = Path(temporary) / "manifest"
            coverage_manifest.write(original, target, split_entries=True)
            before = {p.relative_to(target): p.read_bytes() for p in coverage_manifest.files(target)}
            updated = copy.deepcopy(original)
            updated["subsystems"][0]["obligationClaims"][0]["evidence"]["runtime"][0]["selectors"].append("test:b")
            folder = next((target / "subsystems/toy").glob("*.selectors"))
            (folder / (coverage_manifest.entry_id("test:b") + ".json")).write_text(json.dumps({"order": 1, "value": "test:b"}))
            after = {p.relative_to(target): p.read_bytes() for p in coverage_manifest.files(target)}
            self.assertEqual([p for p in before if before[p] != after.get(p)], [])
            self.assertEqual(len(after) - len(before), 1)
            self.assertEqual(coverage_manifest.load(target), updated)

    def test_distinct_evidence_bindings_can_share_an_artifact(self):
        bindings = [{"artifact": "a.rs", "command": command, "selectors": selectors} for command, selectors in (("cargo test", ["test:a"]), ("cargo test", ["test:b"]), ("cargo test --all-features", ["test:a"]))]
        document = {"subsystems": [{"id": "toy", "obligationClaims": [{"id": "INV-X", "evidence": {"runtime": bindings}}]}]}
        with tempfile.TemporaryDirectory() as temporary:
            target = Path(temporary) / "manifest"
            coverage_manifest.write(document, target, split_entries=True)
            self.assertEqual(coverage_manifest.load(target), document)

    def test_layout_refusals(self):
        document = {"subsystems": [{"id": "toy", "sourceFiles": ["a.rs", "b.rs"], "obligations": ["INV-X"]}]}
        for split in (False, True):
            for mutation in ("stray", "duplicate-key", "symlink", "duplicate-id"):
                with self.subTest(split=split, mutation=mutation), tempfile.TemporaryDirectory() as temporary:
                    target = Path(temporary) / "manifest"
                    coverage_manifest.write(document, target, split_entries=split)
                    subsystem = target / "subsystems" / ("toy" if split else "toy.json")
                    metadata = subsystem / "index.json" if split else subsystem
                    if mutation == "stray":
                        (target / "stray").write_text("{}")
                    elif mutation == "duplicate-key":
                        metadata.write_text('{"sourceFiles":[],"sourceFiles":[]}')
                    elif mutation == "symlink":
                        metadata.unlink()
                        metadata.symlink_to(target / "index.json")
                    else:
                        other = target / "subsystems" / ("toy.json" if split else "toy")
                        if split:
                            other.write_text("{}")
                        else:
                            other.mkdir()
                            (other / "index.json").write_text("{}")
                    expected = {"stray": "unknown coverage entries", "duplicate-key": "duplicate JSON key", "symlink": "not a regular file", "duplicate-id": "duplicate subsystem"}[mutation]
                    with self.assertRaisesRegex(coverage_manifest.InvalidCoverageLayout, expected):
                        coverage_manifest.load(target)

    def test_subsystem_order_is_identical_in_old_new_and_mixed_layouts(self):
        expected = {"subsystems": [{"id": "a-b"}, {"id": "a"}]}
        for split in (False, True):
            with tempfile.TemporaryDirectory() as temporary:
                target = Path(temporary) / "manifest"
                coverage_manifest.write(expected, target, split_entries=split)
                self.assertEqual(coverage_manifest.load(target), expected)
                if split:
                    (target / "subsystems/a-b/index.json").rename(target / "subsystems/a-b.json")
                    (target / "subsystems/a-b").rmdir()
                    self.assertEqual(coverage_manifest.load(target), expected)

    def test_entry_field_validation(self):
        for body in ({"../escaped": []}, {"a": {"b": []}, "a.b": []}):
            with tempfile.TemporaryDirectory() as temporary:
                target = Path(temporary) / "manifest"
                with self.assertRaisesRegex(coverage_manifest.InvalidCoverageLayout, "entry field"):
                    coverage_manifest.write({"subsystems": [{"id": "toy", **body}]}, target, split_entries=True)
                self.assertFalse((target / "subsystems/escaped").exists())
        for declaration in ([], {"$entries": "elsewhere"}, {"$entries": "sourceFiles", "extra": True}):
            with tempfile.TemporaryDirectory() as temporary:
                target = Path(temporary) / "manifest"
                coverage_manifest.write({"subsystems": [{"id": "toy", "sourceFiles": []}]}, target, split_entries=True)
                (target / "subsystems/toy/index.json").write_text(json.dumps({"sourceFiles": declaration}))
                with self.assertRaises(coverage_manifest.InvalidCoverageLayout):
                    coverage_manifest.load(target)

    def test_entry_identity_and_envelope_refusals(self):
        for change in ("identity", "unknown", "order", "directory", "obligation-suffix"):
            with self.subTest(change=change), tempfile.TemporaryDirectory() as temporary:
                target = Path(temporary) / "manifest"
                coverage_manifest.write({"subsystems": [{"id": "toy", "sourceFiles": ["a.rs"], "obligations": ["INV-X"]}]}, target, split_entries=True)
                entry = next((target / "subsystems/toy/sourceFiles").iterdir())
                if change == "obligation-suffix":
                    entry = next((target / "subsystems/toy/obligations").iterdir())
                    entry.rename(entry.with_stem(entry.stem + "dup1"))
                elif change == "directory":
                    entry.unlink(); entry.mkdir()
                else:
                    value = json.loads(entry.read_text())
                    if change == "identity": value["value"] = "different.rs"
                    if change == "unknown": value["extra"] = True
                    if change == "order": value["order"] = True
                    entry.write_text(json.dumps(value))
                with self.assertRaises(coverage_manifest.InvalidCoverageLayout):
                    coverage_manifest.load(target)


if __name__ == "__main__":
    unittest.main()
