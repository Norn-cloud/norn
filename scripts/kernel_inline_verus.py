"""Check the inline Verus contracts (version_check & artifact_governance) and verifier verdicts."""
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys

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

crate = "norn_kernel_version"
ARTIFACT_CRATE = "norn_ir_artifact_governance"
ARTIFACT_LEMMAS = frozenset({
    "active_authority_bearing_rejected",
    "approved_authority_bearing_rejected",
    "source_schema_always_directly_writable",
    "non_authoritative_lifecycle_directly_writable",
    "direct_artifact_write_truth_table",
})


def require(condition, message):
    if not condition:
        raise SystemExit(message)


# G0 checks two one-expression Rust functions. Pin every byte outside those
# expressions, including signatures, cfg attributes, ghost helpers and requires.
BODY = re.compile(
    r"^pub\(crate\) const fn (?P<name>\w+)\([^\n]*\) -> [^\n{]+ \{\n"
    r"    (?P<expression>[^\n{}]+)\n\}$", re.MULTILINE,
)

# artifact_governance checks direct_artifact_write_allowed. Pin every byte outside
# its executable body: all 17 ArtifactKind variants, all 7 ArtifactLifecycle variants,
# spec predicates, proof lemmas, cfg attributes and verus_spec postconditions.
ARTIFACT_BODY = re.compile(
    r"^pub const fn (?P<name>\w+)\([^)]*\) -> [^\n{]+ \{\n"
    r"(?P<body>.*?)\n\}$", re.MULTILINE | re.DOTALL,
)


def statement_digests(source, names):
    matches = list(BODY.finditer(source))
    require(sorted(m["name"] for m in matches) == sorted(names), "unexpected version functions")
    for match in reversed(matches):
        start, end = match.span("expression")
        source = source[:start] + "<exec expression>" + source[end:]
    return {name: hashlib.sha256((name + "\0" + source).encode()).hexdigest() for name in names}


def approved_statements(source, symbols):
    digests = statement_digests(source, symbols)
    for name, digest in digests.items():
        require(digest == symbols[name]["statementDigest"], f"{name}: approved statement changed")
    return digests


def artifact_statement_digests(source, names):
    matches = list(ARTIFACT_BODY.finditer(source))
    require(sorted(m["name"] for m in matches) == sorted(names), "unexpected artifact_governance functions")
    for match in reversed(matches):
        start, end = match.span("body")
        source = source[:start] + "<exec body>" + source[end:]
    return {name: hashlib.sha256((name + "\0" + source).encode()).hexdigest() for name in names}


def approved_artifact_statements(source, symbols):
    digests = artifact_statement_digests(source, symbols)
    for name, digest in digests.items():
        require(digest == symbols[name]["statementDigest"], f"{name}: approved statement changed")
    return digests


def check_verdict(data, returncode, symbols, mutant=None):
    summary = data["verification-results"]
    require(returncode == int(mutant is not None)
            and summary["is-verifying-entire-crate"] is True
            and summary["encountered-vir-error"] is False
            and summary["success"] is (mutant is None)
            and summary["errors"] == int(mutant is not None)
            and summary["verified"] > 0, f"incomplete verification: {summary}")
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


def check_artifact_verdict(data, returncode, symbols, mutant=None):
    summary = data["verification-results"]
    require(returncode == int(mutant is not None)
            and summary["is-verifying-entire-crate"] is True
            and summary["encountered-vir-error"] is False
            and summary["success"] is (mutant is None)
            and summary["errors"] == int(mutant is not None)
            and summary["verified"] > 0, f"incomplete verification: {summary}")
    results = {}
    for module in data["times-ms"]["smt"]["smt-run-module-times"]:
        for row in module["function-breakdown"]:
            results.setdefault(row["mode:"], {})[row["function"]] = row["success"]
    expected = {f"{ARTIFACT_CRATE}::{name}": name != mutant for name in symbols}
    require(results.get("exec", {}) == expected,
            f"unexpected executable verification results: {results.get('exec', {})}")
    proofs = results.get("proof", {})
    passing_lemmas = {fn.removeprefix(ARTIFACT_CRATE + "::") for fn, ok in proofs.items() if ok}
    require(ARTIFACT_LEMMAS <= passing_lemmas, f"missing or failed lemmas: {proofs}")
    for name, contract in symbols.items():
        notes = data["func-details"][f"{ARTIFACT_CRATE}::{name}"]
        label = contract["postcondition"]
        require(notes["obligation_proof_notes"] == [label]
                and notes["failed_proof_notes"] == ([label] if name == mutant else []),
                f"{name}: incorrect postcondition result: {notes}")


