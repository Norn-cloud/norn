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
  output, a property no longer required on an output, a pattern, format or
  bound removed or loosened on an output, an input's `default` changed or
  removed, a CLI default or arity changed, a diagnostic's severity changed;
- a contract-shaped file (a `schema`/`schemas` directory, `*.schema.json`,
  `*.contract.json`) that is neither enrolled here nor listed as internal.

Refusal codes are read from Rust source by a small lexer. It fails closed: a
source it cannot fully classify (an unterminated literal or comment, unbalanced
delimiters, a test `cfg` attribute it cannot scope) fails the check and names
the file and line. It never skips or cuts a file short.

The only escape is an entry for each break in `scripts/ci/contract-breaks.json`
added by the change itself, naming the ruling that approved it. That file is a
gate file: changing it needs an admin's `/approve-gates <head sha>` (the
trusted-policy check), so a break is approved by the same mechanism as a gate
change and never by the author alone. In a merge group every merge step is
checked against the entries that step adds.

In CI it runs inside the required `core` job (`core.sh cheap`).

Usage:
  contract_diff.py --ci   # the GitHub event of this job (GITHUB_EVENT_NAME/PATH)
  contract_diff.py --base REV --head REV
  contract_diff.py --merge-group BASE HEAD
  contract_diff.py --surface ROOT   # print the extracted surface of a checkout
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
from collections import Counter
from dataclasses import dataclass, field
from pathlib import Path
from typing import Callable

OPENAPI = "sdks/openapi/norn-api.openapi.json"
SDK_INDEX = "packages/norn-sdk/src/index.ts"
CLI_SNAPSHOT = "crates/norn-cli/cli.contract.json"
TS_CLI_DIR = "tools/norn-ts/src"
TS_CLI_SNAPSHOT = "tools/norn-ts/cli.contract.json"
DIAGNOSTIC_CODES = "crates/norn-checker/conformance/diagnostic-codes"
SCHEMA_DIRS = ("crates/norn-ir/schema", "crates/norn-kernel/schema", "tools/norn-ts/schema",
               "crates/norn-builtins/src/schemas", "crates/norn-projections/schemas")
# Single schema files outside a schema directory. The metrics ledger is append-only
# and kept forever, so its record format only ever changes additively. The port
# inventory schema types files kept in each ported application's own repository.
SCHEMA_FILES = ("scripts/metrics/record.schema.json", "tools/norn-port/inventory.schema.json")
APPROVALS = "scripts/ci/contract-breaks.json"
# Which list a contract-shaped file goes on is a rule, not a judgement:
# - enrolled (SCHEMA_DIRS, SCHEMA_FILES): it describes data that outlives or
#   leaves its own tool: IR, API, CLI, ledgers, anything a user or another
#   system reads. It may only change additively.
# - internal (below): exactly one in-repo tool reads it and it may change
#   freely. Each entry says which tool.
# Both lists are in this gate file, so every addition needs `/approve-gates`.
INTERNAL = (
    "scripts/ci/implementation-receipt.schema.json",  # read by scripts/ci/implementation_receipts.py
    "verification/coverage-v1/result.schema.json",  # read by verification/coverage-v1/validate.py
    "verification/stripmine-2026-08-22/schemas/",  # proof bookkeeping of that one verification run
    "registry/standards/atlas.schema.json",  # planning data read by scripts/standards_atlas.py; to be restructured
)
# Example and test inputs are not contracts.
NOT_CONTRACT_DIRS = ("fixtures", "test", "tests")
CODE = re.compile(r"[a-z][a-z0-9_]*(?:\.[a-z0-9_]+)+")
# A literal that is a code, or starts with one as `a.b: detail`.
CODE_LITERAL = re.compile(r"(" + CODE.pattern + r")(?:: .*)?", re.DOTALL)
# Where a Rust comment, string, raw string, char literal or lifetime may start.
RUST_TOKEN = re.compile(r"""//|/\*|(?<![A-Za-z0-9_])b?r(#*)"|"|'""")
STRING = re.compile(r'"((?:[^"\\]|\\.)*)"', re.DOTALL)  # across lines: `\` before a newline continues it
CHAR = re.compile(r"'(?:\\(?:x[0-9a-fA-F]{2}|u\{[0-9a-fA-F_]+\}|.)|[^\\'])'", re.DOTALL)
LIFETIME = re.compile(r"'[A-Za-z_][A-Za-z0-9_]*(?!')")
DELIMITER = re.compile(r"[()\[\]{}]")
CFG = re.compile(r"#\s*(!?)\s*\[\s*cfg\s*\(")
ATTRIBUTE = re.compile(r"\s*#\s*\[")
INNER_ATTRIBUTE = re.compile(r"\s*#\s*!\s*\[")
MODULE = re.compile(r"\bmod\s+(\w+)\s*;")
# Items that end at their `;`, or after their first `{ .. }` body.
BLOCK_ITEM = re.compile(r"\s*(?:pub(?:\s*\([^)]*\))?\s+)?(?:(?:default|async|const|unsafe|extern(?:\s*\"\")?)\s+)*"
                        r"(?:fn|mod|struct|enum|union|impl|trait|macro_rules\s*!)\b")
