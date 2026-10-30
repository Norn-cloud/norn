#!/usr/bin/env python3
"""The stable CI entrypoint of the default-deny gates (#3472). No gate logic lives here.

The policy workflow pins exactly these two commands, so the trusted lane's
check of its own workflow does not change when the gates do; gate changes land
in base code (crates/norn-skeleton-guard), never in the workflow:

- policy lane, `python3 scripts/ci/validate-runner-policy.py`: this checkout's
  gates, `cargo xtask ci policy`;
- trusted lane, `python3 trusted/scripts/ci/validate-runner-policy.py --root
  candidate --maintainer-evidence <file>`: the gate binary built from the
  checkout that holds this file (the pull request's base) checks the pull
  request as data. Nothing from the candidate runs. The evidence file, recorded
  by the base-owned workflow from the GitHub API, lets a same-repository head
  that a maintainer approved on its exact commit change workflows and gates.
"""

import argparse
import os
import subprocess
import sys
from pathlib import Path

# Matches the public workflow and repository toolchain.
TOOLCHAIN = "1.98.0"
REPO = Path(__file__).resolve().parents[2]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--root", help="the pull request checkout, checked as data by this checkout's gates")
    parser.add_argument("--maintainer-evidence", help="API evidence of a maintainer approval of the exact head")
    args = parser.parse_args()
    env = {**os.environ, "RUSTUP_TOOLCHAIN": TOOLCHAIN}
    subprocess.run(["rustup", "toolchain", "install", TOOLCHAIN, "--profile", "minimal", "--no-self-update"], check=True, env=env)
    if args.root is None:
        command = ["cargo", "xtask", "ci", "policy"]
    else:
        command = ["cargo", "run", "--locked", "--quiet", "--manifest-path", str(REPO / "Cargo.toml"),
                   "-p", "norn-skeleton-guard", "--bin", "xtask", "--", "ci", "trusted",
                   "--root", str(Path(args.root).resolve()), "--base-root", str(REPO)]
        if args.maintainer_evidence is not None:
            command += ["--maintainer-evidence", str(Path(args.maintainer_evidence).resolve())]
    result = subprocess.run(command, cwd=REPO, env=env).returncode
    if result != 0 or args.root is None:
        return result
    # Execute the base validator over candidate bytes; never import candidate Python.
    return subprocess.run([sys.executable, str(REPO / "scripts/proof_enrollment.py"),
                           "--root", str(Path(args.root).resolve())], cwd=REPO, env=env).returncode


if __name__ == "__main__":
    sys.exit(main())
