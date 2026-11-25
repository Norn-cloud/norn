"""Check the inline overlay-core contracts, their mutants, and D107 VIR/THIR agreement.

usage: kernel_overlay_verus.py run REPO TOOLS WORK
       kernel_overlay_verus.py compare VIR THIR
"""
import hashlib
import json
import os
from collections import Counter
from pathlib import Path
import re
import subprocess
import sys

CRATE = "overlay_core"
SOURCE = "crates/norn-kernel/src/commit/overlay_core.rs"
PATCHES = "verification/mutants/overlay_core"
# Each checked executable function, its named postcondition and the mutants it must kill.
SYMBOLS = {
    "resolve": {"postcondition": "resolve", "mutants": ["put_from_before", "delete_reads_before"]},
    "edge_delta": {"postcondition": "edge_delta", "mutants": ["remove_kept"]},
}
# Ghost lemmas that must be present and verified with the executable contracts.
LEMMAS = {
    "overlay_frame", "written_never_before", "deletion_excludes", "row_after_functional",
    "edge_apply_exact", "edge_idempotent", "edge_fold_exact",
}


def require(condition, message):
    if not condition:
        raise SystemExit(message)


# --- Verus verdicts -------------------------------------------------------------------------

def check_verdict(data, returncode, mutant_of=None):
    summary = data["verification-results"]
    failing = mutant_of is not None
    require(returncode == int(failing)
            and summary["is-verifying-entire-crate"] is True
            and summary["encountered-vir-error"] is False
            and summary["success"] is (not failing)
            and summary["errors"] == int(failing)
            and summary["verified"] > 0, f"incomplete verification: {summary}")
    results = {}
    for module in data["times-ms"]["smt"]["smt-run-module-times"]:
        for row in module["function-breakdown"]:
            results.setdefault(row["mode:"], {})[row["function"]] = row["success"]
    executable = results.get("exec", {})
    for name in SYMBOLS:
        require(f"{CRATE}::{name}" in executable, f"{name}: not verified as executable code")
    require(all(ok == (fn != f"{CRATE}::{mutant_of}") for fn, ok in executable.items()),
            f"unexpected executable verification results: {executable}")
    proofs = results.get("proof", {})
    require(LEMMAS <= {fn.removeprefix(CRATE + "::") for fn, ok in proofs.items() if ok},
            f"missing or failed lemmas: {proofs}")
    for name, contract in SYMBOLS.items():
        notes = data["func-details"][f"{CRATE}::{name}"]
        label = contract["postcondition"]
        require(notes["obligation_proof_notes"] == [label]
                and notes["failed_proof_notes"] == ([label] if name == mutant_of else []),
                f"{name}: incorrect postcondition result: {notes}")


def verify(path, tools, work, mutant_of=None):
    env = os.environ.copy()
    for override in ("VARGO_TARGET_DIR", "__VERUS_DRIVER_VIA_CARGO__"):
        env.pop(override, None)
    bundle = tools / "verus-x86-linux"
    env.update(RUSTUP_HOME=str(tools / "rustup"), VERUS_ROOT=str(bundle),
               VERUS_Z3_PATH=str(bundle / "z3"), VERUS_USE_RUSTUP="1")
    logs = [] if mutant_of else ["--log", "vir", "--log", "vir-option=no_fn_details",
                                 "--log-dir", str(work / "vir")]
    result = subprocess.run([
        bundle / "verus", "--crate-name", CRATE, "--crate-type=lib", "--edition=2024",
        "--no-cheating", "--no-external-by-default", "--num-threads", "1",
        "-Zcrate-attr=verus::untrusted", "--time", "--output-json", *logs, path,
    ], cwd=work, env=env, stdout=subprocess.PIPE, text=True, timeout=300)
    try:
        data = json.loads(result.stdout)
    except ValueError:
        raise SystemExit(f"verus produced no verdict (exit {result.returncode}): {result.stdout[-2000:]}")
    check_verdict(data, result.returncode, mutant_of)
    return data["verification-results"]


# --- D107: typed source spans of the two compiler dumps -------------------------------------

def span(location):
    return re.search(r"overlay_core\.rs:(\d+:\d+: \d+:\d+)", location)[1]


