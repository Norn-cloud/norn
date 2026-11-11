#!/usr/bin/env python3
"""Fail a change that breaks one of Norn's frozen external contracts (D84).

Norn's external contracts are additive-only: the OpenAPI document (and the SDK
generated from it), the IR and kernel JSON schemas, the CLI commands and flags,
refusal and diagnostic codes, and the receipt and ledger wire formats (which are
those schemas and OpenAPI components). Studio's server documents are OpenAPI
components, so they are covered by the OpenAPI rules.

The check compares each surface at the merge-base of BASE and HEAD with the
surface at HEAD, so a change that landed on the base branch after the head
branched never reads as a removal. Additions pass. These fail:

- a removed or renamed endpoint, method, operation id, parameter, response,
  component, schema file, property, enum value, CLI command or flag, refusal
  code or diagnostic code;
- a narrowed type: a type alternative or enum value removed, a property newly
  required on an input, a bound tightened, a pattern, format or const changed,
  an open object closed, a parameter or request body newly required;
- a changed wire format: a `$ref` retargeted, a type alternative added to an
  output, a property no longer required on an output, a CLI default or arity
  changed, a diagnostic's severity changed.

The only escape is a line `contract-break-approved: <ruling>` in the pull
request body, where `<ruling>` names the decision that approved the break
(`D85`, `#1234` or a URL). In a merge group every merge step is checked against
the body of the pull request it merges.

Usage:
  contract_diff.py --base REV --head REV [--body-file FILE]
  contract_diff.py --merge-group BASE HEAD --bodies-file FILE
  contract_diff.py --surface ROOT   # print the extracted surface of a checkout
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from dataclasses import dataclass, field
from pathlib import Path
from typing import Callable

OPENAPI = "sdks/openapi/norn-api.openapi.json"
SDK_INDEX = "packages/norn-sdk/src/index.ts"
CLI_SNAPSHOT = "crates/norn-cli/cli.contract.json"
TS_CLI_DIR = "tools/norn-ts/src"
TS_CLI_SNAPSHOT = "tools/norn-ts/cli.contract.json"
DIAGNOSTIC_CODES = "crates/norn-checker/conformance/diagnostic-codes"
SCHEMA_DIRS = ("crates/norn-ir/schema", "crates/norn-kernel/schema", "tools/norn-ts/schema")

MARKER = re.compile(r"^\s*contract-break-approved:\s*(D\d+|#\d+|https?://\S+)\s*$", re.MULTILINE)
CODE = re.compile(r"[a-z][a-z0-9_]*(?:\.[a-z0-9_]+)+")
CODE_CONST = re.compile(r'const\s+[A-Z][A-Z0-9_]*\s*:\s*&(?:\'static\s+)?str\s*=\s*"(' + CODE.pattern + r')"\s*;')
STRING = re.compile(r'"((?:[^"\\]|\\.)*)"')
CODE_FN = re.compile(r"fn\s+code\s*\(")
TEST_MODULE = re.compile(r"#\[cfg\(test\)\]\s*(?:#\[[^\]]*\]\s*)*mod\s")

# A tree is how the check reads one revision: list the files under a directory
# and read one file, or None when it does not exist.
@dataclass(frozen=True)
class Tree:
    list: Callable[[str], list[str]]
    read: Callable[[str], str | None]


def git(*args: str) -> str:
    """Run git in the working directory: the repository being checked."""
    return subprocess.run(["git", *args], check=True, capture_output=True, text=True).stdout


class _Objects:
    """One `git cat-file --batch` process shared by every revision read."""
    process: subprocess.Popen | None = None

    @classmethod
    def read(cls, spec: str) -> str | None:
        if cls.process is None:
            cls.process = subprocess.Popen(["git", "cat-file", "--batch"], stdin=subprocess.PIPE, stdout=subprocess.PIPE)
        assert cls.process.stdin and cls.process.stdout
        cls.process.stdin.write(spec.encode() + b"\n")
        cls.process.stdin.flush()
        header = cls.process.stdout.readline().split()
        if len(header) != 3 or header[1] != b"blob":
            return None
        data = cls.process.stdout.read(int(header[2]) + 1)[:-1]
        return data.decode("utf-8", errors="replace")


def git_tree(rev: str) -> Tree:
    rev = git("rev-parse", "--verify", f"{rev}^{{commit}}").strip()

    def list_files(directory: str) -> list[str]:
        out = git("ls-tree", "-r", "--name-only", rev, "--", directory)
        return [line for line in out.splitlines() if line]

    return Tree(list_files, lambda path: _Objects.read(f"{rev}:{path}"))


def disk_tree(root: Path) -> Tree:
    def list_files(directory: str) -> list[str]:
        base = root / directory
        if not base.is_dir():
            return []
        return sorted(str(path.relative_to(root)) for path in base.rglob("*") if path.is_file())

    def read(path: str) -> str | None:
        try:
            return (root / path).read_text(encoding="utf-8")
        except (FileNotFoundError, IsADirectoryError):
            return None

    return Tree(list_files, read)


# ---------------------------------------------------------------- JSON schemas

IN, OUT = "in", "out"


@dataclass
class Documents:
    """The two documents a schema comparison resolves `$ref`s in."""
    old: object
    new: object
    seen: set[tuple[str, str]] = field(default_factory=set)


def resolve(root: object, ref: str) -> object | None:
    if not ref.startswith("#/"):
        return None
    node = root
    for part in ref[2:].split("/"):
        part = part.replace("~1", "/").replace("~0", "~")
        if not isinstance(node, dict) or part not in node:
            return None
        node = node[part]
    return node


def _types(schema: dict, root: object = None, depth: int = 0) -> set[str] | None:
    """The JSON types a schema admits; a union's are its alternatives' types.
    None when unconstrained."""
    value = schema.get("type")
    if value is not None:
        return {value} if isinstance(value, str) else set(value)
    members = schema.get("oneOf") or schema.get("anyOf")
    if not members or root is None or depth > 16:
        return None
    union: set[str] = set()
    for member in members:
        member = _target(member, root)
        found = _types(member, root, depth + 1) if isinstance(member, dict) else None
        if found is None:
            return None
        union |= found
    return union


def _values(schema: object, root: object, depth: int = 0) -> list | None:
    """The values a schema enumerates (enum, const, or a union of those);
    None when it admits values it does not list."""
    schema = _target(schema, root)
    if not isinstance(schema, dict) or depth > 16:
        return None
    if "const" in schema:
        return [schema["const"]]
    if isinstance(schema.get("enum"), list):
        return list(schema["enum"])
    members = schema.get("oneOf") or schema.get("anyOf")
    if not members:
        return None
    values: list = []
    for member in members:
        found = _values(member, root, depth + 1)
        if found is None:
            return None
        values.extend(found)
    return values


def _json_type(value: object) -> set[str]:
    if value is None:
        return {"null"}
    if isinstance(value, bool):
        return {"boolean"}
    if isinstance(value, int):
        return {"integer", "number"}
    if isinstance(value, float):
        return {"number"}
    if isinstance(value, str):
        return {"string"}
    return {"array"} if isinstance(value, list) else {"object"}


def _accepts(schema: object, value: object, root: object, depth: int = 0) -> bool:
    """Whether a schema still admits an enumerated value (by its enum, const,
    union and type; finer constraints are compared separately)."""
    schema = _target(schema, root)
    if schema is True or not isinstance(schema, dict) or depth > 16:
        return schema is not False
    if "const" in schema:
        return schema["const"] == value
    if isinstance(schema.get("enum"), list):
        return value in schema["enum"]
    members = schema.get("oneOf") or schema.get("anyOf")
    if members:
        return any(_accepts(member, value, root, depth + 1) for member in members)
    types = _types(schema)
    return types is None or bool(types & _json_type(value))


def _identity(schema: object) -> str | None:
    """What names a union alternative: a tag constant, a lone required key,
    a constant or an enum, never its position."""
    if not isinstance(schema, dict):
        return None
    if "title" in schema:
        return f"title {schema['title']}"
    properties = schema.get("properties")
    if isinstance(properties, dict):
        for name, prop in sorted(properties.items()):
            if isinstance(prop, dict) and "const" in prop:
                return f"{name}={json.dumps(prop['const'], sort_keys=True)}"
            if isinstance(prop, dict) and isinstance(prop.get("enum"), list) and len(prop["enum"]) == 1:
                return f"{name}={json.dumps(prop['enum'][0], sort_keys=True)}"
        required = schema.get("required") or []
        if len(properties) == 1 and list(properties) == list(required):
            return f"tag {required[0]}"
    if "const" in schema:
        return f"const {json.dumps(schema['const'], sort_keys=True)}"
    if isinstance(schema.get("enum"), list):
        return f"enum {json.dumps(sorted(map(json.dumps, schema['enum'])))}"
    return None


def _target(schema: object, root: object) -> object:
    """A schema as it validates: its `$ref` followed and `allOf` structs merged."""
    if isinstance(schema, dict) and "$ref" in schema:
        schema = resolve(root, schema["$ref"])
    return _flatten(schema, root) if isinstance(schema, dict) else schema


def _alternatives(schemas: list, root: object) -> dict[str, object]:
    """Union alternatives keyed by what identifies them: a tag (through a
    `$ref`), else a JSON type no other alternative has, else the `$ref`;
    position only as the last resort."""
    candidates = []
    for schema in schemas:
        target = _target(schema, root)
        typed = f"type {json.dumps(target['type'])}" if isinstance(target, dict) and "type" in target else None
        ref = f"$ref {schema['$ref']}" if isinstance(schema, dict) and "$ref" in schema else None
        candidates.append((_identity(target), typed, ref))
    keyed: dict[str, object] = {}
    for index, (schema, keys) in enumerate(zip(schemas, candidates)):
        key = next((k for slot, k in enumerate(keys)
                    if k is not None and sum(other[slot] == k for other in candidates) == 1), f"#{index}")
        keyed[key] = schema
    return keyed


JSON_TYPES = {"null", "boolean", "number", "string", "array", "object"}
ANNOTATIONS = {"description", "title", "examples", "default", "deprecated", "$comment", "readOnly", "writeOnly", "format"}


def _is_any(schema: object, root: object, visiting: frozenset[str] = frozenset()) -> bool:
    """Whether a schema accepts every JSON value: `true`, annotations only, or
    a union of every JSON type whose items and values accept anything (a
    recursive JSON-value definition counts)."""
    if schema is True:
        return True
    if not isinstance(schema, dict):
        return False
    if "$ref" in schema:
        ref = schema["$ref"]
        return ref in visiting or _is_any(resolve(root, ref), root, visiting | {ref})
    if not set(schema) - ANNOTATIONS:
        return True
    if set(schema) - ANNOTATIONS - {"anyOf", "oneOf"}:
        return False
    covered: set[str] = set()
    for member in schema.get("anyOf") or schema.get("oneOf") or []:
        if isinstance(member, dict) and "$ref" in member:
            ref = member["$ref"]
            if ref in visiting:
                return False
            member, visiting = resolve(root, ref), visiting | {ref}
        if not isinstance(member, dict) or set(member) - ANNOTATIONS - {"type", "items", "additionalProperties"}:
            return False
        if not all(_is_any(member.get(key, True), root, visiting) for key in ("items", "additionalProperties")):
            return False
        covered |= _types(member) or set()
    return JSON_TYPES <= covered


def _flatten(schema: dict, root: object) -> dict:
    """Merge plain object members of `allOf` (how serde `flatten` serializes)
    into the schema, so wrapping a struct in `allOf` is not a change."""
    members = schema.get("allOf")
    if not members:
        return schema
    merged = {k: v for k, v in schema.items() if k != "allOf"}
    kept = []
    for member in members:
        target = _target(member, root)
        if isinstance(target, dict) and "properties" in target and not set(target) & {"oneOf", "anyOf", "allOf", "$ref"}:
            merged.setdefault("type", target.get("type", "object"))
            merged["properties"] = {**(merged.get("properties") or {}), **target["properties"]}
            merged["required"] = sorted(set(merged.get("required") or []) | set(target.get("required") or []))
        else:
            kept.append(member)
    if kept:
        merged["allOf"] = kept
    return merged


def compare_schema(old: object, new: object, where: str, directions: frozenset[str], out: list[str],
                   docs: Documents) -> None:
    """Append every non-additive change from `old` to `new` to `out`.

    `directions` says whether the schema is read as an input (a narrowing
    breaks senders), an output (a widening breaks readers) or both. A `$ref`
    is compared by what it points to, so inlining or renaming an equivalent
    schema is not a change.
    """
    old_ref = old.get("$ref") if isinstance(old, dict) else None
    new_ref = new.get("$ref") if isinstance(new, dict) else None
    if old_ref is not None or new_ref is not None:
        if old_ref == new_ref:
            return  # The named schema itself is compared once, by name.
        if (old_ref or "", new_ref or "") in docs.seen:
            return
        docs.seen.add((old_ref or "", new_ref or ""))
        if old_ref is not None:
            old = resolve(docs.old, old_ref)
        if new_ref is not None:
            new = resolve(docs.new, new_ref)
        if old is None or new is None:
            out.append(f"{where}: $ref {old_ref or new_ref} does not resolve")
            return
    old = True if _is_any(old, docs.old) else old
    new = True if _is_any(new, docs.new) else new
    if isinstance(old, bool) or isinstance(new, bool):
        if old == new:
            return
        narrowed = old is True or new is False
        if narrowed and IN in directions:
            out.append(f"{where}: input schema narrowed from {json.dumps(old)} to {json.dumps(new)}")
        if not narrowed and OUT in directions:
            out.append(f"{where}: output schema widened from {json.dumps(old)} to {json.dumps(new)}")
        return
    if not isinstance(old, dict) or not isinstance(new, dict):
        if old != new:
            out.append(f"{where}: schema changed")
        return
    old, new = _flatten(old, docs.old), _flatten(new, docs.new)
    old_types, new_types = _types(old, docs.old), _types(new, docs.new)
    if old_types != new_types:
        if new_types is not None and (old_types is None or old_types - new_types):
            removed = sorted(old_types - new_types) if old_types else ["any"]
            out.append(f"{where}: type narrowed, {', '.join(removed)} removed")
        elif OUT in directions and old_types is not None:
            added = sorted((new_types or {"any"}) - old_types)
            out.append(f"{where}: output type widened with {', '.join(added)}")
    old_values, new_values = _values(old, docs.old), _values(new, docs.new)
    if old_values is not None:
        for value in old_values:
            if not _accepts(new, value, docs.new):
                out.append(f"{where}: enum value {json.dumps(value)} removed")
    elif new_values is not None and IN in directions:
        out.append(f"{where}: input values narrowed to an enumeration")
    for key in ("pattern", "format"):
        if key in new and old.get(key) != new.get(key):
            out.append(f"{where}: {key} changed from {json.dumps(old.get(key))} to {json.dumps(new.get(key))}")
    for key, tighter in (("maximum", min), ("exclusiveMaximum", min), ("maxLength", min), ("maxItems", min),
                         ("maxProperties", min), ("minimum", max), ("exclusiveMinimum", max),
                         ("minLength", max), ("minItems", max), ("minProperties", max)):
        if key in new and (key not in old or tighter(old[key], new[key]) != old[key]):
            out.append(f"{where}: {key} tightened from {old.get(key)} to {new[key]}")
    old_props = old.get("properties") or {}
    new_props = new.get("properties") or {}
    for name, schema in old_props.items():
        if name not in new_props:
            out.append(f"{where}.{name}: property removed")
        else:
            compare_schema(schema, new_props[name], f"{where}.{name}", directions, out, docs)
    old_required, new_required = set(old.get("required") or []), set(new.get("required") or [])
    if IN in directions:
        for name in sorted(new_required - old_required):
            out.append(f"{where}.{name}: newly required on an input")
    if OUT in directions:
        for name in sorted(old_required - new_required):
            if name in new_props:
                out.append(f"{where}.{name}: no longer required on an output")
    if "additionalProperties" in old or "additionalProperties" in new:
        compare_schema(old.get("additionalProperties", True), new.get("additionalProperties", True),
                       f"{where}{{*}}", directions, out, docs)
    for key in ("items", "contains", "propertyNames", "not", "if", "then", "else"):
        if key in old or (key == "items" and "items" in new and old_types == {"array"}):
            compare_schema(old.get(key, True), new.get(key, True), f"{where}[{key}]", directions, out, docs)
    old_items, new_items = old.get("prefixItems") or [], new.get("prefixItems") or []
    for index, schema in enumerate(old_items):
        if index >= len(new_items):
            out.append(f"{where}[prefixItems.{index}]: tuple item removed")
        else:
            compare_schema(schema, new_items[index], f"{where}[prefixItems.{index}]", directions, out, docs)
    for key in ("oneOf", "anyOf"):
        if key not in old or old_values is not None:
            continue  # A new union is judged by its types and values above.
        if key not in new:
            if OUT in directions and new_types is None and new_values is None:
                out.append(f"{where}: output {key} replaced by an open schema")
            continue
        old_alts = _alternatives(old[key], docs.old)
        new_alts = _alternatives(new[key], docs.new)
        for name, schema in old_alts.items():
            if name not in new_alts:
                out.append(f"{where}[{key} {name}]: alternative removed")
            else:
                compare_schema(schema, new_alts[name], f"{where}[{key} {name}]", directions, out, docs)
    old_all, new_all = old.get("allOf") or [], new.get("allOf") or []
    for index, schema in enumerate(new_all):
        if index < len(old_all):
            compare_schema(old_all[index], schema, f"{where}[allOf.{index}]", directions, out, docs)
        elif IN in directions:
            out.append(f"{where}[allOf.{index}]: constraint added to an input")
    for key in ("$defs", "definitions"):
        # A schema file's own definitions are not public names: one that is
        # removed or renamed is compared where it is used, through its `$ref`.
        old_defs, new_defs = old.get(key) or {}, new.get(key) or {}
        for name in old_defs.keys() & new_defs.keys():
            compare_schema(old_defs[name], new_defs[name], f"{where}/{key}/{name}", directions, out, docs)


# ---------------------------------------------------------------- OpenAPI

METHODS = ("get", "put", "post", "delete", "options", "head", "patch", "trace")
COMPONENT = "#/components/schemas/"
COMPONENT_REF = re.compile(r'"#/components/schemas/([^"]+)"')
POSITIONAL = re.compile(r"^(.+?OneOf)(\d+)$")


def _refs(value: object) -> set[str]:
    return set(COMPONENT_REF.findall(json.dumps(value)))


def canonical_openapi(document: dict) -> dict:
    """Rename positional union components (`FooOneOf3`, `FooOneOf3Bar`) after
    the tag that identifies them (`FooOneOf<kind="x">`), so inserting a
    variant does not read as renaming every later one."""
    schemas = (document.get("components") or {}).get("schemas") or {}
    roots: dict[str, str] = {}
    for name, schema in schemas.items():
        match = POSITIONAL.match(name)
        identity = _identity(schema) if match else None
        if match and identity is not None:
            roots[name] = f"{match.group(1)}<{identity}>"
    if not roots:
        return document
    renamed = {}
    for name in schemas:
        root = max((r for r in roots if name.startswith(r) and not name[len(r):len(r) + 1].isdigit()), key=len, default=None)
        if root is not None:
            renamed[name] = roots[root] + name[len(root):]
    if len(set(renamed.values())) != len(renamed):
        return document  # Ambiguous tags: keep the generated names.
    text = COMPONENT_REF.sub(lambda m: json.dumps(COMPONENT + renamed.get(m.group(1), m.group(1))), json.dumps(document))
    result = json.loads(text)
    result["components"]["schemas"] = {renamed.get(k, k): v for k, v in result["components"]["schemas"].items()}
    return result


def component_directions(document: dict) -> dict[str, frozenset[str]]:
    """Each component's use: reached from a request (in), a response (out) or both."""
    schemas = (document.get("components") or {}).get("schemas") or {}
    reached: dict[str, set[str]] = {name: set() for name in schemas}

    def spread(roots: set[str], direction: str) -> None:
        stack = list(roots)
        while stack:
            name = stack.pop()
            if name not in reached or direction in reached[name]:
                continue
            reached[name].add(direction)
            stack.extend(_refs(schemas[name]))

    for item in (document.get("paths") or {}).values():
        for method in METHODS:
            operation = item.get(method) if isinstance(item, dict) else None
            if not isinstance(operation, dict):
                continue
            spread(_refs(operation.get("parameters")) | _refs(operation.get("requestBody")) | _refs(item.get("parameters")), IN)
            spread(_refs(operation.get("responses")), OUT)
    # A component no route reaches is published for SDK users to build and read.
    return {name: frozenset(found or {IN, OUT}) for name, found in reached.items()}


