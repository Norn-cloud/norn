"""Compare the executable IR of inline Verus functions (Verus/rustc 1.100).

Decode only the constructs admitted here. A new binding pattern, statement,
expression or compiler format requires extending the decoder before qualification.
"""
from collections import Counter
import json
import re


def require(ok, message):
    if not ok:
        raise ValueError(message)


def one(pattern, text):
    found = re.findall(pattern, text, re.MULTILINE | re.DOTALL)
    require(len(found) == 1, f"expected one {pattern!r}, found {len(found)}")
    return found[0]


def sexp(text):
    """The pinned --log vir printer emits one S-expression per declaration."""
    stack = [[]]
    for token in re.findall(r'"(?:\\.|[^"\\])*"|[()]|[^\s()"]+|\S', text):
        if token == "(":
            node = []
            stack[-1].append(node)
            stack.append(node)
        elif token == ")":
            require(len(stack) > 1, "unmatched VIR close")
            stack.pop()
        else:
            stack[-1].append(token)
    require(len(stack) == 1 and len(stack[0]) == 1, "incomplete VIR declaration")
    return stack[0][0]


def field(node, key):
    require(node.count(key) == 1, f"missing or duplicate VIR {key}")
    return node[node.index(key) + 1]


def vir_type(node):
    match node:
        case ["Typ", "Bool"]:
            return "bool"
        case ["Typ", "Int", ["IntRange", sign, width]] if sign in ("U", "I") and width.isdigit():
            return sign.lower() + width
        case ["Typ", "Datatype", ["Dt", "Path", "core::t%option::t%Option"], [inner], []]:
            return f"core::option::Option<{vir_type(inner)}>"
    raise ValueError(f"unsupported VIR type: {node}")


def var_name(node):
    match node:
        case ["VarIdent", name, ["VarIdentDisambiguate", "RustcId", ident]] if ident.isdigit():
            return json.loads(name)
    raise ValueError(f"unsupported VIR binding: {node}")


def vir_expr(node):
    match node:
        case ["@@", [">", op, *args], typ]:
            ty = vir_type(typ)
        case _:
            raise ValueError(f"missing typed VIR expression: {node}")
    match op, args:
        case "Block", [[], child]:
            return (ty, "Block", [vir_expr(child)])
        case "Const", [["Constant", "Int", n]]:
            return (ty, "literal:" + n, [])
        case "ReadPlace", [["@@", ["Place", "Local", var], place_ty],
                           ["UnfinalizedReadKind", ":preliminary_kind", ["ReadKind", "Copy"], ":id", _]]:
            require(ty == vir_type(place_ty), "read and local place type differ")
            return (ty, "read:" + var_name(var), [])
        case ("Logical" | "Binary" | "Unary"), [[kind, operator], *children]:
            require((op, kind, len(children)) in {
                ("Logical", "LogicalOp", 2), ("Binary", "BinaryOp", 2), ("Unary", "UnaryOp", 1)
            }, "unsupported VIR operator")
            return (ty, operator, [vir_expr(c) for c in children])
        case "Call", [":target", ["CallTarget", "Fun", ["CallTargetKind", "Static"],
                      ["Fun", ":path", path], [], [], ["CallTargetAttrs", ":autospec",
                      ["AutospecUsage", "Final"], ":const_var", "false", ":assume_external_allowed", "false"]],
                      ":args", children, ":post_args", "None", ":body", "None"]:
            # The pinned VIR printer encodes Rust's type namespace and impl disambiguators.
            path = re.sub(r"::impl&%(\d+)(?=::)", r"::{impl#\1}", path.replace("::t%", "::"))
            return (ty, "call:" + path, [vir_expr(c) for c in children])
    raise ValueError(f"unsupported VIR exec expression: {op} {args}")


def canonical(root, locals_):
    callees, expressions = [], []

    def visit(node):
        ty, op, children = node
        if op.startswith("call:"):
            callees.append((len(expressions), op.removeprefix("call:")))
            op = "call"
        expressions.append((op, ty, len(children)))
        for child in children:
            visit(child)

    visit(root)
    return dict(callees=callees, expr_types=expressions, local_types=locals_)


