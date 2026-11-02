#!/usr/bin/env python3
"""Positive/negative enrollment regressions against the actual public tree."""

import copy
import importlib.util
import json
import shutil
import subprocess
import tempfile
import tomllib
import unittest
from pathlib import Path

import coverage_manifest
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

    def test_plan_workflow_contract_is_exact(self):
        spec = importlib.util.spec_from_file_location("coverage_validator", ROOT / "verification/coverage-v1/validate.py")
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        workflow = module.yaml.load(module.FORMAL_WORKFLOW.read_text(), Loader=module.WorkflowLoader)
        for mutation in ("ref", "depth", "credentials", "extra-step", "base", "head", "command", "verdict"):
            with self.subTest(mutation=mutation), tempfile.TemporaryDirectory() as directory:
                candidate = copy.deepcopy(workflow)
                steps = candidate["jobs"]["plan"]["steps"]
                if mutation == "ref":
                    steps[0]["with"]["ref"] = "main"
                elif mutation == "depth":
                    steps[0]["with"]["fetch-depth"] = 1
                elif mutation == "credentials":
                    steps[0]["with"]["persist-credentials"] = True
                elif mutation == "extra-step":
                    steps.insert(0, {"run": "echo unexpected"})
                elif mutation in {"base", "head"}:
                    steps[-1]["env"]["BASE" if mutation == "base" else "HEAD_SHA"] = "HEAD~1"
                elif mutation == "command":
                    steps[-1]["run"] = steps[-1]["run"].replace(' "$HEAD_SHA"', "")
                else:
                    candidate["jobs"]["full"]["steps"][0]["run"] += "exit 0\n"
                path = Path(directory) / "workflow.yml"
                path.write_text(module.yaml.safe_dump(candidate, sort_keys=False))
                with self.assertRaises(module.InvalidCoverage):
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


class AffectedPlanTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        members = tomllib.loads((ROOT / "Cargo.toml").read_text())["workspace"]["members"]
        for manifest in [ROOT / "Cargo.toml", *(ROOT / m / "Cargo.toml" for m in members)]:
            target = self.root / manifest.relative_to(ROOT)
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(manifest, target)
        gate = self.root / "crates/norn-skeleton-guard/src/ci/mod.rs"
        gate.parent.mkdir(parents=True)
        shutil.copyfile(ROOT / gate.relative_to(self.root), gate)
        self.git("init", "-q")
        self.git("config", "user.name", "synthetic")
        self.git("config", "user.email", "synthetic@example.invalid")
        self.base = self.commit("README.md")
        self.full = self.plan()

    def git(self, *args):
        return subprocess.check_output(["git", *args], cwd=self.root, text=True).strip()

    def commit(self, path):
        target = self.root / path
        target.parent.mkdir(parents=True, exist_ok=True)
        with target.open("a") as output:
            output.write("\n")
        self.git("add", ".")
        self.git("commit", "-qm", "synthetic change")
        return self.git("rev-parse", "HEAD")

    def plan(self, *args):
        return json.loads(subprocess.check_output(
            ["bash", str(ROOT / "scripts/ci/core.sh"), "plan", *args],
            cwd=self.root, text=True))

    def test_combined_group_diff_and_explicit_head(self):
        first = self.commit("crates/norn-cell/src/lib.rs")
        head = self.commit("crates/norn-expr/src/lib.rs")
        self.commit(".github/workflows/synthetic.yml")
        combined = self.plan("--affected", self.base, head)
        self.assertEqual(combined["mode"], "affected")
        self.assertIn("heavy-1", combined["shard"])
        self.assertIn("heavy-3", combined["shard"])
        self.assertEqual(combined["rest_shards"], 3)
        self.assertEqual(combined["phase"], ["guards", "provenance"])
        self.assertNotIn("heavy-1", self.plan("--affected", self.base, first)["shard"])
        self.assertEqual(self.plan("--affected", self.base)["mode"], "full")

    def test_unknown_and_sensitive_paths_force_exact_full_suite(self):
        paths = [".github/workflows/synthetic.yml", "Cargo.lock",
                 "rust-toolchain.toml", "verification/kernel-l1/lean-toolchain",
                 "scripts/ci/core.sh", "verification/coverage-v1/validate.py",
                 "crates/norn-skeleton-guard/src/ci/mod.rs", "unknown/path"]
        for path in paths:
            with self.subTest(path=path):
                base = self.git("rev-parse", "HEAD")
                self.commit(path)
                plan = self.plan("--affected", base)
                self.assertEqual({k: v for k, v in plan.items() if k != "reason"},
                                 {k: v for k, v in self.full.items() if k != "reason"})

    def test_markdown_only_keeps_policy_and_provenance(self):
        self.commit("docs/synthetic.md")
        plan = self.plan("--affected", self.base)
        self.assertEqual(plan["shard"], ["rest-1"])
        self.assertEqual(plan["phase"], ["guards", "provenance"])
        self.assertEqual(plan["packages"], "norn-skeleton-guard")


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