def compare_openapi(old: dict, new: dict, out: list[str]) -> None:
    old, new = canonical_openapi(old), canonical_openapi(new)
    docs = Documents(old, new)
    old_paths, new_paths = old.get("paths") or {}, new.get("paths") or {}
    for path, item in old_paths.items():
        if path not in new_paths:
            out.append(f"openapi {path}: endpoint removed")
            continue
        for method in METHODS:
            if method not in item:
                continue
            where = f"openapi {method.upper()} {path}"
            after = new_paths[path].get(method)
            if after is None:
                out.append(f"{where}: method removed")
                continue
            compare_operation(item[method], after, where, out, docs)
    directions = component_directions(new)
    old_schemas = (old.get("components") or {}).get("schemas") or {}
    new_schemas = (new.get("components") or {}).get("schemas") or {}
    # Every component is an exported SDK type, so its name is a contract.
    for name, schema in old_schemas.items():
        where = f"openapi {COMPONENT}{name}"
        if name not in new_schemas:
            out.append(f"{where}: removed")
        else:
            compare_schema(schema, new_schemas[name], where, directions.get(name, frozenset({IN, OUT})), out, docs)


def compare_operation(old: dict, new: dict, where: str, out: list[str], docs: Documents) -> None:
    if old.get("operationId") != new.get("operationId"):
        out.append(f"{where}: operationId changed from {old.get('operationId')} to {new.get('operationId')}")
    params = lambda op: {(p.get("in"), p.get("name")): p for p in op.get("parameters") or [] if isinstance(p, dict)}
    old_params, new_params = params(old), params(new)
    for key, param in old_params.items():
        if key not in new_params:
            out.append(f"{where}: {key[0]} parameter {key[1]} removed")
            continue
        if new_params[key].get("required") and not param.get("required"):
            out.append(f"{where}: {key[0]} parameter {key[1]} newly required")
        compare_schema(param.get("schema", True), new_params[key].get("schema", True),
                       f"{where} {key[0]} {key[1]}", frozenset({IN}), out, docs)
    for key, param in new_params.items():
        if key not in old_params and param.get("required"):
            out.append(f"{where}: required {key[0]} parameter {key[1]} added")
    old_body, new_body = old.get("requestBody"), new.get("requestBody")
    if old_body is not None and new_body is None:
        out.append(f"{where}: request body removed")
    elif new_body is not None:
        if new_body.get("required") and not (old_body or {}).get("required"):
            out.append(f"{where}: request body newly required")
        compare_content((old_body or {}).get("content") or {}, new_body.get("content") or {},
                        f"{where} request", frozenset({IN}), out, docs)
    old_responses, new_responses = old.get("responses") or {}, new.get("responses") or {}
    for status, response in old_responses.items():
        if status not in new_responses:
            out.append(f"{where}: response {status} removed")
        else:
            compare_content(response.get("content") or {}, new_responses[status].get("content") or {},
                            f"{where} {status}", frozenset({OUT}), out, docs)


