"""Conservative AST recognizer shared by extraction and original-clause parity.

A clause is a source unit inside a controller, never proof of its enclosing
method. No imports, document construction or Frappe database state are executed.
"""
from __future__ import annotations

import ast

from json_compare import equal


def dotted(node):
    if isinstance(node, ast.Name):
        return node.id
    if isinstance(node, ast.Attribute):
        return dotted(node.value) + "." + node.attr
    return ast.unparse(node)


def validation_clause(node):
    """Recognize only `if self.a and self.b: frappe.throw(_("literal"))`.

    Link/text domains must separately be established by metadata and exact mapped
    field bindings. Python truthiness is never claimed for arbitrary values.
    """
    if not isinstance(node, ast.If) or node.orelse or not equal(len(node.body), 1):
        return None
    test = node.test
    if not isinstance(test, ast.BoolOp) or not isinstance(test.op, ast.And):
        return None
    fields = []
    absent = all(isinstance(part, ast.UnaryOp) and isinstance(part.op, ast.Not) for part in test.values)
    for part in test.values:
        if absent:
            part = part.operand
        if not (isinstance(part, ast.Attribute) and isinstance(part.value, ast.Name)
                and equal(part.value.id, "self")):
            return None
        fields.append(part.attr)
    statement = node.body[0]
    if not isinstance(statement, ast.Expr) or not isinstance(statement.value, ast.Call):
        return None
    call = statement.value
    if not equal(dotted(call.func), "frappe.throw") or call.keywords or not equal(len(call.args), 1):
        return None
    message = call.args[0]
    if isinstance(message, ast.Call) and equal(dotted(message.func), "_") and not message.keywords and equal(len(message.args), 1):
        message = message.args[0]
    if not isinstance(message, ast.Constant) or not isinstance(message.value, str):
        return None
    return {"fields": fields, "message": message.value, **({"absent": True} if absent else {})}


def clause_program(tree, symbol, filename):
    """Compile an unchanged recognized If AST inside a minimal self wrapper."""
    method_symbol, separator, line = symbol.partition("#if:")
    if not separator:
        raise ValueError("expected a source clause symbol")
    class_name, method_name = method_symbol.split(".")
    owners = [node for node in tree.body if isinstance(node, ast.ClassDef) and equal(node.name, class_name)]
    if not equal(len(owners), 1):
        raise ValueError("ambiguous or absent controller")
    methods = [node for node in owners[0].body if isinstance(node, ast.FunctionDef) and equal(node.name, method_name)]
    if not equal(len(methods), 1):
        raise ValueError("ambiguous or absent controller method")
    # Only top-level method clauses: nested clauses require preceding control
    # flow and are deliberately refused rather than given invented reachability.
    clauses = [node for node in methods[0].body if isinstance(node, ast.If) and equal(node.lineno, int(line))]
    if not equal(len(clauses), 1) or validation_clause(clauses[0]) is None:
        raise ValueError("not an admitted independent validation clause")
    wrapper = ast.parse("def __clause__(self):\n pass\n").body[0]
    ast.copy_location(wrapper, methods[0])
    wrapper.body = [clauses[0]]  # Source AST is retained, including its locations.
    imports = ast.parse("import frappe\nfrom frappe import _\n").body
    return compile(ast.fix_missing_locations(ast.Module(body=[*imports, wrapper], type_ignores=[])), filename, "exec", dont_inherit=True)


def function_inventory(tree, *, nested=False):
    """Shared source-unit inventory for behaviour proposals and code ontology.

    Returns original AST nodes; callers add their own typed projections. Nested
    functions remain separate scopes and are never attributed to their parent.
    """
    def walk(owner, prefix=""):
        for node in owner.body:
            if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
                symbol = prefix + node.name
                yield symbol, node
                if nested:
                    yield from walk(node, symbol + ".<locals>.")
            elif isinstance(node, ast.ClassDef):
                yield from walk(node, prefix + node.name + ".")
    yield from walk(tree)


def scope_nodes(node):
    """Walk one function body without crossing nested lexical scopes."""
    yield node
    for child in ast.iter_child_nodes(node):
        if isinstance(child, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef, ast.Lambda)):
            continue
        yield from scope_nodes(child)