# Items and statements that end at their `;`.
LINE_ITEM = re.compile(r"\s*(?:pub(?:\s*\([^)]*\))?\s+)?(?:let|const|static|type|use|extern\s+crate)\b")

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
        skip = {"node_modules", "target", ".git"}
        return sorted(str(path.relative_to(root)) for path in base.rglob("*")
                      if path.is_file() and not skip & set(path.relative_to(root).parts))

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
    seen: set[tuple[str, str, frozenset[str]]] = field(default_factory=set)


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
        return _same(schema["const"], value)
    if isinstance(schema.get("enum"), list):
        return any(_same(member, value) for member in schema["enum"])
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
_MISSING = object()


def _same(left: object, right: object) -> bool:
    """JSON equality: `1` is not `true` and `0` is not `false`."""
    return json.dumps(left, sort_keys=True) == json.dumps(right, sort_keys=True)


def _default(schema: object, root: object, depth: int = 0) -> object:
    """The default a schema node gives: its own, else the one its `$ref` target gives."""
    if not isinstance(schema, dict) or depth > 16:
        return _MISSING
    if "default" in schema:
        return schema["default"]
    return _default(resolve(root, schema["$ref"]), root, depth + 1) if "$ref" in schema else _MISSING


def _compare_default(old: object, new: object, where: str, out: list[str], docs: Documents) -> None:
    """An input whose effective `default` (its own, else its `$ref` target's)
    changed or went away. The server acts on it when the field is absent, so
    this covers a default beside a `$ref`, one added over the target's, and one
    moved between the two. Two nodes that only point at the same target are
    left to that target, which is compared once by name."""
    if not isinstance(old, dict):
        return
    same_target = isinstance(new, dict) and old.get("$ref") is not None and old.get("$ref") == new.get("$ref")
    if same_target and "default" not in old and "default" not in new:
        return
    before, after = _default(old, docs.old), _default(new, docs.new)
    if before is _MISSING or (after is not _MISSING and _same(before, after)):
        return
    shown = "removed" if after is _MISSING else json.dumps(after)
    out.append(f"{where}: input default changed from {json.dumps(before)} to {shown}")


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
    if IN in directions:
        _compare_default(old, new, where, out, docs)
    old_ref = old.get("$ref") if isinstance(old, dict) else None
    new_ref = new.get("$ref") if isinstance(new, dict) else None
    if old_ref is not None or new_ref is not None:
        if old_ref == new_ref:
            return  # The named schema itself is compared once, by name.
        key = (old_ref or "", new_ref or "", directions)
        if key in docs.seen:
            return
        docs.seen.add(key)
        if old_ref is not None:
            old = resolve(docs.old, old_ref)
        if new_ref is not None:
            new = resolve(docs.new, new_ref)
        if old is None or new is None:
            out.append(f"{where}: $ref {old_ref or new_ref} does not resolve")
            return
    # A schema that only annotates (`{"format": "date"}`) accepts anything, but
    # its format is still what an output promises and an input is parsed as.
    if isinstance(old, dict) and "format" in old and _is_any(old, docs.old):
        after = new.get("format") if isinstance(new, dict) else None
        if after is None and OUT in directions:
            out.append(f"{where}: output format {json.dumps(old['format'])} removed")
        elif after is not None and after != old["format"]:
            out.append(f"{where}: format changed from {json.dumps(old['format'])} to {json.dumps(after)}")
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
        elif key in old and key not in new and OUT in directions:
            out.append(f"{where}: output {key} {json.dumps(old[key])} removed")
    for key, tighter in (("maximum", min), ("exclusiveMaximum", min), ("maxLength", min), ("maxItems", min),
                         ("maxProperties", min), ("minimum", max), ("exclusiveMinimum", max),
                         ("minLength", max), ("minItems", max), ("minProperties", max)):
        if key in new and (key not in old or tighter(old[key], new[key]) != old[key]):
            out.append(f"{where}: {key} tightened from {old.get(key)} to {new[key]}")
        elif key in old and new.get(key) != old[key] and OUT in directions:
            out.append(f"{where}: output {key} loosened from {old[key]} to {new.get(key, 'none')}")

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


