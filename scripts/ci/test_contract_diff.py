#!/usr/bin/env python3
"""contract_diff: properties over generated contracts, and the pull request and
merge-group scenarios end to end in a scratch repository.

Properties (each over generated schemas, fixed seeds so a failure replays):
- a contract never breaks itself, and re-encoding it (reordering, inlining or
  extracting a `$ref`, wrapping a struct in `allOf`) is not a change;
- every additive edit passes, in every direction;
- every non-additive edit fails and names the place it changed;
- an additive edit never hides a break made alongside it.
"""

import copy
import json
import random
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import contract_diff as cd  # noqa: E402

BOTH = frozenset({cd.IN, cd.OUT})
SEEDS = range(300)
SCALARS = ("string", "integer", "boolean", "number")


def diff_schema(old, new, directions=BOTH):
    out: list[str] = []
    cd.compare_schema(old, new, "s", directions, out, cd.Documents(old, new))
    return out


class Gen:
    """Random JSON schemas of the shapes Norn's generators emit."""

    def __init__(self, seed: int):
        self.random = random.Random(seed)
        self.names = iter(f"p{i}" for i in range(10_000))

    def scalar(self) -> dict:
        kind = self.random.choice(SCALARS)
        schema: dict = {"type": kind}
        if kind == "string" and self.random.random() < 0.4:
            schema["enum"] = [f"v{i}" for i in range(self.random.randint(2, 4))]
        elif kind == "string" and self.random.random() < 0.3:
            schema["maxLength"] = self.random.randint(8, 64)
        return schema

    def value(self, depth: int) -> dict:
        roll = self.random.random()
        if depth <= 0 or roll < 0.45:
            return self.scalar()
        if roll < 0.6:
            return {"type": "array", "items": self.value(depth - 1)}
        if roll < 0.75:
            return self.union(depth - 1)
        return self.object(depth - 1)

    def object(self, depth: int) -> dict:
        properties = {next(self.names): self.value(depth) for _ in range(self.random.randint(1, 4))}
        required = [name for name in properties if self.random.random() < 0.5]
        schema = {"type": "object", "properties": properties}
        if required:
            schema["required"] = required
        return schema

    def union(self, depth: int) -> dict:
        alternatives = []
        for tag in range(self.random.randint(2, 3)):
            alternative = self.object(max(depth, 0))
            alternative["properties"]["kind"] = {"type": "string", "enum": [f"k{tag}"]}
            alternative["required"] = sorted({*alternative.get("required", []), "kind"})
            alternatives.append(alternative)
        return {"oneOf": alternatives}

    def document(self) -> dict:
        root = self.object(3)
        root["$defs"] = {"Shared": self.object(1)}
        root["properties"]["shared"] = {"$ref": "#/$defs/Shared"}
        return root


def nodes(schema, path=()):
    """Every (path, object schema) pair reachable through properties, items and unions."""
    if not isinstance(schema, dict):
        return
    yield path, schema
    for name, child in (schema.get("properties") or {}).items():
        yield from nodes(child, (*path, "properties", name))
    if isinstance(schema.get("items"), dict):
        yield from nodes(schema["items"], (*path, "items"))
    for index, child in enumerate(schema.get("oneOf") or []):
        yield from nodes(child, (*path, "oneOf", index))
    for name, child in (schema.get("$defs") or {}).items():
        yield from nodes(child, (*path, "$defs", name))


def pick(rng, schema, accept):
    found = [(path, node) for path, node in nodes(schema) if accept(node)]
    return rng.choice(found) if found else (None, None)


# Additive edits: (name, edit(rng, schema) -> bool applied).
def add_optional_property(rng, s):
    _, node = pick(rng, s, lambda n: n.get("type") == "object")
    node["properties"]["added"] = {"type": rng.choice(SCALARS)}
    return True


def add_enum_value(rng, s):
    # A one-value enum is a union tag: widening it changes which variant it is.
    _, node = pick(rng, s, lambda n: len(n.get("enum") or []) > 1)
    if node is None:
        return False
    node["enum"].append("added")
    return True


def add_union_alternative(rng, s):
    _, node = pick(rng, s, lambda n: "oneOf" in n)
    if node is None:
        return False
    node["oneOf"].append({"type": "object", "required": ["kind"], "properties": {"kind": {"type": "string", "enum": ["added"]}}})
    return True


def loosen_bound(rng, s):
    _, node = pick(rng, s, lambda n: "maxLength" in n)
    if node is None:
        return False
    node["maxLength"] += 10
    return True


def add_unused_definition(rng, s):
    s["$defs"]["Unused"] = {"type": "object", "required": ["x"], "properties": {"x": {"type": "string"}}}
    return True


ADDITIVE = (add_optional_property, add_enum_value, add_union_alternative, add_unused_definition)
# Additive for senders only: a reader of an output gets more than it was promised.
INPUT_ADDITIVE = (loosen_bound,)


# Re-encodings: the same contract written differently.
def reorder(rng, s):
    for _, node in nodes(s):
        if "properties" in node:
            items = list(node["properties"].items())
            rng.shuffle(items)
            node["properties"] = dict(items)
        if "oneOf" in node:
            rng.shuffle(node["oneOf"])
    return True


def inline_ref(rng, s):
    if s["properties"].get("shared") != {"$ref": "#/$defs/Shared"}:
        return False
    s["properties"]["shared"] = copy.deepcopy(s["$defs"]["Shared"])
    return True


def extract_ref(rng, s):
    path, node = pick(rng, s, lambda n: n.get("type") == "object" and n is not s)
    if node is None or path[:1] == ("$defs",):
        return False
    s["$defs"]["Extracted"] = copy.deepcopy(node)
    node.clear()
    node["$ref"] = "#/$defs/Extracted"
    return True


def flatten_into_all_of(rng, s):
    _, node = pick(rng, s, lambda n: n.get("type") == "object" and "properties" in n and n is not s)
    if node is None:
        return False
    inner = {"type": "object", "properties": node.pop("properties")}
    if "required" in node:
        inner["required"] = node.pop("required")
    node.pop("type")
    node["allOf"] = [inner]
    return True


