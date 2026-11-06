#!/usr/bin/env python3
"""Regressions for base-owned implementation receipts (D43), with stub tools."""

from __future__ import annotations

import copy
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import tomllib
import unittest

HERE = Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location("implementation_receipts", HERE / "implementation_receipts.py")
receipts = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(receipts)

STUB = """#!/usr/bin/env python3
import pathlib, sys
tool, args = pathlib.Path(sys.argv[0]).name, sys.argv[1:]
if args == ["--version"]:
    print(tool + " 0.0-stub"); sys.exit(0)
if tool == "kani":
    good = "GOOD" in pathlib.Path("src/lib.rs").read_text()
    print("Checking harness proof...")
    print("VERIFICATION:- " + ("SUCCESSFUL" if good else "FAILED"))
    print("Complete - " + ("1 successfully verified harnesses, 0 failures, 1 total." if good
                           else "0 successfully verified harnesses, 1 failures, 1 total."))
    sys.exit(0 if good else 1)
if tool == "fstar.exe":
    # Stands in for type checking: a module marked ill-typed fails.
    sys.exit(1 if any("ill-typed" in pathlib.Path(a).read_text() for a in args if a.endswith(".fst")) else 0)
if tool == "cargo-hax":
    out = pathlib.Path(args[args.index("--output-dir") + 1]); out.mkdir(parents=True, exist_ok=True)
    (out / "Lib.fst").write_text("module Lib\\nlet f (x: bool) : bool = x\\n")
"""
KANI = {"id": "INV-KANI", "proof": "implementation_proved", "correspondenceSource": "src/lib.rs",
        "correspondenceMutations": [{"name": "flip", "before": "GOOD", "after": "BAD"}],
        "evidence": {"proof": [
            {"artifact": "proof/h.rs", "command": "kani", "selectors": ["harness:proof"]},
            {"artifact": "src/lib.rs", "command": "kani", "selectors": ["predicate:f"]}]}}
BINDING = {"package": "lib", "items": ["lib::f"], "module": "Lib.fst", "proofRoot": "fst",
           "models": [], "proofs": ["Proof.fst"]}
EXTRACTION = {"id": "INV-EXTRACT", "proof": "implementation_proved", "extractionBinding": BINDING,
              "evidence": {"proof": [
                  {"artifact": "fst/Proof.fst", "command": "scripts/x.sh", "selectors": ["theorem:t"]},
                  {"artifact": "out/Lib.fst", "command": "scripts/x.sh", "selectors": ["predicate:Lib.f"]}]}}


