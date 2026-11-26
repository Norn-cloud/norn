#!/usr/bin/env python3
"""Fail closed when a promoted Lean/F* proof contains an undeclared hole."""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path


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

coverage_manifest = _sibling("coverage_manifest")


ROOT = Path(__file__).resolve().parents[1]
ASSUMPTIONS = ROOT / "verification/coverage-v1/trusted-assumptions.json"
LEAN_ROOT = ROOT / "verification"
FSTAR_ROOT = ROOT / "verification/stripmine-2026-08-22/proofs/fstar"


def fail(message: str) -> None:
    raise ValueError(message)


def relative(path: Path) -> str:
    return path.resolve().relative_to(ROOT).as_posix()


CHAR_ESCAPE_LITERAL = re.compile(
    r"'\\(?:x[0-9A-Fa-f]{2}|u[0-9A-Fa-f]{4}|u\{[0-9A-Fa-f]+\}|[0-9]{1,3}|[^\n])'"
)
RAW_STRING_OPEN = re.compile(r'r(#*)"')


def _is_lean_letter_like(ch: str) -> bool:
    """Exact mirror of Lean v4.34.1 `Init.Meta.Defs.isLetterLike`."""
    code = ord(ch)
    return (
        (0x03B1 <= code <= 0x03C9 and code != 0x03BB)
        or (0x0391 <= code <= 0x03A9 and code not in (0x03A0, 0x03A3))
        or (0x03CA <= code <= 0x03FB)
        or (0x1F00 <= code <= 0x1FFE)
        or (0x2100 <= code <= 0x214F)
        or (0x1D49C <= code <= 0x1D59F)
        or (0x00C0 <= code <= 0x00FF and code not in (0x00D7, 0x00F7))
        or (0x0100 <= code <= 0x017F)
    )


def _is_lean_subscript_alnum(ch: str) -> bool:
    """Exact mirror of Lean v4.34.1 `Init.Meta.Defs.isSubScriptAlnum`."""
    code = ord(ch)
    return (
        (0x2080 <= code <= 0x2089)
        or (0x2090 <= code <= 0x209C)
        or (0x1D62 <= code <= 0x1D6A)
        or code == 0x2C7C
    )


def _is_ident_start(ch: str, *, is_lean: bool) -> bool:
    if "a" <= ch <= "z" or "A" <= ch <= "Z" or ch == "_":
        return True
    return is_lean and _is_lean_letter_like(ch)


def _is_ident_rest(ch: str, *, is_lean: bool) -> bool:
    if _is_ident_start(ch, is_lean=is_lean) or "0" <= ch <= "9" or ch == "'":
        return True
    return is_lean and (ch in ("!", "?") or _is_lean_subscript_alnum(ch))


