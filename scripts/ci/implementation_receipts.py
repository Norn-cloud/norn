#!/usr/bin/env python3
"""Base-owned implementation-proof wrappers and their receipt verifier (D43, D47).

`implementation_proved` claims are admitted only through receipts. Kani and
hax/F* run only through this file, which the trusted workflow checks out from
the pull request's base commit. The candidate checkout is data: claims, the
harness, production source and proof files. It never supplies a command; each
wrapper builds its own argv from typed claim fields. A claim's kind is the
wrapper that ran it, never a reading of script text.

`run` writes one receipt per claim on the receipts runner. Kani and hax compile
candidate code there, so that runner is candidate-influenced and every receipt
it writes is `candidate-reported`: consistency of its logs and digests proves
nothing on its own. `verify` runs on a fresh runner that executes no candidate
code and labels each claim by what it re-checks itself (D47):

- extraction: F* re-typechecks the uploaded module beside the declared,
  head-committed proof inputs with a freshly downloaded, digest-pinned F*, after
  an option and tactic allowlist and the proof-hole scan. Label `rechecked`.
  The hax step (Rust to F*) stays candidate-reported; the cargo guard limits it.
- Kani: nothing is re-run, so the claim is `candidate-reported` and refused as
  implementation_proved until Kani runs isolated.
- a pull request that changes no implementation claim and no input of one runs
  no tool: label `unchanged`, the base's state carries over.

The formats are implementation-receipt.schema.json.
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
import tarfile
import tempfile
import tomllib
import urllib.request

HERE = Path(__file__).resolve()
TRUSTED = HERE.parents[2]
sys.path.insert(0, str(TRUSTED / "scripts"))
import coverage_manifest  # noqa: E402

SCHEMA = "norn.implementation-receipt.v1"
JOB = "implementation-receipts"
CANDIDATE_REPORTED, RECHECKED, UNCHANGED = "candidate-reported", "rechecked", "unchanged"
# The F* the verifier downloads itself; bootstrap-rust-extraction-tools.sh pins the same release.
FSTAR_VERSION = "2026.08.16"
FSTAR_URL = (f"https://github.com/FStarLang/FStar/releases/download/v{FSTAR_VERSION}/"
             f"fstar-v{FSTAR_VERSION}-Linux-x86_64.tar.gz")
FSTAR_SHA256 = "e8718fad41c51ef212aa6bfd7418796ab5e32f24e490eb0d7bef9357486fa6ab"
# F* options a re-checked file may set: solver budgets only. F* lets pragmas set
# --load/--load_cmxs (native plugins), --z3cliopt and --admit_*, so everything
# else is refused.
FSTAR_OPTIONS = {"fuel", "ifuel", "initial_fuel", "max_fuel", "initial_ifuel", "max_ifuel",
                 "z3rlimit", "z3rlimit_factor", "z3seed"}
FSTAR_PRAGMA = re.compile(r"#(set|push|reset)-options\b")
FSTAR_PRAGMA_ARGUMENT = re.compile(r'#(?:set|push|reset)-options\s*"([^"\\]*)"')
# Meta-programs run inside F* at typecheck time; re-checked files may not invoke them.
# The FStar.Attributes tactic hooks count as meta-programs too.
FSTAR_META = re.compile(r"%splice|#lang-|\b(?:by|splice|set_options|Tactics|Reflection|plugin|"
                        r"synth_by_tactic|assert_by_tactic|launch_process|handle_smt_goals|"
                        r"resolve_implicits|override_resolve_implicits_handler|ite_soundness_by)\b")
KANI_SUCCESS = "1 successfully verified harnesses, 0 failures, 1 total."
KANI_FAILURE = "0 successfully verified harnesses, 1 failures, 1 total."
HAX_TOOLCHAIN = "nightly-2025-11-08-x86_64-unknown-linux-gnu"
# Library escape hatches prove anything without a local axiom: the unsound
# Prims values of the pinned F* (_assume, admit, magic, unsafe_coerce), admitP,
# and the FStar.Attributes that admit a declaration, its termination, or accept
# its failure (admitted, admit_termination, expect_failure, expect_lax_failure).
FSTAR_HOLE = re.compile(r"\b(?:admit|admitP|assume|_assume|axiom|val|magic|unsafe_coerce|"
                        r"admit_smt_query|admit_smt_queries|admitted|admit_termination|"
                        r"expect_failure|expect_lax_failure)\b")
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
    holes = proof_holes()
    source = path.read_text(encoding="utf-8")
    code = holes.strip_comments_and_strings(source, line_marker="//", block=("(*", "*)"))
    return holes.fstar_admission_option_present(source) or FSTAR_HOLE.search(code) is not None


def mutated(source: str, mutation: dict | None) -> str:
    """Apply one declared production mutation, which must match exactly once."""
    if mutation is None:
        return source
    require(source.count(mutation["before"]) == 1, f"{mutation['name']}: mutation must match exactly once")
    return source.replace(mutation["before"], mutation["after"])


def lock_identities(path: Path) -> list[tuple]:
    """Every Cargo.lock package as (name, version, source, checksum)."""
    return [(p["name"], p["version"], p.get("source"), p.get("checksum"))
            for p in tomllib.loads(path.read_text())["package"]]


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
    locked = set(lock_identities(TRUSTED / "Cargo.lock"))
    candidate = lock_identities(root / "Cargo.lock")
    sources: dict[tuple, set] = {}
    for name, version, source, checksum in candidate:
        sources.setdefault((name, version), set()).add((source, checksum))
    # Cargo identifies a package by name, version and source; one name and
    # version from two sources could let a lookup pick the trusted one.
    require(all(len(found) == 1 for found in sources.values()),
            "Cargo.lock has one name and version from more than one source")
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
            entries = [entry for entry in candidate if entry[:3] == (meta["name"], meta["version"], meta["source"])]
            require(len(entries) == 1 and entries[0] in locked,
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
                          or not all(defines_once(generated, s) for s in symbols))
                if failed:
                    break
                (directory / "outputs").mkdir()
                shutil.copyfile(generated, directory / "outputs" / plan["module"])
                outputs[plan["module"]] = sha256(generated)
        shutil.rmtree(isolated, ignore_errors=True)
        failed |= sha256(root / "Cargo.lock") != inputs["Cargo.lock"]
    used = sorted({entry["tool"] for entry in invocations})
    receipt = {
        "schema": SCHEMA, "trust": CANDIDATE_REPORTED, "kind": kind, "claim": claim["id"],
        "wrapper": os.path.relpath(HERE, os.environ.get("GITHUB_WORKSPACE", os.getcwd())),
        "wrapperSha256": sha256(HERE), "repository": os.environ.get("GITHUB_REPOSITORY", ""),
        "headSha": head, "treeSha": git(root, "rev-parse", "HEAD^{tree}"),
        "runId": os.environ.get("GITHUB_RUN_ID", ""), "runAttempt": os.environ.get("GITHUB_RUN_ATTEMPT", ""),
        "job": os.environ.get("GITHUB_JOB", ""), "tools": {name: tool_identity(name, env) for name in used},
        "inputs": inputs, "invocations": invocations, "outputs": outputs, "exitCode": int(failed),
    }
    (directory / "receipt.json").write_text(json.dumps(receipt, indent=2, sort_keys=True) + "\n")
    return int(failed)


def check(root: Path, claim: dict, directory: Path, expected: dict, fstar: Path | None) -> dict:
    """Refuse `claim` unless its receipt is consistent and what it reports is re-checked here.

    Consistency (wrapper, run, head, digests, logs) only rejects a stale or
    broken receipt: a compromised receipts runner can forge all of it. The
    verdict comes from what this runner re-checks itself.
    """
    receipt = json.loads((directory / "receipt.json").read_text())
    require(isinstance(receipt, dict) and receipt.get("schema") == SCHEMA
            and receipt.get("trust") == CANDIDATE_REPORTED, "not an implementation receipt")
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
        return {"claim": claim["id"], "kind": "kani", "verdict": CANDIDATE_REPORTED, "rechecked": [],
                "candidateReported": ["kani-baseline", "kani-mutations"]}
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
        require(fstar is not None, "no independent F* to re-check the extraction")
        recheck_fstar(root, plan, generated, fstar)
        return {"claim": claim["id"], "kind": "extraction", "verdict": RECHECKED,
                "rechecked": ["fstar-typecheck", "fstar-option-allowlist", "proof-hole-scan"],
                "candidateReported": ["hax-extraction"]}


def fstar_inputs_safe(path: Path) -> None:
    """Refuse an F* file that could make the re-checking F* run code or weaken a proof."""
    source = path.read_text(encoding="utf-8")
    holes = proof_holes()
    raw = holes.strip_comments_and_strings(source, line_marker="//", block=("(*", "*)"), preserve_strings=True)
    code = holes.strip_comments_and_strings(source, line_marker="//", block=("(*", "*)"))
    arguments = FSTAR_PRAGMA_ARGUMENT.findall(raw)
    require(len(arguments) == len(FSTAR_PRAGMA.findall(raw)), f"{path.name}: unparsed F* option pragma")
    for argument in arguments:
        words = argument.split()
        require(len(words) % 2 == 0 and all(
            flag.startswith("--") and flag[2:] in FSTAR_OPTIONS and value.isdigit()
            for flag, value in zip(words[::2], words[1::2])), f"{path.name}: F* option {argument!r} is not allowed")
    require(FSTAR_META.search(code) is None, f"{path.name}: F* meta-programs are not re-checked")
    require(not fstar_holes(path), f"{path.name}: proof hole")


def defines_once(module: Path, symbol: str) -> bool:
    """Whether `module` has exactly one top-level `let symbol`, ignoring comments and strings."""
    code = proof_holes().strip_comments_and_strings(module.read_text(encoding="utf-8"), line_marker="//",
                                                    block=("(*", "*)"))
    return len(re.findall(rf"(?m)^let {re.escape(symbol)} ", code)) == 1


def plain_path(root: Path, path: str) -> Path:
    """`path` under `root`, refusing a symlink anywhere along it: F* must read committed bytes."""
    current = root
    for part in Path(path).parts:
        current = current / part
        require(not current.is_symlink(), f"{path}: symlink in a declared input path")
    return current


def recheck_fstar(root: Path, plan: dict, generated: Path, fstar: Path) -> None:
    """Re-run every F* step of an extraction claim on this runner.

    The inputs are the uploaded module and the declared proof files from the
    head commit, in an isolated directory, so F* resolves nothing undeclared.
    Each declared item must be defined exactly once in the uploaded module;
    whether that definition is hax's faithful extraction stays candidate-reported.
    """
    require(generated.is_file() and not generated.is_symlink() and not generated.parent.is_symlink(),
            "no extracted module was uploaded")
    with tempfile.TemporaryDirectory(prefix="fstar-recheck-") as name:
        work = Path(name) / "work"
        (work / "out").mkdir(parents=True)
        (work / "proofs").mkdir()
        shutil.copyfile(generated, work / "out" / plan["module"])
        for path in plan["inputs"][1:]:
            shutil.copyfile(plain_path(root, path), work / "proofs" / Path(path).name)
        for path in sorted(f for f in work.rglob("*") if f.is_file()):
            fstar_inputs_safe(path)
        for item in plan["items"]:
            require(defines_once(generated, item.rsplit("::", 1)[1]),
                    f"{item} is not defined exactly once in the uploaded module")
        env = {"PATH": f"{fstar.parent}:/usr/bin:/bin", "HOME": name}
        for index, argv in enumerate(plan["argv"][1:]):
            done = invoke([str(fstar), *argv[1:]], work, env, Path(name) / f"{index}.log")
            require(done["exitCode"] == 0, f"independent F* re-check failed: {' '.join(argv[1:])}")


def proof_holes():
    """The trusted proof-hole scanner module."""
    spec = importlib.util.spec_from_file_location("trusted_proof_holes", TRUSTED / "scripts/check-proof-holes.py")
    holes = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(holes)
    return holes


def install_fstar(directory: Path) -> Path:
    """Download the pinned F* release into `directory` and return its fstar.exe."""
    archive = directory / "fstar.tar.gz"
    directory.mkdir(parents=True, exist_ok=True)
    with urllib.request.urlopen(FSTAR_URL, timeout=600) as response, archive.open("wb") as sink:
        shutil.copyfileobj(response, sink)
    require(sha256(archive) == FSTAR_SHA256, "downloaded F* does not match its pinned digest")
    with tarfile.open(archive) as bundle:
        bundle.extractall(directory / "fstar", filter="data")
    found = [f for f in (directory / "fstar").rglob("fstar.exe") if f.is_file() and os.access(f, os.X_OK)]
    require(len(found) == 1, "the F* release does not hold exactly one fstar.exe")
    return found[0]


def git_object(root: Path, path: str) -> str | None:
    """The committed object id of `path` at HEAD, or None when absent."""
    done = subprocess.run(["git", "-C", str(root), "rev-parse", "--verify", "-q", f"HEAD:{path}"],
                          capture_output=True, text=True)
    return done.stdout.strip() if done.returncode == 0 else None


def workspace_closure(root: Path, package: str) -> list[str]:
    """The directories of a workspace package and of every local package it builds with."""
    workspace = tomllib.loads((root / "Cargo.toml").read_text())["workspace"]
    members = {tomllib.loads((root / m / "Cargo.toml").read_text())["package"]["name"]: m
               for m in workspace["members"]}
    shared = workspace.get("dependencies", {})
    require(package in members, f"extraction package {package} is not a workspace member")
    todo, seen = [members[package]], set()
    while todo:
        directory = os.path.normpath(todo.pop())
        if directory in seen:
            continue
        seen.add(directory)
        manifest = tomllib.loads((root / directory / "Cargo.toml").read_text())
        tables = [manifest.get(k, {}) for k in ("dependencies", "build-dependencies")]
        tables += [t.get(k, {}) for t in manifest.get("target", {}).values() for k in ("dependencies", "build-dependencies")]
        for table in tables:
            for key, spec in table.items():
                spec = spec if isinstance(spec, dict) else {}
                name = spec.get("package", key)
                if "path" in spec:
                    todo.append(os.path.join(directory, spec["path"]))
                elif spec.get("workspace") and "path" in shared.get(key, {}):
                    todo.append(shared[key]["path"])
                elif name in members:
                    todo.append(members[name])
    return sorted(seen)


def scope_inputs(root: Path, claims: dict[str, dict]) -> list[str]:
    """Every committed path whose change can change an implementation claim's verdict."""
    paths = {"verification/coverage-v1/manifest", "Cargo.toml", "Cargo.lock", *CARGO_CONFIG}
    for claim in claims.values():
        if kind_of(claim) == "kani":
            plan = kani_plan(claim)
            paths |= {plan["harness"], plan["source"]}
        else:
            plan = extraction_plan(claim, "")
            paths |= {*plan["inputs"], *workspace_closure(root, plan["package"])}
    return sorted(paths)