def compare_content(old: dict, new: dict, where: str, directions: frozenset[str], out: list[str], docs: Documents) -> None:
    for media, body in old.items():
        if media not in new:
            out.append(f"{where}: media type {media} removed")
        else:
            compare_schema(body.get("schema", True), new[media].get("schema", True), f"{where} {media}", directions, out, docs)


# ---------------------------------------------------------------- surfaces

def sdk_exports(text: str | None) -> set[str]:
    if text is None:
        return set()
    names = set(re.findall(r"^export\s+(?:declare\s+)?(?:type|interface|const|function|class|enum)\s+([A-Za-z_$][\w$]*)", text, re.MULTILINE))
    for group in re.findall(r"^export\s+(?:type\s+)?\{([^}]*)\}", text, re.MULTILINE):
        for item in group.split(","):
            item = item.strip()
            if item:
                names.add(item.split(" as ")[-1].strip())
    return names


def _function_body(text: str, start: int) -> str:
    """The brace-balanced body after `start`, ignoring braces in strings and chars."""
    open_at = text.find("{", start)
    if open_at < 0:
        return ""
    depth, index = 0, open_at
    while index < len(text):
        char = text[index]
        if char == '"':
            match = STRING.match(text, index)
            index = match.end() if match else index + 1
            continue
        if char == "{":
            depth += 1
        elif char == "}":
            depth -= 1
            if depth == 0:
                return text[open_at:index + 1]
        index += 1
    return text[open_at:]