def vir_functions(text, names):
    functions = {}
    for decl in re.findall(r"^\(Function\n.*?(?=\n\n|\Z)", text, re.MULTILINE | re.DOTALL):
        header = re.match(r"\(Function\s+\(Fun :path norn_kernel_version::(\w+)\)", decl)
        if not header or header[1] not in names:
            continue
        name = header[1]
        fn = sexp(decl)
        require(field(fn, ":mode") == "Exec" and name not in functions, f"invalid VIR {name}")
        locals_ = [(var_name(field(p, ":name")), vir_type(field(p, ":typ"))) for p in field(fn, ":params")]
        functions[name] = canonical(vir_expr(field(fn, ":body")), locals_)
    return functions


def thir_type(ty):
    require(re.fullmatch(r"(?:bool|[ui](?:8|16|32|64|128)|(?:std|core)::option::Option<u64>)", ty),
            f"unsupported THIR type {ty!r}")
    return ty.replace("std::option::Option", "core::option::Option")


def entries(body, section, item):
    content = one(rf"^    {section}: \[\n(.*?)^    \],$", body)
    records = re.findall(rf"^        {item} \{{\n(.*?)^        \}},\n", content, re.MULTILINE | re.DOTALL)
    require("".join(f"        {item} {{\n{s}        }},\n" for s in records) == content,
            f"unparsed THIR {section}")
    return records


def check_thir_record(body):
    lines = body.rstrip().splitlines()
    require(len(lines) >= 2 and lines[0] == "Thir {" and lines[-1] == "}",
            "incomplete THIR record")
    pairs = {"(": ")", "[": "]", "{": "}"}
    closing = []
    # Debug strings (including source paths) may contain literal delimiters.
    for token in re.findall(r'"(?:\\.|[^"\\])*"|["(){}\[\]]', "\n".join(lines[1:-1])):
        if token in pairs:
            closing.append(pairs[token])
        elif not token.startswith('"'):
            require(closing and closing.pop() == token, "unbalanced THIR record")
        else:
            require(len(token) > 1, "unterminated THIR string")
    require(not closing, "unbalanced THIR record")


def callee(record, indent):
    # A field ends at its next sibling, the enclosing struct's close, or the
    # end of the extracted Expr. Keep multiline types intact for fullmatch.
    prefix = " " * indent
    ty = one(rf"^{prefix}ty: (.*?),\n(?={prefix}\w+:|{prefix[:-4]}\}},|\Z)", record)
    match = re.fullmatch(
        r"FnDef\(\s*DefId\((\d+:\d+) ~ (\w+)\[([0-9a-f]+)\]((?:::(?:\w+|\{impl#\d+\}))+)"
        r"\),\s*Binder \{\s*value: \[\],\s*bound_vars: \[\],\s*\},\s*\)", ty)
    require(match, f"expected a top-level THIR FnDef, got {ty!r}")
    return match.groups()