def _verus_env(tools):
    env = os.environ.copy()
    for override in ("VARGO_TARGET_DIR", "__VERUS_DRIVER_VIA_CARGO__"):
        env.pop(override, None)
    bundle = tools / "verus-x86-linux"
    env.update(RUSTUP_HOME=str(tools / "rustup"), VERUS_ROOT=str(bundle),
               VERUS_Z3_PATH=str(bundle / "z3"), VERUS_USE_RUSTUP="1")
    return env


def verify(path, tools, work, symbols, mutant=None):
    # A fresh process per case, with only the pinned solver and library bundle.
    logs = [] if mutant else ["--log", "vir", "--log", "vir-option=no_span+no_fn_details",
                              "--log-dir", str(work / "vir")]
    result = subprocess.run([
        tools / "verus-x86-linux/verus", "--crate-name", crate, "--crate-type=lib",
        "--edition=2024", "--no-cheating", "--no-external-by-default", "--num-threads", "1",
        "-Zcrate-attr=verus::untrusted", "--time", "--output-json", *logs, path,
    ], cwd=work, env=_verus_env(tools), stdout=subprocess.PIPE, text=True, timeout=120)
    check_verdict(json.loads(result.stdout), result.returncode, symbols, mutant)


def verify_artifact(path, tools, work, symbols, mutant=None):
    logs = [] if mutant else ["--log", "vir", "--log", "vir-option=no_fn_details",
                              "--log-dir", str(work / "artifact_vir")]
    result = subprocess.run([
        tools / "verus-x86-linux/verus", "--crate-name", ARTIFACT_CRATE, "--crate-type=lib",
        "--edition=2024", "--no-cheating", "--no-external-by-default", "--num-threads", "1",
        "-Zcrate-attr=verus::untrusted", "--time", "--output-json", *logs, path,
    ], cwd=work, env=_verus_env(tools), stdout=subprocess.PIPE, text=True, timeout=120)
    try:
        data = json.loads(result.stdout)
    except ValueError:
        raise SystemExit(
            f"verus produced no verdict (exit {result.returncode}): {result.stdout[-2000:]}"
        ) from None
    check_artifact_verdict(data, result.returncode, symbols, mutant)


def compare_production_ir(repo, work, symbols):
    # Use Cargo's production package, features, and normal rustc configuration.
    # Only the product crate needs access to the pinned compiler's THIR printer.
    thir = work / "kernel.thir"
    # A run-specific diagnostic directory forces Cargo to run the printer even
    # on warm caches, without changing cfgs, codegen options or source paths.
    with thir.open("w") as dump:
        subprocess.run(["cargo", "+1.100.0", "rustc", "--locked", "--release", "-p", "norn-kernel",
                        "--lib", "--", "-Zunpretty=thir-flat", f"-Zdump-mir-dir={work / 'mir'}"],
                       cwd=repo, env={**os.environ, "RUSTC_BOOTSTRAP": "norn_kernel"},
                       stdout=dump, check=True)
    with thir.open() as lines:
        return _sibling("kernel_verus_ir").compare((work / "vir/crate.vir").read_text(), lines, symbols)


def compare_artifact_production_ir(repo, work, symbols):
    thir = work / "norn_ir.thir"
    with thir.open("w") as dump:
        subprocess.run(["cargo", "+1.100.0", "rustc", "--locked", "--release", "-p", "norn-ir",
                        "--lib", "--", "-Zunpretty=thir-tree", f"-Zdump-mir-dir={work / 'artifact_mir'}"],
                       cwd=repo, env={**os.environ, "RUSTC_BOOTSTRAP": "norn_ir"},
                       stdout=dump, check=True)
    return _sibling("kernel_verus_ir").compare_artifact_governance(
        (work / "artifact_vir/crate.vir").read_text(), thir.read_text(), symbols,
    )


