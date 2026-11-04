#!/usr/bin/env python3
"""Require production negation correspondence, then reject two seeded mutants.

Run with the pinned Kani 0.67.0 installation on PATH. Mutants live in a disposable
copy; the checkout's production source and proof are never modified.
"""

import os
from pathlib import Path
import shutil
import subprocess
import tempfile


ROOT = Path(__file__).resolve().parents[2]
SOURCE = Path("crates/norn-checker/src/exact_rational.rs")
HARNESS = Path("verification/implementation-v1/rational_neg_kani.rs")


def main():
    scratch = Path.home() / ".cache/agent-scratch/exp8-negation-mutations"
    scratch.mkdir(parents=True, exist_ok=True)
    environment = {**os.environ, "RUSTFLAGS": "--edition=2024"}
    # Absolute tool homes survive the move into the disposable copy.
    for name in ("KANI_HOME", "RUSTUP_HOME"):
        if name in environment:
            environment[name] = str(Path(environment[name]).resolve())
    original = (ROOT / SOURCE).read_text()
    mutations = {
        "baseline": original,
        "overflow-clamp": original.replace(
            "i64::try_from(numerator.checked_div(a)?).ok()?",
            "i64::try_from(numerator.checked_div(a)?).unwrap_or(i64::MAX)",
        ),
        "negation-sign": original.replace(
            "i128::from(self.numerator).checked_neg()?", "i128::from(self.numerator)"
        ),
    }
    try:
        with tempfile.TemporaryDirectory(dir=scratch) as directory:
            work = Path(directory)
            for path in (SOURCE, HARNESS):
                (work / path).parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(ROOT / path, work / path)
            for name, source in mutations.items():
                if name != "baseline" and source == original:
                    raise RuntimeError(f"{name}: mutation no longer matches production")
                (work / SOURCE).write_text(source)
                result = subprocess.run(
                    ["kani", "--fail-fast", "--output-format", "terse", str(HARNESS)],
                    cwd=work, env=environment, capture_output=True, text=True, timeout=120,
                )
                expected = "VERIFICATION:- SUCCESSFUL" if name == "baseline" else "VERIFICATION:- FAILED"
                if (result.returncode == 0) != (name == "baseline") or expected not in result.stdout:
                    raise RuntimeError(f"{name}: unexpected proof result\n{result.stdout}\n{result.stderr}")
                print(f"{name}: {'proved' if name == 'baseline' else 'detected'}")
    finally:
        if not any(scratch.iterdir()):
            scratch.rmdir()


if __name__ == "__main__":
    main()