REENCODINGS = (reorder, inline_ref, extract_ref, flatten_into_all_of)


# Breaking edits: return the name the report must mention, or None if not applicable.
def remove_property(rng, s):
    path, node = pick(rng, s, lambda n: len(n.get("properties") or {}) > 1)
    name = rng.choice([p for p in node["properties"] if p != "kind"] or [None])
    if name is None:
        return None
    del node["properties"][name]
    node["required"] = [r for r in node.get("required", []) if r != name]
    return name


def rename_property(rng, s):
    _, node = pick(rng, s, lambda n: any(p != "kind" for p in n.get("properties") or {}))
    name = rng.choice([p for p in node["properties"] if p != "kind"])
    node["properties"][name + "_renamed"] = node["properties"].pop(name)
    return name


def remove_enum_value(rng, s):
    _, node = pick(rng, s, lambda n: len(n.get("enum") or []) > 1)
    if node is None:
        return None
    value = node["enum"].pop()
    return json.dumps(value)


def retype(rng, s):
    _, node = pick(rng, s, lambda n: n.get("type") in SCALARS and "enum" not in n)
    if node is None:
        return None
    node["type"] = rng.choice([t for t in SCALARS if t != node["type"] and {t, node["type"]} != {"integer", "number"}])
    node.pop("maxLength", None)
    return "type"


def require_optional(rng, s):
    _, node = pick(rng, s, lambda n: set(n.get("properties") or {}) - set(n.get("required") or []))
    if node is None:
        return None
    name = rng.choice(sorted(set(node["properties"]) - set(node.get("required") or [])))
    node["required"] = [*node.get("required", []), name]
    return name


def remove_alternative(rng, s):
    _, node = pick(rng, s, lambda n: len(n.get("oneOf") or []) > 1)
    if node is None:
        return None
    node["oneOf"].pop(rng.randrange(len(node["oneOf"])))
    return "alternative removed"


def tighten_bound(rng, s):
    _, node = pick(rng, s, lambda n: "maxLength" in n)
    if node is None:
        return None
    node["maxLength"] -= 1
    return "maxLength"


BREAKING = (remove_property, rename_property, remove_enum_value, retype, require_optional, remove_alternative, tighten_bound)


