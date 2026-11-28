"""Check the inline OWL-Time Allen contracts, their mutants, wrapper mapping, and D107 VIR/THIR agreement.

usage: owl_time_inline_verus.py run REPO TOOLS WORK
       owl_time_inline_verus.py compare VIR THIR
"""
from collections import Counter
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys

CRATE = "allen_core"
SOURCE = "crates/norn-expr/src/temporal/allen_core.rs"
WRAPPER_SOURCE = "crates/norn-expr/src/temporal/allen.rs"
PATCHES = "verification/mutants/allen_core"
# Each checked executable function, its named postcondition and the mutants it must kill.
SYMBOLS = {
    "converse": {"postcondition": "converse", "mutants": ["converse_meets_self"]},
    "shares_instant": {"postcondition": "shares_instant", "mutants": ["shares_instant_meets_true"]},
    "holds": {"postcondition": "holds", "mutants": ["holds_meets_accepts_before"]},
    "classify": {"postcondition": "classify", "mutants": ["classify_meets_as_overlaps"]},
}
# Ghost lemmas that must be present and verified with the executable contracts.
LEMMAS = {
    "exactly_one_relation_holds",
    "pairwise_disjoint",
    "converse_holds_iff",
    "converse_symmetry_and_involution",
    "sharing_an_instant_is_half_open_overlap",
    "improper_intervals_have_no_relation",
    "scheduling_compositions",
    "scheduling_compositions_reachable",
}

# Closed datatype variant tables in declaration order (matches rustc VariantIdx).
DATATYPE_VARIANTS = {
    "Option": ("None", "Some"),
    "EndpointOrder": ("Less", "Equal", "Greater"),
    "AllenRelation": (
        "Before",
        "Meets",
        "Overlaps",
        "Starts",
        "During",
        "Finishes",
        "Equals",
        "FinishedBy",
        "Contains",
        "StartedBy",
        "OverlappedBy",
        "MetBy",
        "After",
    ),
}
# Production-only derived trait methods gated out of Verus by `not(verus_keep_ghost)`.
PROD_ONLY_ALLEN_TRAIT_METHODS = frozenset({"fmt", "eq", "assert_receiver_is_total_eq", "hash"})


def require(condition, message):
    if not condition:
        raise SystemExit(message)


# --- Verus verdicts -------------------------------------------------------------------------

def check_verdict(data, returncode, mutant_of=None):
    summary = data["verification-results"]
    failing = mutant_of is not None
    require(
        returncode == int(failing)
        and summary["is-verifying-entire-crate"] is True
        and summary["encountered-vir-error"] is False
        and summary["success"] is (not failing)
        and summary["errors"] == int(failing)
        and summary["verified"] > 0,
        f"incomplete verification: {summary}",
    )
    results = {}
    for module in data["times-ms"]["smt"]["smt-run-module-times"]:
        for row in module["function-breakdown"]:
            results.setdefault(row["mode:"], {})[row["function"]] = row["success"]
    executable = results.get("exec", {})
    for name in SYMBOLS:
        require(f"{CRATE}::{name}" in executable, f"{name}: not verified as executable code")
    require(
        all(ok == (fn != f"{CRATE}::{mutant_of}") for fn, ok in executable.items()),
        f"unexpected executable verification results: {executable}",
    )
    proofs = results.get("proof", {})
    require(
        LEMMAS <= {fn.removeprefix(CRATE + "::") for fn, ok in proofs.items() if ok},
        f"missing or failed lemmas: {proofs}",
    )
    for name, contract in SYMBOLS.items():
        notes = data["func-details"][f"{CRATE}::{name}"]
        label = contract["postcondition"]
        require(
            notes["obligation_proof_notes"] == [label]
            and notes["failed_proof_notes"] == ([label] if name == mutant_of else []),
            f"{name}: incorrect postcondition result: {notes}",
        )