def in_scope(root: Path, base: Path) -> bool:
    """Whether the candidate changes an implementation claim or any input of one.

    Compared by committed git object ids, so no tool and no candidate code runs.
    """
    claims = implementation_claims(root)
    if not claims:
        return False
    if claims != implementation_claims(base):
        return True
    return any(git_object(root, path) != git_object(base, path) for path in scope_inputs(root, claims))


def verify(root: Path, receipts: Path, expected: dict, fstar: Path | None,
           scoped: bool = True) -> tuple[list[str], list[dict]]:
    """Label every implementation_proved claim; only `rechecked` or `unchanged` ones are admitted.

    Kani claims are refused even when unchanged: no Kani verdict is re-checked
    anywhere, so none may stand as implementation_proved.
    """
    claims = implementation_claims(root)
    present = {p.name for p in receipts.iterdir()} if receipts.is_dir() else set()
    errors = [f"{name}: receipt for no implementation_proved claim" for name in sorted(present - set(claims))]
    verdicts = []
    for name, claim in sorted(claims.items()):
        try:
            if kind_of(claim) == "kani":
                verdicts.append({"claim": name, "kind": "kani", "verdict": CANDIDATE_REPORTED, "rechecked": [],
                                 "candidateReported": ["kani-baseline", "kani-mutations"]})
                if name in present:
                    check(root, claim, receipts / name, expected, fstar)
                raise Refused("Kani runs on the candidate-influenced receipts runner, so its verdict is "
                              "candidate-reported; implementation_proved needs an independent re-check")
            if not scoped:
                verdicts.append({"claim": name, "kind": "extraction", "verdict": UNCHANGED, "rechecked": [],
                                 "candidateReported": []})
                continue
            require(name in present, "no receipt from the base-owned wrapper")
            verdicts.append(check(root, claim, receipts / name, expected, fstar))
        except (Refused, OSError, ValueError, KeyError, TypeError, AttributeError, subprocess.SubprocessError) as error:
            errors.append(f"{name}: {error}")
    return errors, verdicts