def strip_comments_and_strings(
    source: str,
    *,
    line_marker: str,
    block: tuple[str, str],
    preserve_strings: bool = False,
) -> str:
    """Remove nested block comments, line comments, char literals, and quoted strings."""

    output: list[str] = []
    index = 0
    depth = 0
    in_string: str | None = None
    interp_braces: list[int] = []
    after_dot = False
    is_lean = line_marker == "--"
    while index < len(source):
        if depth:
            if source.startswith(block[0], index):
                depth += 1
                output.extend(" " * len(block[0]))
                index += len(block[0])
            elif source.startswith(block[1], index):
                depth -= 1
                output.extend(" " * len(block[1]))
                index += len(block[1])
            else:
                output.append("\n" if source[index] == "\n" else " ")
                index += 1
            continue
        if in_string is not None:
            if source[index] == "\\":
                width = min(2, len(source) - index)
                output.extend(source[index : index + width] if preserve_strings else " " * width)
                index += width
            elif source[index] == '"':
                output.append('"' if preserve_strings else " ")
                index += 1
                in_string = None
            elif in_string == "interp" and source[index] == "{":
                output.append("{")
                index += 1
                interp_braces.append(1)
                in_string = None
            elif is_lean and in_string == "plain" and source[index] == "{":
                fail("unescaped '{' inside Lean string literal is forbidden in trusted proof sources")
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
            after_dot = False
        elif source.startswith(block[0], index):
            output.extend(" " * len(block[0]))
            index += len(block[0])
            depth = 1
            after_dot = False
        elif is_lean and source[index] == "«":
            close = source.find("»", index + 1)
            if close < 0:
                fail("unterminated French-quoted identifier in trusted Lean source")
            span = source[index : close + 1]
            # Keep identifier text visible to hole patterns so `«ofReduceBool»`
            # or `«implemented_by»` cannot hide behind French quotes, while
            # neutralizing quote/brace characters so `«"»` or `«}»` cannot
            # desynchronize string or interpolation lexing.
            output.extend(
                span
                if preserve_strings
                else "".join(" " if ch in ('«', '»', '"', "'", "{", "}") else ch for ch in span)
            )
            index = close + 1
            after_dot = False
        elif _is_ident_start(source[index], is_lean=is_lean):
            if (
                is_lean
                and not after_dot
                and source[index] == "r"
                and RAW_STRING_OPEN.match(source, index) is not None
            ):
                fail("raw string literal is forbidden in trusted Lean proof sources")
            if (
                is_lean
                and not after_dot
                and source[index] in ("s", "m", "f")
                and source.startswith('!"', index + 1)
            ):
                output.extend(source[index : index + 3] if preserve_strings else "   ")
                index += 3
                in_string = "interp"
                after_dot = False
                continue
            end = index + 1
            while end < len(source) and _is_ident_rest(source[end], is_lean=is_lean):
                end += 1
            output.append(source[index:end])
            index = end
            after_dot = False
        elif source[index] == "'":
            char_width = 0
            if (
                index + 2 < len(source)
                and source[index + 1] not in ("\\", "'")
                and source[index + 2] == "'"
            ):
                char_width = 3
            elif index + 3 < len(source) and source[index + 1] == "\\":
                matched = CHAR_ESCAPE_LITERAL.match(source, index)
                if matched is not None:
                    char_width = matched.end() - index
            if char_width:
                span = source[index : index + char_width]
                output.extend(
                    span
                    if preserve_strings
                    else "".join("\n" if ch == "\n" else " " for ch in span)
                )
                index += char_width
            else:
                output.append(source[index])
                index += 1
            after_dot = False
        elif source[index] == '"':
            output.append('"' if preserve_strings else " ")
            index += 1
            in_string = "plain"
            after_dot = False
        elif interp_braces and source[index] == "{":
            interp_braces[-1] += 1
            output.append("{")
            index += 1
            after_dot = False
        elif interp_braces and source[index] == "}":
            interp_braces[-1] -= 1
            output.append("}")
            index += 1
            after_dot = False
            if interp_braces[-1] == 0:
                interp_braces.pop()
                in_string = "interp"
        elif source[index] == ".":
            is_single_dot = (index == 0 or source[index - 1] != ".") and (
                index + 1 == len(source) or source[index + 1] != "."
            )
            output.append(".")
            index += 1
            after_dot = is_single_dot
        else:
            output.append(source[index])
            index += 1
            after_dot = False
    if depth:
        fail("unterminated block comment in trusted proof source")
    if in_string is not None or interp_braces:
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

FSTAR_PROOF_HOLE = re.compile(
    r"\b(?:admit|_assume|assume|admitted|admit_termination|axiom|val|"
    r"admit_smt_query|admit_smt_queries|expect_failure|expect_lax_failure|"
    r"magic|tadmit|tadmit_t|admitP|unsafe_coerce)\b"
)