def thir_function(body):
    check_thir_record(body)
    # G0's executable locals are the parameters. A nonempty stmt inventory
    # (including a let declaration) must never silently disappear from the comparison.
    require({"    stmts: [],", "    arms: [],"} <= set(body.splitlines()), "unsupported THIR statements")
    locals_, bindings = [], {}
    for param in entries(body, "params", "Param"):
        name = one(r'^                        name: "(\w+)",$', param)
        ty = thir_type(one(r"^            ty: ([^\n]+),$", param))
        ident = one(r"var: LocalVarId\(\s*(HirId\([^\n]+\)),\s*\)", param)
        require(thir_type(one(r"^                        ty: ([^\n]+),$", param)) == ty,
                "THIR binding and parameter type differ")
        bindings[ident] = name
        locals_.append((name, ty))

    block, = entries(body, "blocks", "Block")
    require("            stmts: []," in block, "nonempty THIR block")
    tail = one(r"expr: Some\(\s*e(\d+),\s*\)", block)
    nodes, used = [], set()

    def child(index):
        index = int(index)
        require(index < len(nodes) and index not in used, "invalid THIR expression reference")
        used.add(index)
        return nodes[index]

    for expr in entries(body, "exprs", "Expr"):
        kind = one(r"^            kind: (\w+) \{$", expr)
        if kind == "ZstLiteral":
            nodes.append(("FnDef", callee(expr, 12), []))
            continue
        ty = thir_type(one(r"^            ty: ([^\n]+),$", expr))
        if kind == "Scope":
            value = child(one(r"value: e(\d+),", expr))
            require(value[0] == ty, "THIR scope/value type mismatch")
            nodes.append(value)
            continue
        if kind == "Block":
            require("block: b0," in expr, "unknown THIR block")
            op, args = "Block", [child(tail)]
        elif kind == "VarRef":
            ident = one(r"id: LocalVarId\(\s*(HirId\([^\n]+\)),\s*\)", expr)
            op, args = "read:" + bindings[ident], []
        elif kind == "Literal":
            require("neg: false," in expr, "unsupported negative THIR literal")
            op, args = "literal:" + one(r"Pu128\(\s*(\d+),\s*\)", expr), []
        elif kind in ("Unary", "LogicalOp", "Binary"):
            op = one(r"op: (\w+),", expr)
            fields = ["arg"] if kind == "Unary" else ["lhs", "rhs"]
            args = [child(one(rf"{key}: e(\d+),", expr)) for key in fields]
        elif kind == "Call":
            identity = callee(expr, 16)
            require(child(one(r"fun: e(\d+),", expr)) == ("FnDef", identity, []), "THIR callee mismatch")
            _, crate, _, path = identity
            op = "call:" + crate + path
            raw = one(r"args: \[\s*((?:e\d+,\s*)*)\],", expr)
            args = [child(i) for i in re.findall(r"e(\d+)", raw)]
        else:
            raise ValueError(f"unsupported THIR exec expression: {kind}")
        nodes.append((ty, op, args))
    require(nodes and used == set(range(len(nodes) - 1)), "incomplete THIR expression inventory")
    return canonical(nodes[-1], locals_)


def thir_functions(lines, names):
    functions = {}
    name = None
    body = []
    for line in lines:
        if line.startswith("DefId("):
            if name:
                functions[name] = thir_function("".join(body))
            header = re.fullmatch(r"DefId\(0:\d+ ~ norn_kernel\[[0-9a-f]+\]::version_check::(\w+)\):\n", line)
            name = header[1] if header and header[1] in names else None
            require(name not in functions, f"duplicate THIR function {name}")
            body = []
        elif name:
            body.append(line)
    if name:
        functions[name] = thir_function("".join(body))
    return functions


def compare(vir, thir, names):
    verified = vir_functions(vir, names)
    shipped = thir_functions(thir, names)
    require(set(names) == verified.keys() == shipped.keys(),
            f"incomplete compiler inventory: VIR {list(verified)}, THIR {list(shipped)}")
    for name in names:
        require(verified[name] == shipped[name],
                f"{name}: VIR/THIR mismatch: {verified[name]} vs {shipped[name]}")
    return verified


# --- D107 typed-span comparison for norn_ir::artifact_governance ----------------------------

ARTIFACT_CRATE = "norn_ir_artifact_governance"
ARTIFACT_DATATYPES = {
    f"{ARTIFACT_CRATE}::t%ArtifactKind": "ArtifactKind",
    f"{ARTIFACT_CRATE}::t%ArtifactLifecycle": "ArtifactLifecycle",
}
TREE_EXPR_HEADER = re.compile(
    r"^ *Expr \{\n\s*ty: (.+)\n\s*temp_scope_id: [^\n]+\n\s*span: ([^\n]+)\n"
    r"\s*kind: *\n\s*([^\s({]+)(?: \{\n\s*ty: ([^\n]+))?",
    re.M,
)
TREE_LITERAL_PATTERN = re.compile(
    r"^ *Pat \{\n\s*ty: ([^\n]+)\n\s*span: ([^\n]+)\n\s*kind: PatKind \{\n\s*Constant \{\n\s*value: ([^\n]+)\n",
    re.M,
)
TREE_BINDING = re.compile(
    r'^ *Binding \{\n\s*name: "(\w+)"\n\s*mode: .*\n\s*var: .*\n\s*ty: (.*)$',
    re.M,
)