class LexError(Exception):
    """The lexer met Rust it cannot fully classify. The check fails: it never
    skips or truncates a file, since what it skipped would leave the surface."""


def _fail(path: str, text: str, index: int, what: str) -> LexError:
    return LexError(f"{path}:{text.count(chr(10), 0, index) + 1}: {what}")


def _lex(text: str, path: str = "<source>") -> tuple[str, list[tuple[int, str]]]:
    """Rust source as structure and literals: the code with comments removed
    and every string, raw string and char literal blanked (so a brace, quote or
    `//` inside one is never read as code), plus each string literal's content
    with its position in that code. Newlines are kept, so a line of the code is
    the same line of the source. Anything unterminated is a `LexError`."""
    out: list[str] = []
    literals: list[tuple[int, str]] = []
    index = position = 0

    def emit(piece: str) -> None:
        nonlocal position
        out.append(piece)
        position += len(piece)

    def blank(placeholder: str, start: int, end: int) -> None:
        emit(placeholder + "\n" * text.count("\n", start, end))

    while True:
        token = RUST_TOKEN.search(text, index)
        if not token:
            emit(text[index:])
            return "".join(out), literals
        emit(text[index:token.start()])
        start, lead = token.start(), token.group(0)
        if lead == "//":
            end = text.find("\n", start)
            index = len(text) if end < 0 else end
        elif lead == "/*":
            depth, index = 1, start + 2
            while depth:
                opened, closed = text.find("/*", index), text.find("*/", index)
                if closed < 0:
                    raise _fail(path, text, start, "unterminated block comment")
                if 0 <= opened < closed:
                    depth, index = depth + 1, opened + 2
                else:
                    depth, index = depth - 1, closed + 2
            blank(" ", start, index)
        elif lead == '"':
            literal = STRING.match(text, start)
            if not literal:
                raise _fail(path, text, start, "unterminated string literal")
            literals.append((position, literal.group(1)))
            blank('""', start, literal.end())
            index = literal.end()
        elif lead == "'":
            literal = CHAR.match(text, start) or LIFETIME.match(text, start)
            if not literal:
                raise _fail(path, text, start, "a quote that is neither a char literal nor a lifetime")
            emit("' '" if literal.re is CHAR else literal.group(0))
            index = literal.end()
        else:
            close = text.find('"' + token.group(1), token.end())
            if close < 0:
                raise _fail(path, text, start, "unterminated raw string literal")
            literals.append((position, text[token.end():close]))
            index = close + 1 + len(token.group(1))
            blank('""', start, index)


