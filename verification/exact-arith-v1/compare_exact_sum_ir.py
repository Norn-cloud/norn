"""D107 one-shot comparison of typed source spans in ExactSum's two compiler dumps."""

import re
import sys
from collections import Counter
from pathlib import Path

# Include the two Clone bodies that Verus checks for the derived Copy types.
EXPECTED = {
    "absolute",
    "plus",
    "minus",
    "add",
    "retract",
    "total",
    "magnitude",
    "is_zero",
    "{impl#4}::clone",
    "{impl#23}::clone",
}

EXPR_HEADER = re.compile(
    r"^ *Expr \{\n\s*ty: (.+)\n\s*temp_scope_id: [^\n]+\n\s*span: ([^\n]+)\n"
    r"\s*kind: *\n\s*([^\s({]+)(?: \{\n\s*ty: ([^\n]+))?",
    re.M,
)
BINDING = re.compile(
    r'^ *Binding \{\n\s*name: "(\w+)"\n\s*mode: .*\n\s*var: .*\n\s*ty: (.*)$', re.M
)


def span(location):
    return re.search(r"exact_sum\.rs:(\d+:\d+: \d+:\d+)", location)[1]


def forms(text):
    """Read the pinned VIR printer's S-expressions, respecting quoted spans."""
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
    if len(stack) != 1:
        raise ValueError("incomplete VIR")
    return stack[0]


def field(node, key):
    if node.count(key) != 1:
        raise ValueError(f"missing or duplicate VIR field {key}")
    return node[node.index(key) + 1]


def vir_type(node):
    match node:
        case ["Typ", "Bool"]:
            return "bool"
        case ["Typ", "Int", ["IntRange", sign, width]] if sign in ("I", "U"):
            return sign.lower() + width
        case ["Typ", "Datatype", ["Dt", "Tuple", _], args, []]:
            return "(" + ", ".join(map(vir_type, args)) + ")"
        case ["Typ", "Datatype", ["Dt", "Path", path], args, []]:
            name = {
                "exact_sum::t%U160": "U160",
                "exact_sum::t%ExactSum": "ExactSum",
                "core::t%option::t%Option": "Option",
            }[path]
            return name + ("<" + ", ".join(map(vir_type, args)) + ">" if args else "")
        case ["Typ", "Decorate", ["TypDecoration", "Never"], "None", _]:
            return "!"
        case ["Typ", "Decorate", ["TypDecoration", "Ref"], "None", inner]:
            return "&" + vir_type(inner)
    raise ValueError(f"unsupported executable VIR type {node}")


def inventory():
    # Both compilers introduce repeated, identically typed wrappers at a
    # source span. Calls and bindings keep their occurrence counts.
    return {"expressions": set(), "locals": Counter(), "calls": Counter()}


def record_binding(out, location, variable, ty):
    out["locals"][span(location).split(": ")[0], variable.strip('"'), ty] += 1


def visit_vir(node, out):
    if not isinstance(node, list):
        return
    if node[:1] == ["Arm"]:
        # Verus materializes true for an unguarded Rust match arm.
        if field(node, ":guard")[2:] != [
            [">", "Const", ["Constant", "Bool", "true"]],
            ["Typ", "Bool"],
        ]:
            raise ValueError("unexpected match guard")
        visit_vir(field(node, ":pattern"), out)
        visit_vir(field(node, ":body"), out)
        return
    if node[:1] == ["@@"]:
        _, location, body, ty = node
        if body[:2] == [">", "Ghost"]:
            return
        if body[:1] in ([">"], ["Place"]):
            out["expressions"].add((span(location), vir_type(ty)))
        if body[:2] == [">", "Call"]:
            target = field(field(body, ":target")[3], ":path")
            target = re.sub(
                r"::impl&%(\d+)", r"::{impl#\1}", target.replace("::t%", "::")
            )
            out["calls"][span(location), target] += 1
        if body[:2] == ["Pattern", "Var"]:
            binding = body[2]
            record_binding(
                out,
                location,
                field(binding, ":name")[1],
                vir_type(field(binding, ":typ")),
            )
    for child in node:
        visit_vir(child, out)


def function_name(path):
    parts = path.split("::")
    return "::".join(parts[-2:]) if parts[-1] == "clone" else parts[-1]