def main() -> int:
    """Decide the scope (`scope`), run the wrappers (`run`) or label their receipts (`verify`)."""
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("command", choices=("scope", "run", "verify"))
    parser.add_argument("--root", type=Path, required=True, help="candidate checkout, read as data")
    parser.add_argument("--head", help="the candidate head commit (run, verify)")
    parser.add_argument("--receipts", type=Path, help="receipt directory (run, verify)")
    parser.add_argument("--tools", type=Path, default=Path.home() / ".cache/norn-verification-tools")
    args = parser.parse_args()
    root = args.root.resolve()
    require(not HERE.is_relative_to(root), "the wrapper must not come from the candidate tree")
    scoped = in_scope(root, TRUSTED)
    if args.command == "scope":
        print(f"in_scope={str(scoped).lower()}")
        return 0
    require(args.head is not None and args.receipts is not None, "--head and --receipts are required")
    if args.command == "run":
        if not scoped:
            print("no implementation claim or input changed from the base; no tool runs")
            return 0
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
    extraction = scoped and any(kind_of(c) == "extraction" for c in implementation_claims(root).values())
    with tempfile.TemporaryDirectory(prefix="fstar-verifier-") as fresh:
        fstar = install_fstar(Path(fresh)) if extraction else None
        errors, verdicts = verify(root, args.receipts, expected, fstar, scoped)
    for verdict in verdicts:
        print(json.dumps(verdict, sort_keys=True))
    for error in errors:
        print(f"[reject] {error}", file=sys.stderr)
    print(f"{len(verdicts)} implementation claims, {len(errors)} rejected")
    return int(bool(errors))


if __name__ == "__main__":
    sys.exit(main())
