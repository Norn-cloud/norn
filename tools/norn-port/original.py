"""Run a source AST dependency closure, with only the declared Frappe stub.

Invoked with Python -I in a subprocess. This is trusted-source isolation,
not an OS security sandbox for hostile source code.
"""
from __future__ import annotations

import ast
import copy
import hashlib
import json
import sys
import symtable
import types
from pathlib import Path

# -I excludes the script directory; admit only this harness's comparison module.
sys.path.insert(0, str(Path(__file__).resolve().parent))
from json_compare import equal
from python_ast import clause_program


class ValidationError(Exception):
    """The Frappe throw surface for pure validation helpers."""


def throw(message, exception=ValidationError):
    raise exception(message)


def install_frappe():
    module = types.ModuleType("frappe")
    module.ValidationError = ValidationError
    module.throw = throw
    module._ = lambda value: value
    sys.modules["frappe"] = module


def closure(path, symbol):
    """Select unchanged source nodes reachable through module globals.

    Unused imports and module-level setup are excluded. A required non-stdlib
    import is refused, never replaced with a fake implementation.
    """
    raw = path.read_bytes()
    tree = ast.parse(raw, filename=str(path))
    futures = [node for node in tree.body
               if isinstance(node, ast.ImportFrom) and equal(node.module, "__future__")]
    postponed = any(equal(alias.name, "annotations") for node in futures for alias in node.names)
    symbols = symtable.symtable(raw, str(path), "exec")
    scopes = {child.get_name(): child for child in symbols.get_children()}
    if "#if:" in symbol:
        return clause_program(tree, symbol, str(path)), hashlib.sha256(raw).hexdigest()
    if "." in symbol:
        raise ValueError("full controller methods need a document/platform harness; clause proof is separate")
    definitions = {}
    imports = {}
    for node in tree.body:
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)):
            definitions[node.name] = node
        elif isinstance(node, (ast.Assign, ast.AnnAssign)):
            targets = node.targets if isinstance(node, ast.Assign) else [node.target]
            for target in targets:
                if isinstance(target, ast.Name):
                    definitions[target.id] = node
        elif isinstance(node, (ast.Import, ast.ImportFrom)):
            for alias in node.names:
                imports[alias.asname or alias.name.split(".")[0]] = node
    selected = {}
    pending = [symbol]
    while pending:
        name = pending.pop()
        if name in selected:
            continue
        node = definitions.get(name) or imports.get(name)
        if node is None:
            # Parameters, locals, and closure placeholders are resolved by
            # Python itself. An actual missing global is a harness failure.
            continue
        selected[name] = node
        if isinstance(node, (ast.Import, ast.ImportFrom)):
            modules = [a.name for a in node.names] if isinstance(node, ast.Import) else [node.module or ""]
            if any(m.split(".")[0] not in sys.stdlib_module_names | {"frappe", "__future__"} for m in modules):
                raise ValueError(f"required source import needs a platform harness: {modules}")
        elif isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)):
            # Local names must not pull in unrelated module setup of the same
            # name. Python's own symbol table is the authority for binding.
            tables = [scopes[node.name]]
            while tables:
                table = tables.pop()
                pending.extend(item.get_name() for item in table.get_symbols()
                               if item.is_global() and item.is_referenced())
                tables.extend(table.get_children())
            if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
                expressions = [*node.decorator_list, *node.args.defaults,
                               *(item for item in node.args.kw_defaults if item is not None)]
                if not postponed:
                    arguments = [*node.args.posonlyargs, *node.args.args, *node.args.kwonlyargs,
                                 *([node.args.vararg] if node.args.vararg else []),
                                 *([node.args.kwarg] if node.args.kwarg else [])]
                    expressions.extend(arg.annotation for arg in arguments if arg.annotation is not None)
                    if node.returns is not None:
                        expressions.append(node.returns)
                pending.extend(item.id for expression in expressions for item in ast.walk(expression)
                               if isinstance(item, ast.Name) and isinstance(item.ctx, ast.Load))
        else:
            expressions = [node]
            if postponed and isinstance(node, ast.AnnAssign):
                expressions = [node.value] if node.value is not None else []
            pending.extend(n.id for expression in expressions for n in ast.walk(expression)
                           if isinstance(n, ast.Name) and isinstance(n.ctx, ast.Load))
    nodes = {id(node): node for node in [*futures, *selected.values()]}
    ordered = sorted(nodes.values(), key=lambda n: n.lineno)
    program = ast.fix_missing_locations(ast.Module(body=ordered, type_ignores=[]))
    # This harness itself uses postponed annotations; never inherit its flags.
    return compile(program, str(path), "exec", dont_inherit=True), hashlib.sha256(raw).hexdigest()


def document(value):
    if isinstance(value, dict):
        return types.SimpleNamespace(**{key: document(item) for key, item in value.items()})
    if isinstance(value, list):
        return [document(item) for item in value]
    return value


def wire(value):
    if isinstance(value, types.SimpleNamespace):
        return {key: wire(item) for key, item in vars(value).items()}
    if isinstance(value, dict):
        return {str(key): wire(item) for key, item in value.items()}
    if isinstance(value, (list, tuple)):
        return [wire(item) for item in value]
    if isinstance(value, set):
        return sorted((wire(item) for item in value), key=lambda item: json.dumps(item, sort_keys=True))
    return value


def call(program, symbol, invocation, case):
    namespace = {"__name__": "__norn_port_original__"}
    exec(program, namespace)
    arguments = copy.deepcopy(case)
    for name in invocation.get("documents", []):
        arguments[name] = document(arguments[name])
    for name in invocation.get("sets", []):
        arguments[name] = set(arguments[name])
    for name in invocation.get("tuples", []):
        arguments[name] = tuple(arguments[name])
    adapted_before = wire(arguments)
    try:
        if invocation.get("clause"):
            if "#if:" not in symbol or not equal(sorted(arguments), ["doc"]):
                raise ValueError("clause invocation requires its exact source symbol and doc input")
            result = namespace["__clause__"](arguments["doc"])
        else:
            result = namespace[symbol](**arguments)
        refusal = None
    except (ValidationError, ValueError) as error:
        result = None
        refusal = {"type": type(error).__name__, "reason": str(error)}
    adapted_after = wire(arguments)
    post_input = copy.deepcopy(case)
    for name in arguments:
        if not equal(adapted_before[name], adapted_after[name]):
            post_input[name] = adapted_after[name]
    return {"value": wire(result), "refusal": refusal, "input": post_input}


def main():
    install_frappe()
    request = json.loads(sys.stdin.readline())
    path = Path(request["path"])
    program, digest = closure(path, request["symbol"])
    print(json.dumps({"sourceSha256": digest}), flush=True)
    for line in sys.stdin:
        case = json.loads(line)
        response = call(program, request["symbol"], request.get("invocation", {}), case)
        print(json.dumps(response, ensure_ascii=True, allow_nan=False), flush=True)


if __name__ == "__main__":
    main()