class ReceiptTest(unittest.TestCase):
    def setUp(self) -> None:
        # Inherited Git location variables would redirect these git commands.
        self.saved = os.environ.copy()
        for name in [n for n in os.environ if n.startswith("GIT_")]:
            del os.environ[name]
        self.tmp = Path(tempfile.mkdtemp())
        self.root = self.tmp / "candidate"
        files = {"src/lib.rs": "pub fn f() -> bool { GOOD }\n", "proof/h.rs": "#[kani::proof] fn proof() {}\n",
                 "fst/Proof.fst": "module Proof\nopen Lib\n", "Cargo.lock": "version = 4\n", ".gitignore": "target/\n",
                 # Round 8: a script that names the tools and only prints a fake result.
                 "scripts/x.sh": "#!/bin/sh\n: cargo-hax fstar.exe\nprintf 'let f = true' > out/Lib.fst\n",
                 "scripts/inner-proof.sh": "#!/bin/sh\ntool=ka; tool+=ni; \"$tool\" proof/h.rs\n"}
        for name, text in files.items():
            (self.root / name).parent.mkdir(parents=True, exist_ok=True)
            (self.root / name).write_text(text)
        (self.tmp / "tools/bin").mkdir(parents=True)
        for tool in ("kani", "cargo-hax", "fstar.exe"):
            (self.tmp / "tools/bin" / tool).write_text(STUB)
            (self.tmp / "tools/bin" / tool).chmod(0o755)
        subprocess.run(["git", "-C", str(self.root), "init", "-q"], check=True)
        self.commit()
        self.env = {"GITHUB_WORKSPACE": str(receipts.TRUSTED.parent), "GITHUB_REPOSITORY": "o/r",
                    "GITHUB_RUN_ID": "7", "GITHUB_RUN_ATTEMPT": "1", "GITHUB_JOB": receipts.JOB}
        os.environ.update(self.env)
        self.claims = {"INV-KANI": copy.deepcopy(KANI), "INV-EXTRACT": copy.deepcopy(EXTRACTION)}
        receipts.implementation_claims = lambda root: self.claims
        receipts.guard_cargo = lambda root, package: None
        self.out = self.tmp / "receipts"
        # The verifier's own F*, never the receipts runner's tool directory.
        verifier = self.tmp / "verifier-fstar"
        verifier.mkdir()
        self.fstar = verifier / "fstar.exe"
        self.fstar.write_text(STUB)
        self.fstar.chmod(0o755)
        self.expected = {"headSha": self.head, "repository": "o/r", "runId": "7", "runAttempt": "1",
                         "job": receipts.JOB}

    def commit(self) -> None:
        for command in (["add", "-A"], ["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "c"]):
            subprocess.run(["git", "-C", str(self.root), *command], check=True)
        self.head = receipts.git(self.root, "rev-parse", "HEAD")

    def tearDown(self) -> None:
        os.environ.clear()
        os.environ.update(self.saved)
        shutil.rmtree(self.tmp)

    def produce(self) -> None:
        for claim in self.claims.values():
            receipts.run(self.root, claim, self.head, self.out, self.tmp / "tools")

    def receipt(self, claim: str) -> Path:
        return self.out / claim / "receipt.json"

    def edit(self, claim: str, **changes) -> None:
        document = json.loads(self.receipt(claim).read_text()) | changes
        self.receipt(claim).write_text(json.dumps(document))

    def rejected(self, claim: str, fragment: str, expected: dict | None = None) -> None:
        errors, _ = receipts.verify(self.root, self.out, expected or self.expected, self.fstar)
        self.assertTrue(any(e.startswith(claim) and fragment in e for e in errors), errors)

    def forge(self, claim: str, module: str) -> None:
        """What a compromised receipts runner can do: any module, with every digest made consistent."""
        (self.out / claim / "outputs/Lib.fst").write_text(module)
        self.edit(claim, outputs={"Lib.fst": receipts.sha256(self.out / claim / "outputs/Lib.fst")}, exitCode=0)

    def test_wrapper_receipts_are_labelled_and_match_the_schema(self) -> None:
        self.produce()
        errors, verdicts = receipts.verify(self.root, self.out, self.expected, self.fstar)
        # D47(c): no verdict admits a claim until the producer is isolated (#969).
        self.assertEqual(errors, [f"INV-EXTRACT: {receipts.LABELS['extraction']}; {receipts.NOT_ADMITTED}",
                                  f"INV-KANI: {receipts.LABELS['kani']}; {receipts.NOT_ADMITTED}"])
        self.assertEqual({v["claim"]: (v["verdict"], v["label"]) for v in verdicts},
                         {"INV-EXTRACT": ("candidate-reported", "F* re-checked theorem about the uploaded model, "
                                                                "correspondence candidate-reported"),
                          "INV-KANI": ("candidate-reported", "candidate-reported Kani result")})
        try:
            import jsonschema
        except ImportError:
            return
        schema = json.loads((HERE / "implementation-receipt.schema.json").read_text())
        for claim in self.claims:
            jsonschema.validate(json.loads(self.receipt(claim).read_text()), schema)
        for verdict in verdicts:
            jsonschema.validate(verdict, schema["$defs"]["verdict"] | {"$defs": schema["$defs"]})

    def test_kani_is_candidate_reported_and_never_admitted(self) -> None:
        # A consistent, successful Kani receipt is still only what the runner reported.
        self.produce()
        self.assertEqual(json.loads(self.receipt("INV-KANI").read_text())["exitCode"], 0)
        self.rejected("INV-KANI", "candidate-reported")
        shutil.rmtree(self.out / "INV-KANI")
        self.rejected("INV-KANI", "candidate-reported")

    def test_forged_well_typed_model_is_never_admitted(self) -> None:
        # Sol S2: a compromised producer uploads a different well-typed function
        # and makes every digest agree. F* accepts it, so the correspondence is
        # only candidate-reported and the claim is never graded rechecked.
        self.produce()
        self.forge("INV-EXTRACT", "module Lib\nlet f (x: bool) : bool = not x\n")
        errors, verdicts = receipts.verify(self.root, self.out, self.expected, self.fstar)
        verdict = next(v for v in verdicts if v["claim"] == "INV-EXTRACT")
        self.assertEqual((verdict["verdict"], verdict["candidateReported"]), ("candidate-reported", ["hax-extraction"]))
        self.assertIn(f"INV-EXTRACT: {receipts.LABELS['extraction']}; {receipts.NOT_ADMITTED}", errors)
        self.assertIn("#969", receipts.NOT_ADMITTED)

    def test_theorem_evidence_must_be_a_rechecked_proof(self) -> None:
        # Sol S4: a theorem cited outside proofRoot/proofs is never re-checked.
        for artifact in ("unchecked/NeverChecked.fst", "fst/Other.fst"):
            claim = copy.deepcopy(EXTRACTION)
            claim["evidence"]["proof"][0]["artifact"] = artifact
            with self.subTest(artifact=artifact), self.assertRaisesRegex(
                    receipts.Refused, "theorem evidence must cite a declared proof under fst"):
                receipts.extraction_plan(claim, "")
        receipts.extraction_plan(EXTRACTION, "")

    def test_oversized_receipt_artifact_is_refused(self) -> None:
        self.produce()
        self.addCleanup(setattr, receipts, "MAX_RECEIPT_FILES", receipts.MAX_RECEIPT_FILES)
        receipts.MAX_RECEIPT_FILES = 2
        errors, verdicts = receipts.verify(self.root, self.out, self.expected, self.fstar)
        self.assertEqual((verdicts, len(errors)), ([], 1))
        self.assertRegex(errors[0], "^receipts: receipt artifact exceeds 2 files")
        receipts.MAX_RECEIPT_FILES = 1024
        self.receipt("INV-EXTRACT").write_text(" " * (receipts.MAX_RECEIPT_JSON_BYTES + 1))
        self.rejected("INV-EXTRACT", "receipt.json is too large")

    def test_malicious_build_script_cannot_forge_an_extraction_receipt(self) -> None:
        # A build.rs that cargo-hax runs owns the receipts runner: it swaps the
        # runner's F* for one that always passes and writes an ill-typed module.
        # The receipt it leaves is consistent, so only the re-check can catch it.
        hax = self.tmp / "tools/bin/cargo-hax"
        hax.write_text(STUB.replace('(out / "Lib.fst").write_text(', '(out / "Lib.fst").write_text("ill-typed " + ') + (
            f"pathlib.Path({str(self.tmp / 'tools/bin/fstar.exe')!r}).write_text('#!/bin/sh\\necho fstar.exe 0.0-stub\\n')\n"))
        self.produce()
        self.assertEqual(json.loads(self.receipt("INV-EXTRACT").read_text())["exitCode"], 0)
        self.rejected("INV-EXTRACT", "independent F* re-check failed")

    def test_tampered_extracted_module_is_rejected(self) -> None:
        for module, fragment in (
                ("module Lib\nlet f (x: bool) : bool = ill-typed\n", "independent F* re-check failed"),
                ("module Lib\nassume val f : bool -> bool\n", "proof hole"),
                ("module Lib\nlet f (x: bool) : bool = x\nlet t : squash False = magic ()\n", "proof hole"),
                ("module Lib\nlet f (x: bool) : bool = unsafe_coerce 0\n", "proof hole"),
                ("module Lib\n[@@admit_termination]\nlet f (x: bool) : bool = x\n", "proof hole"),
                ("module Lib\nlet f (x: bool) : bool = x\n[@@expect_failure]\nlet t () : Lemma False = ()\n",
                 "proof hole"),
                ("module Lib\n[@@resolve_implicits]\nlet f (x: bool) : bool = x\n", "meta-programs"),
                # FStarLang/FStar#4554: this proves False in the pinned F*.
                ("module Lib\nlet f (x: bool) : bool = x\nlet g (n:nat) : (r:nat{r == n}) =\n"
                 "  let rec go (m:nat) : (r:nat{r == n}) = n in go n\n", "let rec is not re-checked"),
                # The same inner let rec at column zero.
                ("module Lib\nlet f (x: bool) : bool = x\nlet g (n:nat) : (r:nat{r == n}) =\n"
                 "let rec go (m:nat) : (r:nat{r == n}) = n in go n\n", "let rec is not re-checked"),
                ("module Lib\nlet f (x: bool) : bool = x\nlet t () : Lemma False = Prims._assume False\n", "proof hole"),
                ("module Lib\nlet f (x: bool) : bool = x\nlet t = admitP False\n", "proof hole"),
                ('module Lib\n#set-options "--load_cmxs evil"\nlet f (x: bool) : bool = x\n', "is not allowed"),
                ('module Lib\n#push-options "--z3cliopt smt.x=1"\nlet f (x: bool) : bool = x\n', "is not allowed"),
                ("module Lib\nlet f (x: bool) : bool = x\nlet _ = assert True by (launch_process \"sh\" [] \"\")\n",
                 "meta-programs"),
                ("module Lib\nopen FStar.Tactics\nlet f (x: bool) : bool = x\n", "meta-programs")):
            shutil.rmtree(self.out, ignore_errors=True)
            self.produce()
            self.forge("INV-EXTRACT", module)
            self.rejected("INV-EXTRACT", fragment)

    def test_symlinked_proof_inputs_and_missing_items_are_rejected(self) -> None:
        # A committed symlink would make F* read bytes that are not in the reviewed commit.
        outside = self.tmp / "outside.fst"
        outside.write_text("module Proof\nopen Lib\n")
        (self.root / "fst/Proof.fst").unlink()
        (self.root / "fst/Proof.fst").symlink_to(outside)
        self.commit()
        self.expected["headSha"] = self.head
        self.produce()
        self.rejected("INV-EXTRACT", "symlink in a declared input path")
        (self.root / "fst/Proof.fst").unlink()
        (self.root / "fst/Proof.fst").write_text("module Proof\nopen Lib\n")
        self.commit()
        self.expected["headSha"] = self.head
        shutil.rmtree(self.out)
        self.produce()
        for module in ("module Lib\nlet g (x: bool) : bool = x\n",
                       # A declaration only inside a comment is no definition.
                       "module Lib\n(*\nlet f (x: bool) : bool = x\n*)\nlet g (x: bool) : bool = x\n"):
            self.forge("INV-EXTRACT", module)
            self.rejected("INV-EXTRACT", "lib::f is not defined exactly once")

    def test_declared_items_may_be_recursive_definitions(self) -> None:
        module = self.tmp / "Rec.fst"
        module.write_text("module Rec\nlet rec f (n: nat) : nat = if n = 0 then 0 else f (n - 1)\n")
        self.assertTrue(receipts.defines_once(module, "f"))
        module.write_text("module Rec\nlet rec f (n: nat) : nat = 0\nlet f (n: nat) : nat = 0\n")
        self.assertFalse(receipts.defines_once(module, "f"))

    def test_solver_budget_options_are_allowed(self) -> None:
        self.produce()
        self.forge("INV-EXTRACT", 'module Lib\n#set-options "--fuel 0 --ifuel 1 --z3rlimit 15"\n'
                                  '#push-options "--z3rlimit 80"\nlet f (x: bool) : bool = x\n#pop-options\n')
        errors, verdicts = receipts.verify(self.root, self.out, self.expected, self.fstar)
        self.assertIn(f"INV-EXTRACT: {receipts.LABELS['extraction']}; {receipts.NOT_ADMITTED}", errors)
        self.assertEqual(len([e for e in errors if e.startswith("INV-EXTRACT")]), 1, errors)

    def test_extraction_needs_an_independent_fstar(self) -> None:
        self.produce()
        errors, _ = receipts.verify(self.root, self.out, self.expected, None)
        self.assertTrue(any(e.startswith("INV-EXTRACT") and "no independent F*" in e for e in errors), errors)

    def test_verifier_fstar_pin_matches_the_bootstrap(self) -> None:
        bootstrap = (receipts.TRUSTED / "scripts/bootstrap-rust-extraction-tools.sh").read_text()
        self.assertIn(f'FSTAR_VERSION="{receipts.FSTAR_VERSION}"', bootstrap)
        self.assertIn(f'"{receipts.FSTAR_SHA256}"', bootstrap)

    def test_scope_follows_claims_and_their_inputs(self) -> None:
        workspace = {"Cargo.toml": '[workspace]\nmembers = ["lib"]\n',
                     "lib/Cargo.toml": '[package]\nname = "lib"\n[dependencies]\ndep = { path = "../dep" }\n'
                                       '[dev-dependencies]\ntest-only = { path = "../test-only" }\n',
                     "lib/src/lib.rs": "", "dep/Cargo.toml": '[package]\nname = "dep"\n', "dep/src/lib.rs": "",
                     "test-only/Cargo.toml": '[package]\nname = "test-only"\n', "unrelated.md": ""}
        for name, text in workspace.items():
            (self.root / name).parent.mkdir(parents=True, exist_ok=True)
            (self.root / name).write_text(text)
        self.commit()
        base = self.tmp / "base"
        subprocess.run(["git", "clone", "-q", str(self.root), str(base)], check=True)
        self.assertFalse(receipts.in_scope(self.root, base))
        for path, expected in (("unrelated.md", True), ("test-only/Cargo.toml", True), ("dep/src/lib.rs", True),
                               ("src/lib.rs", True), ("fst/Proof.fst", True), ("Cargo.lock", True),
                               ("verification/coverage-v1/manifest/claim.json", True)):
            (self.root / path).parent.mkdir(parents=True, exist_ok=True)
            with (self.root / path).open("a") as handle:
                handle.write("\n// changed\n")
            self.commit()
            self.assertEqual(receipts.in_scope(self.root, base), expected, path)
            subprocess.run(["git", "-C", str(self.root), "reset", "-q", "--hard", "HEAD~1"], check=True)

    def assert_outside_rust_change_in_scope(self, source: str) -> None:
        (self.root / "src/lib.rs").write_text(source)
        (self.root / "outside.rs").write_text("pub fn external() {}\n")
        self.commit()
        base = self.tmp / "base"
        subprocess.run(["git", "clone", "-q", str(self.root), str(base)], check=True)
        self.assertFalse(receipts.in_scope(self.root, base))
        (self.root / "outside.rs").write_text("pub fn external() { }\n")
        self.commit()
        self.assertTrue(receipts.in_scope(self.root, base))

    def test_scope_covers_commented_path_attribute(self) -> None:
        self.assert_outside_rust_change_in_scope('#[/*gap*/path = "../../outside.rs"] mod external;\n')

    def test_scope_covers_commented_include_macro(self) -> None:
        self.assert_outside_rust_change_in_scope('include /*gap*/ !("../../outside.rs");\n')

    def test_scope_covers_commented_out_cfg_attr(self) -> None:
        self.assert_outside_rust_change_in_scope('// #[cfg_attr(all(), path = "../../outside.rs")]\n')

    def test_scope_covers_macro_generated_path_attribute(self) -> None:
        self.assert_outside_rust_change_in_scope(
            'macro_rules! m { ($a:ident) => { #[$a = "../../outside.rs"] mod external; } }\n'
            'm!(path);\n')

    def test_scope_out_only_for_identical_trees(self) -> None:
        base = self.tmp / "base"
        subprocess.run(["git", "clone", "-q", str(self.root), str(base)], check=True)
        self.assertFalse(receipts.in_scope(self.root, base))
        for path in ("code.md", "docs/code.rs", "Docs/code.md"):
            with self.subTest(path=path):
                (self.root / "src/lib.rs").write_text('include!("../code.md");\n')
                target = self.root / path
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_text("// initial input\n")
                self.commit()
                subprocess.run(["git", "-C", str(base), "fetch", "-q", "origin"], check=True)
                subprocess.run(["git", "-C", str(base), "reset", "--hard", "origin/HEAD"], check=True, capture_output=True)
                target.write_text("// changed input\n")
                self.commit()
                self.assertTrue(receipts.in_scope(self.root, base))

    def test_scope_covers_nonstandard_build_inputs(self) -> None:
        for path, consumer in (
                ("external/code.inc", 'include!("../external/code.inc");\n'),
                ("data.bin", 'const DATA: &[u8] = include_bytes!("../data.bin");\n'),
                ("schema.txt", 'fn main() { std::fs::read("schema.txt").unwrap(); }\n')):
            with self.subTest(path=path):
                target = self.root / path
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_bytes(b"original\0" if path.endswith(".bin") else b"original")
                consumer_path = self.root / ("build.rs" if path == "schema.txt" else "src/lib.rs")
                consumer_path.write_text(consumer)
                self.commit()
                base = self.tmp / "base"
                subprocess.run(["git", "clone", "-q", str(self.root), str(base)], check=True)
                target.write_bytes(b"changed\0" if path.endswith(".bin") else b"changed")
                self.commit()
                self.assertTrue(receipts.in_scope(self.root, base))
                shutil.rmtree(base)

    def test_scope_covers_submodule_gitlink(self) -> None:
        first = self.head
        subprocess.run(["git", "-C", str(self.root), "update-index", "--add", "--cacheinfo",
                        f"160000,{first},docs/submodule.md"], check=True)
        subprocess.run(["git", "-C", str(self.root), "-c", "user.name=t", "-c", "user.email=t@t",
                        "commit", "-qm", "add gitlink"], check=True)
        second = receipts.git(self.root, "rev-parse", "HEAD")
        base = self.tmp / "base"
        subprocess.run(["git", "clone", "-q", str(self.root), str(base)], check=True)
        subprocess.run(["git", "-C", str(self.root), "update-index", "--cacheinfo",
                        f"160000,{second},docs/submodule.md"], check=True)
        subprocess.run(["git", "-C", str(self.root), "-c", "user.name=t", "-c", "user.email=t@t",
                        "commit", "-qm", "change gitlink"], check=True)
        self.assertTrue(receipts.in_scope(self.root, base))

    def test_scope_covers_docs_mixed_with_inc(self) -> None:
        base = self.tmp / "base"
        subprocess.run(["git", "clone", "-q", str(self.root), str(base)], check=True)
        (self.root / "README.md").write_text("Documentation\n")
        (self.root / "code.inc").write_text("pub fn f() {}\n")
        self.commit()
        self.assertTrue(receipts.in_scope(self.root, base))

    def test_scope_never_exempts_binary_deleted_renamed_or_mode_changed_docs(self) -> None:
        (self.root / "docs").mkdir()
        target = self.root / "docs/guide.md"
        target.write_text("Documentation\n")
        self.commit()
        base = self.tmp / "base"
        subprocess.run(["git", "clone", "-q", str(self.root), str(base)], check=True)
        for change in ("binary", "delete", "rename", "mode"):
            with self.subTest(change=change):
                if change == "binary":
                    target.write_bytes(b"binary\0data")
                elif change == "delete":
                    target.unlink()
                elif change == "rename":
                    target.rename(self.root / "docs/renamed.md")
                else:
                    target.chmod(0o755)
                self.commit()
                self.assertTrue(receipts.in_scope(self.root, base))
                subprocess.run(["git", "-C", str(self.root), "reset", "-q", "--hard", "HEAD~1"], check=True)

    def test_scope_covers_added_and_deleted_build_inputs_without_claims(self) -> None:
        self.claims = {}
        base = self.tmp / "base"
        subprocess.run(["git", "clone", "-q", str(self.root), str(base)], check=True)
        for path in ("external/new.rs", "external/Cargo.toml", "external/Cargo.lock",
                     "rust-toolchain-custom", ".cargo/custom.json", "external/build.rs", "settings.json"):
            with self.subTest(path=path):
                target = self.root / path
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_text("new input\n")
                self.commit()
                self.assertTrue(receipts.in_scope(self.root, base))
                self.assertTrue(receipts.in_scope(base, self.root))
                subprocess.run(["git", "-C", str(self.root), "reset", "-q", "--hard", "HEAD~1"], check=True)

    def test_unchanged_claims_are_labelled_but_never_admitted(self) -> None:
        errors, verdicts = receipts.verify(self.root, self.out, self.expected, None, scoped=False)
        self.assertEqual({v["claim"]: (v["verdict"], v["label"]) for v in verdicts},
                         {"INV-EXTRACT": ("unchanged", receipts.UNCHANGED_LABEL),
                          "INV-KANI": ("unchanged", receipts.UNCHANGED_LABEL)})
        self.assertTrue(all(v["rechecked"] == v["candidateReported"] == [] for v in verdicts), verdicts)
        self.assertEqual([e.split(":")[0] for e in errors], ["INV-EXTRACT", "INV-KANI"])
        self.assertTrue(all(e.endswith(receipts.NOT_ADMITTED) for e in errors), errors)

    def test_fake_extraction_script_without_a_receipt_is_rejected(self) -> None:
        self.produce()
        shutil.rmtree(self.out / "INV-EXTRACT")
        self.rejected("INV-EXTRACT", "no receipt from the base-owned wrapper")

    def test_handwritten_receipts_are_rejected(self) -> None:
        self.produce()
        self.edit("INV-EXTRACT", wrapperSha256="0" * 64)
        self.rejected("INV-EXTRACT", "not from the base wrapper")
        self.produce_again()
        (self.out / "INV-EXTRACT/logs/1.log").write_text("forged\n")
        self.rejected("INV-EXTRACT", "log digest mismatch")

    def produce_again(self) -> None:
        shutil.rmtree(self.out)
        self.produce()

    def test_wrapper_from_the_candidate_tree_is_rejected(self) -> None:
        self.produce()
        copy_path = self.root / "scripts/ci/implementation_receipts.py"
        copy_path.parent.mkdir(parents=True)
        shutil.copyfile(receipts.HERE, copy_path)
        self.edit("INV-KANI", wrapper=os.path.relpath(copy_path, self.env["GITHUB_WORKSPACE"]))
        self.rejected("INV-KANI", "wrapper from the candidate tree")

    def test_kani_nested_or_obfuscated_cannot_become_an_extraction_claim(self) -> None:
        # A Kani harness behind an extraction binding: the extraction wrapper refuses it.
        nested = copy.deepcopy(KANI) | {"extractionBinding": BINDING}
        with self.assertRaisesRegex(receipts.Refused, "theorem and predicate evidence only"):
            receipts.run(self.root, nested, self.head, self.out, self.tmp / "tools")
        # An obfuscated Kani script with theorem evidence and no binding is a Kani
        # claim, so it still needs a harness and mutations, which it lacks.
        hidden = copy.deepcopy(EXTRACTION)
        del hidden["extractionBinding"]
        for entry in hidden["evidence"]["proof"]:
            entry["command"] = "scripts/inner-proof.sh"
        self.claims = {hidden["id"]: hidden}
        with self.assertRaisesRegex(receipts.Refused, "harness and predicate evidence only"):
            receipts.run(self.root, hidden, self.head, self.out, self.tmp / "tools")
        self.rejected(hidden["id"], "candidate-reported")

    def test_receipt_kind_must_be_the_wrapper_the_claim_needs(self) -> None:
        self.produce()
        self.claims["INV-KANI"] = self.claims["INV-KANI"] | {"extractionBinding": BINDING}
        self.rejected("INV-KANI", "kind does not match")

    def test_wrong_head_run_attempt_or_job_is_rejected(self) -> None:
        self.produce()
        for key, value in (("headSha", "f" * 40), ("runId", "8"), ("runAttempt", "2"), ("job", "core")):
            self.rejected("INV-KANI", f"receipt {key}", self.expected | {key: value})

    def test_mismatched_output_digest_is_rejected(self) -> None:
        self.produce()
        (self.out / "INV-EXTRACT/outputs/Lib.fst").write_text("module Lib\nlet f (x: bool) : bool = true\n")
        self.rejected("INV-EXTRACT", "output digest mismatch")

    def test_changed_input_is_rejected(self) -> None:
        self.produce()
        self.edit("INV-KANI", inputs={"src/lib.rs": "0" * 64})
        self.rejected("INV-KANI", "input digests")

    def test_nonzero_exit_is_rejected(self) -> None:
        # The mutant replaces nothing that matters, so it still verifies.
        self.claims["INV-KANI"]["correspondenceMutations"] = [{"name": "noop", "before": "pub fn", "after": "pub  fn"}]
        self.produce()
        self.assertEqual(json.loads(self.receipt("INV-KANI").read_text())["exitCode"], 1)
        self.rejected("INV-KANI", "failed wrapper run")
        self.edit("INV-KANI", exitCode=0)
        self.rejected("INV-KANI", "noop: Kani verdict does not prove the claim")

    def test_fstar_cannot_resolve_undeclared_candidate_modules(self) -> None:
        # An undeclared helper with an axiom, opened by the declared proof.
        (self.root / "fst/Helper.fst").write_text("module Helper\nassume val bad : squash False\n")
        self.commit()
        commands = []
        self.addCleanup(setattr, receipts, "invoke", receipts.invoke)
        receipts.invoke = (lambda real: lambda argv, cwd, env, log: commands.append(
            (argv, sorted(str(f.relative_to(cwd)) for f in Path(cwd).rglob("*.fst")))) or real(argv, cwd, env, log))(receipts.invoke)
        receipts.run(self.root, self.claims["INV-EXTRACT"], self.head, self.out, self.tmp / "tools")
        for argv, visible in commands[1:]:
            self.assertEqual(visible, ["out/Lib.fst", "proofs/Proof.fst"], argv)
        # A hole in a declared proof fails the run.
        (self.root / "fst/Proof.fst").write_text("module Proof\nlet x = admit ()\n")
        self.commit()
        shutil.rmtree(self.out)
        self.assertEqual(receipts.run(self.root, self.claims["INV-EXTRACT"], self.head, self.out, self.tmp / "tools"), 1)

    def test_extraction_guard_refuses_local_build_scripts(self) -> None:
        metadata = {"packages": [{"id": "a", "name": "lib", "version": "0.1.0", "source": None,
                                  "targets": [{"kind": ["lib"]}]},
                                 {"id": "b", "name": "evil", "version": "0.1.0", "source": None,
                                  "targets": [{"kind": ["custom-build"]}]}],
                    "resolve": {"nodes": [{"id": "a", "deps": [{"pkg": "b", "dep_kinds": [{"kind": None}]}]},
                                          {"id": "b", "deps": []}]}}
        fake = self.tmp / "cargo-bin/cargo"
        fake.parent.mkdir()
        fake.write_text(f"#!/bin/sh\ncat <<'EOF'\n{json.dumps(metadata)}\nEOF\n")
        fake.chmod(0o755)
        os.environ["PATH"] = f"{fake.parent}:{os.environ['PATH']}"
        guard = importlib.util.module_from_spec(SPEC)
        SPEC.loader.exec_module(guard)
        for name in ("Cargo.lock", *guard.CARGO_CONFIG):
            if (guard.TRUSTED / name).is_file():
                (self.root / name).parent.mkdir(exist_ok=True)
                shutil.copyfile(guard.TRUSTED / name, self.root / name)
        with self.assertRaisesRegex(guard.Refused, "evil: local build script or proc macro"):
            guard.guard_cargo(self.root, "lib")

    def test_extraction_guard_keys_build_time_packages_on_their_source(self) -> None:
        guard = importlib.util.module_from_spec(SPEC)
        SPEC.loader.exec_module(guard)
        # Read the trusted lock directly: the same packages lock_identities parses.
        packages = tomllib.loads((guard.TRUSTED / "Cargo.lock").read_text())["package"]
        name, version, source, lock_digest = next((p["name"], p["version"], p.get("source"), p.get("checksum"))
                                                  for p in packages if p["name"] == "serde" and p.get("source"))
        evil = f'git+https://example.invalid/serde#{"0" * 40}'
        metadata = {"packages": [{"id": "a", "name": "lib", "version": "0.1.0", "source": None,
                                  "targets": [{"kind": ["lib"]}]},
                                 {"id": "b", "name": name, "version": version, "source": evil,
                                  "targets": [{"kind": ["custom-build"]}]}],
                    "resolve": {"nodes": [{"id": "a", "deps": [{"pkg": "b", "dep_kinds": [{"kind": None}]}]},
                                          {"id": "b", "deps": []}]}}
        fake = self.tmp / "cargo-bin/cargo"
        fake.parent.mkdir()
        fake.write_text(f"#!/bin/sh\ncat <<'EOF'\n{json.dumps(metadata)}\nEOF\n")
        fake.chmod(0o755)
        os.environ["PATH"] = f"{fake.parent}:{os.environ['PATH']}"
        for config in guard.CARGO_CONFIG:
            if (guard.TRUSTED / config).is_file():
                (self.root / config).parent.mkdir(exist_ok=True)
                shutil.copyfile(guard.TRUSTED / config, self.root / config)
        lock = (guard.TRUSTED / "Cargo.lock").read_text()
        git_entry = f'\n[[package]]\nname = "{name}"\nversion = "{version}"\nsource = "{evil}"\n'
        # The git package beside the trusted registry entry of the same name and version.
        (self.root / "Cargo.lock").write_text(lock + git_entry)
        with self.assertRaisesRegex(guard.Refused, "more than one source"):
            guard.guard_cargo(self.root, "lib")
        # The git package alone, in place of the registry one.
        registry = f'name = "{name}"\nversion = "{version}"\nsource = "{source}"\nchecksum = "{lock_digest}"\n'
        self.assertIn(registry, lock)
        (self.root / "Cargo.lock").write_text(lock.replace(registry, f'name = "{name}"\nversion = "{version}"\n'
                                                                    f'source = "{evil}"\n'))
        with self.assertRaisesRegex(guard.Refused, f"{name} {version}: build-time code not locked on the base"):
            guard.guard_cargo(self.root, "lib")


if __name__ == "__main__":
    unittest.main()