def run_version_check(repo, tools, work, head, manifest):
    source = repo / "crates/norn-kernel/src/version_check.rs"
    patches = repo / "verification/mutants/version_check"
    symbols = manifest["inlineVerus"]["version_check"]
    subprocess.run(["git", "diff", "--exit-code", head, "--", source, patches,
                    "verification/coverage-v1/manifest", "scripts/verify-kernel-inline-verus.sh",
                    "scripts/kernel_inline_verus.py", "scripts/kernel_verus_ir.py",
                    "scripts/bootstrap-verification-tools.sh"], check=True)
    require(symbols and {p.stem for p in patches.glob("*.patch")}
            == {item["mutant"] for item in symbols.values()}, "incomplete mutant inventory")
    original = source.read_text()
    require("#![deny(clippy::default_numeric_fallback)]" in original.splitlines(),
            "missing module-level default_numeric_fallback denial")
    digests = approved_statements(original, symbols)
    verify(source, tools, work, symbols)
    compared = compare_production_ir(repo, work, symbols)
    records = []
    for name, contract in symbols.items():
        patch = patches / (contract["mutant"] + ".patch")
        case = work / ("version_" + patch.stem)
        case.mkdir()
        copy = case / source.name
        copy.write_text(original)
        subprocess.run(["patch", "--batch", "--forward", "--fuzz=0", copy, patch], cwd=case, check=True)
        # G0's two Rust bodies are single expressions. Require a change inside
        # precisely this body, keeping the specification and all other items intact.
        body = next(m for m in BODY.finditer(original) if m["name"] == name)
        start, end = body.span("expression")
        mutated = copy.read_text()
        require(mutated != original and mutated.startswith(original[:start])
                and mutated.endswith(original[end:]), f"{patch}: changes outside {name}")
        verify(copy, tools, work, symbols, name)
        records.append({
            "head": head, "symbol": f"norn_kernel::version_check::{name}", "verifier": "verus",
            "toolDigest": (tools / "verus-install.sha256").read_text().split()[0],
            "sourceDigest": hashlib.sha256(source.read_bytes()).hexdigest(),
            "statementDigest": digests[name],
            "virThirComparison": {"equal": True, **compared[name]},
            "secondLensGate": contract["secondLensGate"],
            "mutants": [{"id": patch.stem, "killedAt": contract["postcondition"]}],
        })
    return records


def run_artifact_governance(repo, tools, work, head, manifest):
    source = repo / "crates/norn-ir/src/artifact_governance.rs"
    patches = repo / "verification/mutants/artifact_governance"
    symbols = manifest["inlineVerus"]["artifact_governance"]
    subprocess.run(["git", "diff", "--exit-code", head, "--", source, patches,
                    "verification/coverage-v1/manifest", "scripts/verify-kernel-inline-verus.sh",
                    "scripts/kernel_inline_verus.py", "scripts/kernel_verus_ir.py",
                    "scripts/bootstrap-verification-tools.sh"], check=True)
    declared_mutants = {mutant for item in symbols.values() for mutant in item["mutants"]}
    require(symbols and declared_mutants and {p.stem for p in patches.glob("*.patch")} == declared_mutants,
            "incomplete artifact_governance mutant inventory")
    original = source.read_text()
    require("#![deny(clippy::default_numeric_fallback)]" in original.splitlines(),
            "missing module-level default_numeric_fallback denial")
    digests = approved_artifact_statements(original, symbols)
    verify_artifact(source, tools, work, symbols)
    compared = compare_artifact_production_ir(repo, work, symbols)
    records = []
    for name, contract in symbols.items():
        body = next(m for m in ARTIFACT_BODY.finditer(original) if m["name"] == name)
        start, end = body.span("body")
        tail = len(original) - end
        killed = []
        for mutant_id in contract["mutants"]:
            patch = patches / f"{mutant_id}.patch"
            case = work / ("artifact_" + mutant_id)
            case.mkdir()
            copy = case / source.name
            copy.write_text(original)
            subprocess.run(["patch", "--batch", "--forward", "--fuzz=0", copy, patch], cwd=case, check=True)
            mutated = copy.read_text()
            require(mutated != original and mutated[:start] == original[:start]
                    and mutated[len(mutated) - tail:] == original[end:], f"{patch}: changes outside {name}")
            verify_artifact(copy, tools, work, symbols, name)
            killed.append({"id": mutant_id, "killedAt": contract["postcondition"]})
        records.append({
            "head": head, "symbol": f"norn_ir::artifact_governance::{name}", "verifier": "verus",
            "toolDigest": (tools / "verus-install.sha256").read_text().split()[0],
            "sourceDigest": hashlib.sha256(source.read_bytes()).hexdigest(),
            "statementDigest": digests[name],
            "lemmas": sorted(ARTIFACT_LEMMAS),
            "virThirComparison": {"equal": True, **compared[name]},
            "secondLensGate": contract["secondLensGate"],
            "mutants": killed,
        })
    return records


def main():
    repo, tools, work = map(Path, sys.argv[1:4])
    target = sys.argv[4] if len(sys.argv) > 4 else "all"
    require(target in {"all", "version_check", "artifact_governance"}, f"unknown target: {target}")
    manifest = _sibling("coverage_manifest").load()
    head = subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip()
    records = []
    if target in {"all", "version_check"}:
        records.extend(run_version_check(repo, tools, work, head, manifest))
    if target in {"all", "artifact_governance"}:
        records.extend(run_artifact_governance(repo, tools, work, head, manifest))
    print(json.dumps(records, indent=2))


if __name__ == "__main__":
    main()
