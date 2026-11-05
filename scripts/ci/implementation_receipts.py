#!/usr/bin/env python3
"""Base-owned implementation-proof wrappers and their receipt verifier (D43).

`implementation_proved` claims are admitted only from receipts. Kani and
hax/F* run only through this file, which the trusted workflow checks out from
the pull request's base commit. The candidate checkout is data: claims, the
harness, production source and proof files. It never supplies a command; each
wrapper builds its own argv from typed claim fields. A claim's kind is the
wrapper that ran it, never a reading of script text.

`run` writes one receipt per claim: every tool process with its tool identity,
argv, exit code and log digest, plus the input and output digests, all bound to
the head commit and the workflow run and job. `verify` runs in a separate job
that executes no candidate code. It admits only receipts from this same file,
for this head, run, attempt and job, and re-derives every verdict from the
uploaded logs and outputs. The format is implementation-receipt.schema.json.
"""

from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import tomllib

HERE = Path(__file__).resolve()
TRUSTED = HERE.parents[2]
sys.path.insert(0, str(TRUSTED / "scripts"))
import coverage_manifest  # noqa: E402

SCHEMA = "norn.implementation-receipt.v1"
JOB = "implementation-receipts"
KANI_SUCCESS = "1 successfully verified harnesses, 0 failures, 1 total."
KANI_FAILURE = "0 successfully verified harnesses, 1 failures, 1 total."
HAX_TOOLCHAIN = "nightly-2025-11-08-x86_64-unknown-linux-gnu"
FSTAR_HOLE = re.compile(r"\b(?:admit|assume|axiom|val|admit_smt_query|admit_smt_queries)\b")
EXTRACTION_FIELDS = {"package", "items", "module", "proofRoot", "models", "proofs"}
# Candidate files that could make cargo execute candidate-chosen programs.
CARGO_CONFIG = (".cargo/config", ".cargo/config.toml", "rust-toolchain", "rust-toolchain.toml")


class Refused(Exception):
    """The candidate data cannot produce or keep a receipt."""


def require(condition: bool, message: str) -> None:
    """Raise Refused with `message` unless `condition` holds."""
    if not condition:
        raise Refused(message)


def sha256(path: Path) -> str:
    """Hex SHA-256 of a file's bytes."""
    return hashlib.sha256(path.read_bytes()).hexdigest()


def git(root: Path, *arguments: str) -> str:
    """Run git in `root` and return its stripped stdout."""
    return subprocess.run(["git", "-C", str(root), *arguments], check=True,
                          capture_output=True, text=True).stdout.strip()


def relative(value: object) -> str:
    """A plain repository-relative path, never absolute or escaping."""
    require(isinstance(value, str) and re.fullmatch(r"[A-Za-z0-9_][A-Za-z0-9_./-]*", value) is not None
            and ".." not in Path(value).parts, f"unsafe repository path {value!r}")
    return value


def implementation_claims(root: Path) -> dict[str, dict]:
    """Every implementation_proved claim in the checkout's coverage manifest, by id."""
    document = coverage_manifest.load(root / "verification/coverage-v1/manifest")
    claims = [c for s in document["subsystems"] for c in s["obligationClaims"]
              if c.get("proof") == "implementation_proved"]
    for claim in claims:
        require(re.fullmatch(r"[A-Z0-9][A-Z0-9-]*", str(claim.get("id"))) is not None, "bad claim id")
    return {claim["id"]: claim for claim in claims}


def kind_of(claim: dict) -> str:
    """The wrapper a claim's typed fields select: extraction iff it has an extractionBinding."""
    return "extraction" if "extractionBinding" in claim else "kani"


def selector_kinds(claim: dict) -> set[str]:
    """The selector kinds of a claim's proof evidence."""
    return {s.split(":", 1)[0] for e in claim["evidence"]["proof"] for s in e["selectors"]}


