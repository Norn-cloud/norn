"""Compare the executable IR of the two G0 scalar functions (Verus/rustc 1.100).

Decode only the constructs admitted here. A new binding pattern, statement,
expression or compiler format requires extending the decoder before qualification.
"""
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