def _blocks(code: str, path: str) -> dict[int, int]:
    """Every opening delimiter's index mapped to the index after its closer.
    A closer without its opener, or an opener left open, is a `LexError`."""
    pairs = {")": "(", "]": "[", "}": "{"}
    closes: dict[int, int] = {}
    stack: list[int] = []
    for delimiter in DELIMITER.finditer(code):
        char = delimiter.group(0)
        if char in "([{":
            stack.append(delimiter.start())
        elif not stack or code[stack[-1]] != pairs[char]:
            raise _fail(path, code, delimiter.start(), f"unbalanced `{char}`")
        else:
            closes[stack.pop()] = delimiter.end()
    if stack:
        raise _fail(path, code, stack[-1], f"`{code[stack[-1]]}` is never closed")
    return closes


def _is_test_cfg(condition: str) -> bool:
    """Whether a `cfg(..)` condition holds only in test builds: `test`, or an
    `all(..)` with such a member. `any(test, ..)` and `not(test)` are production."""
    condition = "".join(condition.split())
    if condition == "test":
        return True
    if not (condition.startswith("all(") and condition.endswith(")")):
        return False
    members, depth, start = [], 0, 4
    for index in range(4, len(condition) - 1):
        depth += (condition[index] == "(") - (condition[index] == ")")
        if condition[index] == "," and depth == 0:
            members.append(condition[start:index])
            start = index + 1
    members.append(condition[start:-1])
    return any(_is_test_cfg(member) for member in members)


def _gated_end(code: str, index: int, closes: dict[int, int], path: str) -> int:
    """The index after what an outer attribute ending before `index` gates.

    - `fn`, `mod`, `struct`, `enum`, `impl`, `trait`, ..: through the item's
      `;`, or through its first `{ .. }` body.
    - `let`, `const`, `static`, `type`, `use`: through its `;`.
    - Anything else is a field, a variant, a match arm, a struct-literal field
      or an expression: through its `,`; or up to the closer of the list it is
      the last member of; or through its `{ .. }` when nothing continues the
      expression after it (a match arm with a block body and no comma).
    Reaching the end of the file without one of these is a `LexError`."""
    block_item, line_item = BLOCK_ITEM.match(code, index), LINE_ITEM.match(code, index)
    cursor = index
    while cursor < len(code):
        char = code[cursor]
        if char in "([" or (char == "{" and not block_item):
            after = closes[cursor]
            if char == "{" and not line_item:
                rest = code[after:].lstrip()
                if rest.startswith(","):
                    return code.index(",", after) + 1
                if not rest.startswith((".", "?", "=", "+", "-", "*", "/", "%", "|", "&", "<", ">", "as ", "else", "(", "[")):
                    return after
            cursor = after
        elif char == "{":
            return closes[cursor]
        elif char == ";" or (char == "," and not block_item and not line_item):
            return cursor + 1
        elif char in ")]}":
            if block_item or line_item:
                break
            return cursor
        else:
            cursor += 1
    raise _fail(path, code, index, "cannot tell where the item under this test `cfg` attribute ends")


@dataclass
class RustFile:
    """One lexed source: its string literals outside test-only items, whether
    the whole file is test-only, and the module files it declares (those
    declared under a test `cfg` separately)."""
    literals: list[str]
    test_only: bool
    modules: list[str]
    test_modules: list[str]
    test_ranges: list[tuple[int, int]] = field(default_factory=list)
    code: str = ""