def _artifact_span(location):
    match = re.search(r"artifact_governance\.rs:(\d+:\d+: \d+:\d+)", location)
    require(match is not None, f"missing artifact_governance span in {location!r}")
    return match[1]


def _artifact_forms(text):
    stack = [[]]
    for token in re.findall(r';;[^\n]*|"(?:\\.|[^"\\])*"|[()]|[^\s()"]+', text):
        if token.startswith(";;"):
            continue
        if token == "(":
            stack.append([])
        elif token == ")":
            require(len(stack) > 1, "unmatched VIR close")
            node = stack.pop()
            stack[-1].append(node)
        else:
            stack[-1].append(token)
    require(len(stack) == 1, "incomplete VIR")
    return stack[0]


def _artifact_vir_type(node):
    match node:
        case ["Typ", "Bool"]:
            return "bool"
        case ["Typ", "Datatype", ["Dt", "Path", path], [], []] if path in ARTIFACT_DATATYPES:
            return ARTIFACT_DATATYPES[path]
        case ["Typ", "Decorate", ["TypDecoration", "Ref"], "None", inner]:
            return "&" + _artifact_vir_type(inner)
    raise ValueError(f"unsupported artifact_governance VIR type {node}")


def _artifact_thir_type(ty):
    ty = ty.replace("&'{erased} ", "&")
    for prefix in ("norn_ir::artifact_governance::", "crate::artifact_governance::", "artifact_governance::"):
        ty = ty.replace(prefix, "")
    require(ty in {"bool", "ArtifactKind", "ArtifactLifecycle", "&ArtifactKind", "&ArtifactLifecycle"},
            f"unsupported artifact_governance THIR type {ty!r}")
    return ty


def _artifact_inventory():
    return {"expressions": set(), "locals": Counter(), "calls": Counter(), "literal_patterns": Counter()}


def _record_artifact_binding(out, location, variable, ty):
    out["locals"][_artifact_span(location).split(": ")[0], variable.strip('"'), ty] += 1


def _artifact_callee(path):
    path = re.sub(r"impl&%(\d+)", r"{impl#\1}", path.replace("::t%", "::"))
    return re.sub(r"\[[0-9a-f]+\]", "", path).removeprefix("norn_ir::artifact_governance::")


def _visit_artifact_vir(node, out):
    if not isinstance(node, list):
        return
    if node[:1] == ["Arm"]:
        require(field(node, ":guard")[2:] == [[">", "Const", ["Constant", "Bool", "true"]], ["Typ", "Bool"]],
                "unexpected match guard")
        _visit_artifact_vir(field(node, ":pattern"), out)
        _visit_artifact_vir(field(node, ":body"), out)
        return
    if node[:1] == ["@@"]:
        _, location, body, ty = node
        if body[:2] == [">", "Ghost"]:
            return
        if body[:2] == ["Pattern", "Expr"]:
            match body[2]:
                case ["@@", _, [">", "Const", ["Constant", "Bool", value]], ["Typ", "Bool"]]:
                    out["literal_patterns"][_artifact_span(location), _artifact_vir_type(ty), value] += 1
                    return
            raise ValueError(f"unsupported VIR literal pattern {body}")
        if body[:1] in ([">"], ["Place"]):
            out["expressions"].add((_artifact_span(location), _artifact_vir_type(ty)))
        if body[:2] == [">", "Call"]:
            out["calls"][_artifact_span(location), _artifact_callee(field(field(body, ":target")[3], ":path"))] += 1
        if body[:2] == ["Pattern", "Var"]:
            binding = body[2]
            _record_artifact_binding(out, location, field(binding, ":name")[1],
                                     _artifact_vir_type(field(binding, ":typ")))
    for child in node:
        _visit_artifact_vir(child, out)


