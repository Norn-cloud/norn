#!/usr/bin/env python3
"""The stable CI entrypoint of the default-deny gates (#3472). No gate logic lives here.

The trusted workflows pin the one command below, so the trusted lane's check of its
own workflow does not change when the gates do; gate changes land in base code
(crates/norn-skeleton-guard), never in the workflow. Without `--root` it checks this
checkout (`cargo xtask ci policy` and the Python isolation policy); no workflow runs
that mode, it is for running by hand.

- trusted lane, `python3 -I -B -X pycache_prefix=<fresh> trusted/scripts/ci/validate-runner-policy.py --root
  candidate --maintainer-evidence <file>`: the gate binary built from the
  checkout that holds this file (the pull request's base) checks the pull
  request as data. Nothing from the candidate runs. The evidence file, recorded
  by the base-owned workflow from the GitHub API, lets a same-repository head
  that an admin approved with `/approve-gates <head sha>` change workflows and
  gates.
  Approval changes pinned contracts, not the runner boundary: only the exact
  merge-group Boat opt-in is admitted, with a hosted fallback when unset.
  Pull requests and trusted-policy stay hosted; custom runner labels are refused.
"""

import argparse
import os
import subprocess
import sys
import tempfile
from pathlib import Path

# Matches the public workflow and repository toolchain.
TOOLCHAIN = "1.98.0"
REPO = Path(__file__).resolve().parents[2]
# Every child Python starts isolated (#513): `-I` (no script directory or cwd on sys.path, no
# PYTHON* environment), `-B` and a fresh bytecode prefix, so no `__pycache__` next to any
# source, standard library included, is read or written. python_isolation.py pins this line.
PYTHON = [sys.executable, "-I", "-B", "-X", f"pycache_prefix={tempfile.mkdtemp(prefix='norn-pycache-')}"]
# Run before any child: the interpreter must honour every flag, or nothing below runs.
ISOLATED = "import sys; sys.exit(0 if sys.flags.isolated and sys.dont_write_bytecode and sys.pycache_prefix else 1)"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--root", help="the pull request checkout, checked as data by this checkout's gates")
    parser.add_argument("--maintainer-evidence", help="API evidence of an admin approval of the exact head")
    args = parser.parse_args()
    env = {**os.environ, "RUSTUP_TOOLCHAIN": TOOLCHAIN}
    subprocess.run(["rustup", "toolchain", "install", TOOLCHAIN, "--profile", "minimal", "--no-self-update"], check=True, env=env)
    if args.root is None:
        command = ["cargo", "xtask", "ci", "policy"]
    else:
        # The guard is its own workspace (D95): its manifest and lockfile sit in the gated
        # crate directory, so the root manifest and lockfile never reach this binary.
        command = ["cargo", "run", "--locked", "--quiet",
                   "--manifest-path", str(REPO / "crates/norn-skeleton-guard/Cargo.toml"),
                   "--bin", "xtask", "--", "ci", "trusted",
                   "--root", str(Path(args.root).resolve()), "--base-root", str(REPO)]
        if args.maintainer_evidence is not None:
            command += ["--maintainer-evidence", str(Path(args.maintainer_evidence).resolve())]
    if subprocess.run([*PYTHON, "-c", ISOLATED], cwd=REPO, env=env).returncode != 0:
        print("child Python is not isolated (-I, -B, pycache_prefix)", file=sys.stderr)
        return 1
    result = subprocess.run(command, cwd=REPO, env=env).returncode
    # Base-owned Python isolation (#513).
    root = str(Path(args.root).resolve()) if args.root is not None else str(REPO)
    isolation = subprocess.run([*PYTHON, str(REPO / "scripts/ci/python_isolation.py"),
                                "--root", root], cwd=REPO, env=env).returncode
    if result != 0 or isolation != 0 or args.root is None:
        return result or isolation
    # Execute the base validator over candidate bytes; never import candidate Python.
    return subprocess.run([*PYTHON, str(REPO / "scripts/proof_enrollment.py"),
                           "--root", root], cwd=REPO, env=env).returncode


if __name__ == "__main__":
    sys.exit(main())