def _rust_file(path: str, text: str) -> RustFile:
    """Lex one source and scope its test-only items. Raises `LexError` rather
    than guess: on an unterminated literal or comment, unbalanced delimiters,
    or a test `cfg` attribute whose extent it cannot determine."""
    code, literals = _lex(text, path)
    closes = _blocks(code, path)
    braces = sorted((start, end) for start, end in closes.items() if code[start] == "{")
    ranges: list[tuple[int, int]] = []
    test_only = False
    for cfg in CFG.finditer(code):
        condition_end = closes[cfg.end() - 1]
        if not _is_test_cfg(code[cfg.end():condition_end - 1]):
            continue
        attribute_end = closes[code.index("[", cfg.start())]
        if cfg.group(1):
            # `#![cfg(test)]` gates what encloses it: the file when it comes
            # before the first item, else the block it is written in.
            lead = 0
            while True:
                inner = INNER_ATTRIBUTE.match(code, lead)
                if not inner or inner.start() > cfg.start():
                    break
                lead = closes[inner.end() - 1]
            if not code[lead:cfg.start()].strip() or lead > cfg.start():
                test_only = True
                continue
            enclosing = [(start, end) for start, end in braces if start < cfg.start() < end]
            if not enclosing:
                raise _fail(path, code, cfg.start(), "`#![cfg(test)]` after the first item but inside no block")
            ranges.append((max(enclosing)[0] + 1, max(enclosing)[1] - 1))
            continue
        index = attribute_end
        while True:
            attribute = ATTRIBUTE.match(code, index)
            if not attribute:
                break
            index = closes[attribute.end() - 1]
        ranges.append((cfg.start(), _gated_end(code, index, closes, path)))
    directory = path.rsplit("/", 1)[0]
    base = directory if path.rsplit("/", 1)[-1] in ("mod.rs", "lib.rs", "main.rs") else path[:-3]
    modules: list[str] = []
    test_modules: list[str] = []
    for module in MODULE.finditer(code):
        gate = next(((start, end) for start, end in ranges if start <= module.start() < end), None)
        declared = [f"{base}/{module.group(1)}.rs", f"{base}/{module.group(1)}/"]
        # `#[path = "x.rs"]` on the declaration names the file itself.
        attributes = code.rfind(";", 0, module.start()) + 1
        declared += [os.path.normpath(f"{directory}/{literal}") for position, literal in literals
                     if max(attributes, code.rfind("}", 0, module.start()) + 1) <= position < module.start()
                     and literal.endswith(".rs")]
        (test_modules if gate else modules).extend(declared)
    kept = [literal for position, literal in literals if not any(start <= position < end for start, end in ranges)]
    return RustFile(kept, test_only, modules, test_modules, ranges, code)


def _codes(literals: list[str]) -> Counter[str]:
    return Counter(match.group(1) for match in map(CODE_LITERAL.fullmatch, literals) if match)


def rust_literals(text: str, path: str = "crates/x/src/lib.rs") -> set[str]:
    """Every code-shaped string literal of one source outside comments and test-only items."""
    return set(_codes(_rust_file(path, text).literals))


def _rust_sources(tree: Tree) -> list[str]:
    """Every `.rs` under a crate's `src`. Nothing is excluded by its name."""
    return [path for path in tree.list("crates")
            if len(path.split("/")) >= 4 and path.split("/")[2] == "src" and path.endswith(".rs")]


def production_files(tree: Tree) -> dict[str, RustFile]:
    """The lexed production sources: every file under `crates/*/src` except a
    `#![cfg(test)]` file, a module file declared under a test `cfg`, and the
    module files those declare in turn. A file is test-only because a test
    `cfg` says so, never because of its name."""
    files = {path: _rust_file(path, tree.read(path) or "") for path in _rust_sources(tree)}

    def owned(modules: list[str]) -> set[str]:
        return {path for path in files for module in modules
                if path == module or (module.endswith("/") and path.startswith(module))}

    test_only = {path for path, file in files.items() if file.test_only}
    test_only |= owned([module for file in files.values() for module in file.test_modules])
    while True:
        more = owned([module for path in test_only for module in files[path].modules + files[path].test_modules]) - test_only
        if not more:
            return {path: file for path, file in files.items() if path not in test_only}
        test_only |= more