def _without_tests(text: str) -> str:
    test = TEST_MODULE.search(text)
    return text[:test.start()] if test else text


def rust_codes(text: str) -> set[str]:
    """Refusal codes a Rust source declares: literals inside `fn code` bodies
    and every `const NAME: &str = "a.b"` code constant, outside test modules."""
    text = _without_tests(text)
    codes = set(CODE_CONST.findall(text))
    for match in CODE_FN.finditer(text):
        for literal in STRING.findall(_function_body(text, match.end())):
            if CODE.fullmatch(literal):
                codes.add(literal)
    return codes


def rust_literals(text: str) -> set[str]:
    """Every code-shaped string literal outside test modules."""
    return {literal for literal in STRING.findall(_without_tests(text)) if CODE.fullmatch(literal)}


def _rust_sources(tree: Tree) -> list[str]:
    paths = []
    for path in tree.list("crates"):
        parts = path.split("/")
        if len(parts) >= 4 and parts[2] == "src" and path.endswith(".rs") and not path.endswith("_tests.rs") \
                and "tests" not in parts[3:-1]:
            paths.append(path)
    return paths


def refusal_codes(tree: Tree) -> set[str]:
    return set().union(*(rust_codes(tree.read(path) or "") for path in _rust_sources(tree)))


def removed_refusal_codes(old: Tree, new: Tree) -> set[str]:
    """Codes the base declares that the head no longer spells anywhere: moving
    a code into a helper, a constant or another crate is not a removal."""
    present = set().union(*(rust_literals(new.read(path) or "") for path in _rust_sources(new)))
    return refusal_codes(old) - present