def verify(path, tools, work, mutant_of=None):
    env = os.environ.copy()
    for override in ("VARGO_TARGET_DIR", "__VERUS_DRIVER_VIA_CARGO__"):
        env.pop(override, None)
    bundle = tools / "verus-x86-linux"
    env.update(
        RUSTUP_HOME=str(tools / "rustup"),
        VERUS_ROOT=str(bundle),
        VERUS_Z3_PATH=str(bundle / "z3"),
        VERUS_USE_RUSTUP="1",
    )
    logs = (
        []
        if mutant_of
        else ["--log", "vir", "--log", "vir-option=no_fn_details", "--log-dir", str(work / "vir")]
    )
    result = subprocess.run(
        [
            bundle / "verus",
            "--crate-name",
            CRATE,
            "--crate-type=lib",
            "--edition=2024",
            "--no-cheating",
            "--no-external-by-default",
            "--num-threads",
            "1",
            "-Zcrate-attr=verus::untrusted",
            "--time",
            "--output-json",
            *logs,
            path,
        ],
        cwd=work,
        env=env,
        stdout=subprocess.PIPE,
        text=True,
        timeout=300,
    )
    try:
        data = json.loads(result.stdout)
    except ValueError:
        raise SystemExit(
            f"verus produced no verdict (exit {result.returncode}): {result.stdout[-2000:]}"
        ) from None
    check_verdict(data, result.returncode, mutant_of)
    return data["verification-results"]


# --- Shipped wrapper contract (`allen.rs`) --------------------------------------------------

def _strip_rust_comments(text):
    text = re.sub(r"/\*.*?\*/", " ", text, flags=re.S)
    text = re.sub(r"//[^\n]*", "", text)
    return text


def _norm_ws(text):
    return " ".join(text.split())


def _braced_block(text, open_index):
    require(open_index < len(text) and text[open_index] == "{", "expected '{'")
    depth = 0
    in_string = False
    escaped = False
    for index in range(open_index, len(text)):
        ch = text[index]
        if in_string:
            if escaped:
                escaped = False
            elif ch == "\\":
                escaped = True
            elif ch == '"':
                in_string = False
            continue
        if ch == '"':
            in_string = True
        elif ch == "{":
            depth += 1
        elif ch == "}":
            depth -= 1
            if depth == 0:
                return text[open_index : index + 1], index + 1
    raise SystemExit("unterminated braced block")