def refusal_codes(tree: Tree) -> Counter[str]:
    """The refusal-code surface, independent of how a code is constructed: every
    string literal of code shape (lowercase dotted segments, alone or as the
    `a.b: detail` prefix of a message) in production Rust under `crates/*/src`
    (see `production_files`), outside comments and items under `#[cfg(test)]`
    or `#[cfg(all(test, ..))]`. Each code maps to how many times production
    spells it. This over-approximates on purpose: a literal that is not a code
    costs an approval when it is removed, and a code the check does not see
    would be a silent break. A file the lexer cannot classify is a `LexError`,
    which fails the check; no file is skipped or cut short."""
    codes: Counter[str] = Counter()
    for file in production_files(tree).values():
        codes += _codes(file.literals)
    return codes


def removed_refusal_codes(old: Tree, new: Tree) -> dict[str, tuple[int, int]]:
    """Codes production spells fewer times at the head than at the base, as
    code -> (before, after). Moving a code within the workspace keeps its
    count; renaming it at one site lowers it, even while another site (an
    error message attribute, say) still spells the old name; merging duplicate
    spellings into one constant lowers it too, and costs an approval.

    Limits of counting, which make this a net for accidents and not a proof:
    - a rename is masked when the same change adds another production spelling
      of the old code anywhere (a second call site, a constant, an item under
      `#[cfg(any(test, ..))]`);
    - counts are per workspace, so a code that moves between crates passes;
    - a code assembled at run time (`format!`, `concat!`) or read from a data
      file is not a literal and is not seen."""
    before, after = refusal_codes(old), refusal_codes(new)
    return {code: (count, after[code]) for code, count in before.items() if after[code] < count}


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


def unenrolled_surfaces(tree: Tree) -> list[str]:
    """Contract-shaped files this check neither compares nor lists as internal."""
    enrolled = {OPENAPI, CLI_SNAPSHOT, TS_CLI_SNAPSHOT}
    found = []
    for path in tree.list("."):
        parts = path.split("/")
        if not path.endswith(".json") or path.startswith(".github/") or set(parts[:-1]) & set(NOT_CONTRACT_DIRS):
            continue
        shaped = path.endswith((".schema.json", ".contract.json")) or bool({"schema", "schemas"} & set(parts[:-1]))
        if not shaped or path in enrolled or path in SCHEMA_FILES or path.startswith(tuple(f"{d}/" for d in SCHEMA_DIRS)) \
                or path.startswith(INTERNAL) or path.startswith(DIAGNOSTIC_CODES):
            continue
        found.append(path)
    return found


def approvals(tree: Tree) -> set[str]:
    """The breaks `contract-breaks.json` approves, as `<break line>`."""
    text = tree.read(APPROVALS)
    if not text:
        return set()
    document = json.loads(text)
    return {entry["break"] for entry in document.get("approved", [])
            if isinstance(entry, dict) and isinstance(entry.get("break"), str)
            and re.fullmatch(r"D\d+|#\d+|https?://\S+", str(entry.get("ruling", "")))}


def schema_files(tree: Tree) -> dict[str, str]:
    files = {path: tree.read(path) or "" for directory in SCHEMA_DIRS for path in tree.list(directory)
             if path.endswith(".json")}
    for path in SCHEMA_FILES:
        text = tree.read(path)
        if text is not None:
            files[path] = text
    return files


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
    for code, (before_count, after_count) in sorted(removed_refusal_codes(old, new).items()):
        out.append(f"refusal code {code}: removed" if after_count == 0
                   else f"refusal code {code}: production spellings reduced from {before_count} to {after_count}")
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
    for path in unenrolled_surfaces(new):
        out.append(f"{path}: contract-shaped file not enrolled in scripts/ci/contract_diff.py (enrol it, or list it as internal)")
    return out


