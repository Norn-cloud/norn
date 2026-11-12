"""Derive the client-generation input from the frozen OpenAPI document.

The frozen document is never edited. This writes a copy that differs only in
names the generator turns into Python identifiers; every wire shape is kept:
- operation tags `crate::<module>` become short module names: the ones in TAGS by
  name, any other `crate::<module>` as `<module>` without a `_routes` suffix (so a
  new route module never stops generation); a tag of any other form fails;
- an object schema (with properties, a map, or free-form) nested inline in a `oneOf`/`anyOf` variant's property is
  moved to a named component and referenced, because the generator otherwise
  names it twice and drops it.
"""
import json, re, sys
TAGS = {
    "crate::actions": "actions",
    "crate::definition_routes": "definitions",
    "crate::routes": "definitions",
    "crate::object_routes": "objects",
    "crate::object_links": "objects",
    "crate::question_routes": "questions",
    "crate::object_profile": "objects",
    "crate::object_review": "objects",
    "crate::mcp": "mcp",
    "crate::run_routes": "runs",
}
src, dst = sys.argv[1], sys.argv[2]
doc = json.load(open(src))
for item in doc["paths"].values():
    for op in item.values():
        if isinstance(op, dict) and "tags" in op:
            unknown = [t for t in op["tags"] if t not in TAGS and not re.fullmatch(r"crate::[a-z][a-z0-9_]*", t)]
            if unknown:
                sys.exit(f"unmapped tag {unknown}: add it to TAGS")
            op["tags"] = sorted({TAGS.get(t) or re.sub(r"_routes$", "", t.removeprefix("crate::")) for t in op["tags"]})
schemas = doc["components"]["schemas"]
hoisted = {}
def hoist(owner, schema):
    for key in ("oneOf", "anyOf"):
        for index, variant in enumerate(schema.get(key, [])):
            for prop, sub in (variant.get("properties") or {}).items():
                # Any inline object, including a free-form one (additionalProperties: true).
                if isinstance(sub, dict) and sub.get("type") == "object":
                    name = f"{owner}{prop[0].upper()}{prop[1:]}"
                    if name in schemas or name in hoisted:
                        sys.exit(f"hoisted name {name} already exists")
                    hoisted[name] = sub
                    variant["properties"][prop] = {"$ref": f"#/components/schemas/{name}"}
for name, schema in list(schemas.items()):
    if isinstance(schema, dict):
        hoist(name, schema)
schemas.update(hoisted)
json.dump(doc, open(dst, "w"), indent=2, sort_keys=False)
print(f"tags mapped; hoisted {sorted(hoisted)}")