def check_wrapper(wrapper_text):
    """Pin the shipped `allen.rs` wrapper mapping, `ALL` array, and improper-interval short-circuit."""
    # Check only production code before `#[cfg(test)]`.
    prod = wrapper_text.split("#[cfg(test)]", 1)[0]
    clean = _strip_rust_comments(prod)

    # 1. `endpoint_order` must be the exact identity map over `Ordering`.
    order_match = re.search(
        r"\bconst fn endpoint_order\(\s*ordering:\s*Ordering\s*\)\s*->\s*EndpointOrder\s*\{",
        clean,
    )
    require(order_match, "allen.rs: missing endpoint_order")
    order_body, _ = _braced_block(clean, order_match.end() - 1)
    expected_order_body = (
        "{ match ordering { "
        "Ordering::Less => EndpointOrder::Less, "
        "Ordering::Equal => EndpointOrder::Equal, "
        "Ordering::Greater => EndpointOrder::Greater, "
        "} }"
    )
    require(_norm_ws(order_body) == expected_order_body, f"allen.rs: endpoint_order drifted: {order_body}")

    # 2. `AllenRelation::ALL` must list all 13 distinct variants in exact canonical order.
    all_match = re.search(r"\bpub const ALL:\s*\[Self;\s*13\]\s*=\s*\[([^\]]+)\];", clean)
    require(all_match, "allen.rs: missing AllenRelation::ALL")
    all_items = [item.strip() for item in all_match.group(1).split(",") if item.strip()]
    expected_all = [f"Self::{variant}" for variant in DATATYPE_VARIANTS["AllenRelation"]]
    require(all_items == expected_all, f"allen.rs: AllenRelation::ALL drifted: {all_items}")

    # 3. `converse` and `shares_instant` must delegate directly to `allen_core`.
    for name, sig, target in (
        ("converse", r"\bpub const fn converse\(\s*self\s*\)\s*->\s*Self\s*\{", "{ allen_core::converse(self) }"),
        (
            "shares_instant",
            r"\bpub const fn shares_instant\(\s*self\s*\)\s*->\s*bool\s*\{",
            "{ allen_core::shares_instant(self) }",
        ),
    ):
        match = re.search(sig, clean)
        require(match, f"allen.rs: missing AllenRelation::{name}")
        body, _ = _braced_block(clean, match.end() - 1)
        require(_norm_ws(body) == target, f"allen.rs: {name} drifted: {body}")

    # 4. `AllenRelation::holds` must short-circuit improper intervals and pass exact comparison order.
    holds_match = re.search(
        r"\bpub fn holds<T:\s*Ord>\(\s*self,\s*start:\s*&T,\s*end:\s*&T,\s*other_start:\s*&T,\s*other_end:\s*&T,?\s*\)\s*->\s*bool\s*\{",
        clean,
    )
    require(holds_match, "allen.rs: missing AllenRelation::holds")
    holds_body, _ = _braced_block(clean, holds_match.end() - 1)
    expected_holds = (
        "{ let start_end = endpoint_order(start.cmp(end)); "
        "if !allen_core::is_less(start_end) { return false; } "
        "let other_start_end = endpoint_order(other_start.cmp(other_end)); "
        "if !allen_core::is_less(other_start_end) { return false; } "
        "allen_core::holds( self, start_end, other_start_end, "
        "endpoint_order(start.cmp(other_start)), "
        "endpoint_order(end.cmp(other_end)), "
        "endpoint_order(end.cmp(other_start)), "
        "endpoint_order(other_end.cmp(start)), ) }"
    )
    require(
        _norm_ws(holds_body).replace("( ", "(").replace(" )", ")")
        == expected_holds.replace("( ", "(").replace(" )", ")"),
        f"allen.rs: AllenRelation::holds drifted: {holds_body}",
    )

    # 5. `allen_relation` must short-circuit improper intervals and pass exact comparison order.
    classify_match = re.search(
        r"\bpub fn allen_relation<T:\s*Ord>\(\s*start:\s*&T,\s*end:\s*&T,\s*other_start:\s*&T,\s*other_end:\s*&T,?\s*\)\s*->\s*Option<AllenRelation>\s*\{",
        clean,
    )
    require(classify_match, "allen.rs: missing allen_relation")
    classify_body, _ = _braced_block(clean, classify_match.end() - 1)
    expected_classify = (
        "{ let start_end = endpoint_order(start.cmp(end)); "
        "if !allen_core::is_less(start_end) { return None; } "
        "let other_start_end = endpoint_order(other_start.cmp(other_end)); "
        "if !allen_core::is_less(other_start_end) { return None; } "
        "allen_core::classify( start_end, other_start_end, "
        "endpoint_order(start.cmp(other_start)), "
        "endpoint_order(end.cmp(other_end)), "
        "endpoint_order(end.cmp(other_start)), "
        "endpoint_order(other_end.cmp(start)), ) }"
    )
    require(
        _norm_ws(classify_body).replace("( ", "(").replace(" )", ")")
        == expected_classify.replace("( ", "(").replace(" )", ")"),
        f"allen.rs: allen_relation drifted: {classify_body}",
    )


# --- D107: typed source spans and semantic values of the two compiler dumps -----------------

def span(location):
    return re.search(r"allen_core\.rs:(\d+:\d+: \d+:\d+)", location)[1]


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


DATATYPES = {f"{CRATE}::t%{n}": n for n in ("AllenRelation", "EndpointOrder")}
DATATYPES["core::t%option::t%Option"] = "Option"


def vir_type(node):
    match node:
        case ["Typ", "Bool"]:
            return "bool"
        case ["Typ", "Int", ["IntRange", "USize"]]:
            return "usize"
        case ["Typ", "Int", ["IntRange", sign, width]] if sign in ("I", "U"):
            return sign.lower() + width
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
    for prefix in ("std::option::", "core::option::", "temporal::allen_core::"):
        ty = ty.replace(prefix, "")
    require(re.fullmatch(r"[\w<>\[\]&(), !]+", ty) and "'" not in ty, f"unsupported THIR type {ty!r}")
    return ty


def inventory():
    return {
        "expressions": set(),
        "literals": Counter(),
        "ctors": Counter(),
        "logical_ops": Counter(),
        "local_reads": Counter(),
        "locals": Counter(),
        "calls": Counter(),
        "literal_patterns": Counter(),
        "pattern_variants": Counter(),
    }


def _normalize_local_id(raw):
    text = " ".join(raw.strip().rstrip(",").split())
    if text.startswith("LocalVarId(") and text.endswith(")"):
        text = text[len("LocalVarId(") : -1].strip().rstrip(",").strip()
    require(bool(text), f"empty THIR local id {raw!r}")
    return text