def verdict(old: Tree, new: Tree, label: str) -> bool:
    """Pass when every break from `old` to `new` is approved by an entry that
    `new` adds to `contract-breaks.json` (a gate file: `/approve-gates`). A
    source the lexer cannot classify fails, and no entry can approve that."""
    try:
        breaks = diff(old, new)
    except LexError as error:
        print(f"{label}: cannot read the refusal-code surface: {error}\n"
              "The check fails instead of skipping what it cannot classify. Teach the lexer in "
              "scripts/ci/contract_diff.py this form (a gate-file change), or write the source another way.")
        return False
    if not breaks:
        print(f"{label}: no contract breaks")
        return True
    approved = approvals(new) - approvals(old)
    unapproved = [line for line in breaks if line not in approved]
    print(f"{label}: {len(breaks)} non-additive contract change(s), {len(breaks) - len(unapproved)} approved")
    for line in breaks:
        print(f"  - {line}" + ("" if line in unapproved else "  [approved]"))
    if not unapproved:
        return True
    print(f"Frozen contracts are additive-only (D84). Make the change additive, or add each line above as\n"
          f'{{"ruling": "D<n>", "break": "<line>"}} to {APPROVALS}; that gate file needs an admin\'s /approve-gates.')
    return False


def merge_group(base: str, head: str) -> bool:
    """Check each merge step of a queue group against what that step adds."""
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
        ok = verdict(git_tree(previous), git_tree(commit), f"merge of {pull_head[:12]}") and ok
        commit = previous
    print("merge group is longer than 64 merges or misses its base")
    return False


def ci(event_name: str, event: dict) -> bool:
    """The check for one GitHub event, as `core.sh cheap` runs it: a pull
    request against its merge-base, a merge group step by step. Other events
    check nothing."""
    if event_name == "pull_request":
        pull = event["pull_request"]
        base, head = pull["base"]["sha"], pull["head"]["sha"]
        merge_base = git("merge-base", base, head).strip()
        return verdict(git_tree(merge_base), git_tree(head), f"{merge_base[:12]}..{head[:12]}")
    if event_name == "merge_group":
        group = event["merge_group"]
        return merge_group(group["base_sha"], group["head_sha"])
    print(f"contract diff: nothing to check for a {event_name or 'local'} run")
    return True


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--base")
    parser.add_argument("--head")
    parser.add_argument("--merge-group", nargs=2, metavar=("BASE", "HEAD"))
    parser.add_argument("--surface", metavar="ROOT", help="print the surface extracted from a checkout")
    parser.add_argument("--ci", action="store_true", help="check the GitHub event this job runs for")
    args = parser.parse_args()
    if args.ci:
        event_path = os.environ.get("GITHUB_EVENT_PATH")
        event = json.loads(Path(event_path).read_text(encoding="utf-8")) if event_path else {}
        return 0 if ci(os.environ.get("GITHUB_EVENT_NAME", ""), event) else 1
    if args.surface:
        tree = disk_tree(Path(args.surface))
        commands, flags = ts_cli(tree)
        print(json.dumps({"refusalCodes": sorted(refusal_codes(tree)), "diagnosticCodes": len(diagnostic_codes(tree)),
                          "unenrolled": unenrolled_surfaces(tree),
                          "schemas": sorted(schema_files(tree)), "sdkExports": sorted(sdk_exports(tree.read(SDK_INDEX))),
                          "authoringCli": {"commands": sorted(commands), "flags": sorted(flags)},
                          "cli": rust_cli(tree)}, indent=2))
        return 0
    if args.merge_group:
        return 0 if merge_group(*args.merge_group) else 1
    if not (args.base and args.head):
        parser.error("--base and --head are required")
    merge_base = git("merge-base", args.base, args.head).strip()
    return 0 if verdict(git_tree(merge_base), git_tree(args.head), f"{merge_base[:12]}..{args.head[:12]}") else 1


if __name__ == "__main__":
    sys.exit(main())