class SchemaProperties(unittest.TestCase):
    def test_a_contract_never_breaks_itself(self):
        for seed in SEEDS:
            schema = Gen(seed).document()
            self.assertEqual(diff_schema(schema, copy.deepcopy(schema)), [], seed)

    def test_re_encoding_a_contract_is_not_a_change(self):
        for seed in SEEDS:
            rng = random.Random(seed)
            schema = Gen(seed).document()
            for encode in REENCODINGS:
                after = copy.deepcopy(schema)
                if encode(rng, after):
                    self.assertEqual(diff_schema(schema, after), [], f"{encode.__name__} seed {seed}")

    def test_additive_edits_pass_in_every_direction(self):
        for seed in SEEDS:
            rng = random.Random(seed)
            schema = Gen(seed).document()
            for edit in ADDITIVE:
                after = copy.deepcopy(schema)
                if not edit(rng, after):
                    continue
                for directions in (frozenset({cd.IN}), frozenset({cd.OUT}), BOTH):
                    self.assertEqual(diff_schema(schema, after, directions), [], f"{edit.__name__} {sorted(directions)} seed {seed}")
            for edit in INPUT_ADDITIVE:
                after = copy.deepcopy(schema)
                if not edit(rng, after):
                    continue
                self.assertEqual(diff_schema(schema, after, frozenset({cd.IN})), [], f"{edit.__name__} seed {seed}")
                self.assertTrue(any("loosened" in line for line in diff_schema(schema, after, frozenset({cd.OUT}))), seed)

    def test_non_additive_edits_fail_and_name_the_change(self):
        applied = {edit.__name__: 0 for edit in BREAKING}
        for seed in SEEDS:
            rng = random.Random(seed)
            schema = Gen(seed).document()
            for edit in BREAKING:
                after = copy.deepcopy(schema)
                mark = edit(rng, after)
                if mark is None:
                    continue
                applied[edit.__name__] += 1
                breaks = diff_schema(schema, after)
                self.assertTrue(any(mark in line for line in breaks), f"{edit.__name__} seed {seed}: {breaks}")
        # Each edit was exercised, so a vacuous pass cannot hide behind the generator.
        self.assertTrue(all(count >= 30 for count in applied.values()), applied)

    def test_an_additive_edit_never_hides_a_break(self):
        for seed in SEEDS:
            rng = random.Random(seed)
            schema = Gen(seed).document()
            after = copy.deepcopy(schema)
            mark = rng.choice(BREAKING)(rng, after)
            if mark is None:
                continue
            for edit in (*ADDITIVE, *REENCODINGS):
                edit(rng, after)
            self.assertNotEqual(diff_schema(schema, after), [], seed)

    def test_direction_decides_widening_and_requiredness(self):
        optional = {"type": "object", "properties": {"a": {"type": "string"}}}
        required = {**optional, "required": ["a"]}
        nullable = {"type": "object", "properties": {"a": {"type": ["string", "null"]}}}
        self.assertTrue(diff_schema(optional, required, frozenset({cd.IN})))
        self.assertFalse(diff_schema(optional, required, frozenset({cd.OUT})))
        self.assertFalse(diff_schema(required, optional, frozenset({cd.IN})))
        self.assertTrue(diff_schema(required, optional, frozenset({cd.OUT})))
        self.assertFalse(diff_schema(optional, nullable, frozenset({cd.IN})))
        self.assertTrue(diff_schema(optional, nullable, frozenset({cd.OUT})))

    def test_an_input_default_is_part_of_the_contract(self):
        before = {"type": "object", "properties": {"tz": {"type": "string", "default": "iso_utc"}}}
        changed = copy.deepcopy(before)
        changed["properties"]["tz"]["default"] = "local"
        removed = copy.deepcopy(before)
        del removed["properties"]["tz"]["default"]
        for after, what in ((changed, '"local"'), (removed, "removed")):
            self.assertTrue(any("default changed" in line and what in line
                                for line in diff_schema(before, after, frozenset({cd.IN}))), what)
            # A default is not something a reader of an output relies on.
            self.assertEqual(diff_schema(before, after, frozenset({cd.OUT})), [], what)
        added = {"type": "object", "properties": {"tz": {"type": "string"}}}
        self.assertEqual(diff_schema(added, before, frozenset({cd.IN})), [])

    def test_a_default_beside_a_ref_is_compared(self):
        # The shape of `question_ir.schema.json` /properties/calendar.
        before = {"type": "object", "properties": {"calendar": {"$ref": "#/$defs/CalendarPolicy", "default": "iso_utc"}},
                  "$defs": {"CalendarPolicy": {"type": "string", "enum": ["iso_utc", "local"]}}}
        changed = copy.deepcopy(before)
        changed["properties"]["calendar"]["default"] = "local"
        removed = copy.deepcopy(before)
        del removed["properties"]["calendar"]["default"]
        self.assertEqual(diff_schema(before, changed, frozenset({cd.IN})),
                         ['s.calendar: input default changed from "iso_utc" to "local"'])
        self.assertEqual(diff_schema(before, removed, frozenset({cd.IN})),
                         ['s.calendar: input default changed from "iso_utc" to removed'])
        self.assertEqual(diff_schema(before, changed, frozenset({cd.OUT})), [])
        # Moving the same default onto the referenced schema, or inlining it, is a re-encoding.
        moved = copy.deepcopy(removed)
        moved["$defs"]["CalendarPolicy"]["default"] = "iso_utc"
        self.assertEqual(diff_schema(before, moved, frozenset({cd.IN})), [])
        inlined = copy.deepcopy(before)
        inlined["properties"]["calendar"] = {"type": "string", "enum": ["iso_utc", "local"], "default": "iso_utc"}
        self.assertEqual(diff_schema(before, inlined, frozenset({cd.IN})), [])
        # A sibling default added over the target's own default changes what the server does.
        over = copy.deepcopy(moved)
        over["properties"]["calendar"]["default"] = "local"
        self.assertEqual(diff_schema(moved, over, frozenset({cd.IN})),
                         ['s.calendar: input default changed from "iso_utc" to "local"'])
        same = copy.deepcopy(moved)
        same["properties"]["calendar"]["default"] = "iso_utc"
        self.assertEqual(diff_schema(moved, same, frozenset({cd.IN})), [])
        # The referenced schema's own default is compared where it is defined.
        own = copy.deepcopy(moved)
        own["$defs"]["CalendarPolicy"]["default"] = "local"
        self.assertTrue(any("default changed" in line for line in diff_schema(moved, own, frozenset({cd.IN}))))

    def test_a_default_or_enum_is_compared_as_json(self):
        for before, after in ((1, True), (0, False), (True, 1)):
            schema = lambda value: {"type": "object", "properties": {"n": {"default": value}}}
            self.assertTrue(diff_schema(schema(before), schema(after), frozenset({cd.IN})), (before, after))
        self.assertTrue(diff_schema({"enum": [1, "a"]}, {"enum": [True, "a"]}))
        # A schema that is only a default or a format is still compared.
        self.assertTrue(diff_schema({"default": "a"}, {"default": "b"}, frozenset({cd.IN})))
        self.assertTrue(diff_schema({"format": "date"}, {}, frozenset({cd.OUT})))
        self.assertTrue(diff_schema({"format": "date"}, {"format": "date-time"}, frozenset({cd.IN})))
        self.assertEqual(diff_schema({"format": "date"}, {}, frozenset({cd.IN})), [])

    def test_an_output_pattern_or_format_removed_widens_it(self):
        for key, value in (("pattern", "^[a-z]+$"), ("format", "date")):
            before = {"type": "string", key: value}
            after = {"type": "string"}
            self.assertTrue(any(f"output {key}" in line for line in diff_schema(before, after, frozenset({cd.OUT}))), key)
            self.assertEqual(diff_schema(before, after, frozenset({cd.IN})), [], key)


def openapi(paths: dict, schemas: dict) -> dict:
    return {"openapi": "3.1.0", "paths": paths, "components": {"schemas": schemas}}


def route(request: str | None, response: str, params=()) -> dict:
    operation = {"operationId": "op", "parameters": list(params),
                 "responses": {"200": {"content": {"application/json": {"schema": {"$ref": f"#/components/schemas/{response}"}}}}}}
    if request:
        operation["requestBody"] = {"required": True, "content": {"application/json": {"schema": {"$ref": f"#/components/schemas/{request}"}}}}
    return {"post": operation}