def record_binding(out, location, variable, ty):
    out["locals"][span(location).split(": ")[0], variable.strip('"'), ty] += 1


def callee(path):
    path = re.sub(r"impl&%(\d+)", r"{impl#\1}", path.replace("::t%", "::"))
    return re.sub(r"\[[0-9a-f]+\]", "", path).removeprefix("norn_expr::temporal::")


def _check_variant(dt_name, variant):
    require(
        dt_name in DATATYPE_VARIANTS and variant in DATATYPE_VARIANTS[dt_name],
        f"unknown variant {dt_name}::{variant}",
    )
    return variant


def visit_vir(node, out):
    if not isinstance(node, list):
        return
    if node[:1] == ["Arm"]:
        # Verus materializes `true` for an unguarded Rust match arm.
        require(
            field(node, ":guard")[2:]
            == [[">", "Const", ["Constant", "Bool", "true"]], ["Typ", "Bool"]],
            "unexpected match guard",
        )
        visit_vir(field(node, ":pattern"), out)
        visit_vir(field(node, ":body"), out)
        return
    if node[:1] == ["@@"]:
        _, location, body, ty = node
        if body[:2] == [">", "Ghost"]:
            return
        if body[:2] == ["Pattern", "Expr"]:
            match body[2]:
                case ["@@", _, [">", "Const", ["Constant", "Bool", value]], ["Typ", "Bool"]]:
                    out["literal_patterns"][span(location), vir_type(ty), value] += 1
                    return
                case ["@@", _, [">", "Const", ["Constant", "Int", value]], lit_ty]:
                    out["literal_patterns"][span(location), vir_type(lit_ty), value] += 1
                    return
            raise ValueError(f"unsupported VIR literal pattern {body}")
        if body[:2] == ["Pattern", "Constructor"]:
            match body:
                case ["Pattern", "Constructor", ["Dt", "Path", dt_path], variant, *_] if dt_path in DATATYPES:
                    dt_name = DATATYPES[dt_path]
                    out["pattern_variants"][
                        span(location), dt_name, _check_variant(dt_name, variant.strip('"'))
                    ] += 1
                case ["Pattern", "Constructor", ["Dt", "Tuple", _], *_]:
                    pass
                case _:
                    raise ValueError(f"unsupported VIR pattern constructor {body}")
        match body:
            case [
                ">",
                "ReadPlace",
                ["@@", _, ["Place", "Local", _], place_ty],
                ["UnfinalizedReadKind", ":preliminary_kind", ["ReadKind", "ImmutBor"], ":id", _],
            ]:
                inner = vir_type(place_ty)
                require(
                    inner.startswith("&") and vir_type(ty) == "&" + inner,
                    f"unsupported VIR shared borrow {body}",
                )
                out["expressions"] |= {(span(location), inner), (span(location), inner[1:])}
            case [">", "Const", ["Constant", "Bool", value]]:
                out["expressions"].add((span(location), vir_type(ty)))
                out["literals"][span(location), vir_type(ty), value] += 1
            case [">", "Const", ["Constant", "Int", value]]:
                out["expressions"].add((span(location), vir_type(ty)))
                out["literals"][span(location), vir_type(ty), value] += 1
            case [">", "Const", *_]:
                raise ValueError(f"unsupported VIR constant {body}")
            case [">", "Ctor", ["Dt", "Path", dt_path], variant, *_] if dt_path in DATATYPES:
                dt_name = DATATYPES[dt_path]
                out["expressions"].add((span(location), vir_type(ty)))
                out["ctors"][
                    span(location), dt_name, _check_variant(dt_name, variant.strip('"'))
                ] += 1
            case [">", "Ctor", ["Dt", "Tuple", _], *_]:
                out["expressions"].add((span(location), vir_type(ty)))
            case [">", "Ctor", *_]:
                raise ValueError(f"unsupported VIR constructor {body}")
            case [">", "Logical", ["LogicalOp", op], _, _]:
                out["expressions"].add((span(location), vir_type(ty)))
                out["logical_ops"][span(location), op] += 1
            case ["Place", "Local", ["VarIdent", var_name, *_]] | [">", "Var", ["VarIdent", var_name, *_]]:
                out["expressions"].add((span(location), vir_type(ty)))
                out["local_reads"][span(location), var_name.strip('"')] += 1
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
        require(
            name not in result and field(function, ":body") != "None",
            f"duplicate or missing executable body for {path}",
        )
        out = result[name] = inventory()
        for _, location, param in field(function, ":params"):
            record_binding(out, location, field(param, ":name")[1], vir_type(field(param, ":typ")))
        visit_vir(field(function, ":body"), out)
    return result


