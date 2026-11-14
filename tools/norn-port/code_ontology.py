"""Offline Frappe code ontology: source evidence, never behaviour parity.

python3 tools/norn-port/code_ontology.py APP... --codegraph /path/to/codegraph
Outputs norn.code-ontology/1 JSON on stdout. Imported code is never executed.
"""
from __future__ import annotations

import argparse
import ast
from collections import Counter, defaultdict
from dataclasses import asdict, dataclass
import json
from pathlib import Path
import sys
import tempfile
from typing import Literal

# Isolated mode (-I) drops the script directory; admit only this directory.
sys.path.insert(0, str(Path(__file__).resolve().parent))
from behaviour import evidence, hook_registrations, production
from python_ast import dotted, function_inventory
from codegraph import CodeGraph, CodeGraphRust, VERSION

Relation = Literal["reads", "writes", "triggers_on", "calls", "schedules", "throws_if", "exposes_api"]
Confidence = Literal["resolved", "unresolved"]
STANDARD_FIELDS = {"name", "owner", "creation", "modified", "modified_by", "docstatus", "idx", "doctype", "parent", "parenttype", "parentfield"}
HOOKS = {"doc_events", "scheduler_events", "permission_query_conditions", "has_permission"}
DOC_CALLS = {"frappe.get_doc", "frappe.new_doc"}
DB_CALLS = {"frappe.db.get_value", "frappe.db.set_value", "frappe.db.get_all", "frappe.db.get_list", "frappe.get_all", "frappe.get_list"}


@dataclass(frozen=True)
class Fact:
    handler: str
    relation: Relation
    entity: str | None
    field: str | None
    target: str | None
    condition: str | None
    source: dict
    confidence: Confidence
    diagnostic: str | None
    script_line: int | None = None


def literal(node):
    try:
        return ast.literal_eval(node)
    except (ValueError, TypeError, SyntaxError):
        return None


def module_name(path):
    parts = list(Path(path).with_suffix("").parts)
    # Standard Frappe apps have app/app/module.py; package starts at inner app.
    if len(parts) > 1 and parts[0] == parts[1]:
        parts.pop(0)
    if parts[-1] == "__init__":
        parts.pop()
    return ".".join(parts)