class OpenApiProperties(unittest.TestCase):
    def setUp(self):
        self.schemas = {"Req": {"type": "object", "required": ["a"], "properties": {"a": {"type": "string"}}},
                        "Res": {"type": "object", "required": ["b"], "properties": {"b": {"type": "string"}}}}
        self.base = openapi({"/v1/x": route("Req", "Res", [{"name": "org", "in": "query", "required": True, "schema": {"type": "string"}}])}, self.schemas)

    def diff(self, new):
        out: list[str] = []
        cd.compare_openapi(self.base, new, out)
        return out

    def edited(self, edit):
        new = copy.deepcopy(self.base)
        edit(new)
        return self.diff(new)

    def test_additions_pass(self):
        for edit in (
            lambda d: d["paths"].__setitem__("/v1/y", route(None, "Res")),
            lambda d: d["paths"]["/v1/x"]["post"]["parameters"].append({"name": "page", "in": "query", "schema": {"type": "string"}}),
            lambda d: d["components"]["schemas"]["Res"]["properties"].__setitem__("c", {"type": "integer"}),
            lambda d: d["components"]["schemas"]["Res"].__setitem__("required", ["b", "c"]) or d["components"]["schemas"]["Res"]["properties"].__setitem__("c", {"type": "integer"}),
            lambda d: d["paths"]["/v1/x"]["post"]["responses"].__setitem__("409", {"description": "conflict"}),
        ):
            self.assertEqual(self.edited(edit), [])

    def test_removals_and_narrowings_fail(self):
        cases = {
            "endpoint removed": lambda d: d["paths"].pop("/v1/x"),
            "method removed": lambda d: d["paths"]["/v1/x"].__setitem__("get", d["paths"]["/v1/x"].pop("post")),
            "operationId changed": lambda d: d["paths"]["/v1/x"]["post"].__setitem__("operationId", "renamed"),
            "parameter org removed": lambda d: d["paths"]["/v1/x"]["post"].__setitem__("parameters", []),
            "required query parameter page added": lambda d: d["paths"]["/v1/x"]["post"]["parameters"].append({"name": "page", "in": "query", "required": True}),
            "response 200 removed": lambda d: d["paths"]["/v1/x"]["post"]["responses"].pop("200"),
            "Res: removed": lambda d: d["components"]["schemas"].__setitem__("Res2", d["components"]["schemas"].pop("Res")),
            ".c: newly required on an input": lambda d: d["components"]["schemas"]["Req"].update(required=["a", "c"], properties={"a": {"type": "string"}, "c": {"type": "string"}}),
            ".b: no longer required on an output": lambda d: d["components"]["schemas"]["Res"].pop("required"),
        }
        for mark, edit in cases.items():
            breaks = self.edited(edit)
            self.assertTrue(any(mark in line for line in breaks), f"{mark}: {breaks}")

    def test_positional_variant_names_follow_their_tag(self):
        def union(tags):
            schemas = {"U": {"oneOf": [{"$ref": f"#/components/schemas/UOneOf{i}"} for i in range(len(tags))]}}
            for i, tag in enumerate(tags):
                schemas[f"UOneOf{i}"] = {"type": "object", "required": ["kind"], "properties": {"kind": {"type": "string", "enum": [tag]}}}
            return openapi({}, schemas)
        out: list[str] = []
        cd.compare_openapi(union(["a", "c"]), union(["a", "b", "c"]), out)
        self.assertEqual(out, [])
        cd.compare_openapi(union(["a", "b", "c"]), union(["a", "c"]), out)
        self.assertTrue(any('kind="b"' in line for line in out), out)


