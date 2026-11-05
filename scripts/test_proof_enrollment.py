#!/usr/bin/env python3
"""Positive/negative enrollment regressions against the actual public tree."""

import copy
import importlib.util
import json
import os
import shutil
import subprocess
import tempfile
import tomllib
import unittest
from unittest import mock
from pathlib import Path

import coverage_manifest
from test_migrate_registry_pr import MigrationTests
from proof_enrollment import ROOT, discover_sources, validate


class EnrollmentTests(unittest.TestCase):
    def setUp(self):
        self.coverage = coverage_manifest.load()
        self.gates = json.loads((ROOT / "verification/coverage-v1/promoted-gates.json").read_text())

    def _validator(self):
        """Load a fresh copy of the coverage validator module."""
        spec = importlib.util.spec_from_file_location("coverage_validator", ROOT / "verification/coverage-v1/validate.py")
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        return module

    def _claim(self, claim_id):
        """Return (subsystem, claim) for an actual manifest claim."""
        return next((s, c) for s in self.coverage["subsystems"] for c in s["obligationClaims"] if c["id"] == claim_id)

    def test_lexical_implementation_classification_is_deleted(self):
        """D43: no command or script text decides an implementation claim's kind."""
        module = self._validator()
        for name in ("implementation_kind", "command_invokes_kani", "repository_script", "extraction_command",
                     "validate_implementation_bindings", "require_wired", "KANI_WORD", "EXTRACTOR_WORD"):
            self.assertFalse(hasattr(module, name), name)

    def test_implementation_claims_carry_the_fields_their_wrapper_reads(self):
        module = self._validator()
        kani_subsystem, kani = self._claim("INV-CERT-INTEGER-NEGATION-CORRESPONDENCE")
        extraction_subsystem, extraction = self._claim("INV-ART-DIRECT-WRITE-PREDICATE")
        module.validate_claim_evidence_shape(kani_subsystem["id"], kani["id"], kani)
        module.validate_claim_evidence_shape(extraction_subsystem["id"], extraction["id"], extraction)
        no_mutations = copy.deepcopy(kani)
        del no_mutations["correspondenceMutations"]
        # Kani behind an extraction binding: the extraction wrapper refuses harness evidence.
        disguised = copy.deepcopy(kani) | {"extractionBinding": extraction["extractionBinding"]}
        # An F* theorem without a binding is a Kani claim, so it still needs a harness and mutations.
        unbound = copy.deepcopy(extraction)
        del unbound["extractionBinding"]
        # Round 8: a gate script that only names the tools gains nothing; untyped bindings refuse.
        untyped = copy.deepcopy(extraction) | {"extractionBinding": {
            "gate": "artifact-governance-rust-extraction",
            "result": "target/verification-results/artifact-governance-hax/Norn_ir.fst"}}
        for label, subsystem, claim, message in (
                ("no-mutations", kani_subsystem, no_mutations, "mutations"),
                ("disguised", kani_subsystem, disguised, "theorem and predicate evidence only"),
                ("unbound", extraction_subsystem, unbound, "harness and predicate evidence only"),
                ("untyped", extraction_subsystem, untyped, "extractionBinding fields are exact")):
            with self.subTest(case=label), self.assertRaisesRegex(module.InvalidCoverage, message):
                module.validate_claim_evidence_shape(subsystem["id"], claim["id"], claim)

    def test_implementation_claims_are_admitted_only_from_receipts(self):
        """Without base-wrapper receipts, every actual implementation claim is refused (D43, D47)."""
        receipts = self._validator().implementation_receipts
        with tempfile.TemporaryDirectory() as directory:
            errors, _ = receipts.verify(ROOT, Path(directory), {}, None)
        claims = sorted(receipts.implementation_claims(ROOT))
        self.assertEqual(claims, ["INV-ART-DIRECT-WRITE-PREDICATE", "INV-CERT-INTEGER-NEGATION-CORRESPONDENCE"])
        self.assertEqual(errors[0], "INV-ART-DIRECT-WRITE-PREDICATE: no receipt from the base-owned wrapper")
        # Kani runs on the candidate-influenced receipts runner: never admitted as implementation_proved.
        self.assertRegex(errors[1], "^INV-CERT-INTEGER-NEGATION-CORRESPONDENCE: .*candidate-reported")
        self.assertEqual(len(errors), 2)

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
        for job in ("core", "test"):
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

    def test_implementation_kani_evidence_requires_proof_and_production_predicate(self):
        spec = importlib.util.spec_from_file_location("coverage_validator", ROOT / "verification/coverage-v1/validate.py")
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        subsystem = next(s for s in self.coverage["subsystems"] if s["id"] == "assurance-certificates")
        original = next(c for c in subsystem["obligationClaims"] if c["id"] == "INV-CERT-INTEGER-NEGATION-CORRESPONDENCE")
        module.validate_claim_evidence_shape(subsystem["id"], original["id"], original)
        for mutation in ("no-proof", "no-predicate", "not-annotated"):
            with self.subTest(mutation=mutation):
                claim = copy.deepcopy(original)
                evidence = claim["evidence"]["proof"]
                if mutation == "no-proof":
                    evidence.pop(0)
                elif mutation == "no-predicate":
                    evidence.pop(1)
                else:
                    evidence[0]["command"] = evidence[0]["command"].replace(evidence[0]["artifact"], "crates/norn-checker/src/exact_rational.rs").replace("--harness integer_negation_correspondence", "--harness checked_neg")
                    evidence[0]["artifact"] = "crates/norn-checker/src/exact_rational.rs"
                    evidence[0]["selectors"] = ["harness:checked_neg"]
                with self.assertRaises(module.InvalidCoverage):
                    module.validate_claim_evidence_shape(subsystem["id"], claim["id"], claim)

    def test_configuration_disabled_implementation_harness_is_rejected(self):
        spec = importlib.util.spec_from_file_location("coverage_validator", ROOT / "verification/coverage-v1/validate.py")
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        subsystem = next(s for s in self.coverage["subsystems"] if s["id"] == "assurance-certificates")
        original = next(c for c in subsystem["obligationClaims"] if c["id"] == "INV-CERT-INTEGER-NEGATION-CORRESPONDENCE")
        for attributes in (
            "#[cfg(any())]\n#[kani::proof]",
            "#[kani::proof]\n#[cfg(any())]",
            "#[cfg_attr(all(), cfg(any()))]\n#[kani::proof]",
            "#[ cfg(any())]\n#[kani::proof]",
            "#[\tcfg_attr(all(), cfg(any()))]\n#[kani::proof]",
            "#![cfg(any())]\n#[kani::proof]",
            "#! [ cfg(any()) ]\n#[kani::proof]",
            "#[r#cfg(any())]\n#[kani::proof]",
            "#[ r#cfg_attr(all(), cfg(any()))]\n#[kani::proof]",
        ):
            with self.subTest(attributes=attributes), tempfile.TemporaryDirectory() as directory:
                module.ROOT = Path(directory)
                claim = copy.deepcopy(original)
                entry = claim["evidence"]["proof"][0]
                harness = module.ROOT / entry["artifact"]
                harness.parent.mkdir(parents=True)
                harness.write_text(attributes + "\nfn integer_negation_correspondence() {}\n#[kani::proof]\nfn other() {}\n")
                with self.assertRaisesRegex(module.InvalidCoverage, "harness must be unconditional"):
                    module.validate_claim_evidence_shape(subsystem["id"], claim["id"], claim)
                harness.write_text("#[cfg(any())]\nmod disabled {\n    #[kani::proof]\n    fn integer_negation_correspondence() {}\n}\n")
                with self.assertRaisesRegex(module.InvalidCoverage, "harness must be unconditional"):
                    module.validate_claim_evidence_shape(subsystem["id"], claim["id"], claim)

    def test_indirect_implementation_harness_is_rejected(self):
        spec = importlib.util.spec_from_file_location("coverage_validator", ROOT / "verification/coverage-v1/validate.py")
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        subsystem = next(s for s in self.coverage["subsystems"] if s["id"] == "assurance-certificates")
        original = next(c for c in subsystem["obligationClaims"] if c["id"] == "INV-CERT-INTEGER-NEGATION-CORRESPONDENCE")
        proof = "#[kani::proof] fn integer_negation_correspondence() {}"
        for prefix, suffix in (
            ("macro_rules! unused { () => { ", " }; }"),
            ('include!("other.rs"); ', ""),
            ('const SOURCE: &str = include_str!("../../crates/norn-checker/src/exact_rational.rs"); ', ""),
            ('const SOURCE: &[u8] = include_bytes !("../../crates/norn-checker/src/exact_rational.rs"); ', ""),
            ('#[path = "other.rs"] mod other; ', ""),
            ('#[path = "other.rs"] ', ""),
        ):
            with self.subTest(prefix=prefix), tempfile.TemporaryDirectory() as directory:
                module.ROOT = Path(directory)
                entry = original["evidence"]["proof"][0]
                harness = module.ROOT / entry["artifact"]
                harness.parent.mkdir(parents=True)
                production = module.ROOT / original["correspondenceSource"]
                production.parent.mkdir(parents=True)
                production.write_text("pub struct Rational;\n")
                harness.write_text(prefix + proof + suffix)
                with self.assertRaisesRegex(module.InvalidCoverage, "cannot contain" if "!" in prefix else ""):
                    module.validate_claim_evidence_shape(subsystem["id"], original["id"], original)
                # Normalized imports bind to claim evidence, without a validator allowlist.
                harness.write_text('#[path = "../../crates/norn-checker/src/../src/exact_rational.rs"] mod production;\n' + proof)
                module.validate_claim_evidence_shape(subsystem["id"], original["id"], original)
                for target in (None, "crates/other.rs"):
                    candidate = copy.deepcopy(original)
                    candidate["correspondenceSource"] = target
                    with self.assertRaises(module.InvalidCoverage):
                        module.validate_claim_evidence_shape(subsystem["id"], original["id"], candidate)

    def test_implementation_harness_requires_top_level_production_import(self):
        spec = importlib.util.spec_from_file_location("coverage_validator", ROOT / "verification/coverage-v1/validate.py")
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        subsystem = next(s for s in self.coverage["subsystems"] if s["id"] == "assurance-certificates")
        claim = next(c for c in subsystem["obligationClaims"] if c["id"] == "INV-CERT-INTEGER-NEGATION-CORRESPONDENCE")
        proof = "#[kani::proof] fn integer_negation_correspondence() {}\n"
        imported = '#[path = "../../crates/norn-checker/src/exact_rational.rs"] mod production;\n'
        local_copy = "struct Rational; impl Rational { fn checked_neg(self) -> Option<Self> { Some(self) } }\n"
        for source, message in (
            ("mod nested {\n" + imported + "}\n" + proof, "imports must be at harness top level"),
            (local_copy + proof, "harness must import the declared correspondenceSource"),
            (imported.replace("exact_rational.rs", "other.rs") + proof, "must import the declared correspondenceSource as a module"),
        ):
            with self.subTest(source=source), tempfile.TemporaryDirectory() as directory:
                module.ROOT = Path(directory)
                harness = module.ROOT / claim["evidence"]["proof"][0]["artifact"]
                harness.parent.mkdir(parents=True)
                production = module.ROOT / claim["correspondenceSource"]
                production.parent.mkdir(parents=True)
                production.write_text(local_copy)
                production.with_name("other.rs").write_text(local_copy)
                harness.write_text(source)
                with self.assertRaisesRegex(module.InvalidCoverage, message):
                    module.validate_claim_evidence_shape(subsystem["id"], claim["id"], claim)
                # Braces in comments and strings do not put the import in a module.
                harness.write_text('/* { */ const TEXT: &str = "{";\n' + imported + proof)
                module.validate_claim_evidence_shape(subsystem["id"], claim["id"], claim)

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
        self.assertEqual(combined["phase"], ["guards", "provenance", "clippy"])
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
        self.assertEqual(plan["phase"], ["guards", "provenance", "clippy"])
        self.assertEqual(plan["packages"], "norn-skeleton-guard")
        self.assertLess(len(plan["bundle"]), len(self.full["bundle"]))
        self.assertTrue(all(len(bundle["tasks"]) >= 2 for bundle in plan["bundle"]))

    def test_bundle_inventory_is_exact_and_deterministic(self):
        command = ["bash", str(ROOT / "scripts/ci/core.sh")]
        expected = ["test/" + value for value in self.full["shard"]]
        expected += ["phase/" + value for value in self.full["phase"]]
        self.assertEqual(sorted(task for bundle in self.full["bundle"] for task in bundle["tasks"]), sorted(expected))
        self.assertGreaterEqual(len(self.full["bundle"]), 4)
        self.assertLessEqual(len(self.full["bundle"]), 6)
        self.assertTrue(all(len(bundle["tasks"]) >= 2 for bundle in self.full["bundle"]))
        reordered = copy.deepcopy(self.full)
        reordered["shard"].reverse()
        reordered["phase"].reverse()
        packed = json.loads(subprocess.check_output(command + ["bundle-plan", json.dumps(reordered)], text=True))
        self.assertEqual(packed["bundle"], self.full["bundle"])
        subprocess.run(command + ["bundle-check", json.dumps(packed)], check=True, timeout=10)
        for mutate in (
            lambda p: p.pop("bundle"),
            lambda p: p["bundle"][0]["tasks"].pop(),
            lambda p: p["bundle"][0]["tasks"].append(p["bundle"][0]["tasks"][0]),
            lambda p: p["bundle"].append(copy.deepcopy(p["bundle"][0])),
            lambda p: p["bundle"].reverse(),
            lambda p: p["phase"].append("unknown-phase"),
            lambda p: p.update(mode="unknown"),
        ):
            broken = copy.deepcopy(self.full)
            mutate(broken)
            for args in (["bundle-check", json.dumps(broken)], ["bundle", json.dumps(broken), "b0"]):
                result = subprocess.run(command + args, capture_output=True, text=True, timeout=10)
                self.assertNotEqual(result.returncode, 0, result.stdout)
        result = subprocess.run(command + ["bundle", json.dumps(self.full), "missing"], capture_output=True, timeout=10)
        self.assertNotEqual(result.returncode, 0)
        incomplete = copy.deepcopy(self.full)
        incomplete["phase"].remove("guards")
        result = subprocess.run(command + ["bundle-plan", json.dumps(incomplete)], capture_output=True, timeout=10)
        self.assertNotEqual(result.returncode, 0)

    def test_bundle_executor_logs_success_and_propagates_failure(self):
        command = ["bash", str(ROOT / "scripts/ci/core.sh")]
        plan = {"mode": "affected", "shard": [], "phase": ["test-shards"]}
        plan = json.loads(subprocess.check_output(command + ["bundle-plan", json.dumps(plan)], text=True))
        tools = self.root / "cargo-home" / "bin"
        tools.mkdir(parents=True)
        cargo = tools / "cargo"
        cargo.write_text('''#!/usr/bin/env python3
import json, os, sys
if os.environ.get("BUNDLE_CANARY_FAIL") == "1":
    sys.exit(23)
args = sys.argv[1:]
if args == ["nextest", "--version"]:
    print("cargo-nextest 0.9.150 (synthetic)")
elif "list" in args:
    expression = args[args.index("-E") + 1] if "-E" in args else None
    matches = expression is None or ("generated_accepted_expressions_are_total_and_typed" in expression and not expression.startswith("not ("))
    print(json.dumps({"rust-suites": {"synthetic": {"testcases": {"only": {"filter-match": {"status": "matches" if matches else "mismatch"}}}}}}))
else:
    sys.exit("unexpected synthetic Cargo invocation")
''')
        cargo.chmod(0o755)
        for fail in ("0", "1"):
            environment = {**os.environ, "CARGO_HOME": str(tools.parent),
                           "NORN_CI_PACKAGES": "", "NORN_CI_REST_SHARDS": "3", "BUNDLE_CANARY_FAIL": fail}
            result = subprocess.run(command + ["bundle", json.dumps(plan), plan["bundle"][0]["id"]],
                                    env=environment, capture_output=True, text=True, timeout=10)
            self.assertEqual(result.returncode, 23 if fail == "1" else 0, result.stderr)
            verdict = "failure" if fail == "1" else "success"
            self.assertEqual(result.stdout.count(f"task=phase/test-shards result={verdict}"), 1)