EXPR_HEADER = re.compile(
    r"^ *Expr \{\n\s*ty: (.+)\n\s*temp_scope_id: [^\n]+\n\s*span: ([^\n]+)\n"
    r"\s*kind: *\n\s*([^\s({]+)(?: \{\n\s*ty: ([^\n]+))?",
    re.M,
)
LITERAL_PATTERN = re.compile(
    r"^ *Pat \{\n\s*ty: ([^\n]+)\n\s*span: ([^\n]+)\n\s*kind: PatKind \{\n\s*Constant \{\n\s*value: ([^\n]+)\n",
    re.M,
)
VARIANT_PATTERN = re.compile(
    r"\bPat \{\n\s*ty: ([^\n]+)\n\s*span: ([^\n]+)\n\s*kind: PatKind \{\n\s*Variant \{",
    re.M,
)
BINDING = re.compile(
    r'^ *Binding \{\n\s*name: "(\w+)"\n\s*mode: [^\n]+\n\s*var: (LocalVarId\((?:[^()]|\((?:[^()]|\([^()]*\))*\))*\)|[^\n]+)\n\s*ty: ([^\n]+)$',
    re.M,
)


def _top_level_fields(block):
    """Extract `key: value` pairs at depth 1 inside a `{ ... }` THIR record."""
    inner = block[1:-1]
    fields = {}
    depth = 0
    in_string = False
    escaped = False
    chunk = []
    entries = []
    for ch in inner:
        if in_string:
            chunk.append(ch)
            if escaped:
                escaped = False
            elif ch == "\\":
                escaped = True
            elif ch == '"':
                in_string = False
            continue
        if ch == '"':
            in_string = True
            chunk.append(ch)
        elif ch in "{[(":
            depth += 1
            chunk.append(ch)
        elif ch in "}])":
            depth -= 1
            chunk.append(ch)
        elif depth == 0 and ch in "\n,":
            item = "".join(chunk).strip()
            if item:
                entries.append(item)
            chunk = []
        else:
            chunk.append(ch)
    tail = "".join(chunk).strip()
    if tail:
        entries.append(tail)
    for entry in entries:
        match = re.match(r"^([a-z_][a-z0-9_]*):\s*(.*)$", entry, flags=re.S)
        if match:
            fields[match.group(1)] = match.group(2).strip()
    return fields


def _parse_thir_literal(block):
    fields = _top_level_fields(block)
    raw = fields.get("lit", "")
    neg = fields.get("neg", "false")
    require(neg in ("true", "false"), f"unsupported THIR literal neg={neg!r} in {block!r}")
    bool_match = (
        re.search(r"\bBool\((true|false)\)", raw)
        or re.search(r'\bkind:\s*Bool,\s*symbol:\s*"(true|false)"', raw)
        or re.fullmatch(r"(true|false)", raw)
    )
    if bool_match:
        require(neg == "false", f"negated bool literal in {block!r}")
        return bool_match.group(1)
    int_match = (
        re.search(r"\bInt\(Pu128\((\d+)\)", raw)
        or re.search(r'\bkind:\s*Integer,\s*symbol:\s*"(\d+)"', raw)
        or re.fullmatch(r"(\d+)", raw)
    )
    if int_match:
        return ("-" if neg == "true" else "") + int_match.group(1)
    raise ValueError(f"unsupported THIR literal {block!r}")


def _parse_thir_variant(dt_name, block):
    require(dt_name in DATATYPE_VARIANTS, f"unsupported THIR datatype {dt_name!r}")
    variants = DATATYPE_VARIANTS[dt_name]
    fields = _top_level_fields(block)
    if "variant_index" in fields:
        idx_match = re.fullmatch(r"(?:VariantIdx\()?(\d+)v?\)?", fields["variant_index"])
        require(idx_match, f"unparsed THIR variant_index in {block!r}")
        idx = int(idx_match.group(1))
        require(0 <= idx < len(variants), f"out-of-range variant_index {idx} for {dt_name}")
        return variants[idx]
    if "variant" in fields:
        name = fields["variant"].strip('"')
        return _check_variant(dt_name, name)
    raise ValueError(f"missing THIR variant identity in {block!r}")