class CodeAndCliProperties(unittest.TestCase):
    def test_a_code_is_declared_however_it_is_constructed(self):
        text = '''
fn refuse() -> Refusal {
    Refusal::new(StatusCode::CONFLICT, "defs.stale", format!("lost {}", "x.y"))
}
fn setup() -> Failure { Failure::setup("norn.dev.io", "norn.json is unreadable") }
fn usage() -> Failure { Failure { code: "norn.dev.usage", message: String::new(), exit: 2 } }
fn dispute() -> CellError { CellError::ComputeRefused("core.world.source_dispute_unqualified") }
fn options() -> Result<(), DecisionRefusal> {
    let invalid = DecisionRefusal::InvalidRequest;
    Err(invalid("decision.option_count"))
}
#[derive(thiserror::Error)]
enum CellError {
    #[error("core.world.frontier_behind")]
    Behind,
    #[error("law.wording.incomplete_locale: {0}")]
    Locale(String),
}
const RAW: &str = r#"raw.code"#;
'''
        codes = {"defs.stale", "x.y", "norn.dev.io", "norn.dev.usage", "core.world.source_dispute_unqualified",
                 "decision.option_count", "core.world.frontier_behind", "law.wording.incomplete_locale", "raw.code"}
        self.assertEqual(cd.rust_literals(text), codes)
        # Renaming a code of any form removes it from the surface: a break.
        for code in codes - {"x.y"}:
            renamed = text.replace(code, f"{code}_renamed")
            self.assertNotIn(code, cd.rust_literals(renamed), code)

            def tree(source):
                files = {"crates/norn-x/src/lib.rs": source}
                return cd.Tree(lambda d: sorted(files) if d in (".", "crates") else [], files.get)
            self.assertEqual(cd.diff(tree(text), tree(renamed)), [f"refusal code {code}: removed"], code)

    def test_tests_and_comments_never_hide_or_declare_a_code(self):
        text = '''
const A: &str = "first.code";
#[cfg(test)]
mod tests {
    const BRACE: char = '{';
    const QUOTE: char = '"';
    const T: &str = "test.only";
}
const B: &str = "after.tests";
#[cfg(all(test, unix))]
#[allow(clippy::unwrap_used, reason = "tests")]
mod unix_tests {
    const U: &str = "unix.test_only";
}
#[cfg(test)]
fn helper() -> &'static str { "test.helper" }
#[cfg(test)]
use crate::x::{a, b};
#[cfg(any(test, feature = "testkit"))]
const KIT: &str = "testkit.code";
#[cfg(not(test))]
const PROD: &str = "prod.code";
const C: &str = "after.all";
// const OLD: &str = "removed.code"; kept in a comment
/* "also.removed" /* nested */ "still.comment" */
const URL: &str = "http://x.y"; // a "comment.code" after a string with //
fn life<'a>(x: &'a str) -> &'a str { "life.time" }
'''
        self.assertEqual(cd.rust_literals(text), {"first.code", "after.tests", "testkit.code", "prod.code", "after.all", "life.time"})

    def test_a_file_is_test_only_because_a_test_cfg_says_so_never_by_its_name(self):
        files = {
            "crates/a/src/lib.rs": 'const A: &str = "prod.a";\n#[cfg(test)]\nmod checks;\n#[cfg(test)]\n#[path = "odd/place.rs"]\nmod elsewhere;\n'
                                   'mod real;\nmod inheritance;\n#[cfg(all(test, unix))]\nmod dev_tests;\nmod commands;\n',
            "crates/a/src/checks.rs": 'mod helpers;\nconst T: &str = "declared.test_module";\n',
            "crates/a/src/checks/helpers.rs": 'const T: &str = "module.of_a_test_module";\n',
            "crates/a/src/odd/place.rs": 'const T: &str = "path.test_module";\n',
            "crates/a/src/real.rs": 'const R: &str = "prod.real";\n',
            "crates/a/src/inheritance.rs": 'const I: &str = "prod.inheritance";\n#[cfg(test)]\nmod tests;\n',
            "crates/a/src/inheritance/tests.rs": 'const T: &str = "nested.tests_file";\n',
            "crates/a/src/dev_tests.rs": 'const T: &str = "suffix.tests_file";\n',
            "crates/a/src/whole.rs": '//! Docs.\n#![allow(dead_code)]\n#![cfg(test)]\nconst T: &str = "inner.attribute";\n',
            # Named like tests but declared as production: they are production.
            "crates/a/src/commands.rs": 'mod test;\nmod tests;\n',
            "crates/a/src/commands/test.rs": 'const C: &str = "prod.test_command";\n',
            "crates/a/src/commands/tests.rs": 'const C: &str = "prod.tests_command";\n',
            "crates/a/tests/it.rs": 'const T: &str = "integration.test";\n',
        }
        tree = cd.Tree(lambda d: sorted(p for p in files if p.startswith(d + "/")), files.get)
        self.assertEqual(set(cd.refusal_codes(tree)),
                         {"prod.a", "prod.real", "prod.inheritance", "prod.test_command", "prod.tests_command"})
        # A code that survives only in a test is removed from production.
        after = {**files, "crates/a/src/real.rs": "\n", "crates/a/src/inheritance/tests.rs": 'const T: &str = "prod.real";\n'}
        after_tree = cd.Tree(lambda d: sorted(p for p in after if p.startswith(d + "/")), after.get)
        self.assertEqual(cd.removed_refusal_codes(tree, after_tree), {"prod.real": (1, 0)})

    def test_a_string_that_continues_over_lines_hides_nothing_after_it(self):
        text = '''
const HELP: &str = "first line \\
    second line with a \\" quote and a { brace \\
    third line";
const RAW: &str = r##"raw over
lines with "# inside and a // comment
"##;
const BYTES: &[u8] = b"bytes \\
    over lines";
/* a block comment /* nested
   over lines */ with "a.comment" */
fn refuse() -> Refusal { Refusal::new(NOT_FOUND, "question.not_found", "no such question") }
'''
        self.assertEqual(cd.rust_literals(text), {"question.not_found"})

        def tree(source):
            files = {"crates/norn-x/src/lib.rs": source}
            return cd.Tree(lambda d: sorted(files) if d in (".", "crates") else [], files.get)
        renamed = text.replace('"question.not_found"', '"question.missing"')
        self.assertEqual(cd.diff(tree(text), tree(renamed)), ["refusal code question.not_found: removed"])
        # Lines of the lexed code are lines of the source.
        code, _ = cd._lex(text)
        self.assertEqual(code.count("\n"), text.count("\n"))

    def test_the_lexer_fails_closed(self):
        cases = {
            "unterminated string literal": 'const A: &str = "a.b";\nconst B: &str = "never closed;\nconst C: &str = "c.d";\n',
            "unterminated raw string literal": 'const A: &str = r#"never closed";\n',
            "unterminated block comment": 'const A: &str = "a.b";\n/* never closed\n',
            "neither a char literal nor a lifetime": "const A: char = ';\n",
            "unbalanced `}`": 'fn f() { } }\nconst C: &str = "c.d";\n',
            "is never closed": 'fn f() { if x { }\nconst C: &str = "c.d";\n',
            "unbalanced `]`": "fn f() { (] }\n",
            "cannot tell where the item under this test `cfg` attribute ends": '#[cfg(test)]\nconst C: &str = "c.d"\n',
        }
        for message, source in cases.items():
            with self.assertRaises(cd.LexError) as raised:
                cd.rust_literals(source, "crates/x/src/lib.rs")
            self.assertIn(message, str(raised.exception))
            self.assertRegex(str(raised.exception), r"^crates/x/src/lib\.rs:\d+: ")
        # A string may span lines, so an unclosed one swallows the next quote: the
        # report is at the quote that is left without a partner, never a silent skip.
        self.assertIn(":3: ", str(self._lex_error(cases["unterminated string literal"])))

    def _lex_error(self, source):
        try:
            cd.rust_literals(source, "crates/x/src/lib.rs")
        except cd.LexError as error:
            return error
        self.fail("no LexError")

    def test_a_test_cfg_on_a_field_variant_arm_or_expression_gates_only_that(self):
        # The shapes of crates/norn-checker/src/inheritance.rs:117 and :210, and their kin.
        text = '''
pub(crate) struct Classification<'a> {
    pub(crate) cyclic: BTreeSet<&'a Name>,
    #[cfg(test)]
    usage: Usage,
}
fn classify() -> Result<Classification, Refusal> {
    let limit = "after.field";
    Ok(Classification {
        cyclic,
        #[cfg(test)]
        usage: meter.usage("test.struct_literal"),
    })
}
fn last_field() -> Classification {
    Classification {
        cyclic,
        #[cfg(test)]
        usage: meter.usage("test.last_field")
    }
}
const AFTER_LITERAL: &str = "after.struct_literal";
enum Kind {
    Real,
    #[cfg(test)]
    Fake { reason: &'static str },
    #[cfg(test)]
    Stub = "test.variant".len() as isize,
    Other,
}
fn arms(kind: Kind) -> &'static str {
    match kind {
        Kind::Real => "arm.real",
        #[cfg(test)]
        Kind::Fake { .. } => { "test.block_arm" }
        #[cfg(test)]
        Kind::Stub => "test.expression_arm",
        Kind::Other => "arm.other",
    }
}
fn statements() {
    #[cfg(test)]
    let probe = if true { "test.let_if" } else { "test.let_else" };
    #[cfg(test)]
    { record("test.block_statement"); }
    #[cfg(test)]
    record("test.call_statement");
    emit("after.statements");
}
#[cfg(test)]
impl Kind { fn code(&self) -> &'static str { "test.impl" } }
#[cfg(test)]
pub(crate) const PROBE: Probe = Probe { code: "test.const_struct" };
#[cfg(test)]
macro_rules! probe { () => { "test.macro" }; }
# [ cfg ( test ) ]
fn spaced() -> &'static str { "test.spaced_attribute" }
const END: &str = "after.everything";
'''
        self.assertEqual(cd.rust_literals(text), {"after.field", "after.struct_literal", "arm.real", "arm.other",
                                                  "after.statements", "after.everything"})

    def test_an_inner_test_cfg_gates_the_file_only_before_its_first_item(self):
        whole = '//! Docs.\n#![allow(dead_code)]\n#![cfg(test)]\nconst T: &str = "test.whole_file";\n'
        self.assertTrue(cd._rust_file("crates/x/src/a.rs", whole).test_only)
        inline = 'const A: &str = "prod.before";\nmod tests {\n    #![cfg(test)]\n    const T: &str = "test.inline";\n}\nconst B: &str = "prod.after";\n'
        file = cd._rust_file("crates/x/src/a.rs", inline)
        self.assertFalse(file.test_only)
        self.assertEqual(set(cd._codes(file.literals)), {"prod.before", "prod.after"})

    def test_the_real_tree_lexes_completely_and_agrees_with_an_independent_count(self):
        # The lexer against a second, simpler method over this repository's own
        # sources: per production file, the code-shaped literals it finds must
        # equal, exactly, a line-by-line regex count over the raw text outside
        # the test-only line ranges and outside lines that are only a comment.
        # A file that does not lex to its end raises LexError here.
        import re
        root = Path(cd.__file__).resolve().parents[2]
        tree = cd.disk_tree(root)
        production = cd.production_files(tree)
        self.assertGreater(len(production), 300)
        quoted = re.compile(r'"(' + cd.CODE.pattern + r')(?:: [^"\n]*)?"')
        line_of = lambda code, index: code.count("\n", 0, index) + 1
        total = 0
        for path, file in production.items():
            text = tree.read(path)
            self.assertEqual(file.code.count("\n"), text.count("\n"), path)
            cd._blocks(file.code, path)
            gated = set()
            for start, end in file.test_ranges:
                gated.update(range(line_of(file.code, start), line_of(file.code, end - 1) + 1))
            independent = sum(len(quoted.findall(line)) for number, line in enumerate(text.split("\n"), 1)
                              if number not in gated and not line.lstrip().startswith("//"))
            found = sum(cd._codes(file.literals).values())
            self.assertEqual(found, independent, f"{path}: the lexer finds {found} code literals, a line regex {independent}")
            total += found
        self.assertGreater(total, 1000)
        # The files the round-three review named: literals after a continued string are seen.
        codes = cd.refusal_codes(tree)
        self.assertGreaterEqual(codes["question.not_found"], 2)
        self.assertGreaterEqual(codes["core.values.inexact"], 1)

    def test_renaming_one_of_several_spellings_is_a_break(self):
        def tree(source):
            files = {"crates/norn-x/src/lib.rs": source}
            return cd.Tree(lambda d: sorted(files) if d in (".", "crates") else [], files.get)
        # The wire code and its error-message attribute spell the same name.
        before = '#[error("core.world.dispute")]\nstruct E;\nfn f() -> E { CellError::ComputeRefused("core.world.dispute") }\n'
        renamed = before.replace('ComputeRefused("core.world.dispute")', 'ComputeRefused("core.world.other")')
        self.assertEqual(cd.diff(tree(before), tree(renamed)),
                         ["refusal code core.world.dispute: production spellings reduced from 2 to 1"])
        moved = '#[error("core.world.dispute")]\nstruct E;\nconst D: &str = "core.world.dispute";\nfn f() -> E { CellError::ComputeRefused(D) }\n'
        self.assertEqual(cd.diff(tree(before), tree(moved)), [])

    def test_an_unenrolled_contract_file_fails(self):
        def tree(files):
            return cd.Tree(lambda d: sorted(files) if d == "." else sorted(p for p in files if p.startswith(d + "/")), files.get)
        enrolled = {f"{cd.SCHEMA_DIRS[0]}/a.schema.json": "{}", "tools/norn-ts/fixtures/schema/x.json": "{}",
                    "crates/x/tests/schema/y.json": "{}", f"{cd.INTERNAL[0]}": "{}"}
        self.assertEqual(cd.unenrolled_surfaces(tree(enrolled)), [])
        # Internal means one named file or directory, not its neighbours.
        atlas = "registry/standards/atlas.schema.json"
        self.assertIn(atlas, cd.INTERNAL)
        self.assertEqual(cd.unenrolled_surfaces(tree({**enrolled, atlas: "{}", "registry/standards/pack.schema.json": "{}"})),
                         ["registry/standards/pack.schema.json"])
        for path in ("crates/norn-new/schema/event.json", "crates/norn-new/schema/v1/event.json", "packages/x/wire.schema.json",
                     "tools/dev/dev.contract.json"):
            self.assertEqual(cd.unenrolled_surfaces(tree({**enrolled, path: "{}"})), [path], path)

    def test_a_single_enrolled_schema_file_is_compared(self):
        def tree(schema):
            files = {cd.SCHEMA_FILES[0]: json.dumps(schema)}
            return cd.Tree(lambda d: sorted(files) if d == "." else [], files.get)
        record = {"type": "object", "required": ["id"], "properties": {"id": {"type": "string"}, "value": {"type": "number"}}}
        grown = copy.deepcopy(record)
        grown["properties"]["unit"] = {"type": "string"}
        shrunk = copy.deepcopy(record)
        del shrunk["properties"]["value"]
        self.assertEqual(cd.diff(tree(record), tree(grown)), [])
        self.assertEqual(cd.diff(tree(record), tree(shrunk)), [f"{cd.SCHEMA_FILES[0]}.value: property removed"])
        self.assertEqual(cd.unenrolled_surfaces(tree(record)), [])

    def test_moving_a_code_is_not_removing_it(self):
        before = 'impl E {\n    pub const fn code(&self) -> &str {\n        match self { Self::A => "decision.guard_false", Self::B => OTHER }\n    }\n}\nconst OTHER: &str = "decision.other";\n'
        moved = 'fn helper() -> &\'static str { "decision.guard_false" }\nconst OTHER: &str = "decision.other";\n'
        self.assertEqual(cd.rust_literals(before), {"decision.guard_false", "decision.other"})
        self.assertLessEqual(cd.rust_literals(before), cd.rust_literals(moved))
        self.assertNotIn("decision.guard_false", cd.rust_literals('const OTHER: &str = "decision.other";\n'))
        self.assertEqual(cd.rust_literals('#[cfg(test)]\nmod tests {\n    const X: &str = "test.only";\n}\n'), set())

    def test_cli_additions_pass_and_removals_fail(self):
        flag = {"long": "org", "short": None, "aliases": [], "positional": False, "takes_value": True,
                "multiple": False, "required": True, "default": [], "values": []}
        base = {"args": {}, "subcommands": {"check": {"args": {"org": flag}, "subcommands": {}}}}

        def diff(edit):
            new = copy.deepcopy(base)
            edit(new)
            out: list[str] = []
            cd.compare_command(base, new, "cli norn", out)
            return out

        self.assertEqual(diff(lambda c: c["subcommands"].__setitem__("dev", {"args": {}, "subcommands": {}})), [])
        self.assertEqual(diff(lambda c: c["subcommands"]["check"]["args"].__setitem__("out", {**flag, "long": "out", "required": False})), [])
        self.assertEqual(diff(lambda c: c["subcommands"]["check"]["args"]["org"].update(aliases=["organization"])), [])
        self.assertEqual(diff(lambda c: c["subcommands"]["check"]["args"]["org"].update(required=False)), [])
        self.assertTrue(diff(lambda c: c["subcommands"].pop("check")))
        self.assertTrue(diff(lambda c: c["subcommands"]["check"]["args"].pop("org")))
        self.assertTrue(diff(lambda c: c["subcommands"]["check"]["args"]["org"].update(long="organization")))
        self.assertTrue(diff(lambda c: c["subcommands"]["check"]["args"].__setitem__("out", {**flag, "long": "out"})))

    def test_the_authoring_cli_may_move_from_sources_to_a_generated_contract(self):
        def tree(files):
            return cd.Tree(lambda d: sorted(p for p in files if p.startswith(d + "/")), files.get)
        source = {f"{cd.TS_CLI_DIR}/cli.ts": 'if (process.argv[2] === "init") {}\nif (command !== "check") {}\noption("--org")\n'}
        flag = lambda long: {"long": long, "short": None, "aliases": [], "positional": False, "takes_value": True,
                             "multiple": False, "required": False, "default": [], "values": []}
        contract = {"args": {}, "subcommands": {"init": {"args": {}, "subcommands": {}},
                                                "check": {"args": {"org": flag("org")}, "subcommands": {}}}}
        generated = {cd.TS_CLI_SNAPSHOT: json.dumps(contract)}
        self.assertEqual(cd.diff(tree(source), tree(generated)), [])
        del contract["subcommands"]["check"]["args"]["org"]
        self.assertEqual(cd.diff(tree(source), tree({cd.TS_CLI_SNAPSHOT: json.dumps(contract)})),
                         ["authoring cli --org: flag removed"])
        breaks = cd.diff(tree(generated), tree({cd.TS_CLI_SNAPSHOT: json.dumps(contract)}))
        self.assertIn("authoring cli norn check org: flag or argument removed", breaks)

    def test_an_approval_names_a_ruling_and_one_break(self):
        def tree(document):
            return cd.Tree(lambda d: [], lambda path: json.dumps(document) if path == cd.APPROVALS else None)
        self.assertEqual(cd.approvals(tree({"approved": [{"ruling": "D85", "break": "a: removed"},
                                                         {"ruling": "#12", "break": "b: removed"}]})),
                         {"a: removed", "b: removed"})
        for entry in ({"ruling": "yes", "break": "a"}, {"ruling": "D85"}, {"break": "a"}, "D85"):
            self.assertEqual(cd.approvals(tree({"approved": [entry]})), set(), entry)