def _artifact_vir_functions(text, names):
    result = {}
    for form in _artifact_forms(text):
        if form[:1] != ["@"] or form[2][:1] != ["Function"]:
            continue
        function = form[2]
        path = field(function[1], ":path")
        if not path.startswith(ARTIFACT_CRATE + "::") or field(function, ":mode") != "Exec":
            continue
        name = path.rsplit("::", 1)[-1]
        if name not in names:
            continue
        require(name not in result and field(function, ":body") != "None",
                f"duplicate or missing executable body for {path}")
        out = result[name] = _artifact_inventory()
        for _, location, param in field(function, ":params"):
            _record_artifact_binding(out, location, field(param, ":name")[1],
                                     _artifact_vir_type(field(param, ":typ")))
        _visit_artifact_vir(field(function, ":body"), out)
    return result


def _artifact_thir_function(body):
    out = _artifact_inventory()
    expressions = list(TREE_EXPR_HEADER.finditer(body))
    require(len(expressions) == len(re.findall(r"^ *Expr \{$", body, re.M)), "unparsed THIR expression")
    for expr in expressions:
        ty, location, kind, target = expr.groups()
        if kind == "Scope" or (kind == "ZstLiteral" and ty.startswith("FnDef(")):
            continue
        require(kind != "Cast", "unexpected THIR cast")
        out["expressions"].add((_artifact_span(location), _artifact_thir_type(ty)))
        if kind == "Call":
            match = re.search(r"FnDef\(DefId\(\d+:\d+ ~ (.*?)\),", target or "")
            require(match is not None, f"unparsed THIR call target {target!r}")
            out["calls"][_artifact_span(location), _artifact_callee(match[1])] += 1
    for ty, location, value in TREE_LITERAL_PATTERN.findall(body):
        out["literal_patterns"][_artifact_span(location), _artifact_thir_type(ty), value] += 1
    require(len(TREE_LITERAL_PATTERN.findall(body)) == body.count("Constant {"), "unparsed THIR constant pattern")
    bindings = list(TREE_BINDING.finditer(body))
    require(len(bindings) == len(re.findall(r"^ *Binding \{$", body, re.M)), "unparsed THIR binding")
    for binding in bindings:
        pat = body.rindex("Pat {\n", 0, binding.start())
        location = re.search(r"Pat \{\n\s*ty: [^\n]+\n\s*span: ([^\n]+)", body[pat:])[1]
        _record_artifact_binding(out, location, binding[1], _artifact_thir_type(binding[2]))
    return out


def _artifact_thir_functions(text, names):
    result = {}
    sections = re.split(r"^DefId\(0:\d+ ~ ([^\n]+?)\):\n", text, flags=re.M)
    for header, body in zip(sections[1::2], sections[2::2], strict=True):
        match = re.fullmatch(r"norn_ir\[[0-9a-f]+\]::artifact_governance::(\w+)", header)
        if not match or match[1] not in names:
            continue
        name = match[1]
        require(name not in result, f"duplicate THIR function {header}")
        result[name] = _artifact_thir_function(body)
    return result


def compare_artifact_governance(vir_text, thir_text, names):
    verified = _artifact_vir_functions(vir_text, names)
    shipped = _artifact_thir_functions(thir_text, names)
    require(set(names) == verified.keys() == shipped.keys(),
            f"incomplete compiler inventory: VIR {sorted(verified)}, THIR {sorted(shipped)}")
    counts = {}
    for name in sorted(names):
        for category, left in verified[name].items():
            right = shipped[name][category]
            require(left == right, f"{name} {category}: VIR only {left - right}, THIR only {right - left}")
        counts[name] = {k: len(v) if isinstance(v, set) else v.total() for k, v in verified[name].items()}
    return counts