def _kind_block(body, expr_match):
    brace = body.find("{", expr_match.start(3))
    require(brace != -1, f"missing THIR block for {expr_match.group(3)}")
    block, _ = _braced_block(body, brace)
    return block


def thir_function(body):
    out = inventory()
    var_names = {}
    bindings = list(BINDING.finditer(body))
    require(len(bindings) == len(re.findall(r"^ *Binding \{$", body, re.M)), "unparsed THIR binding")
    for binding in bindings:
        pat = body.rindex("Pat {\n", 0, binding.start())
        location = re.search(r"Pat \{\n\s*ty: [^\n]+\n\s*span: ([^\n]+)", body[pat:])[1]
        var_id = _normalize_local_id(binding[2])
        require(var_id not in var_names, f"duplicate THIR local id {var_id!r}")
        var_names[var_id] = binding[1]
        record_binding(out, location, binding[1], thir_type(binding[3]))
    expressions = list(EXPR_HEADER.finditer(body))
    require(len(expressions) == len(re.findall(r"^ *Expr \{$", body, re.M)), "unparsed THIR expression")
    for expr in expressions:
        ty, location, kind, target = expr.groups()
        if kind == "Scope" or (kind == "ZstLiteral" and ty.startswith("FnDef(")):
            continue
        require(kind != "Cast", "unexpected THIR cast")
        norm_ty = thir_type(ty)
        expr_span = span(location)
        out["expressions"].add((expr_span, norm_ty))
        if kind == "Call":
            out["calls"][
                expr_span, callee(re.search(r"FnDef\(DefId\(\d+:\d+ ~ (.*?)\),", target)[1])
            ] += 1
        elif kind == "Literal":
            out["literals"][expr_span, norm_ty, _parse_thir_literal(_kind_block(body, expr))] += 1
        elif kind == "Adt":
            dt_name = norm_ty.split("<", 1)[0]
            out["ctors"][expr_span, dt_name, _parse_thir_variant(dt_name, _kind_block(body, expr))] += 1
        elif kind == "LogicalOp":
            op = _top_level_fields(_kind_block(body, expr)).get("op")
            require(op in ("And", "Or"), f"unsupported THIR LogicalOp {op!r}")
            out["logical_ops"][expr_span, op] += 1
        elif kind == "VarRef":
            raw_id = _top_level_fields(_kind_block(body, expr)).get("id")
            require(raw_id is not None, f"missing THIR VarRef id at {expr_span}")
            var_id = _normalize_local_id(raw_id)
            require(var_id in var_names, f"unresolved THIR VarRef id {var_id!r} at {expr_span}")
            out["local_reads"][expr_span, var_names[var_id]] += 1
    for ty, location, value in LITERAL_PATTERN.findall(body):
        out["literal_patterns"][span(location), thir_type(ty), value] += 1
    require(len(LITERAL_PATTERN.findall(body)) == body.count("Constant {"), "unparsed THIR constant pattern")
    variant_pats = list(VARIANT_PATTERN.finditer(body))
    require(
        len(variant_pats) == len(re.findall(r"^\s*Variant \{$", body, re.M)),
        "unparsed THIR variant pattern",
    )
    for pat in variant_pats:
        pat_ty, location = pat.groups()
        dt_name = thir_type(pat_ty).split("<", 1)[0]
        block, _ = _braced_block(body, pat.end() - 1)
        out["pattern_variants"][span(location), dt_name, _parse_thir_variant(dt_name, block)] += 1
    return out


def thir_functions(text):
    result = {}
    skipped_derived = set()
    sections = re.split(r"^DefId\(0:\d+ ~ ([^\n]+?)\):\n", text, flags=re.M)
    for header, body in zip(sections[1::2], sections[2::2], strict=True):
        if not re.fullmatch(
            r"norn_expr\[[0-9a-f]+\]::temporal::allen_core::(?:\{impl#\d+\}::)?\w+", header
        ):
            continue
        name = function_name(header)
        impl_match = re.fullmatch(r"\{impl#\d+\}::(\w+)", name)
        if impl_match and impl_match.group(1) in PROD_ONLY_ALLEN_TRAIT_METHODS:
            method = impl_match.group(1)
            require(
                method not in skipped_derived
                and re.search(r"ty:\s*&(?:'\{erased\}\s+)?temporal::allen_core::AllenRelation\b", body),
                f"unexpected derived THIR method {header}",
            )
            skipped_derived.add(method)
            continue
        require(name not in result, f"duplicate THIR function {header}")
        result[name] = thir_function(body)
    return result