class Scenarios(unittest.TestCase):
    """The script end to end over a scratch repository."""

    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.git("init", "-q", "-b", "main")
        self.git("config", "user.email", "test@example.invalid")
        self.git("config", "user.name", "test")
        self.write(cd.OPENAPI, openapi({"/v1/x": route("Req", "Res")}, {
            "Req": {"type": "object", "properties": {"a": {"type": "string"}}},
            "Res": {"type": "object", "required": ["b"], "properties": {"b": {"type": "string"}}}}))
        self.write("crates/norn-api/src/refusal.rs", 'pub const SCOPE_DENIED: &str = "scope.denied";\n')
        self.write(f"{cd.DIAGNOSTIC_CODES}/x.y.json", {"code": "x.y", "severity": "error"})
        self.commit("base")

    def tearDown(self):
        if cd._Objects.process is not None:
            cd._Objects.process.kill()
            cd._Objects.process.wait()
            cd._Objects.process = None
        self.temporary.cleanup()

    def git(self, *args):
        return subprocess.run(["git", *args], cwd=self.root, check=True, capture_output=True, text=True).stdout.strip()

    def write(self, path, value):
        target = self.root / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(value if isinstance(value, str) else json.dumps(value, indent=2))

    def commit(self, message):
        self.git("add", "-A")
        self.git("commit", "-q", "-m", message)
        return self.git("rev-parse", "HEAD")

    def run_script(self, *args):
        command = [sys.executable, str(Path(cd.__file__)), *args]
        return subprocess.run(command, cwd=self.root, capture_output=True, text=True)

    def approve(self, ruling, *breaks):
        path = self.root / cd.APPROVALS
        document = json.loads(path.read_text()) if path.exists() else {"format": "norn-contract-breaks/1", "approved": []}
        document["approved"] += [{"ruling": ruling, "break": line} for line in breaks]
        self.write(cd.APPROVALS, document)

    def test_a_pull_request_is_judged_against_its_merge_base(self):
        self.git("switch", "-q", "-c", "feature")
        self.write("crates/norn-api/src/refusal.rs", 'pub const SCOPE_DENIED: &str = "scope.denied";\npub const NEW: &str = "scope.new";\n')
        head = self.commit("additive")
        self.git("switch", "-q", "main")
        # After the branch point, main adds a code the feature branch never saw.
        self.write("crates/norn-api/src/refusal.rs", 'pub const SCOPE_DENIED: &str = "scope.denied";\npub const LATER: &str = "scope.later";\n')
        self.commit("main moves on")
        result = self.run_script("--base", "main", "--head", head)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_a_break_fails_until_the_change_adds_its_approval(self):
        self.git("switch", "-q", "-c", "feature")
        self.write("crates/norn-api/src/refusal.rs", "\n")
        self.git("rm", "-q", f"{cd.DIAGNOSTIC_CODES}/x.y.json")
        head = self.commit("break")
        refused = self.run_script("--base", "main", "--head", head)
        self.assertEqual(refused.returncode, 1)
        self.assertIn("refusal code scope.denied: removed", refused.stdout)
        self.assertIn("diagnostic code x.y: removed", refused.stdout)
        # One approved break does not approve the other.
        self.approve("D85", "refusal code scope.denied: removed")
        partial = self.commit("approve one")
        self.assertEqual(self.run_script("--base", "main", "--head", partial).returncode, 1)
        self.approve("D85", "diagnostic code x.y: removed")
        approved = self.run_script("--base", "main", "--head", self.commit("approve both"))
        self.assertEqual(approved.returncode, 0, approved.stdout)
        self.assertIn("2 approved", approved.stdout)

    def test_a_source_the_lexer_cannot_classify_fails_the_check_and_cannot_be_approved(self):
        self.git("switch", "-q", "-c", "feature")
        self.write("crates/norn-api/src/broken.rs", 'pub const A: &str = "never closed;\n')
        self.approve("D85", "refusal code scope.denied: removed")
        refused = self.run_script("--base", "main", "--head", self.commit("unlexable"))
        self.assertEqual(refused.returncode, 1, refused.stdout + refused.stderr)
        self.assertIn("crates/norn-api/src/broken.rs:1: unterminated string literal", refused.stdout)

    def test_an_approval_already_on_the_base_approves_nothing_new(self):
        self.approve("D85", "refusal code scope.denied: removed")
        self.commit("an old approval")
        self.git("switch", "-q", "-c", "feature")
        self.write("crates/norn-api/src/refusal.rs", "\n")
        head = self.commit("break")
        self.assertEqual(self.run_script("--base", "main", "--head", head).returncode, 1)

    def test_a_merge_group_checks_each_merge_against_what_it_adds(self):
        base = self.git("rev-parse", "HEAD")
        self.git("switch", "-q", "-c", "approving")
        self.approve("D85", "openapi /v1/x: endpoint removed")
        approving = self.commit("approval without a break")
        self.git("switch", "-q", "-c", "breaking", base)
        self.write(cd.OPENAPI, openapi({}, {}))
        breaking = self.commit("break")
        self.git("switch", "-q", "--detach", base)
        self.git("merge", "-q", "--no-ff", "-m", "queue 1", approving)
        self.git("merge", "-q", "--no-ff", "-m", "queue 2", breaking)
        head = self.git("rev-parse", "HEAD")
        # Another queued pull request's approval does not approve this break.
        refused = self.run_script("--merge-group", base, head)
        self.assertEqual(refused.returncode, 1, refused.stdout)
        self.assertIn("endpoint removed", refused.stdout)

    def test_ci_judges_the_pull_request_and_skips_other_events(self):
        self.git("switch", "-q", "-c", "feature")
        self.write("crates/norn-api/src/refusal.rs", "\n")
        head = self.commit("break")
        event = {"pull_request": {"number": 7, "base": {"sha": self.git("rev-parse", "main")}, "head": {"sha": head}}}
        cwd = Path.cwd()
        try:
            import os
            os.chdir(self.root)
            self.assertFalse(cd.ci("pull_request", event))
            self.approve("#12", "refusal code scope.denied: removed")
            event["pull_request"]["head"]["sha"] = self.commit("approve")
            self.assertTrue(cd.ci("pull_request", event))
            self.assertTrue(cd.ci("push", {}))
        finally:
            os.chdir(cwd)

    def test_the_repository_contracts_extract(self):
        surface = subprocess.run([sys.executable, str(Path(cd.__file__)), "--surface", str(Path(cd.__file__).resolve().parents[2])],
                                 capture_output=True, text=True, check=True)
        document = json.loads(surface.stdout)
        self.assertIn("scope.denied", document["refusalCodes"])
        self.assertGreater(document["diagnosticCodes"], 0)
        self.assertIn("crates/norn-ir/schema/action.schema.json", document["schemas"])
        self.assertIn("init", document["authoringCli"]["commands"])
        self.assertIn("check", document["cli"].get("subcommands", {}))
        self.assertIn("defs.stale", document["refusalCodes"])
        self.assertIn(cd.SCHEMA_FILES[0], document["schemas"])
        self.assertEqual(document["unenrolled"], [])


if __name__ == "__main__":
    unittest.main()