class CoverageLayoutTests(unittest.TestCase):
    def test_entry_layout_preserves_every_claim_and_order(self):
        document = coverage_manifest.load()
        subsystem = next(s for s in document["subsystems"] if s["runtime"]["artifacts"])
        subsystem["runtime"]["artifacts"] *= 2
        with tempfile.TemporaryDirectory() as temporary:
            target = Path(temporary) / "manifest"
            coverage_manifest.write(document, target)
            for folder in sorted(target.rglob("*"), reverse=True):
                if folder.is_dir() and not any(folder.iterdir()):
                    folder.rmdir()
            self.assertEqual(coverage_manifest.load(target), document)
            self.assertTrue(coverage_manifest.digest(target))

    def test_migration_preserves_duplicates_and_rolls_back_failures(self):
        for failure in (None, "conversion", "commit", "inventory", "index"):
            with self.subTest(failure=failure), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                directory = root / "verification/coverage-v1/manifest"
                record_root = root / ".github/ci/provenance/artifacts/verification/coverage-v1/manifest"
                body = {"runtime": {"artifacts": ["a.rs", "a.rs"]}}
                if failure == "conversion":
                    body["../invalid"] = []
                for path, value in ((directory / "index.json", {}), (directory / "subsystems/toy.json", body),
                                    (record_root / "index.json.json", {}), (record_root / "subsystems/toy.json.json", {})):
                    path.parent.mkdir(parents=True, exist_ok=True)
                    path.write_text(json.dumps(value))
                if failure == "inventory":
                    (directory / "subsystems/already-new").mkdir()
                if failure == "index":
                    (directory / "index.json").write_text('{"subsystems": []}')
                before = {p.relative_to(root): p.read_bytes() for p in root.rglob("*") if p.is_file()}
                rename = Path.rename
                def commit(path, target):
                    if failure == "commit" and path.name == "records":
                        raise OSError("synthetic swap failure")
                    return rename(path, target)
                with mock.patch.multiple(coverage_manifest, ROOT=root, COVERAGE=directory), mock.patch.object(Path, "rename", commit):
                    if failure:
                        with self.assertRaises((coverage_manifest.InvalidCoverageLayout, OSError)):
                            coverage_manifest.migrate_entries()
                    else:
                        coverage_manifest.migrate_entries()
                        self.assertEqual(coverage_manifest.load(directory), {"subsystems": [{"id": "toy", **body}]})
                if failure:
                    self.assertEqual({p.relative_to(root): p.read_bytes() for p in root.rglob("*") if p.is_file()}, before)

    def test_nested_evidence_additions_touch_independent_files(self):
        original = {"subsystems": [{"id": "toy", "obligationClaims": [{"id": "INV-X", "evidence": {"runtime": [{"artifact": "a.rs", "selectors": ["test:a"]}]}}]}]}
        with tempfile.TemporaryDirectory() as temporary:
            target = Path(temporary) / "manifest"
            coverage_manifest.write(original, target)
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
            coverage_manifest.write(document, target)
            self.assertEqual(coverage_manifest.load(target), document)

    def test_layout_refusals(self):
        document = {"subsystems": [{"id": "toy", "sourceFiles": ["a.rs"], "obligations": ["INV-X"]}]}
        for mutation, expected in (("stray", "unknown coverage entries"), ("duplicate-key", "duplicate JSON key"), ("symlink", "not a regular file"), ("legacy", "unknown subsystem files")):
            with self.subTest(mutation=mutation), tempfile.TemporaryDirectory() as temporary:
                target = Path(temporary) / "manifest"
                coverage_manifest.write(document, target)
                metadata = target / "subsystems/toy/index.json"
                if mutation == "stray":
                    (target / "stray").write_text("{}")
                elif mutation == "duplicate-key":
                    metadata.write_text('{"sourceFiles":[],"sourceFiles":[]}')
                elif mutation == "symlink":
                    metadata.unlink()
                    metadata.symlink_to(target / "index.json")
                else:
                    (target / "subsystems/toy.json").write_text('{}')
                with self.assertRaisesRegex(coverage_manifest.InvalidCoverageLayout, expected):
                    coverage_manifest.load(target)

    def test_subsystem_order_uses_legacy_identity_order(self):
        expected = {"subsystems": [{"id": "a-b"}, {"id": "a"}]}
        with tempfile.TemporaryDirectory() as temporary:
            target = Path(temporary) / "manifest"
            coverage_manifest.write(expected, target)
            self.assertEqual(coverage_manifest.load(target), expected)

    def test_entry_field_validation(self):
        for body in ({"../escaped": []}, {"a": {"b": []}, "a.b": []}):
            with tempfile.TemporaryDirectory() as temporary:
                target = Path(temporary) / "manifest"
                with self.assertRaisesRegex(coverage_manifest.InvalidCoverageLayout, "entry field"):
                    coverage_manifest.write({"subsystems": [{"id": "toy", **body}]}, target)
                self.assertFalse((target / "subsystems/escaped").exists())
        for declaration in ([], {"$entries": "elsewhere"}, {"$entries": "sourceFiles", "extra": True}):
            with tempfile.TemporaryDirectory() as temporary:
                target = Path(temporary) / "manifest"
                coverage_manifest.write({"subsystems": [{"id": "toy", "sourceFiles": []}]}, target)
                (target / "subsystems/toy/index.json").write_text(json.dumps({"sourceFiles": declaration}))
                with self.assertRaises(coverage_manifest.InvalidCoverageLayout):
                    coverage_manifest.load(target)

    def test_entry_identity_and_envelope_refusals(self):
        for change in ("identity", "unknown", "order", "directory", "obligation-suffix"):
            with self.subTest(change=change), tempfile.TemporaryDirectory() as temporary:
                target = Path(temporary) / "manifest"
                coverage_manifest.write({"subsystems": [{"id": "toy", "sourceFiles": ["a.rs"], "obligations": ["INV-X"]}]}, target)
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