def compare(vir_text, thir_text):
    verified, shipped = vir_functions(vir_text), thir_functions(thir_text)
    require(
        verified.keys() == shipped.keys() and set(SYMBOLS) <= verified.keys(),
        f"incomplete functions: VIR {sorted(verified)}, THIR {sorted(shipped)}",
    )
    counts = {}
    for name in sorted(verified):
        for category, left in verified[name].items():
            right = shipped[name][category]
            require(
                left == right,
                f"{name} {category}: VIR only {left - right}, THIR only {right - left}",
            )
        counts[name] = {k: len(v) if isinstance(v, set) else v.total() for k, v in verified[name].items()}
    return counts


def production_thir(repo, work):
    thir = work / "norn_expr.thir"
    with thir.open("w") as dump:
        subprocess.run(
            [
                "cargo",
                "+1.100.0",
                "rustc",
                "--locked",
                "--release",
                "-p",
                "norn-expr",
                "--lib",
                "--",
                "-Zunpretty=thir-tree",
                f"-Zdump-mir-dir={work / 'mir'}",
            ],
            cwd=repo,
            env={**os.environ, "RUSTC_BOOTSTRAP": "norn_expr"},
            stdout=dump,
            check=True,
        )
    return thir.read_text()


# --- Mutants ----------------------------------------------------------------------------------

def function_body(source, name):
    match = re.search(rf"^pub\(crate\) (?:const )?fn {name}\b.*?^\}}\n", source, re.M | re.S)
    require(match, f"missing function {name}")
    return match.span()


def run(repo, tools, work):
    source = repo / SOURCE
    wrapper = repo / WRAPPER_SOURCE
    patches = repo / PATCHES
    head = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=repo, text=True).strip()
    subprocess.run(
        [
            "git",
            "diff",
            "--exit-code",
            head,
            "--",
            SOURCE,
            WRAPPER_SOURCE,
            PATCHES,
            "scripts/owl_time_inline_verus.py",
            "scripts/verify-owl-time-inline-verus.sh",
        ],
        cwd=repo,
        check=True,
    )
    owners = {m: name for name, c in SYMBOLS.items() for m in c["mutants"]}
    require({p.stem for p in patches.glob("*.patch")} == owners.keys(), "incomplete mutant inventory")
    original = source.read_text()
    require(
        "#![deny(clippy::default_numeric_fallback)]" in original.splitlines(),
        "missing module-level default_numeric_fallback denial",
    )
    check_wrapper(wrapper.read_text())
    summary = verify(source, tools, work)
    compared = compare((work / "vir/crate.vir").read_text(), production_thir(repo, work))
    killed = {}
    for mutant, name in sorted(owners.items()):
        case = work / mutant
        case.mkdir()
        copy = case / source.name
        copy.write_text(original)
        subprocess.run(
            ["patch", "--batch", "--forward", "--fuzz=0", copy, patches / f"{mutant}.patch"],
            cwd=case,
            check=True,
            stdout=subprocess.DEVNULL,
        )
        start, end = function_body(original, name)
        mutated = copy.read_text()
        tail = len(original) - end
        require(
            mutated != original
            and mutated[:start] == original[:start]
            and mutated[len(mutated) - tail :] == original[end:],
            f"{mutant}: changes outside {name}",
        )
        verify(copy, tools, work, name)
        killed.setdefault(name, []).append({"id": mutant, "killedAt": SYMBOLS[name]["postcondition"]})
    print(
        json.dumps(
            {
                "head": head,
                "verifier": "verus",
                "toolDigest": (tools / "verus-install.sha256").read_text().split()[0],
                "sourceDigest": hashlib.sha256(source.read_bytes()).hexdigest(),
                "wrapperSourceDigest": hashlib.sha256(wrapper.read_bytes()).hexdigest(),
                "verified": summary["verified"],
                "lemmas": sorted(LEMMAS),
                "virThirComparison": compared,
                "symbols": {f"norn_expr::temporal::allen_core::{n}": killed[n] for n in SYMBOLS},
            },
            indent=2,
        )
    )


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