def diagnostic_codes(tree: Tree) -> dict[str, dict]:
    codes = {}
    for path in tree.list(DIAGNOSTIC_CODES):
        if path.endswith(".json"):
            document = json.loads(tree.read(path) or "{}")
            codes[document.get("code", Path(path).stem)] = document
    return codes


def _command_surface(command: dict) -> tuple[set[str], set[str]]:
    """A command tree's top-level command names and every long flag in it."""
    flags: set[str] = set()
    stack = [command]
    while stack:
        node = stack.pop()
        flags |= {f"--{arg['long']}" for arg in (node.get("args") or {}).values() if arg.get("long")}
        stack.extend((node.get("subcommands") or {}).values())
    return set(command.get("subcommands") or {}), flags


def ts_cli(tree: Tree) -> tuple[set[str], set[str]]:
    """The TypeScript authoring CLI's commands and flags: from its generated
    contract when it has one, else read from its sources."""
    snapshot = tree.read(TS_CLI_SNAPSHOT)
    if snapshot:
        return _command_surface(json.loads(snapshot))
    commands: set[str] = set()
    flags: set[str] = set()
    for path in tree.list(TS_CLI_DIR):
        if not path.endswith(".ts") or path.endswith(".test.ts"):
            continue
        text = tree.read(path) or ""
        if path.endswith("/cli.ts"):
            commands |= set(re.findall(r'argv\[2\]\s*===\s*"([a-z][a-z-]*)"', text))
            commands |= set(re.findall(r'command\s*[!=]==\s*"([a-z][a-z-]*)"', text))
        flags |= set(re.findall(r'"(--[a-z][a-z0-9-]*)"', text))
    return commands, flags