class FrappeRules:
    def __init__(self, sources, graph, revision="working-tree"):
        self.sources, self.graph, self.revision = sources, graph, revision
        self.facts = []
        self.handlers = {}
        self.catalog = {}
        self.controllers = {}
        self.trees = {}
        self.symbols = defaultdict(list)
        self.hook_bindings = defaultdict(set)
        self.node_handlers = {}
        self.document_names = defaultdict(set)
        self.resolved_call_sites = defaultdict(list)
        self.graph_locations = defaultdict(list)
        for node in graph["nodes"]:
            self.graph_locations[(node["source_file"], node["start_line"], node["name"])].append(node)
        self.script_documents = []
        self.diagnostics = []

    def source(self, path, line=1, pointer=""):
        return evidence(path, self.sources[path], self.revision, line, pointer)

    def emit(self, handler, relation, path, line, entity=None, field=None, target=None,
             condition=None, diagnostic=None, pointer="", script_line=None):
        if entity is not None and entity not in self.catalog:
            diagnostic = diagnostic or "unknown_entity"
        if field is not None and (entity is None or entity not in self.catalog or field not in self.catalog[entity] | STANDARD_FIELDS):
            diagnostic = diagnostic or "unknown_field"
        if relation in {"reads", "writes", "triggers_on"} and entity is None:
            diagnostic = diagnostic or "dynamic_entity"
        self.facts.append(Fact(handler, relation, entity, field, target, condition,
                               self.source(path, line, pointer), "unresolved" if diagnostic else "resolved", diagnostic, script_line))

    def handler(self, path, symbol, line, kind="function"):
        identity = path + ":" + symbol
        self.handlers[identity] = {"id": identity, "symbol": symbol, "kind": kind, "source": self.source(path, line)}
        return identity

    def prepare(self):
        for path, raw in sorted(self.sources.items()):
            if path.endswith(".json"):
                try:
                    value = json.loads(raw)
                except (ValueError, UnicodeError):
                    self.diagnostics.append({"code": "invalid_metadata", "source": self.source(path)})
                    continue
                for index, row in enumerate(value if isinstance(value, list) else [value]):
                    if not isinstance(row, dict):
                        continue
                    if row.get("doctype") == "DocType" or ("fields" in row and "name" in row and not row.get("doctype")):
                        name = row.get("name")
                        if isinstance(name, str):
                            fields = {f["fieldname"] for f in (row.get("fields") or []) if isinstance(f, dict) and isinstance(f.get("fieldname"), str)}
                            if name in self.catalog and self.catalog[name] != fields:
                                raise ValueError("conflicting DocType metadata: " + name)
                            self.catalog[name] = fields
                            self.controllers[str(Path(path).with_suffix(".py"))] = name
                    if row.get("doctype") == "Client Script" and isinstance(row.get("script"), str):
                        self.script_documents.append((path, row, f"/{index}/script" if isinstance(value, list) else "/script"))
            if path.endswith(".py"):
                try:
                    self.trees[path] = ast.parse(raw, filename=path)
                except (SyntaxError, UnicodeError) as error:
                    self.diagnostics.append({"code": "python_parse_error", "source": self.source(path, getattr(error, "lineno", 1) or 1)})
        for path, tree in self.trees.items():
            for symbol, node in function_inventory(tree, nested=True):
                identity = self.handler(path, symbol, node.lineno, "controller_method" if path in self.controllers and "." in symbol else "function")
                self.symbols[module_name(path) + "." + symbol].append(identity)
                # Exact file + line binds tree-sitter identity to the existing
                # Python inventory. Never fuzzy-match names across modules.
                for line in {node.lineno, *(d.lineno for d in node.decorator_list)}:
                    for item in self.graph_locations[(path, line, node.name)]:
                        if item["kind"] in {"function", "method"}:
                            self.node_handlers[item["id"]] = identity
        for edge in self.graph["links"]:
            if edge.get("relation", "").lower() == "calls" and edge["source"] in self.node_handlers and edge["target"] in self.node_handlers:
                self.resolved_call_sites[(self.node_handlers[edge["source"]], edge.get("line"))].append(self.node_handlers[edge["target"]])

    def hooks(self):
        for path, tree in self.trees.items():
            if Path(path).name != "hooks.py":
                continue
            for item in hook_registrations(tree):
                if item["name"] not in HOOKS:
                    continue
                node, name = item["node"], item["name"]
                if item["kind"] == "dynamic_hook":
                    handler = self.handler(path, name + "@" + str(node.lineno), node.lineno, "hook")
                    self.emit(handler, "schedules" if name == "scheduler_events" else "triggers_on", path, node.lineno, diagnostic="dynamic_hook")
                    continue
                callback = item["callback"]
                targets = self.symbols.get(callback, [])
                handler = targets[0] if len(targets) == 1 else self.handler(path, callback + "@" + str(node.lineno), node.lineno, "hook")
                pointer = item["pointer"].split("/")[1:]
                entity = pointer[0] if name != "scheduler_events" and pointer else None
                if entity == "*":
                    entity = None
                diagnostic = "dynamic_hook" if item.get("dynamic") else ("unresolved_handler" if len(targets) != 1 else None)
                self.emit(handler, "schedules" if name == "scheduler_events" else "triggers_on", path, node.lineno,
                          entity=entity, target=name + item["pointer"], diagnostic=diagnostic,
                          pointer=item["pointer"])
                if len(targets) == 1 and name != "scheduler_events":
                    # Every document registration constrains a shared callback,
                    # including wildcard, missing metadata and dynamic targets.
                    self.hook_bindings[handler].add(entity if entity in self.catalog and not diagnostic else None)

    def python(self):
        for path, tree in self.trees.items():
            module = module_name(path)
            for symbol, node in function_inventory(tree, nested=True):
                identity = path + ":" + symbol
                bindings = {arg.arg: None for arg in node.args.posonlyargs + node.args.args + node.args.kwonlyargs}
                entities = self.hook_bindings.get(identity, set())
                args = node.args.posonlyargs + node.args.args
                if path in self.controllers and "." in symbol and ".<locals>." not in symbol and args and args[0].arg == "self":
                    bindings["self"] = self.controllers[path]
                if len(entities) == 1 and args and args[0].arg == "doc":
                    bindings["doc"] = next(iter(entities))
                elif entities and args and args[0].arg == "doc":
                    bindings["doc"] = None
                for decorator in node.decorator_list:
                    if dotted(decorator.func if isinstance(decorator, ast.Call) else decorator) == "frappe.whitelist":
                        self.emit(identity, "exposes_api", path, decorator.lineno, target=module + "." + symbol)
                # Inventory is shared with behaviour.py; this visitor adds only
                # source-specific edges and conservative local receiver typing.
                self.statements(path, identity, node.body, bindings, [])

    def statements(self, path, handler, statements, bindings, guards):
        for statement in statements:
            # Remember document aliases before any control-flow invalidation.
            self.document_names[handler].update(name for name, entity in bindings.items() if entity is not None)
            if isinstance(statement, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)):
                continue
            if isinstance(statement, ast.If):
                self.expression(path, handler, statement.test, bindings, guards)
                left, right = dict(bindings), dict(bindings)
                self.statements(path, handler, statement.body, left, guards + [ast.unparse(statement.test)])
                self.statements(path, handler, statement.orelse, right, guards + ["not (" + ast.unparse(statement.test) + ")"])
                for key in left.keys() | right.keys():
                    bindings[key] = left.get(key) if key in left and key in right and left[key] == right[key] else None
                continue
            if isinstance(statement, (ast.Assign, ast.AnnAssign)):
                value = statement.value
                self.expression(path, handler, value, bindings, guards)
                targets = statement.targets if isinstance(statement, ast.Assign) else [statement.target]
                entity = None
                if isinstance(value, ast.Call) and dotted(value.func) in DOC_CALLS:
                    entity = literal(value.args[0]) if value.args else literal(next((k.value for k in value.keywords if k.arg == "doctype"), None))
                    if isinstance(entity, dict):
                        entity = entity.get("doctype")
                    if not isinstance(entity, str):
                        entity = None
                elif isinstance(value, ast.Name):
                    entity = bindings.get(value.id)
                for target in targets:
                    self.expression(path, handler, target, bindings, guards)
                    for name in (n.id for n in ast.walk(target) if isinstance(n, ast.Name) and isinstance(n.ctx, ast.Store)):
                        bindings[name] = entity if isinstance(target, ast.Name) else None
                continue
            if isinstance(statement, (ast.For, ast.AsyncFor, ast.While, ast.Try, ast.TryStar, ast.With, ast.AsyncWith, ast.Match)):
                # Control-flow joins and loop receivers are not inferred. Kill
                # bindings before inspecting compound bodies to avoid claiming
                # loop targets or reassignments retain an earlier DocType.
                for n in ast.walk(statement):
                    if isinstance(n, ast.Name) and isinstance(n.ctx, (ast.Store, ast.Del)):
                        bindings[n.id] = None
                    elif isinstance(n, (ast.ExceptHandler, ast.MatchAs, ast.MatchStar)) and n.name:
                        bindings[n.name] = None
                    elif isinstance(n, ast.MatchMapping) and n.rest:
                        bindings[n.rest] = None
                for child in ast.iter_child_nodes(statement):
                    if isinstance(child, ast.expr):
                        self.expression(path, handler, child, bindings, guards)
                for name in ("body", "orelse", "finalbody"):
                    self.statements(path, handler, getattr(statement, name, []), dict(bindings), guards)
                for arm in getattr(statement, "handlers", []) + getattr(statement, "cases", []):
                    self.statements(path, handler, arm.body, dict(bindings), guards)
                continue
            self.expression(path, handler, statement, bindings, guards)
            if isinstance(statement, ast.Delete):
                for n in ast.walk(statement):
                    if isinstance(n, ast.Name) and isinstance(n.ctx, ast.Del):
                        bindings[n.id] = None

    def expression(self, path, handler, expression, bindings, guards):
        if expression is None:
            return
        document_names = self.document_names[handler]
        document_names.update(name for name, entity in bindings.items() if entity is not None)
        document_names.update({"doc", "self"})

        def scoped_nodes(node, environment):
            yield node, dict(environment)
            if isinstance(node, (ast.ListComp, ast.SetComp, ast.DictComp, ast.GeneratorExp)):
                local = dict(environment)
                for generator in node.generators:
                    # The first iterable is evaluated outside the comprehension;
                    # each target shadows only the comprehension's local scope.
                    yield from scoped_nodes(generator.iter, local)
                    for target in ast.walk(generator.target):
                        if isinstance(target, ast.Name):
                            local[target.id] = None
                    yield from scoped_nodes(generator.target, local)
                    for condition in generator.ifs:
                        yield from scoped_nodes(condition, local)
                for field in ("elt", "key", "value"):
                    value = getattr(node, field, None)
                    if value is not None:
                        yield from scoped_nodes(value, local)
                return
            for child in ast.iter_child_nodes(node):
                if not isinstance(child, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef, ast.Lambda)):
                    yield from scoped_nodes(child, environment)

        scoped = list(scoped_nodes(expression, bindings))
        nodes = [node for node, _ in scoped]
        # A walrus can rebind the enclosing function from a comprehension. Keep
        # that existing conservative invalidation, without leaking loop targets.
        rebound = {target.id for node in nodes if isinstance(node, ast.NamedExpr)
                   for target in ast.walk(node.target) if isinstance(target, ast.Name)}
        for name in rebound:
            bindings[name] = None
            for _, environment in scoped:
                environment[name] = None
        for node, environment in scoped:
            if isinstance(node, ast.Attribute) and isinstance(node.value, ast.Name):
                receiver = node.value.id
                # A method call is not a field read. Only a bound document (or
                # conventional unresolved doc/self receiver) creates field edges.
                if environment.get(receiver) is not None or receiver in document_names:
                    if any(isinstance(parent, ast.Call) and parent.func is node for parent in nodes):
                        continue
                    relation = "writes" if isinstance(node.ctx, (ast.Store, ast.Del)) else "reads"
                    self.emit(handler, relation, path, node.lineno, entity=environment.get(receiver), field=node.attr,
                              diagnostic="untyped_document" if not environment.get(receiver) else None)
                    if isinstance(expression, ast.AugAssign) and node is expression.target:
                        self.emit(handler, "reads", path, node.lineno, entity=environment.get(receiver), field=node.attr,
                                  diagnostic="untyped_document" if not environment.get(receiver) else None)
            if not isinstance(node, ast.Call):
                continue
            name = dotted(node.func)
            if name in DOC_CALLS | DB_CALLS:
                self.database(path, handler, node, name)
            elif name == "frappe.throw":
                self.emit(handler, "throws_if", path, node.lineno, condition=" and ".join("(" + g + ")" for g in guards) or None,
                          target=ast.unparse(node.args[0]) if node.args else None,
                          diagnostic=None if guards else "unguarded_throw")
            else:
                # Generic static calls are supplied by CodeGraph below. Keep
                # unresolved syntactic calls as diagnostics instead of silently
                # dropping the edges its resolver cannot establish.
                candidates = [target for target in self.resolved_call_sites[(handler, node.lineno)]
                              if target.split(":")[-1].split(".")[-1] == name.split(".")[-1]]
                if not candidates and isinstance(node.func, ast.Name) and node.func.id not in environment:
                    candidates = self.symbols.get(module_name(path) + "." + name, [])
                candidates = sorted(set(candidates))
                self.emit(handler, "calls", path, node.lineno, target=candidates[0] if len(candidates) == 1 else name,
                          diagnostic=None if len(candidates) == 1 else "unresolved_call")

    def database(self, path, handler, node, name):
        def arg(index, keyword):
            return node.args[index] if len(node.args) > index else next((k.value for k in node.keywords if k.arg == keyword), None)
        entity = literal(arg(0, "doctype"))
        if isinstance(entity, dict):
            entity = entity.get("doctype")
        if not isinstance(entity, str):
            entity = None
        relation = "writes" if name == "frappe.db.set_value" else "reads"
        if name in DOC_CALLS:
            self.emit(handler, "reads" if name == "frappe.get_doc" else "writes", path, node.lineno, entity=entity, target=name)
            return
        field_node = arg(2, "fieldname") if name in {"frappe.db.get_value", "frappe.db.set_value"} else next((k.value for k in node.keywords if k.arg == "fields"), None)
        fields = literal(field_node)
        if field_node is None and name.endswith(("get_all", "get_list", "get_value")):
            fields = "name"
            if name.endswith(("get_all", "get_list")) and len(node.args) > 1:
                fields = None  # positional query options require signature-specific resolution
        if isinstance(fields, dict):
            fields = list(fields)
        if isinstance(fields, str):
            fields = [fields]
        if not isinstance(fields, (list, tuple)) or not fields:
            self.emit(handler, relation, path, node.lineno, entity=entity, target=name, diagnostic="dynamic_fields")
            return
        for field in fields:
            if not isinstance(field, str) or not field.isidentifier():
                self.emit(handler, relation, path, node.lineno, entity=entity, target=name, diagnostic="field_expression")
            else:
                self.emit(handler, relation, path, node.lineno, entity=entity, field=field, target=name)

    def static_calls(self):
        nodes = {n["id"]: n for n in self.graph["nodes"]}
        for edge in self.graph["links"]:
            if edge.get("relation", "").lower() != "calls":
                continue
            handler = self.node_handlers.get(edge["source"])
            target = self.node_handlers.get(edge["target"])
            if handler and target:
                source = nodes[edge["source"]]
                self.emit(handler, "calls", source["source_file"], edge.get("line") or source["start_line"], target=target)

    def report(self):
        self.prepare()
        self.hooks()
        self.python()
        self.static_calls()
        for path, raw in sorted(self.sources.items()):
            if path.endswith(".js"):
                self.javascript(path, raw.decode("utf-8"))
        for path, row, pointer in self.script_documents:
            self.javascript(path, row["script"], pointer, row.get("dt"))
        facts = [json.loads(key) for key in sorted({json.dumps(asdict(f), sort_keys=True) for f in self.facts})]
        matrix = defaultdict(set)
        for fact in facts:
            if fact["entity"] and fact["confidence"] == "resolved":
                matrix[fact["entity"]].add(fact["handler"])
        apps = {}
        for app in sorted({p.split("/")[0] for p in self.sources}):
            edges = [f for f in facts if f["handler"].split("/")[0] == app]
            entity_edges = [f for f in edges if f["relation"] in {"reads", "writes", "triggers_on"}]
            resolved = sum(f["confidence"] == "resolved" for f in entity_edges)
            apps[app] = {"handlers": sum(h.split("/")[0] == app for h in self.handlers),
                         "entityEdges": len(entity_edges), "resolvedEntityEdges": resolved,
                         "resolvedPercent": round(100 * resolved / len(entity_edges), 2) if entity_edges else None,
                         "unresolved": dict(Counter(f["diagnostic"] for f in edges if f["diagnostic"]).most_common())}
        return {"format": "norn.code-ontology/1", "substrate": "codegraph-rust/" + VERSION,
                "handlers": sorted(self.handlers.values(), key=lambda h: h["id"]), "facts": facts,
                "diagnostics": self.diagnostics, "coverage": apps,
                "entities": {k: sorted(v) for k, v in sorted(matrix.items())},
                "limitations": ["Static source coverage; no runtime composition, execution, inferred permissions or parity.",
                                "Coverage denominator is reads + writes + triggers_on, including unresolved edges; calls/schedules/API/throws are reported separately.",
                                "DocType and field resolution requires supplied metadata; external stock entities remain typed gaps."]}

    def javascript(self, path, text, pointer="", declared_entity=None):
        # Reuse the maintained tree-sitter grammar; comments, string contents,
        # templates, nested scopes and computed properties are never regex code.
        from tree_sitter import Language, Parser
        import tree_sitter_javascript

        raw = text.encode("utf-8")
        tree = Parser(Language(tree_sitter_javascript.language())).parse(raw)
        def spelling(node):
            return raw[node.start_byte:node.end_byte].decode("utf-8") if node else ""
        def walk(node):
            yield node
            for child in node.named_children:
                yield from walk(child)
        def location(node):
            embedded_line = node.start_point.row + 1
            return (1 if pointer else embedded_line, pointer)
        def string(node):
            if node is not None and node.type == "string":
                try:
                    value = ast.literal_eval(spelling(node))
                    return value if isinstance(value, str) else None
                except (ValueError, SyntaxError):
                    pass
            return None
        def emit(handler, relation, node, **kwargs):
            line, source_pointer = location(node)
            self.emit(handler, relation, path, line, pointer=source_pointer,
                      script_line=node.start_point.row + 1 if pointer else None, **kwargs)
        if tree.root_node.has_error:
            self.diagnostics.append({"code": "javascript_parse_error", "source": self.source(path, 1, pointer)})
            return
        for call in (n for n in walk(tree.root_node) if n.type == "call_expression"):
            if spelling(call.child_by_field_name("function")) != "frappe.ui.form.on":
                continue
            args = call.child_by_field_name("arguments").named_children
            entity = string(args[0]) if args else None
            if declared_entity and entity != declared_entity:
                entity = None
            handler = self.handler(path, "form@" + str(call.start_byte) + pointer, location(call)[0], "client_script")
            emit(handler, "triggers_on", call, entity=entity, target="frappe.ui.form.on",
                 diagnostic="dynamic_client_target" if entity is None else None)
            if len(args) < 2 or args[1].type != "object":
                emit(handler, "calls", call, target="client_handlers", diagnostic="dynamic_client_handlers")
                continue
            for member in args[1].named_children:
                key_node = member.child_by_field_name("key") or member.child_by_field_name("name")
                key = string(key_node) or (spelling(key_node) if key_node and key_node.type in {"property_identifier", "identifier"} else None)
                function = member.child_by_field_name("value") if member.type == "pair" else member
                if not key or function is None or function.type not in {"function_expression", "arrow_function", "method_definition"}:
                    emit(handler, "calls", member, target=key, diagnostic="dynamic_client_handlers")
                    continue
                params = function.child_by_field_name("parameters")
                args = params.named_children if params else [function.child_by_field_name("parameter")]
                receiver = spelling(args[0]) if args and args[0] and args[0].type == "identifier" else None
                body = function.child_by_field_name("body")
                if body is None:
                    continue
                event_handler = self.handler(path, "form." + str(entity) + "." + key + "@" + str(member.start_byte) + pointer,
                                             location(member)[0], "client_handler")
                emit(event_handler, "triggers_on", member, entity=entity, target=key,
                     diagnostic="dynamic_client_target" if entity is None else None)
                nodes = list(walk(body))
                reassigned = receiver is None or any(n.type in {"assignment_expression", "augmented_assignment_expression", "variable_declarator"}
                                                   and spelling(n.child_by_field_name("left") or n.child_by_field_name("name")) == receiver for n in nodes)
                def receiver_diagnostic(node):
                    owner = node.parent
                    while owner and owner != body:
                        if owner.type == "catch_clause":
                            parameter = owner.child_by_field_name("parameter")
                            if parameter and any(spelling(part) == receiver for part in walk(parameter)):
                                return "untyped_document"
                        if owner.type in {"function_expression", "arrow_function", "function_declaration", "method_definition", "generator_function", "generator_function_declaration"}:
                            return "untyped_document"
                        owner = owner.parent
                    return "untyped_document" if reassigned else None

                # Apply the same scope boundary to property and set_value writes.
                for n in nodes:
                    if n.type == "member_expression" and receiver and spelling(n.child_by_field_name("object")) == receiver + ".doc":
                        prop = n.child_by_field_name("property")
                        field = spelling(prop) if prop and prop.type == "property_identifier" else None
                        parent = n.parent
                        assignment = parent.type in {"assignment_expression", "augmented_assignment_expression"} and parent.child_by_field_name("left") == n
                        update = parent.type == "update_expression" and parent.child_by_field_name("argument") == n
                        write = assignment or update
                        diagnostic = receiver_diagnostic(n)
                        emit(event_handler, "writes" if write else "reads", n, entity=entity if not diagnostic else None,
                             field=field, diagnostic=diagnostic or ("dynamic_fields" if field is None else None))
                        if update or (write and parent.type == "augmented_assignment_expression"):
                            emit(event_handler, "reads", n, entity=entity if not diagnostic else None, field=field, diagnostic=diagnostic)
                    if n.type == "call_expression" and receiver and spelling(n.child_by_field_name("function")) == receiver + ".set_value":
                        call_args = n.child_by_field_name("arguments").named_children
                        field = string(call_args[0]) if call_args else None
                        diagnostic = receiver_diagnostic(n)
                        emit(event_handler, "writes", n, entity=entity if not diagnostic else None, field=field,
                             diagnostic=diagnostic or ("dynamic_fields" if field is None else None))


