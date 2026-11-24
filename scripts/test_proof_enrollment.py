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


def _sibling(name, directory=None):
    """Load sibling module `name` from its source by explicit path: never through sys.path,
    never from a bytecode cache, never a same-named module loaded some other way (#513)."""
    import pathlib
    import sys
    import types

    path = (directory or pathlib.Path(__file__).resolve().parent) / f"{name}.py"
    loaded = sys.modules.get(name)
    if getattr(loaded, "__norn_sibling__", None) == str(path):
        return loaded
    module = types.ModuleType(name)
    module.__file__ = str(path)
    module.__norn_sibling__ = str(path)
    sys.modules[name] = module
    try:
        exec(compile(path.read_bytes(), str(path), "exec", dont_inherit=True), module.__dict__)
    except BaseException:
        del sys.modules[name]
        raise
    return module

coverage_manifest = _sibling("coverage_manifest")
_proof_enrollment = _sibling("proof_enrollment")
ROOT = _proof_enrollment.ROOT
discover_sources = _proof_enrollment.discover_sources
validate = _proof_enrollment.validate


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

    def test_plan_workflow_contract_is_exact(self):
        spec = importlib.util.spec_from_file_location("coverage_validator", ROOT / "verification/coverage-v1/validate.py")
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        workflow = module.yaml.load(module.FORMAL_WORKFLOW.read_text(), Loader=module.WorkflowLoader)
        for mutation in ("ref", "depth", "credentials", "extra-step", "base", "head", "command", "verdict"):
            with self.subTest(mutation=mutation), tempfile.TemporaryDirectory() as directory:
                candidate = copy.deepcopy(workflow)
                # The gate plan is the plan job's step after its candidate checkout.
                steps = candidate["jobs"]["plan"]["steps"]
                checkout = next(step for step in steps if step.get("uses", "").startswith("actions/checkout@"))
                planner = next(step for step in steps if step.get("id") == "plan")
                if mutation == "ref":
                    checkout["with"]["ref"] = "main"
                elif mutation == "depth":
                    checkout["with"]["fetch-depth"] = 1
                elif mutation == "credentials":
                    checkout["with"]["persist-credentials"] = True
                elif mutation == "extra-step":
                    steps.insert(steps.index(planner), {"run": "echo unexpected"})
                elif mutation in {"base", "head"}:
                    planner["env"]["BASE" if mutation == "base" else "HEAD_SHA"] = "HEAD~1"
                elif mutation == "command":
                    planner["run"] = planner["run"].replace(' "$HEAD_SHA"', "")
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

    def hosted_plan(self, *args):
        return subprocess.check_output(
            ["bash", str(ROOT / "scripts/ci/core.sh"), "plan", *args],
            cwd=self.root, text=True, env={**os.environ, "CARGO_TARGET_DIR": str(self.root / ".git/target")})

    def plan(self, *args):
        return json.loads(self.hosted_plan(*args))

    def test_combined_group_diff_and_explicit_head(self):
        first = self.commit("crates/norn-cell/src/lib.rs")
        head = self.commit("crates/norn-expr/src/lib.rs")
        self.commit(".github/workflows/synthetic.yml")
        combined = self.plan("--affected", self.base, head)
        self.assertEqual(combined["mode"], "affected")
        self.assertIn("heavy-1", combined["shard"])
        self.assertIn("heavy-3", combined["shard"])
        self.assertEqual(combined["rest_shards"], 3)
        # norn-cell and norn-expr reach norn-checker-wasm, so the Studio browser rows run (#1113).
        # norn-cell is reached, so the DuckDB backend is built and tested too (#1206), and so is
        # the Rivet host's own workspace, which builds norn-cell by path (D131).
        self.assertEqual(combined["phase"], ["guards", "provenance", "clippy", "browser", "duckdb", "rivet"])
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

    def test_duckdb_runs_when_norn_cell_is_reached_not_only_changed(self):
        # norn-kernel is upstream of norn-cell, so a kernel-only change reaches it and plans the
        # DuckDB phase; norn-api is not, so an api-only change does not (#1206).
        for path, planned in (("crates/norn-kernel/src/lib.rs", True), ("crates/norn-api/src/lib.rs", False)):
            with self.subTest(path=path):
                base = self.git("rev-parse", "HEAD")
                self.commit(path)
                plan = self.plan("--affected", base)
                self.assertEqual(plan["mode"], "affected")
                self.assertNotIn("norn-cell", plan["reason"])
                self.assertEqual("duckdb" in plan["phase"], planned, plan["phase"])

    def test_rivet_runs_when_a_crate_it_builds_is_reached(self):
        # crates/norn-cell-rivet is its own workspace and builds norn-api, norn-cell and
        # norn-cell-do by path: a change that reaches one of them plans its phase (norn-kernel
        # reaches norn-cell), and a Markdown-only change does not.
        for path, planned in (("crates/norn-api/src/lib.rs", True), ("crates/norn-kernel/src/lib.rs", True), ("docs/synthetic.md", False)):
            with self.subTest(path=path):
                base = self.git("rev-parse", "HEAD")
                self.commit(path)
                plan = self.plan("--affected", base)
                self.assertEqual(plan["mode"], "affected")
                self.assertEqual("rivet" in plan["phase"], planned, plan["phase"])
        self.assertIn("rivet", self.full["phase"])
        # Like formal, the Rivet conformance suite is a Boat gate that no CI plan selects.
        self.assertNotIn("rivet-conformance", self.full["phase"])

    def test_an_affected_rust_change_plans_rest_shards(self):
        # Only a plan that reaches no package drops its rest shards; a Rust change keeps them.
        for path in ("crates/norn-ir/src/lib.rs", "crates/norn-cli/src/main.rs"):
            with self.subTest(path=path):
                base = self.git("rev-parse", "HEAD")
                self.commit(path)
                plan = self.plan("--affected", base)
                self.assertEqual(plan["mode"], "affected")
                self.assertTrue([shard for shard in plan["shard"] if shard.startswith("rest-")], plan["shard"])
                self.assertTrue(plan["packages"])

    def test_markdown_only_keeps_policy_and_provenance(self):
        self.commit("docs/synthetic.md")
        plan = self.plan("--affected", self.base)
        # The guard is its own workspace (#1270): no package is reached, so no test shard runs;
        # the guards phase runs the guard's suite.
        self.assertEqual(plan["shard"], [])
        self.assertEqual(plan["phase"], ["guards", "provenance", "clippy"])
        self.assertEqual(plan["packages"], "")
        self.assertLess(len(plan["bundle"]), len(self.full["bundle"]))
        self.assertTrue(all(len(bundle["tasks"]) >= 2 for bundle in plan["bundle"]))

    def test_hosted_bundle_packing_matches_xtask(self):
        command = ["bash", str(ROOT / "scripts/ci/core.sh")]
        cases = [()]
        for package in ("norn-api", "norn-cell", "norn-checker"):
            base = self.git("rev-parse", "HEAD")
            self.commit(f"crates/{package}/src/lib.rs")
            cases.append(("--affected", base, self.git("rev-parse", "HEAD")))
        for args in cases:
            with self.subTest(args=args):
                hosted = self.hosted_plan(*args)
                raw = json.loads(hosted)
                if args:
                    self.assertEqual(raw["mode"], "affected")
                    self.assertTrue(raw["packages"])
                del raw["bundle"]
                rust = subprocess.check_output(command + ["bundle-plan", json.dumps(raw)], cwd=ROOT, text=True)
                self.assertEqual(rust, hosted)

    def test_bundle_inventory_is_exact_and_deterministic(self):
        command = ["bash", str(ROOT / "scripts/ci/core.sh")]
        expected = ["test/" + value for value in self.full["shard"]]
        expected += ["phase/" + value for value in self.full["phase"]]
        self.assertEqual(sorted(task for bundle in self.full["bundle"] for task in bundle["tasks"]), sorted(expected))
        # Each bundle is one Boat start: the full plan fills the eight-bundle budget exactly.
        self.assertEqual(len(self.full["bundle"]), 8)
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

    def built_xtask(self):
        target = os.environ.get("CARGO_TARGET_DIR") or str(ROOT / "target")
        metadata = json.loads(subprocess.check_output([
            "cargo", "metadata", "--locked", "--no-deps", "--format-version", "1",
            "--manifest-path", "crates/norn-skeleton-guard/Cargo.toml",
        ], cwd=ROOT, env={**os.environ, "CARGO_TARGET_DIR": target}, text=True))
        return Path(metadata["target_directory"]) / "debug" / "xtask"

    def test_bundle_executor_prunes_only_between_incompatible_builds(self):
        command = ["bash", str(ROOT / "scripts/ci/core.sh")]
        task_script = self.root / "scripts/ci/core.sh"
        task_script.parent.mkdir(parents=True)
        task_script.write_text(r"""set -eu
printf '%s\n' "$*" >> calls
if [[ $(wc -l < calls) -eq 1 ]]; then
    [[ ! -e target/debug && ! -e target/norn-checker-wasm ]]
    mkdir -p target/debug/.fingerprint target/norn-checker-wasm/pkg
else
    case "$*" in
        "test rest-1"|browser) [[ -d target/debug/.fingerprint && -d target/norn-checker-wasm/pkg ]] ;;
        doc) [[ ! -e target/debug && ! -e target/norn-checker-wasm ]] ;;
        *) exit 24 ;;
    esac
fi
""")
        environment = {**os.environ, "CARGO_TARGET_DIR": str(self.root / "target"),
                       "NORN_CI_PACKAGES": ""}
        for label, plan, expected_calls, shared in (
            ("incompatible", {"mode": "affected", "shard": ["rest-1"], "phase": ["doc"]}, ["test rest-1", "doc"], False),
            ("shared-test", {"mode": "affected", "shard": ["rest-1", "rest-2"], "phase": []}, ["test rest-2", "test rest-1"], True),
            ("shared-wasm-pkg", {"mode": "affected", "shard": [], "phase": ["integration-authoring", "browser"]}, ["integration-authoring", "browser"], True),
        ):
            with self.subTest(case=label):
                shutil.rmtree(self.root / "target", ignore_errors=True)
                packed = subprocess.check_output(command + ["bundle-plan", json.dumps(plan)], cwd=ROOT, text=True)
                execute = [str(self.built_xtask()), "ci", "bundle"]
                calls = self.root / "calls"
                for flag in ("--help", "-h", "--version"):
                    for arguments in ([flag, "b0"], [packed, flag]):
                        result = subprocess.run(execute + arguments, cwd=self.root,
                                                env=environment, capture_output=True, text=True, timeout=10)
                        self.assertNotEqual(result.returncode, 0, result.stdout)
                        self.assertFalse(calls.exists(), result.stdout)
                result = subprocess.run(execute + [packed, "b0"], cwd=self.root,
                                        env=environment, capture_output=True, text=True, timeout=10)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertEqual(calls.read_text().splitlines(), expected_calls)
                self.assertEqual((self.root / "target/debug").exists(), shared)
                self.assertEqual((self.root / "target/norn-checker-wasm").exists(), shared)
                calls.unlink()

    def test_wasm_pkg_cache_reuses_clean_tree_and_rebuilds_when_dirty(self):
        tools = self.root / "tools/bin"
        tools.mkdir(parents=True)
        wasm_bindgen = tools / "wasm-bindgen"
        wasm_bindgen.write_text("#!/usr/bin/env bash\necho 'wasm-bindgen 0.2.129'\n")
        wasm_bindgen.chmod(0o755)
        build_log = self.root / ".git/wasm-build-calls.log"
        build_script = self.root / "crates/norn-checker-wasm/scripts/build-pkg.sh"
        build_script.parent.mkdir(parents=True, exist_ok=True)
        build_script.write_text(f"""#!/usr/bin/env bash
set -euo pipefail
out="$1"
mkdir -p "$out"
if [[ "${{NORN_WASM_ENGINE:-0}}" == "1" ]]; then
  printf 'engine\\n' >> "{build_log}"
  : > "$out/norn_engine_wasm_bg.wasm"
else
  printf 'pkg\\n' >> "{build_log}"
  : > "$out/norn_checker_wasm_bg.wasm"
fi
printf '{{}}\\n' > "$out/package.json"
printf 'ok\\n' > "$out/SIZE.txt"
""")
        build_script.chmod(0o755)
        self.commit("crates/norn-checker/src/lib.rs")
        target_dir = self.root / ".git/wasm-target"
        out_pkg = self.root / ".git/out-pkg"
        out_engine = self.root / ".git/out-engine"
        env = {
            **os.environ,
            "PATH": f"{tools}:{os.environ['PATH']}",
            "CARGO_TARGET_DIR": str(target_dir),
        }
        harness = (
            'eval "$(sed \'/^case "${1:-all}" in$/,$d\' "$1")"\n'
            'ensure_checker_wasm_pkg "$2"\n'
            'ensure_checker_wasm_engine "$3"\n'
        )
        def run_ensure():
            stamp = target_dir / ".norn-built-tree"
            if stamp.exists():
                past = stamp.stat().st_mtime - 10
                os.utime(stamp, (past, past))
            result = subprocess.run(
                ["bash", "-c", harness, "_", str(ROOT / "scripts/ci/core.sh"), str(out_pkg), str(out_engine)],
                cwd=self.root, env=env, capture_output=True, text=True, timeout=15,
            )
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            return build_log.read_text().splitlines()

        # Cold clean tree builds both packages once and copies them to out dirs.
        self.assertEqual(run_ensure(), ["pkg", "engine"])
        self.assertTrue((out_pkg / "norn_checker_wasm_bg.wasm").is_file())
        self.assertTrue((out_engine / "norn_engine_wasm_bg.wasm").is_file())
        # Second run on the same clean tree reuses both cached packages.
        self.assertEqual(run_ensure(), ["pkg", "engine"])
        # Untracked files outside Rust/WASM inputs do not invalidate reuse.
        scratch = self.root / "apps/studio/src/checker/untracked.txt"
        scratch.parent.mkdir(parents=True, exist_ok=True)
        scratch.write_text("copied output\n")
        self.assertEqual(run_ensure(), ["pkg", "engine"])
        # Uncommitted edit to a tracked file under crates/ invalidates both caches.
        tracked = self.root / "crates/norn-checker/src/lib.rs"
        tracked.write_text("// dirty edit\n")
        self.assertEqual(run_ensure(), ["pkg", "engine"] * 2)
        # Reverting back to clean HEAD must NOT reuse the dirty build; it rebuilds clean once,
        # and then a subsequent clean run reuses the clean cache.
        self.git("checkout", "--", "crates/norn-checker/src/lib.rs")
        self.assertEqual(run_ensure(), ["pkg", "engine"] * 3)
        self.assertEqual(run_ensure(), ["pkg", "engine"] * 3)
        # Untracked file under crates/ also invalidates both caches.
        untracked_rs = self.root / "crates/norn-checker/src/new_file.rs"
        untracked_rs.write_text("pub fn extra() {}\n")
        self.assertEqual(run_ensure(), ["pkg", "engine"] * 4)
        untracked_rs.unlink()
        # Dirty Cargo.toml also invalidates both caches.
        manifest = self.root / "Cargo.toml"
        original_manifest = manifest.read_text()
        manifest.write_text(original_manifest + "\n# dirty\n")
        self.assertEqual(run_ensure(), ["pkg", "engine"] * 5)
        manifest.write_text(original_manifest)

        # A direct overwrite with a clean tree and fresh SIZE.txt still forces a rebuild.
        self.assertEqual(run_ensure(), ["pkg", "engine"] * 6)
        canonical = target_dir / "norn-checker-wasm"
        (canonical / "pkg/norn_checker_wasm_bg.wasm").write_bytes(b"overwritten checker")
        (canonical / "engine/norn_engine_wasm_bg.wasm").write_bytes(b"overwritten engine")
        self.assertEqual(run_ensure(), ["pkg", "engine"] * 7)
        self.assertEqual(run_ensure(), ["pkg", "engine"] * 7)

    def test_bundle_executor_logs_success_and_propagates_failure(self):
        command = ["bash", str(ROOT / "scripts/ci/core.sh")]
        plan = {"mode": "affected", "shard": [], "phase": ["test-shards"]}
        plan = json.loads(subprocess.check_output(command + ["bundle-plan", json.dumps(plan)], text=True))
        xtask = self.built_xtask()
        tools = self.root / "cargo-home" / "bin"
        tools.mkdir(parents=True)
        cargo = tools / "cargo"
        cargo.write_text('''#!/usr/bin/env python3
import json, os, sys
args = sys.argv[1:]
if args[:2] == ["xtask", "ci"]:
    os.execv(os.environ["BUNDLE_CANARY_XTASK"], [os.environ["BUNDLE_CANARY_XTASK"], *args[1:]])
if os.environ.get("BUNDLE_CANARY_FAIL") == "1":
    sys.exit(23)
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
                           "NORN_CI_PACKAGES": "", "NORN_CI_REST_SHARDS": "3",
                           "BUNDLE_CANARY_XTASK": str(xtask), "BUNDLE_CANARY_FAIL": fail}
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