def forms(text):
    stack = [[]]
    for token in re.findall(r';;[^\n]*|"(?:\\.|[^"\\])*"|[()]|[^\s()"]+', text):
        if token.startswith(";;"):
            continue
        if token == "(":
            stack.append([])
        elif token == ")":
            node = stack.pop()
            stack[-1].append(node)
        else:
            stack[-1].append(token)
    require(len(stack) == 1, "incomplete VIR")
    return stack[0]


def field(node, key):
    require(node.count(key) == 1, f"missing or duplicate VIR field {key}")
    return node[node.index(key) + 1]


DATATYPES = {f"{CRATE}::t%{n}": n for n in ("RowChange", "RowSource", "EdgeDelta")}
DATATYPES["core::t%option::t%Option"] = "Option"


def vir_type(node):
    match node:
        case ["Typ", "Bool"]:
            return "bool"
        case ["Typ", "Int", ["IntRange", "USize"]]:
            return "usize"
        case ["Typ", "Int", ["IntRange", sign, width]] if sign in ("I", "U"):
            return sign.lower() + width
        case ["Typ", "TypParam", name]:
            return name.strip('"')
        case ["Typ", "Primitive", ["Primitive", "Slice"], [inner]]:
            return f"[{vir_type(inner)}]"
        case ["Typ", "Datatype", ["Dt", "Tuple", _], args, []]:
            return "(" + ", ".join(map(vir_type, args)) + ")"
        case ["Typ", "Datatype", ["Dt", "Path", path], args, []] if path in DATATYPES:
            return DATATYPES[path] + ("<" + ", ".join(map(vir_type, args)) + ">" if args else "")
        case ["Typ", "Decorate", ["TypDecoration", "Never"], "None", _]:
            return "!"
        case ["Typ", "Decorate", ["TypDecoration", "Ref"], "None", inner]:
            return "&" + vir_type(inner)
    raise ValueError(f"unsupported executable VIR type {node}")


def thir_type(ty):
    ty = re.sub(r"'\{erased\}, ", "", ty).replace("&'{erased} ", "&")
    ty = re.sub(r"\b([A-Z]\w*)/#\d+", r"\1", ty)  # rustc prints a type parameter with its index
    for prefix in ("std::option::", "core::option::", "commit::overlay_core::"):
        ty = ty.replace(prefix, "")
    require(re.fullmatch(r"[\w<>\[\]&(), !]+", ty) and "'" not in ty, f"unsupported THIR type {ty!r}")
    return ty


def inventory():
    return {"expressions": set(), "locals": Counter(), "calls": Counter(), "literal_patterns": Counter()}


def record_binding(out, location, variable, ty):
    out["locals"][span(location).split(": ")[0], variable.strip('"'), ty] += 1


def callee(path):
    path = re.sub(r"impl&%(\d+)", r"{impl#\1}", path.replace("::t%", "::"))
    return re.sub(r"\[[0-9a-f]+\]", "", path).removeprefix("norn_kernel::commit::")


def visit_vir(node, out):
    if not isinstance(node, list):
        return
    if node[:1] == ["Arm"]:
        # Verus materializes `true` for an unguarded Rust match arm.
        require(field(node, ":guard")[2:] == [[">", "Const", ["Constant", "Bool", "true"]],
                                              ["Typ", "Bool"]], "unexpected match guard")
        visit_vir(field(node, ":pattern"), out)
        visit_vir(field(node, ":body"), out)
        return
    if node[:1] == ["@@"]:
        _, location, body, ty = node
        if body[:2] == [">", "Ghost"]:
            return
        if body[:2] == ["Pattern", "Expr"]:
            # A literal pattern: THIR keeps it as a constant pattern, not an expression.
            match body[2]:
                case ["@@", _, [">", "Const", ["Constant", "Bool", value]], ["Typ", "Bool"]]:
                    out["literal_patterns"][span(location), vir_type(ty), value] += 1
                    return
            raise ValueError(f"unsupported VIR literal pattern {body}")
        match body:
            case [">", "ReadPlace", ["@@", _, ["Place", "Local", _], place_ty],
                  ["UnfinalizedReadKind", ":preliminary_kind", ["ReadKind", "ImmutBor"], ":id", _]]:
                # Passing a shared reference: rustc reborrows it (`&*place`), which THIR records
                # as the reference and its pointee at this span. Verus records `&&` instead.
                inner = vir_type(place_ty)
                require(inner.startswith("&") and vir_type(ty) == "&" + inner,
                        f"unsupported VIR shared borrow {body}")
                out["expressions"] |= {(span(location), inner), (span(location), inner[1:])}
            case [">", *_] | ["Place", *_]:
                out["expressions"].add((span(location), vir_type(ty)))
        if body[:2] == [">", "Call"]:
            out["calls"][span(location), callee(field(field(body, ":target")[3], ":path"))] += 1
        if body[:2] == ["Pattern", "Var"]:
            binding = body[2]
            record_binding(out, location, field(binding, ":name")[1], vir_type(field(binding, ":typ")))
    for child in node:
        visit_vir(child, out)