def kani_plan(claim: dict) -> dict:
    """The one harness, production source and mutations a Kani claim declares."""
    require(selector_kinds(claim) == {"harness", "predicate"}, "a Kani claim needs harness and predicate evidence only")
    harnesses = [(e["artifact"], s.split(":", 1)[1]) for e in claim["evidence"]["proof"]
                 for s in e["selectors"] if s.startswith("harness:")]
    require(len(harnesses) == 1, "a Kani claim needs exactly one harness")
    mutations = claim.get("correspondenceMutations")
    require(isinstance(mutations, list) and mutations and len({m.get("name") for m in mutations}) == len(mutations)
            and all(isinstance(m.get("before"), str) and isinstance(m.get("after"), str)
                    and m["before"] != m["after"] for m in mutations),
            "a Kani claim needs nonempty unique production mutations")
    harness, name = harnesses[0]
    require(re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", name) is not None, "bad harness name")
    return {"harness": relative(harness), "name": name, "source": relative(claim.get("correspondenceSource")),
            "mutations": mutations, "argv": ["kani", "--fail-fast", "--output-format", "terse",
                                             "--harness", name, harness]}


def extraction_plan(claim: dict, output: str) -> dict:
    """The argv sequence an extraction claim's typed binding selects."""
    require(selector_kinds(claim) == {"theorem", "predicate"}, "an extraction claim needs theorem and predicate evidence only")
    require(not any(k.startswith("correspondence") for k in claim), "an extraction claim cannot carry Kani fields")
    binding = claim["extractionBinding"]
    require(isinstance(binding, dict) and set(binding) == EXTRACTION_FIELDS, "extractionBinding fields are exact")
    item = r"[A-Za-z_][A-Za-z0-9_]*(?:::[A-Za-z_][A-Za-z0-9_]*)+"
    fst = r"[A-Za-z][A-Za-z0-9_.]*\.fst"
    require(re.fullmatch(r"[a-z0-9][a-z0-9_-]*", str(binding["package"])) is not None
            and isinstance(binding["items"], list) and binding["items"]
            and all(re.fullmatch(item, str(i)) for i in binding["items"])
            and re.fullmatch(r"[A-Z][A-Za-z0-9_]*\.fst", str(binding["module"])) is not None
            and all(isinstance(binding[k], list) and all(re.fullmatch(fst, str(f)) for f in binding[k])
                    for k in ("models", "proofs")) and binding["proofs"], "invalid extractionBinding")
    root = relative(binding["proofRoot"])
    include = " ".join(["-**", *(f"+{i}" for i in binding["items"])])
    argv = [["cargo-hax", "--no-custom-target-directory", "-C", "--locked", "-p", binding["package"],
             ";", "into", "-i", include, "--output-dir", output, "fstar"]]
    # F* runs in an isolated directory holding only the declared files and the
    # generated module, so it cannot resolve an undeclared candidate module.
    argv += [["fstar.exe", "--include", "proofs", f"proofs/{model}"] for model in binding["models"]]
    argv += [["fstar.exe", "--include", "proofs", "--include", "out", f"out/{binding['module']}"]]
    argv += [["fstar.exe", "--include", "proofs", "--include", "out", f"proofs/{p}"] for p in binding["proofs"]]
    inputs = ["Cargo.lock", *(f"{root}/{f}" for f in (*binding["models"], *binding["proofs"]))]
    return {"argv": argv, "inputs": inputs, **binding}


def kani_verdict(log: str, name: str) -> bool | None:
    """True proved, False refuted, None for any other inventory of `name`."""
    text = re.sub(r"\x1b\[[0-?]*[ -/]*[@-~]", "", log)
    checked = re.findall(r"^Checking harness (.+)\.\.\.$", text, re.MULTILINE)
    verdicts = re.findall(r"^VERIFICATION:- (.+)$", text, re.MULTILINE)
    summaries = re.findall(r"^Complete - (.+)$", text, re.MULTILINE)
    if checked != [name]:
        return None
    if verdicts == ["SUCCESSFUL"] and summaries == [KANI_SUCCESS]:
        return True
    return False if verdicts == ["FAILED"] and summaries == [KANI_FAILURE] else None


def fstar_holes(path: Path) -> bool:
    """The trusted proof-hole scanner, applied to one candidate F* file."""
    spec = importlib.util.spec_from_file_location("trusted_proof_holes", TRUSTED / "scripts/check-proof-holes.py")
    holes = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(holes)
    source = path.read_text(encoding="utf-8")
    code = holes.strip_comments_and_strings(source, line_marker="//", block=("(*", "*)"))
    return holes.fstar_admission_option_present(source) or FSTAR_HOLE.search(code) is not None


def mutated(source: str, mutation: dict | None) -> str:
    """Apply one declared production mutation, which must match exactly once."""
    if mutation is None:
        return source
    require(source.count(mutation["before"]) == 1, f"{mutation['name']}: mutation must match exactly once")
    return source.replace(mutation["before"], mutation["after"])


def guard_cargo(root: Path, package: str) -> None:
    """Refuse extraction when cargo could run candidate-controlled code.

    Workspace and path packages in the package's build closure may not have
    build scripts or proc macros, registry ones that do must be locked exactly
    as on the base, and cargo configuration must equal the base's.
    """
    for name in CARGO_CONFIG:
        base, head = TRUSTED / name, root / name
        require(base.is_file() == head.is_file() and (not base.is_file() or sha256(base) == sha256(head)),
                f"{name} differs from the base")
    metadata = json.loads(subprocess.run(
        ["cargo", "metadata", "--locked", "--format-version", "1", "--manifest-path", str(root / "Cargo.toml")],
        check=True, capture_output=True, text=True).stdout)
    packages = {p["id"]: p for p in metadata["packages"]}
    nodes = {n["id"]: n for n in metadata["resolve"]["nodes"]}
    start = [i for i, p in packages.items() if p["name"] == package and p["source"] is None]
    require(len(start) == 1, f"extraction package {package} is not one workspace package")
    locked = {(p["name"], p["version"], p.get("source"), p.get("checksum"))
              for p in tomllib.loads((TRUSTED / "Cargo.lock").read_text())["package"]}
    candidate = {(p["name"], p["version"]): (p.get("source"), p.get("checksum"))
                 for p in tomllib.loads((root / "Cargo.lock").read_text())["package"]}
    seen, todo = set(), list(start)
    while todo:
        current = todo.pop()
        if current in seen:
            continue
        seen.add(current)
        todo += [d["pkg"] for d in nodes[current]["deps"]
                 if any(k["kind"] != "dev" for k in d["dep_kinds"])]
        meta = packages[current]
        if any(k in ("custom-build", "proc-macro") for t in meta["targets"] for k in t["kind"]):
            require(meta["source"] is not None, f"{meta['name']}: local build script or proc macro")
            require((meta["name"], meta["version"], *candidate[(meta["name"], meta["version"])]) in locked,
                    f"{meta['name']} {meta['version']}: build-time code not locked on the base")


def tool_identity(name: str, env: dict) -> dict:
    """The resolved path, binary SHA-256 and version line of one tool."""
    path = shutil.which(name, path=env["PATH"])
    require(path is not None, f"{name} is not installed")
    version = subprocess.run([path, "--version"], env=env, capture_output=True, text=True, timeout=120)
    return {"path": path, "sha256": sha256(Path(path).resolve()),
            # Some tools (hax) print the name and the version on separate lines.
            "version": " ".join((version.stdout or version.stderr).split()[:8])}


def invoke(argv: list[str], cwd: Path, env: dict, log: Path) -> dict:
    """Run one tool process, keep its output as `log`, and describe the invocation."""
    try:
        done = subprocess.run(argv, cwd=cwd, env=env, capture_output=True, text=True, timeout=5400)
        text, code = done.stdout + done.stderr, done.returncode
    except subprocess.TimeoutExpired:
        text, code = "timeout\n", 124
    log.write_text(text)
    return {"tool": argv[0], "argv": argv, "exitCode": code, "log": log.name, "logSha256": sha256(log)}


def run(root: Path, claim: dict, head: str, out: Path, tools: Path) -> int:
    """Run one claim's wrapper and write its receipt; return its exit code."""
    require(git(root, "rev-parse", "HEAD") == head and not git(root, "status", "--porcelain"),
            "the candidate checkout must be the clean head commit")
    kind = kind_of(claim)
    kani_plan(claim) if kind == "kani" else extraction_plan(claim, "")
    directory = out / claim["id"]
    (directory / "logs").mkdir(parents=True)
    env = {**os.environ, "PATH": f"{tools}/bin:{os.environ['PATH']}", "RUSTUP_HOME": str(tools / "rustup"),
           "KANI_HOME": str(tools / "kani-home"), "RUSTFLAGS": "--edition=2024"}
    invocations, outputs, failed = [], {}, False
    if kind == "kani":
        plan = kani_plan(claim)
        inputs = {path: sha256(root / path) for path in (plan["harness"], plan["source"])}
        work = Path(tempfile.mkdtemp(prefix="kani-receipt-"))
        original = (root / plan["source"]).read_text()
        for index, mutation in enumerate([None, *plan["mutations"]]):
            shutil.rmtree(work, ignore_errors=True)
            for path in (plan["harness"], plan["source"]):
                (work / path).parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(root / path, work / path)
            (work / plan["source"]).write_text(mutated(original, mutation))
            entry = invoke(plan["argv"], work, env, directory / "logs" / f"{index}.log")
            entry |= {"mutation": mutation and mutation["name"], "sourceSha256": sha256(work / plan["source"])}
            verdict = kani_verdict((directory / "logs" / entry["log"]).read_text(), plan["name"])
            failed |= verdict is not (mutation is None) or (entry["exitCode"] == 0) != (mutation is None)
            invocations.append(entry)
        shutil.rmtree(work, ignore_errors=True)
    else:
        output = f"target/implementation-receipts/{claim['id']}"
        plan = extraction_plan(claim, output)
        guard_cargo(root, plan["package"])
        inputs = {path: sha256(root / path) for path in plan["inputs"]}
        env |= {"CARGO_HOME": str(tools / "cargo"), "RUSTUP_TOOLCHAIN": HAX_TOOLCHAIN,
                "PATH": f"{tools}/bin:{tools}/cargo/bin:{os.environ['PATH']}",
                "CARGO_TARGET_DIR": str(root / "target/hax")}
        env.pop("RUSTFLAGS")
        isolated = Path(tempfile.mkdtemp(prefix="fstar-receipt-"))
        for index, argv in enumerate(plan["argv"]):
            invocations.append(invoke(argv, root if index == 0 else isolated, env, directory / "logs" / f"{index}.log"))
            if invocations[-1]["exitCode"] != 0:
                failed = True
                break
            if index == 0:
                generated = root / output / plan["module"]
                symbols = [i.rsplit("::", 1)[1] for i in plan["items"]]
                if generated.is_file():
                    (isolated / "out").mkdir()
                    (isolated / "proofs").mkdir()
                    shutil.copyfile(generated, isolated / "out" / plan["module"])
                    for path in plan["inputs"][1:]:
                        shutil.copyfile(root / path, isolated / "proofs" / Path(path).name)
                # Every file F* can resolve beyond its own library is scanned.
                failed = (not generated.is_file() or any(fstar_holes(f) for f in isolated.rglob("*") if f.is_file())
                          or any(len(re.findall(rf"(?m)^let {s} ", generated.read_text())) != 1 for s in symbols))
                if failed:
                    break
                (directory / "outputs").mkdir()
                shutil.copyfile(generated, directory / "outputs" / plan["module"])
                outputs[plan["module"]] = sha256(generated)
        shutil.rmtree(isolated, ignore_errors=True)
        failed |= sha256(root / "Cargo.lock") != inputs["Cargo.lock"]
    used = sorted({entry["tool"] for entry in invocations})
    receipt = {
        "schema": SCHEMA, "kind": kind, "claim": claim["id"],
        "wrapper": os.path.relpath(HERE, os.environ.get("GITHUB_WORKSPACE", os.getcwd())),
        "wrapperSha256": sha256(HERE), "repository": os.environ.get("GITHUB_REPOSITORY", ""),
        "headSha": head, "treeSha": git(root, "rev-parse", "HEAD^{tree}"),
        "runId": os.environ.get("GITHUB_RUN_ID", ""), "runAttempt": os.environ.get("GITHUB_RUN_ATTEMPT", ""),
        "job": os.environ.get("GITHUB_JOB", ""), "tools": {name: tool_identity(name, env) for name in used},
        "inputs": inputs, "invocations": invocations, "outputs": outputs, "exitCode": int(failed),
    }
    (directory / "receipt.json").write_text(json.dumps(receipt, indent=2, sort_keys=True) + "\n")
    return int(failed)


def check(root: Path, claim: dict, directory: Path, expected: dict) -> None:
    """Refuse `claim` unless its receipt is this wrapper's, for this run, and proves it."""
    receipt = json.loads((directory / "receipt.json").read_text())
    require(isinstance(receipt, dict) and receipt.get("schema") == SCHEMA, "not an implementation receipt")
    workspace = Path(os.environ.get("GITHUB_WORKSPACE", os.getcwd()))
    wrapper = (workspace / str(receipt.get("wrapper"))).resolve()
    require(not wrapper.is_relative_to(root.resolve()), "receipt names a wrapper from the candidate tree")
    require(wrapper == HERE and receipt.get("wrapperSha256") == sha256(HERE), "receipt is not from the base wrapper")
    for key, value in expected.items():
        require(receipt.get(key) == value, f"receipt {key} is {receipt.get(key)!r}, expected {value!r}")
    require(git(root, "rev-parse", "HEAD") == expected["headSha"]
            and receipt.get("treeSha") == git(root, "rev-parse", "HEAD^{tree}"), "receipt is for another tree")
    require(receipt.get("exitCode") == 0, "receipt records a failed wrapper run")
    require(receipt.get("kind") == kind_of(claim), "receipt kind does not match the claim's wrapper")
    invocations = receipt.get("invocations")
    require(isinstance(invocations, list), "receipt has no invocations")
    logs = []
    for entry in invocations:
        log = directory / "logs" / str(entry.get("log"))
        require(log.parent == (directory / "logs") and log.is_file() and sha256(log) == entry.get("logSha256"),
                "invocation log digest mismatch")
        logs.append(log.read_text())
    tools = receipt.get("tools")
    require(isinstance(tools, dict) and set(tools) == {e.get("tool") for e in invocations} and all(
        isinstance(t, dict) and re.fullmatch(r"[0-9a-f]{64}", str(t.get("sha256"))) and t.get("version")
        for t in tools.values()), "receipt tool identities are incomplete")
    if receipt["kind"] == "kani":
        plan = kani_plan(claim)
        sources = (root / plan["source"]).read_text()
        steps = [None, *plan["mutations"]]
        require(receipt.get("inputs") == {p: sha256(root / p) for p in (plan["harness"], plan["source"])},
                "receipt input digests do not match the candidate")
        require(len(invocations) == len(steps) and receipt.get("outputs") == {}, "receipt invocations are incomplete")
        for entry, mutation, log in zip(invocations, steps, logs):
            require(entry.get("argv") == plan["argv"] and entry.get("mutation") == (mutation and mutation["name"])
                    and entry.get("sourceSha256") == hashlib.sha256(mutated(sources, mutation).encode()).hexdigest(),
                    "receipt invocation does not match the claim")
            proved = mutation is None
            require(kani_verdict(log, plan["name"]) is proved and (entry.get("exitCode") == 0) == proved,
                    f"{entry.get('mutation') or 'baseline'}: Kani verdict does not prove the claim")
    else:
        output = f"target/implementation-receipts/{claim['id']}"
        plan = extraction_plan(claim, output)
        require(receipt.get("inputs") == {p: sha256(root / p) for p in plan["inputs"]},
                "receipt input digests do not match the candidate")
        require([e.get("argv") for e in invocations] == plan["argv"]
                and all(e.get("exitCode") == 0 for e in invocations), "extraction invocations do not prove the claim")
        generated = directory / "outputs" / plan["module"]
        require(receipt.get("outputs") == {plan["module"]: sha256(generated) if generated.is_file() else None},
                "receipt output digest mismatch")
        require(not fstar_holes(generated), "extracted module contains a proof hole")


def verify(root: Path, receipts: Path, expected: dict) -> list[str]:
    """Every implementation_proved claim needs exactly one admissible receipt."""
    claims = implementation_claims(root)
    present = {p.name for p in receipts.iterdir()} if receipts.is_dir() else set()
    errors = [f"{name}: receipt for no implementation_proved claim" for name in sorted(present - set(claims))]
    for name, claim in sorted(claims.items()):
        try:
            require(name in present, "no receipt from the base-owned wrapper")
            check(root, claim, receipts / name, expected)
        except (Refused, OSError, ValueError, KeyError, TypeError, AttributeError) as error:
            errors.append(f"{name}: {error}")
    return errors


def main() -> int:
    """Run the wrappers (`run`) or admit their receipts (`verify`)."""
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("command", choices=("run", "verify"))
    parser.add_argument("--root", type=Path, required=True, help="candidate checkout, read as data")
    parser.add_argument("--head", required=True)
    parser.add_argument("--receipts", type=Path, required=True)
    parser.add_argument("--tools", type=Path, default=Path.home() / ".cache/norn-verification-tools")
    args = parser.parse_args()
    root = args.root.resolve()
    require(not HERE.is_relative_to(root), "the wrapper must not come from the candidate tree")
    if args.command == "run":
        claims = implementation_claims(root)
        environment = {**os.environ, "NORN_VERIFICATION_TOOL_ROOT": str(args.tools)}
        for script, kind in (("bootstrap-verification-tools.sh", "kani"),
                             ("bootstrap-rust-extraction-tools.sh", "extraction")):
            if any(kind_of(c) == kind for c in claims.values()):
                subprocess.run(["bash", str(TRUSTED / "scripts" / script)], check=True, env=environment)
        status = 0
        for claim in claims.values():
            try:
                status |= run(root, claim, args.head, args.receipts, args.tools)
            except Refused as error:
                print(f"{claim['id']}: refused: {error}", file=sys.stderr)
                status = 1
        return status
    expected = {"headSha": args.head, "repository": os.environ.get("GITHUB_REPOSITORY", ""),
                "runId": os.environ.get("GITHUB_RUN_ID", ""), "runAttempt": os.environ.get("GITHUB_RUN_ATTEMPT", ""),
                "job": JOB}
    errors = verify(root, args.receipts, expected)
    for error in errors:
        print(f"[reject] {error}", file=sys.stderr)
    print(f"{len(implementation_claims(root))} implementation claims, {len(errors)} rejected")
    return int(bool(errors))


if __name__ == "__main__":
    sys.exit(main())