def collect(roots):
    """Read only declared app sources/metadata; reject symlinks and app collisions."""
    sources = {}
    for root in roots:
        root = root.absolute()
        if any(parent.is_symlink() for parent in root.parents):
            raise ValueError("symlink ancestor in source root: " + str(root))
        if root.is_symlink() or not root.is_dir():
            raise ValueError("app root must be an ordinary directory: " + str(root))
        for path in sorted(root.rglob("*")):
            if any(part in {"node_modules", ".git", ".codegraph", "dist", "build", ".venv", "__pycache__"} for part in path.relative_to(root).parts):
                continue
            if path.is_symlink():
                raise ValueError("symlink in source app: " + str(path))
            if not path.is_file() or path.suffix not in {".py", ".js", ".json"} or not production(path.relative_to(root)):
                continue
            name = root.name + "/" + path.relative_to(root).as_posix()
            if name in sources:
                raise ValueError("duplicate source app/path: " + name)
            sources[name] = path.read_bytes()
    return sources


def analyze(sources: dict[str, bytes], substrate: CodeGraph, revision="working-tree"):
    return FrappeRules(sources, substrate.extract(sources), revision).report()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("apps", nargs="+", type=Path)
    parser.add_argument("--codegraph", required=True, type=Path)
    parser.add_argument("--scratch", type=Path, help="directory for this run's copies; default: a fresh temporary directory, removed afterwards")
    parser.add_argument("--allow-unpinned-codegraph", action="store_true",
                        help="run a binary whose bytes are not the pinned release's; prints a warning, refused when CI is set")
    parser.add_argument("--revision", default="working-tree", help="evidence revision; default explicitly denotes unpinned working tree")
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="norn-code-ontology-") as fresh:
        substrate = CodeGraphRust(args.codegraph, args.scratch or Path(fresh), args.allow_unpinned_codegraph)
        report = analyze(collect(args.apps), substrate, args.revision)
    print(json.dumps(report, indent=2, sort_keys=True, ensure_ascii=True))


if __name__ == "__main__":
    main()