def function_name(path):
    path = callee(path)
    parts = path.split("::")
    return "::".join(parts[-2:]) if parts[-2].startswith("{impl#") else parts[-1]


def vir_functions(text):
    result = {}
    for form in forms(text):
        if form[:1] != ["@"] or form[2][:1] != ["Function"]:
            continue
        function = form[2]
        path = field(function[1], ":path")
        if not path.startswith(CRATE + "::") or field(function, ":mode") != "Exec":
            continue
        name = function_name(path)
        require(name not in result and field(function, ":body") != "None",
                f"duplicate or missing executable body for {path}")
        out = result[name] = inventory()
        for _, location, param in field(function, ":params"):
            record_binding(out, location, field(param, ":name")[1], vir_type(field(param, ":typ")))
        visit_vir(field(function, ":body"), out)
    return result


EXPR_HEADER = re.compile(
    r"^ *Expr \{\n\s*ty: (.+)\n\s*temp_scope_id: [^\n]+\n\s*span: ([^\n]+)\n"
    r"\s*kind: *\n\s*([^\s({]+)(?: \{\n\s*ty: ([^\n]+))?", re.M)
LITERAL_PATTERN = re.compile(
    r"^ *Pat \{\n\s*ty: ([^\n]+)\n\s*span: ([^\n]+)\n\s*kind: PatKind \{\n\s*Constant \{\n\s*value: ([^\n]+)\n", re.M)
BINDING = re.compile(r'^ *Binding \{\n\s*name: "(\w+)"\n\s*mode: .*\n\s*var: .*\n\s*ty: (.*)$', re.M)


def thir_function(body):
    out = inventory()
    expressions = list(EXPR_HEADER.finditer(body))
    require(len(expressions) == len(re.findall(r"^ *Expr \{$", body, re.M)), "unparsed THIR expression")
    for expr in expressions:
        ty, location, kind, target = expr.groups()
        if kind == "Scope" or (kind == "ZstLiteral" and ty.startswith("FnDef(")):
            continue  # rustc's region wrappers and the FnDef recorded at the call below
        require(kind != "Cast", "unexpected THIR cast")
        out["expressions"].add((span(location), thir_type(ty)))
        if kind == "Call":
            out["calls"][span(location), callee(re.search(r"FnDef\(DefId\(\d+:\d+ ~ (.*?)\),", target)[1])] += 1
    for ty, location, value in LITERAL_PATTERN.findall(body):
        out["literal_patterns"][span(location), thir_type(ty), value] += 1
    require(len(LITERAL_PATTERN.findall(body)) == body.count("Constant {"), "unparsed THIR constant pattern")
    bindings = list(BINDING.finditer(body))
    require(len(bindings) == len(re.findall(r"^ *Binding \{$", body, re.M)), "unparsed THIR binding")
    for binding in bindings:
        pat = body.rindex("Pat {\n", 0, binding.start())
        location = re.search(r"Pat \{\n\s*ty: [^\n]+\n\s*span: ([^\n]+)", body[pat:])[1]
        record_binding(out, location, binding[1], thir_type(binding[2]))
    return out


def thir_functions(text):
    result = {}
    sections = re.split(r"^DefId\(0:\d+ ~ ([^\n]+?)\):\n", text, flags=re.M)
    for header, body in zip(sections[1::2], sections[2::2], strict=True):
        if not re.fullmatch(r"norn_kernel\[[0-9a-f]+\]::commit::overlay_core::(?:\{impl#\d+\}::)?\w+", header):
            continue
        name = function_name(header)
        require(name not in result, f"duplicate THIR function {header}")
        result[name] = thir_function(body)
    return result


