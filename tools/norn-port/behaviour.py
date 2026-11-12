"""Hook registration ledger and source evidence shared by the code ontology.

Only source text is read: nothing here imports a source module or contacts a
site. The ledger is conservative: a nonliteral registration is a typed gap.
"""
from __future__ import annotations

import ast
import hashlib

HOOKS = {"doc_events", "scheduler_events", "has_permission", "permission_query_conditions",
         "override_doctype_class", "extend_doctype_class", "before_request", "after_request",
         "after_install", "after_migrate", "after_sync", "customer_loyalty_points"}


def production(path):
    return not any(part in {"tests", "__pycache__"} for part in path.parts) and not path.name.startswith("test_")


def evidence(path, raw, revision, line=1, pointer=""):
    return {"path": path, "line": line, "pointer": pointer, "revision": revision,
            "sha256": hashlib.sha256(raw).hexdigest()}


def literal_targets(node, pointer=""):
    """Walk literal structure with original AST locations, without execution."""
    if isinstance(node, ast.Constant) and isinstance(node.value, str):
        yield pointer, node.value, node
    elif isinstance(node, ast.Dict):
        for key, value in zip(node.keys, node.values):
            if isinstance(key, ast.Constant):
                yield from literal_targets(value, pointer + "/" + str(key.value))
    elif isinstance(node, (ast.List, ast.Tuple)):
        for index, value in enumerate(node.elts):
            yield from literal_targets(value, pointer + "/" + str(index))


def target_literal(node):
    if not isinstance(node, ast.Constant) or not isinstance(node.value, str):
        return False
    parts = node.value.split(".")
    return len(parts) > 1 and all(part.isidentifier() for part in parts)


def hook_registrations(tree):
    """One conservative registration ledger for source capture and inventory.

    Static callback containers retain their hook name and native pointer. All
    nonliteral assignments, mutations and top-level control flow remain gaps,
    including unknown hook names. No dynamic registration is evaluated.
    """
    for node in tree.body:
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)):
            continue  # Complete function/method units are inventoried separately.
        names = []
        if isinstance(node, ast.Assign):
            names = [target.id for target in node.targets if isinstance(target, ast.Name)]
        elif isinstance(node, ast.AnnAssign) and isinstance(node.target, ast.Name):
            names = [node.target.id]
        literals = [literal for literal in ast.walk(node) if target_literal(literal)]
        static = False
        if names:
            try:
                ast.literal_eval(node.value)
                static = True
            except (ValueError, TypeError, SyntaxError, AttributeError):
                pass
        if static and (literals or any(name in HOOKS for name in names)):
            for name in names:
                for pointer, callback, literal in literal_targets(node.value):
                    yield {"kind": "hook_target", "name": name, "pointer": pointer,
                           "callback": callback, "node": literal, "dynamic": False}
        elif not static and not isinstance(node, (ast.Import, ast.ImportFrom)):
            if isinstance(node, ast.Expr) and isinstance(node.value, ast.Constant):
                continue  # Module documentation is not a registration.
            roots = sorted({n.id for n in ast.walk(node) if isinstance(n, ast.Name) and n.id in HOOKS})
            name = "+".join(names or roots) or "unknown_hook"
            yield {"kind": "dynamic_hook", "name": name, "node": node}
            for literal in literals:
                yield {"kind": "hook_target", "name": name, "pointer": "/dynamic",
                       "callback": literal.value, "node": literal, "dynamic": True}
        elif isinstance(node, ast.ImportFrom) and node.module:
            # Imported hook values can contain registrations unavailable to the
            # local AST. Capture the module and retain an unresolved binding.
            for alias in node.names:
                name = alias.asname or alias.name
                if name in HOOKS:
                    yield {"kind": "dynamic_hook", "name": name, "node": node,
                           "capture": node.module + ".__hook_import__"}