def rust_cli(tree: Tree) -> dict:
    text = tree.read(CLI_SNAPSHOT)
    return json.loads(text) if text else {}


def compare_command(old: dict, new: dict, where: str, out: list[str]) -> None:
    for name, arg in (old.get("args") or {}).items():
        after = (new.get("args") or {}).get(name)
        if after is None:
            out.append(f"{where} {name}: flag or argument removed")
            continue
        for key in ("long", "short", "aliases", "positional", "takes_value", "multiple", "default"):
            if arg.get(key) != after.get(key):
                if key == "aliases" and set(arg.get(key) or []) <= set(after.get(key) or []):
                    continue
                out.append(f"{where} {name}: {key} changed from {json.dumps(arg.get(key))} to {json.dumps(after.get(key))}")
        if after.get("required") and not arg.get("required"):
            out.append(f"{where} {name}: newly required")
        if arg.get("values") and not set(arg["values"]) <= set(after.get("values") or arg["values"]):
            out.append(f"{where} {name}: accepted values removed: {sorted(set(arg['values']) - set(after.get('values') or []))}")
        if after.get("values") and not arg.get("values"):
            out.append(f"{where} {name}: accepted values narrowed to {after['values']}")
    for name, arg in (new.get("args") or {}).items():
        if name not in (old.get("args") or {}) and arg.get("required"):
            out.append(f"{where} {name}: required flag or argument added")
    for name, command in (old.get("subcommands") or {}).items():
        after = (new.get("subcommands") or {}).get(name)
        if after is None:
            out.append(f"{where} {name}: command removed")
        else:
            compare_command(command, after, f"{where} {name}", out)