# `constant` is Lean's general constant-declaration command. `native_decide`,
# `+native`, `ofReduceBool`, `ofReduceNat`, `bv_decide`, `implemented_by`,
# `extern`, `skipKernelTC`, `addDecl*`, `addAndCompile`, `axiomDecl`,
# `modifyEnv`, `setEnv`, `#eval`, `initialize`, `run_cmd`, `elab`,
# `elab_rules`, `sorryAx`, and `unsafe` move trust outside Lean's kernel or
# inject arbitrary declarations. `opaque` definitions can be kernel-sound when
# they include a body, but rejecting them in promoted proof sources keeps
# selector and extraction correspondence transparent and avoids a
# declaration-shaped escape hatch. The scanner always runs after comments and
# strings have been removed by the nested-comment-aware lexer.
LEAN_PROOF_HOLE = re.compile(
    r"\b(?:sorry|sorryAx|admit|axiom|constant|native_decide|implemented_by|unsafe|opaque|"
    r"ofReduceBool|ofReduceNat|bv_decide|skipKernelTC|addDecl\w*|addAndCompile|"
    r"axiomDecl|modifyEnv|setEnv|initialize|extern|run_cmd|elab|elab_rules)\b|"
    r"(?<!\w)(?:\+native|#eval)\b|"
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
        'def quoted := "constant native_decide implemented_by unsafe opaque \\{ok}"\n'
        'def interpClean (n : Nat) := s!"count={n} sorry"\n'
        "def quoteChar := '\"'\n"
        "def x' := \"still clean\"\n"
        'def «"» := true\n'
        'def theorem_ok : True := by exact True.intro\n'
    )
    if lean_proof_hole_locations(lean_clean):
        fail("Lean proof-hole scanner canary failed")
    for lean_bad in (
        "def theorem_bad : True := by sorry\n",
        "def quote_blind := '\"'\ndef theorem_bad : True := by sorry\ndef close := \"\"\n",
        "def esc_blind := '\\\"'\ndef theorem_bad : True := by sorry\ndef close := \"\"\n",
        'def «"» := 1\ndef theorem_bad : True := by sorry\ndef close := ""\n',
        'def Foo.r (s : String) : String := s\ndef a := Foo.r"\\""\naxiom bad : False\n-- "\n',
        "def x : Bool := !'\"' == 'a'\naxiom bad : False\ndef y : Char := '\"'\n",
        "def x' (s : String) : Bool := true\ndef b : Bool := !x'\"'\"\naxiom bad : False\ndef y : Char := '\"'\n",
        "def z : Char := 'a'x'\"'\" x'\naxiom bad : False\ndef y : Char := '\"'\n",
        "def c : Char := д'\"'\naxiom bad : False\ndef y : Char := '\"'\n",
        "def é' : Char := 'a'\ndef b : Prop := é'<'\x22'\naxiom bad : False\ndef y : Char := '\"'\n",
        "def ā' : Char := 'a'\ndef b : Prop := ā'<'\x22'\naxiom bad : False\ndef y : Char := '\"'\n",
        "def xⱼ' : Char := 'a'\ndef b : Prop := xⱼ'<'\x22'\naxiom bad : False\ndef y : Char := '\"'\n",
        "def b : Nat := x₊'\"'\naxiom bad : False\ndef y : Char := '\"'\n",
        "def b : Nat := x²'\"'\naxiom bad : False\ndef y : Char := '\"'\n",
        "def l : List Char := ['\n','\"']\naxiom bad : False\ndef y : Char := '\"'\n",
        'def x : Bool := !s!"{(Lean.ofReduceBool : Nat)}" == ""\n',
        'def interp_hide := s!"{(Lean.ofReduceBool : Nat)}"\n',
        "def guillemet_hide := Lean.«ofReduceBool»\n",
        "set_option debug.«skipKernelTC» true\n",
        "set_option «warningAsError» false\n",
        "@[«implemented_by» alt] def forged : True := True.intro\n",
        "def forged := «sorryAx» _ false\n",
        "axiom impossible : False\n",
        "constant impossible : False\n",
        "example : True := by native_decide\n",
        "example : True := by decide +native\n",
        "def forged := Lean.ofReduceBool\n",
        "def forged := Lean.ofReduceNat\n",
        "example : True := by bv_decide\n",
        "set_option debug.skipKernelTC true\n",
        "def forged := Lean.addDecl\n",
        "def forged := Lean.Kernel.Environment.addDeclWithoutChecking\n",
        "#eval Lean.addAndCompile (.axiomDecl default)\n",
        "initialize pure ()\n",
        "def forged := Lean.modifyEnv\n",
        "def forged := Lean.setEnv\n",
        '@[extern "c_forged"] def forged : True := True.intro\n',
        "run_cmd pure ()\n",
        "elab \"x\" : term => pure ()\n",
        "@[implemented_by alternate] def forged : True := True.intro\n",
        "unsafe def forged : True := True.intro\n",
        "opaque forged : True := True.intro\n",
        "set_option warningAsError false\n",
    ):
        if not lean_proof_hole_locations(lean_bad):
            fail(f"Lean proof-hole scanner canary failed for {lean_bad.strip()!r}")

    for lean_rejected in (
        'def raw_blind := r"\\"\naxiom impossible : False\ndef close := \'"\'\n',
        'def x : Bool := !r"\\" == ""\naxiom bad : False\ndef y : Char := \'"\'\n',
        'def c := дr"\\"\naxiom bad : False\ndef y : Char := \'"\'\n',
        'def xs! (s : String) : Bool := true\ndef b : Bool := !xs!"{"\naxiom bad : False\ndef q := "}"\n',
        'def raw_hashed := r#"sorry"#\n',
        'def a : Nat := panic!"{"\naxiom bad : False\ndef b := "}"\n',
        'def a := throwError "{(Lean.ofReduceBool : Nat)}"\n',
        'def a := dbg_trace "{(by run_tac Lean.addDecl d; exact 0 : Nat)}"; 0\n',
        'def a := s! "{Lean.ofReduceBool}"\n',
        'def a := m! "{Lean.modifyEnv}"\n',
    ):
        try:
            lean_proof_hole_locations(lean_rejected)
        except ValueError:
            continue
        fail(f"Lean proof-hole scanner canary did not reject {lean_rejected.strip()!r}")

    fstar_clean = strip_comments_and_strings(
        "// admit\n(* assume *)\nlet id (x' : 'a) : 'a = x'\nlet q = '\"'\nlet theorem_ok = ()\n",
        line_marker="//",
        block=("(*", "*)"),
    )
    if FSTAR_PROOF_HOLE.search(fstar_clean):
        fail("F* clean proof-hole scanner canary failed")
    for fstar_bad_src in (
        "let theorem_bad = admit ()\n",
        "let q = '\"'\nlet theorem_bad = admit ()\nlet s = \"\"\n",
        "let theorem_bad = _assume ()\n",
        "let theorem_bad = admitted ()\n",
        "let theorem_bad = magic ()\n",
        "let theorem_bad = tadmit ()\n",
        "let theorem_bad = tadmit_t ()\n",
        "let theorem_bad = admitP True\n",
        "let theorem_bad = unsafe_coerce ()\n",
    ):
        fstar_bad = strip_comments_and_strings(
            fstar_bad_src,
            line_marker="//",
            block=("(*", "*)"),
        )
        if not FSTAR_PROOF_HOLE.search(fstar_bad):
            fail(f"F* proof-hole scanner canary failed for {fstar_bad_src.strip()!r}")

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
    coverage = coverage_manifest.load()
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
    for path_string in sorted(discovered_fstar):
        check_fstar_source(ROOT / path_string, FSTAR_PROOF_HOLE)
    generated_paths = [
        path.resolve() if path.is_absolute() else (ROOT / path).resolve()
        for path in args.fstar_generated
    ]
    for path in generated_paths:
        if not path.is_file() or ROOT not in path.parents:
            fail(f"generated F* source must be an existing workspace file: {path}")
        check_fstar_source(path, FSTAR_PROOF_HOLE)

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
