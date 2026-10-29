#!/usr/bin/env python3
"""Fail closed when a promoted Lean/F* proof contains an undeclared hole."""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
COVERAGE = ROOT / "verification/coverage-v1/coverage.json"
ASSUMPTIONS = ROOT / "verification/coverage-v1/trusted-assumptions.json"
LEAN_ROOT = ROOT / "verification"
FSTAR_ROOT = ROOT / "verification/stripmine-2026-08-22/proofs/fstar"


def fail(message: str) -> None:
    raise ValueError(message)


def relative(path: Path) -> str:
    return path.resolve().relative_to(ROOT).as_posix()


def strip_comments_and_strings(
    source: str,
    *,
    line_marker: str,
    block: tuple[str, str],
    preserve_strings: bool = False,
) -> str:
    """Remove nested block comments, line comments, and quoted strings."""

    output: list[str] = []
    index = 0
    depth = 0
    in_string = False
    while index < len(source):
        if depth:
            if source.startswith(block[0], index):
                depth += 1
                index += len(block[0])
            elif source.startswith(block[1], index):
                depth -= 1
                index += len(block[1])
            else:
                output.append("\n" if source[index] == "\n" else " ")
                index += 1
            continue
        if in_string:
            if source[index] == "\\":
                width = min(2, len(source) - index)
                output.extend(source[index : index + width] if preserve_strings else " " * width)
                index += width
            elif source[index] == '"':
                output.append('"' if preserve_strings else " ")
                index += 1
                in_string = False
            else:
                output.append(
                    source[index]
                    if preserve_strings
                    else ("\n" if source[index] == "\n" else " ")
                )
                index += 1
            continue
        if source.startswith(line_marker, index):
            newline = source.find("\n", index)
            if newline < 0:
                output.extend(" " * (len(source) - index))
                break
            output.extend(" " * (newline - index))
            index = newline
        elif source.startswith(block[0], index):
            output.extend(" " * len(block[0]))
            index += len(block[0])
            depth = 1
        elif source[index] == '"':
            output.append('"' if preserve_strings else " ")
            index += 1
            in_string = True
        else:
            output.append(source[index])
            index += 1
    if depth:
        fail("unterminated block comment in trusted proof source")
    if in_string:
        fail("unterminated string in trusted proof source")
    return "".join(output)


def locations(pattern: re.Pattern[str], source: str) -> list[str]:
    return [str(source.count("\n", 0, match.start()) + 1) for match in pattern.finditer(source)]


def check_fstar_source(path: Path, forbidden: re.Pattern[str]) -> None:
    source = path.read_text(encoding="utf-8")
    if fstar_admission_option_present(source):
        fail(f"forbidden F* admission option in {relative(path)}")
    code = strip_comments_and_strings(
        source, line_marker="//", block=("(*", "*)")
    )
    hits = locations(forbidden, code)
    if hits:
        fail(f"forbidden F* proof hole in {relative(path)} at lines {', '.join(hits)}")


FSTAR_ADMISSION_OPTION = re.compile(
    r"--(?:admit_smt_quer(?:y|ies)|admit_except|lax|unsafe_tactic_exec)\b",
    re.IGNORECASE,
)

# `constant` is Lean's general constant-declaration command. `native_decide`
# and `implemented_by` move trust to native compilation or an alternate
# implementation; `unsafe` declarations are outside Lean's trusted logical
# fragment. `opaque` definitions can be kernel-sound when they include a body,
# but rejecting them in promoted proof sources keeps selector and extraction
# correspondence transparent and avoids a declaration-shaped escape hatch.
# The scanner always runs after comments and strings have been removed by the
# nested-comment-aware lexer.
LEAN_PROOF_HOLE = re.compile(
    r"\b(?:sorry|admit|axiom|constant|native_decide|implemented_by|unsafe|opaque)\b|"
    r"\bset_option\s+warningAsError\s+false\b"
)


def lean_proof_hole_locations(source: str) -> list[str]:
    """Return source lines containing executable Lean admission constructs."""

    code = strip_comments_and_strings(
        source,
        line_marker="--",
        block=("/-", "-/"),
    )
    return locations(LEAN_PROOF_HOLE, code)