def schema_files(tree: Tree) -> dict[str, str]:
    return {path: tree.read(path) or "" for directory in SCHEMA_DIRS for path in tree.list(directory)
            if path.endswith(".json")}


def diff(old: Tree, new: Tree) -> list[str]:
    """Every non-additive contract change from `old` to `new`."""
    out: list[str] = []
    before, after = old.read(OPENAPI), new.read(OPENAPI)
    if before is not None:
        if after is None:
            out.append(f"{OPENAPI}: removed")
        else:
            compare_openapi(json.loads(before), json.loads(after), out)
    for name in sorted(sdk_exports(old.read(SDK_INDEX)) - sdk_exports(new.read(SDK_INDEX))):
        out.append(f"sdk {SDK_INDEX}: export {name} removed")
    old_schemas, new_schemas = schema_files(old), schema_files(new)
    for path, text in old_schemas.items():
        if path not in new_schemas:
            out.append(f"{path}: schema removed")
        else:
            before_schema, after_schema = json.loads(text), json.loads(new_schemas[path])
            compare_schema(before_schema, after_schema, path, frozenset({IN, OUT}), out, Documents(before_schema, after_schema))
    for code in sorted(removed_refusal_codes(old, new)):
        out.append(f"refusal code {code}: removed")
    old_diagnostics, new_diagnostics = diagnostic_codes(old), diagnostic_codes(new)
    for code, document in old_diagnostics.items():
        if code not in new_diagnostics:
            out.append(f"diagnostic code {code}: removed")
        elif document.get("severity") != new_diagnostics[code].get("severity"):
            out.append(f"diagnostic code {code}: severity changed from {document.get('severity')} to {new_diagnostics[code].get('severity')}")
    old_cli, new_cli = rust_cli(old), rust_cli(new)
    if old_cli and not new_cli:
        out.append(f"{CLI_SNAPSHOT}: removed")
    elif old_cli:
        compare_command(old_cli, new_cli, "cli norn", out)
    old_ts, new_ts = old.read(TS_CLI_SNAPSHOT), new.read(TS_CLI_SNAPSHOT)
    if old_ts and new_ts:
        compare_command(json.loads(old_ts), json.loads(new_ts), "authoring cli norn", out)
    (old_commands, old_flags), (new_commands, new_flags) = ts_cli(old), ts_cli(new)
    for command in sorted(old_commands - new_commands):
        out.append(f"authoring cli {command}: command removed")
    for flag in sorted(old_flags - new_flags):
        out.append(f"authoring cli {flag}: flag removed")
    return out


