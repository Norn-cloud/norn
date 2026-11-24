#!/usr/bin/env bash
set -euo pipefail

if [[ $# != 1 || ( $1 != all && $1 != version_check ) ]]; then
  echo "usage: $0 {version_check|all}" >&2
  exit 2
fi
cd "$(dirname "$0")/.."
repo_root="$PWD"
tool_root="$(realpath -m "${NORN_VERIFICATION_TOOL_ROOT:-$repo_root/target/verification-tools}")"
scripts/bootstrap-verification-tools.sh --check-verus
export RUSTUP_HOME="$tool_root/rustup"
export VERUS_ROOT="$tool_root/verus-x86-linux"
export VERUS_Z3_PATH="$VERUS_ROOT/z3"
export VERUS_USE_RUSTUP=1
unset VARGO_TARGET_DIR __VERUS_DRIVER_VIA_CARGO__
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

python3 -I -B -X "pycache_prefix=$work/pycache" - "$repo_root" "$tool_root" "$work" <<'PY'
import hashlib
import json
from pathlib import Path
import runpy
import subprocess
import sys

repo, tools, work = map(Path, sys.argv[1:])
source = repo / "crates/norn-kernel/src/version_check.rs"
patches = repo / "verification/mutants/version_check"
manifest = runpy.run_path(str(repo / "scripts/coverage_manifest.py"))["load"]()
symbols = manifest["inlineVerus"]["version_check"]
crate = "norn_kernel_version"
head = subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip()
subprocess.run(["git", "diff", "--exit-code", head, "--", source, patches,
                "verification/coverage-v1/manifest", "scripts/verify-kernel-inline-verus.sh",
                "scripts/bootstrap-verification-tools.sh"], check=True)


def require(condition, message):
    if not condition:
        raise SystemExit(message)


def verify(path, mutant=None):
    # A fresh process per case, with only the pinned solver and library bundle.
    result = subprocess.run([
        tools / "verus-x86-linux/verus", "--crate-name", crate, "--crate-type=lib",
        "--edition=2024", "--no-cheating", "--no-external-by-default", "--num-threads", "1",
        "-Zcrate-attr=verus::untrusted", "--time", "--output-json", path,
    ], cwd=work, stdout=subprocess.PIPE, text=True, timeout=120)
    data = json.loads(result.stdout)
    summary = data["verification-results"]
    require(result.returncode == int(mutant is not None)
            and summary["is-verifying-entire-crate"] is True
            and summary["encountered-vir-error"] is False
            and summary["success"] is (mutant is None)
            and summary["errors"] == int(mutant is not None)
            and summary["verified"] > 0, f"{path}: incomplete verification: {summary}")
    functions = {}
    for module in data["times-ms"]["smt"]["smt-run-module-times"]:
        for row in module["function-breakdown"]:
            if row["mode:"] == "exec":
                functions[row["function"]] = row["success"]
    expected = {f"{crate}::{name}": name != mutant for name in symbols}
    require(functions == expected, f"unexpected executable verification results: {functions}")
    for name, contract in symbols.items():
        notes = data["func-details"][f"{crate}::{name}"]
        label = contract["postcondition"]
        require(notes["obligation_proof_notes"] == [label]
                and notes["failed_proof_notes"] == ([label] if name == mutant else []),
                f"{name}: incorrect postcondition result: {notes}")


require(symbols and {p.stem for p in patches.glob("*.patch")}
        == {item["mutant"] for item in symbols.values()}, "incomplete mutant inventory")
verify(source)
records = []
original = source.read_text()
for name, contract in symbols.items():
    patch = patches / (contract["mutant"] + ".patch")
    case = work / patch.stem
    case.mkdir()
    copy = case / source.name
    copy.write_text(original)
    subprocess.run(["patch", "--batch", "--forward", "--fuzz=0", copy, patch], cwd=case, check=True)
    # G0's two Rust bodies are single expressions. Require a change inside
    # precisely this body, keeping the specification and all other items intact.
    before, body = original.split(f"pub(crate) const fn {name}(", 1)
    signature, body = body.split("{\n", 1)
    _, after = body.split("\n}", 1)
    prefix = before + f"pub(crate) const fn {name}(" + signature + "{\n"
    mutated = copy.read_text()
    require(mutated != original and mutated.startswith(prefix) and mutated.endswith("\n}" + after),
            f"{patch}: changes outside {name}")
    verify(copy, name)
    spec = original.split("verus! {", 1)[1].split("\n}", 1)[0]
    attribute = before.rsplit("#[cfg_attr(", 1)[1]
    records.append({
        "head": head, "symbol": f"norn_kernel::version_check::{name}", "verifier": "verus",
        "toolDigest": (tools / "verus-install.sha256").read_text().split()[0],
        "sourceDigest": hashlib.sha256(source.read_bytes()).hexdigest(),
        "statementDigest": hashlib.sha256((spec + attribute).encode()).hexdigest(),
        "secondLens": contract["secondLens"],
        "mutants": [{"id": patch.stem, "killedAt": contract["postcondition"]}],
    })
print(json.dumps(records, indent=2))
PY
