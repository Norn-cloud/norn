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
        self.saved = os.environ.copy()
        os.environ.update(self.env)
        self.claims = {"INV-KANI": copy.deepcopy(KANI), "INV-EXTRACT": copy.deepcopy(EXTRACTION)}
        receipts.implementation_claims = lambda root: self.claims
        receipts.guard_cargo = lambda root, package: None
        self.out = self.tmp / "receipts"
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
        errors = receipts.verify(self.root, self.out, expected or self.expected)
        self.assertTrue(any(e.startswith(claim) and fragment in e for e in errors), errors)

    def test_wrapper_receipts_are_admitted_and_match_the_schema(self) -> None:
        self.produce()
        self.assertEqual(receipts.verify(self.root, self.out, self.expected), [])
        try:
            import jsonschema
        except ImportError:
            return
        schema = json.loads((HERE / "implementation-receipt.schema.json").read_text())
        for claim in self.claims:
            jsonschema.validate(json.loads(self.receipt(claim).read_text()), schema)

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
        self.rejected(hidden["id"], "no receipt")

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


if __name__ == "__main__":
    unittest.main()