def compare(vir_text, thir_text):
    verified, shipped = vir_functions(vir_text), thir_functions(thir_text)
    require(verified.keys() == shipped.keys() and set(SYMBOLS) <= verified.keys(),
            f"incomplete functions: VIR {sorted(verified)}, THIR {sorted(shipped)}")
    counts = {}
    for name in sorted(verified):
        for category, left in verified[name].items():
            right = shipped[name][category]
            require(left == right, f"{name} {category}: VIR only {left - right}, THIR only {right - left}")
        counts[name] = {k: len(v) if isinstance(v, set) else v.total() for k, v in verified[name].items()}
    return counts


def production_thir(repo, work):
    # Release Cargo build of the shipped kernel; a run-specific dump directory forces rustc to run.
    thir = work / "kernel.thir"
    with thir.open("w") as dump:
        subprocess.run(["cargo", "+1.100.0", "rustc", "--locked", "--release", "-p", "norn-kernel",
                        "--lib", "--", "-Zunpretty=thir-tree", f"-Zdump-mir-dir={work / 'mir'}"],
                       cwd=repo, env={**os.environ, "RUSTC_BOOTSTRAP": "norn_kernel"},
                       stdout=dump, check=True)
    return thir.read_text()


# --- Mutants ----------------------------------------------------------------------------------

def function_body(source, name):
    match = re.search(rf"^pub\(crate\) (?:const )?fn {name}\b.*?^\}}\n", source, re.M | re.S)
    require(match, f"missing function {name}")
    return match.span()


def run(repo, tools, work):
    source = repo / SOURCE
    patches = repo / PATCHES
    head = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=repo, text=True).strip()
    subprocess.run(["git", "diff", "--exit-code", head, "--", SOURCE, PATCHES,
                    "scripts/kernel_overlay_verus.py", "scripts/verify-kernel-overlay-verus.sh"],
                   cwd=repo, check=True)
    owners = {m: name for name, c in SYMBOLS.items() for m in c["mutants"]}
    require({p.stem for p in patches.glob("*.patch")} == owners.keys(), "incomplete mutant inventory")
    original = source.read_text()
    require("#![deny(clippy::default_numeric_fallback)]" in original.splitlines(),
            "missing module-level default_numeric_fallback denial")
    summary = verify(source, tools, work)
    compared = compare((work / "vir/crate.vir").read_text(), production_thir(repo, work))
    killed = {}
    for mutant, name in sorted(owners.items()):
        case = work / mutant
        case.mkdir()
        copy = case / source.name
        copy.write_text(original)
        subprocess.run(["patch", "--batch", "--forward", "--fuzz=0", copy, patches / f"{mutant}.patch"],
                       cwd=case, check=True, stdout=subprocess.DEVNULL)
        start, end = function_body(original, name)
        mutated = copy.read_text()
        tail = len(original) - end
        require(mutated != original and mutated[:start] == original[:start]
                and mutated[len(mutated) - tail:] == original[end:], f"{mutant}: changes outside {name}")
        verify(copy, tools, work, name)
        killed.setdefault(name, []).append({"id": mutant, "killedAt": SYMBOLS[name]["postcondition"]})
    print(json.dumps({
        "head": head, "verifier": "verus",
        "toolDigest": (tools / "verus-install.sha256").read_text().split()[0],
        "sourceDigest": hashlib.sha256(source.read_bytes()).hexdigest(),
        "verified": summary["verified"], "lemmas": sorted(LEMMAS),
        "virThirComparison": compared,
        "symbols": {f"norn_kernel::commit::overlay_core::{n}": killed[n] for n in SYMBOLS},
    }, indent=2))


def main():
    match sys.argv[1:]:
        case ["run", repo, tools, work]:
            run(Path(repo), Path(tools), Path(work))
        case ["compare", vir, thir]:
            print(json.dumps(compare(Path(vir).read_text(), Path(thir).read_text()), indent=2))
        case _:
            raise SystemExit(__doc__)


if __name__ == "__main__":
    main()