def fstar_admission_option_present(source: str) -> bool:
    """Return true when F* source or an invocation enables an unsafe admission mode."""

    code = strip_comments_and_strings(
        source,
        line_marker="//",
        block=("(*", "*)"),
        preserve_strings=True,
    )
    return FSTAR_ADMISSION_OPTION.search(code) is not None


def extraction_admission_present(source: str) -> bool:
    """Return true when a Rust-to-F* extraction command enables admission."""

    return FSTAR_ADMISSION_OPTION.search(source) is not None


def run_lexer_canaries() -> None:
    lean_clean = (
        '-- sorry native_decide unsafe\n'
        '/- axiom /- constant hidden : False -/ implemented_by opaque -/\n'
        'def quoted := "constant native_decide implemented_by unsafe opaque"\n'
        'def theorem_ok : True := by exact True.intro\n'
    )
    if lean_proof_hole_locations(lean_clean):
        fail("Lean proof-hole scanner canary failed")
    for lean_bad in (
        "def theorem_bad : True := by sorry\n",
        "axiom impossible : False\n",
        "constant impossible : False\n",
        "example : True := by native_decide\n",
        "@[implemented_by alternate] def forged : True := True.intro\n",
        "unsafe def forged : True := True.intro\n",
        "opaque forged : True := True.intro\n",
        "set_option warningAsError false\n",
    ):
        if not lean_proof_hole_locations(lean_bad):
            fail(f"Lean proof-hole scanner canary failed for {lean_bad.strip()!r}")

    fstar_pattern = re.compile(
        r"\b(?:admit|assume|axiom|val|admit_smt_query|admit_smt_queries)\b"
    )
    fstar_clean = strip_comments_and_strings(
        "// admit\n(* assume *)\nlet theorem_ok = ()\n",
        line_marker="//",
        block=("(*", "*)"),
    )
    fstar_bad = strip_comments_and_strings(
        "let theorem_bad = admit ()\n",
        line_marker="//",
        block=("(*", "*)"),
    )
    if fstar_pattern.search(fstar_clean) or not fstar_pattern.search(fstar_bad):
        fail("F* proof-hole scanner canary failed")

    fstar_option_clean = '// #push-options "--lax"\nlet theorem_ok = ()\n'
    if fstar_admission_option_present(fstar_option_clean):
        fail("F* commented source-admission scanner canary failed")
    for directive in ("#set-options", "#push-options"):
        for option in (
            "--admit_smt_query true",
            "--admit_smt_queries true",
            "--admit_except RuleHitPolicyProof",
            "--lax",
            "--unsafe_tactic_exec true",
        ):
            source = f'{directive} "{option}"\nlet theorem_bad = ()\n'
            if not fstar_admission_option_present(source):
                fail(
                    "F* source-admission scanner canary failed for "
                    f"{directive} {option}"
                )

    for option in ("--admit_smt_query", "--admit_except", "--lax", "--unsafe_tactic_exec"):
        if not extraction_admission_present(f"fstar.exe {option}"):
            fail(f"F* invocation-admission scanner canary failed for {option}")
    if extraction_admission_present("hax into fstar"):
        fail("F* extraction-admission scanner canary failed")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--fstar-generated", action="append", default=[], type=Path)
    args = parser.parse_args()
    run_lexer_canaries()
    coverage = json.loads(COVERAGE.read_text(encoding="utf-8"))
    assumptions = json.loads(ASSUMPTIONS.read_text(encoding="utf-8"))
    if assumptions.get("schemaVersion") != "norn.verification.trusted-assumptions.v1":
        fail("trusted-assumptions.json has the wrong schemaVersion")
    records = assumptions.get("assumptions")
    if not isinstance(records, list) or not records:
        fail("trusted-assumptions.json must declare the trusted boundary")
    required_fields = {"id", "owner", "boundary", "justification", "blastRadius", "falsificationTest"}
    ids: set[str] = set()
    for record in records:
        if not isinstance(record, dict) or set(record) != required_fields:
            fail("every trusted assumption must declare exactly the required fields")
        if not all(isinstance(record[field], str) and record[field].strip() for field in required_fields):
            fail("trusted assumption fields must be non-empty strings")
        if record["id"] in ids:
            fail(f"duplicate trusted assumption id: {record['id']}")
        ids.add(record["id"])

    proof_artifacts = {
        artifact
        for subsystem in coverage["subsystems"]
        for layer in ("runtime", "model", "proof")
        for artifact in subsystem[layer]["artifacts"]
    }
    discovered_lean = {relative(path) for path in LEAN_ROOT.rglob("*.lean")}
    declared_lean = {path for path in proof_artifacts if path.endswith(".lean")}
    if discovered_lean != declared_lean:
        fail(
            "promoted Lean inventory mismatch: "
            f"undeclared={sorted(discovered_lean - declared_lean)}, "
            f"missing={sorted(declared_lean - discovered_lean)}"
        )

    commands = {
        command
        for subsystem in coverage["subsystems"]
        for layer in ("runtime", "model", "proof")
        for command in subsystem[layer]["commands"]
    }
    composite_lean_gates = coverage.get("leanCompositeGates", {})
    if not isinstance(composite_lean_gates, dict):
        fail("leanCompositeGates must be an object")
    for proof_path, gate_command in composite_lean_gates.items():
        if proof_path not in discovered_lean:
            fail(f"composite Lean gate names an undeclared proof: {proof_path}")
        if not isinstance(gate_command, str) or gate_command not in commands:
            fail(f"composite Lean gate is not a declared promoted command: {gate_command!r}")
        if not gate_command.startswith("scripts/") or not gate_command.endswith(".sh"):
            fail(f"composite Lean gate must be a repository script: {gate_command}")
        gate_path = ROOT / gate_command
        if not gate_path.is_file():
            fail(f"composite Lean gate script is missing: {gate_command}")
        gate_source = gate_path.read_text(encoding="utf-8")
        if proof_path not in gate_source or "-DwarningAsError=true" not in gate_source:
            fail(
                f"composite Lean gate must bind its exact proof and warning-as-error: {gate_command}"
            )
    for path_string in sorted(discovered_lean):
        expected = f"lean -DwarningAsError=true {path_string}"
        if expected not in commands and path_string not in composite_lean_gates:
            fail(f"Lean proof has no exact warning-as-error gate: {path_string}")
        path = ROOT / path_string
        hits = lean_proof_hole_locations(path.read_text(encoding="utf-8"))
        if hits:
            fail(f"forbidden Lean proof hole in {path_string} at lines {', '.join(hits)}")

    discovered_fstar = {relative(path) for path in FSTAR_ROOT.rglob("*.fst")}
    declared_fstar = {path for path in proof_artifacts if path.endswith(".fst")}
    if discovered_fstar != declared_fstar:
        fail(
            "promoted F* inventory mismatch: "
            f"undeclared={sorted(discovered_fstar - declared_fstar)}, "
            f"missing={sorted(declared_fstar - discovered_fstar)}"
        )
    forbidden_fstar = re.compile(
        r"\b(?:admit|assume|axiom|val|admit_smt_query|admit_smt_queries)\b"
    )
    for path_string in sorted(discovered_fstar):
        check_fstar_source(ROOT / path_string, forbidden_fstar)
    generated_paths = [
        path.resolve() if path.is_absolute() else (ROOT / path).resolve()
        for path in args.fstar_generated
    ]
    for path in generated_paths:
        if not path.is_file() or ROOT not in path.parents:
            fail(f"generated F* source must be an existing workspace file: {path}")
        check_fstar_source(path, forbidden_fstar)

    proof_scripts = tuple(
        sorted(
            {
                ROOT / command
                for subsystem in coverage["subsystems"]
                for command in subsystem["proof"]["commands"]
                if command.startswith("scripts/") and command.endswith(".sh")
            }
        )
    )
    if not proof_scripts:
        fail("no promoted proof scripts were discovered")
    for path in proof_scripts:
        if not path.is_file():
            fail(f"promoted proof script is missing: {relative(path)}")
        source = path.read_text(encoding="utf-8")
        if extraction_admission_present(source):
            fail(f"proof admission flag present in {relative(path)}")

    print(
        f"checked {len(discovered_lean)} Lean, {len(discovered_fstar)} trusted F*, "
        f"and {len(generated_paths)} generated F* sources; "
        f"checked {len(proof_scripts)} proof scripts; "
        f"declared {len(records)} external trust assumptions; no proof holes found"
    )
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, ValueError, json.JSONDecodeError) as error:
        print(f"proof-hole validation failed: {error}", file=sys.stderr)
        raise SystemExit(1)