def vir_functions(text):
    result = {}
    for form in forms(text):
        if form[:1] != ["@"] or form[2][:1] != ["Function"]:
            continue
        function = form[2]
        path = field(function[1], ":path")
        if not path.startswith("exact_sum::") or field(function, ":mode") != "Exec":
            continue
        name = function_name(re.sub(r"impl&%(\d+)", r"{impl#\1}", path))
        if name in result or field(function, ":body") == "None":
            raise ValueError(f"duplicate or missing executable body for {path}")
        out = result[name] = inventory()
        for _, location, param in field(function, ":params"):
            record_binding(
                out, location, field(param, ":name")[1], vir_type(field(param, ":typ"))
            )
        visit_vir(field(function, ":body"), out)
    return result


def thir_type(ty):
    return (
        ty.replace("std::option::", "")
        .replace("exact_sum::", "")
        .replace("&'{erased} ", "&")
    )


def thir_function(body):
    out = inventory()
    expressions = list(EXPR_HEADER.finditer(body))
    if len(expressions) != len(re.findall(r"^ *Expr \{$", body, re.M)):
        raise ValueError("unparsed THIR expression")
    for expr in expressions:
        ty, location, kind, callee = expr.groups()
        if kind == "Scope" or (kind == "ZstLiteral" and ty.startswith("FnDef(")):
            continue  # rustc's region wrappers and the FnDef recorded at the call below
        if kind == "Cast":
            # VIR anchors the cast's result type on its operand.
            operand = re.match(
                r" \{\n\s*source:\s*Expr \{\n\s*ty: [^\n]+\n"
                r"\s*temp_scope_id: [^\n]+\n\s*span: ([^\n]+)",
                body[expr.end() :],
            )
            location = operand[1]
        out["expressions"].add((span(location), thir_type(ty)))
        if kind == "Call":
            target = re.search(r"FnDef\(DefId\(\d+:\d+ ~ (.*?)\),", callee)[1]
            target = re.sub(r"\[[a-f0-9]+\]", "", target)
            target = re.sub(r"^norn_(?:ir|graph)::exact_sum::", "exact_sum::", target)
            out["calls"][span(location), target] += 1
    bindings = list(BINDING.finditer(body))
    if len(bindings) != len(re.findall(r"^ *Binding \{$", body, re.M)):
        raise ValueError("unparsed THIR binding")
    for binding in bindings:
        # Each Binding belongs to the immediately preceding Pat declaration.
        pat = body.rindex("Pat {\n", 0, binding.start())
        location = re.search(r"Pat \{\n\s*ty: [^\n]+\n\s*span: ([^\n]+)", body[pat:])[1]
        record_binding(out, location, binding[1], thir_type(binding[2]))
    return out


def thir_functions(text):
    result = {}
    sections = re.split(r"^DefId\(0:\d+ ~ ([^\n]+?)\):\n", text, flags=re.M)
    for header, body in zip(sections[1::2], sections[2::2], strict=True):
        if not re.fullmatch(
            r"(?:norn_(?:ir|graph)\[[0-9a-f]+\]::)?exact_sum(?:\[[0-9a-f]+\])?"
            r"::(?:\{impl#\d+\}::)?\w+",
            header,
        ):
            continue
        name = function_name(header)
        if name in EXPECTED:
            if name in result:
                raise ValueError(f"duplicate THIR function {header}")
            result[name] = thir_function(body)
    return result


def main(vir_path, thir_path):
    verified = vir_functions(Path(vir_path).read_text())
    shipped = thir_functions(Path(thir_path).read_text())
    if verified.keys() != EXPECTED or shipped.keys() != EXPECTED:
        raise ValueError(
            f"incomplete functions: VIR {verified.keys()}, THIR {shipped.keys()}"
        )
    for name in sorted(EXPECTED):
        for category, left in verified[name].items():
            right = shipped[name][category]
            if left != right:
                raise ValueError(
                    f"{name} {category}: VIR only {left - right}, THIR only {right - left}"
                )
        print(
            name,
            {
                k: len(v) if isinstance(v, set) else v.total()
                for k, v in verified[name].items()
            },
        )


if __name__ == "__main__":
    main(*sys.argv[1:])