def approval(body: str | None) -> str | None:
    match = MARKER.search(body or "")
    return match.group(1) if match else None


def verdict(breaks: list[str], body: str | None, label: str) -> bool:
    if not breaks:
        print(f"{label}: no contract breaks")
        return True
    ruling = approval(body)
    heading = f"{label}: {len(breaks)} non-additive contract change(s)"
    print(heading + (f", approved by {ruling}" if ruling else ""))
    for line in breaks:
        print(f"  - {line}")
    if ruling:
        return True
    print("Frozen contracts are additive-only (D84). Make the change additive, or add the line\n"
          "`contract-break-approved: <ruling>` (D<n>, #<issue> or a URL) to the pull request body.")
    return False


def merge_group(base: str, head: str, bodies: dict[str, list[str]]) -> bool:
    """Check each merge step of a queue group against its pull request's body."""
    ok, commit = True, git("rev-parse", head).strip()
    base = git("rev-parse", base).strip()
    for _ in range(64):
        if commit == base:
            return ok
        parents = git("rev-list", "--parents", "-n", "1", commit).split()[1:]
        if len(parents) != 2:
            print(f"merge group commit {commit} is not a two-parent merge")
            return False
        previous, pull_head = parents
        breaks = diff(git_tree(previous), git_tree(commit))
        body = "\n".join(bodies.get(pull_head, []))
        ok = verdict(breaks, body, f"merge of {pull_head[:12]}") and ok
        commit = previous
    print("merge group is longer than 64 merges or misses its base")
    return False


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--base")
    parser.add_argument("--head")
    parser.add_argument("--body-file", help="the pull request body")
    parser.add_argument("--merge-group", nargs=2, metavar=("BASE", "HEAD"))
    parser.add_argument("--bodies-file", help="JSON {pull head sha: [bodies]} for a merge group")
    parser.add_argument("--surface", metavar="ROOT", help="print the surface extracted from a checkout")
    args = parser.parse_args()
    if args.surface:
        tree = disk_tree(Path(args.surface))
        commands, flags = ts_cli(tree)
        print(json.dumps({"refusalCodes": sorted(refusal_codes(tree)), "diagnosticCodes": len(diagnostic_codes(tree)),
                          "schemas": sorted(schema_files(tree)), "sdkExports": sorted(sdk_exports(tree.read(SDK_INDEX))),
                          "authoringCli": {"commands": sorted(commands), "flags": sorted(flags)},
                          "cli": rust_cli(tree)}, indent=2))
        return 0
    if args.merge_group:
        bodies = json.loads(Path(args.bodies_file).read_text(encoding="utf-8")) if args.bodies_file else {}
        return 0 if merge_group(*args.merge_group, bodies) else 1
    if not (args.base and args.head):
        parser.error("--base and --head are required")
    merge_base = git("merge-base", args.base, args.head).strip()
    body = Path(args.body_file).read_text(encoding="utf-8") if args.body_file else None
    breaks = diff(git_tree(merge_base), git_tree(args.head))
    return 0 if verdict(breaks, body, f"{merge_base[:12]}..{args.head[:12]}") else 1


if __name__ == "__main__":
    sys.exit(main())
