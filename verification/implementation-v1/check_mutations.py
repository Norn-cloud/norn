#!/usr/bin/env python3
"""Require the selected proof to detect every declared production mutation."""

import argparse
import importlib.util
import os
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts"))
import coverage_manifest


def check_inventory(result, name, success):
    """Require Kani to check exactly `name` with the expected single verdict."""
    output = re.sub(r"\x1b\[[0-?]*[ -/]*[@-~]", "", result.stdout + result.stderr)
    checked = re.findall(r"^Checking harness (.+)\.\.\.$", output, re.MULTILINE)
    verdicts = re.findall(r"^VERIFICATION:- (.+)$", output, re.MULTILINE)
    summaries = re.findall(r"^Complete - (.+)$", output, re.MULTILINE)
    expected = "SUCCESSFUL" if success else "FAILED"
    summary = ("1 successfully verified harnesses, 0 failures, 1 total." if success
               else "0 successfully verified harnesses, 1 failures, 1 total.")
    if (checked != [name] or verdicts != [expected] or summaries != [summary]
            or (result.returncode == 0) != success):
        raise RuntimeError(f"expected {expected} for exactly {name}\n{output}")


def check_claim(claim, *, root=ROOT, harness_text=None):
    """Prove the baseline, then require every declared production mutant to fail."""
    proofs = [(entry, selector.split(":", 1)[1])
              for entry in claim["evidence"]["proof"]
              for selector in entry["selectors"] if selector.startswith("harness:")]
    if len(proofs) != 1:
        raise RuntimeError("mutation correspondence requires exactly one Kani harness")
    entry, name = proofs[0]
    source = Path(claim["correspondenceSource"])
    harness = Path(entry["artifact"])
    arguments = shlex.split(entry["command"])
    environment = dict(os.environ)
    if arguments[0] == "env":
        for assignment in arguments[1:arguments.index("kani")]:
            key, value = assignment.split("=", 1)
            environment[key] = value
    for key in ("KANI_HOME", "RUSTUP_HOME"):
        if key in environment:
            environment[key] = str((root / environment[key]).resolve())
    original = (root / source).read_text()
    mutations = claim["correspondenceMutations"]
    # Keep working copies under the repository's ignored target/ tree.
    scratch = root / "target" / "correspondence-scratch"
    scratch.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="correspondence-", dir=scratch) as directory:
        work = Path(directory)
        for path in (source, harness):
            (work / path).parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(root / path, work / path)
        if harness_text is not None:
            (work / harness).write_text(harness_text)
            spec = importlib.util.spec_from_file_location("coverage_validator", ROOT / "verification/coverage-v1/validate.py")
            validator = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(validator)
            validator.ROOT = work
            validator.validate_claim_evidence_shape("canary", claim["id"], claim)
        for mutation in [None, *mutations]:
            label = mutation["name"] if mutation else "baseline"
            candidate = original
            if mutation:
                if original.count(mutation["before"]) != 1 or mutation["before"] == mutation["after"]:
                    raise RuntimeError(f"{label}: mutation must replace exactly one production occurrence")
                candidate = original.replace(mutation["before"], mutation["after"])
            (work / source).write_text(candidate)
            result = subprocess.run(
                ["kani", "--fail-fast", "--output-format", "terse", "--harness", name, str(harness)],
                cwd=work, env=environment, capture_output=True, text=True, timeout=120,
            )
            try:
                check_inventory(result, name, mutation is None)
            except RuntimeError as error:
                raise RuntimeError(f"{label}: {error}") from error
            print(f"{label}: {'proved' if mutation is None else 'Kani failed as required'}", flush=True)


def regressions(claim):
    """Run real Kani canaries: unused production import and empty proof.

    The promoted mutation gate passes --regressions, so these run in every full
    profile. A disconnected harness proves the baseline and then also proves the
    first declared production mutant, which must be rejected.
    """
    first = claim["correspondenceMutations"][0]["name"]
    harness = next(e["artifact"] for e in claim["evidence"]["proof"]
                   if any(s.startswith("harness:") for s in e["selectors"]))
    real = (ROOT / harness).read_text()
    production = (ROOT / claim["correspondenceSource"]).read_text().split("#[cfg(test)]")[0]
    local = real.replace("let input = exact_rational::Rational", "let input = local::Rational")
    local += "\nmod local {\n" + production + "\n}\n"
    empty = real[:real.index("#[kani::proof]")] + "#[kani::proof]\nfn integer_negation_correspondence() {}\n"
    for label, text in (("unused-import-local-copy", local), ("empty-proof", empty)):
        try:
            check_claim(claim, harness_text=text)
        except RuntimeError as error:
            # Baseline must pass; the first unchanged mutant must still verify.
            if not str(error).startswith(f"{first}: expected FAILED") or "VERIFICATION:- SUCCESSFUL" not in str(error):
                raise
            print(f"{label}: rejected (production mutation still verifies)", flush=True)
        else:
            raise RuntimeError(f"{label}: disconnected proof accepted")


def main():
    """Run the mutation gate for one claim, optionally with its canaries."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--claim", required=True)
    parser.add_argument("--regressions", action="store_true")
    args = parser.parse_args()
    claims = [c for s in coverage_manifest.load()["subsystems"] for c in s["obligationClaims"]
              if c["id"] == args.claim]
    if len(claims) != 1:
        raise RuntimeError("expected exactly one correspondence claim")
    check_claim(claims[0])
    if args.regressions:
        regressions(claims[0])


if __name__ == "__main__":
    main()
