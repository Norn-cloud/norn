#!/usr/bin/env python3
"""Translate the applicable DMN TCK subset into norn-decisions fixtures (T6-DMN-1, #2688).

Usage: translate.py <tck-checkout> <out-dir>

The checkout must be dmn-tck/tck at the commit pinned in manifest.json. Every
compliance-level-2 model is either translated into `cases/<model>.json` (a
RuleIR v2 decide rule plus the model's TCK test cases) or listed as excluded
with a reason. Four reachable no-hit gaps are excluded from equivalence claims,
but their original upstream cases still run and exact-output witnesses enforce
that exclusions shrink when fixed. The translation is exact only where the JDM hit policy is
equivalent to the DMN one:

- UNIQUE, ANY, FIRST -> `first`. A valid UNIQUE table has disjoint rules and a
  valid ANY table's overlapping rules agree on their outputs, so the first
  matching row is the DMN result.
- PRIORITY -> rows stably sorted by output priority (the order of each output
  column's `outputValues`, columns left to right), then `first`.
- RULE ORDER, COLLECT -> `collect` (matching rows in rule order).
- OUTPUT ORDER -> rows sorted as for PRIORITY, then `collect`.
- COLLECT SUM / MIN / MAX / COUNT -> `collect`, then the aggregate in an
  expression node (`sum`, `min`, `max`, `len`); no hit is null for every
  aggregate (DMN 1.5 table 39).

Input entries must be FEEL simple unary tests that ZEN unary syntax reads the
same way: comparisons, literal lists, closed or open ranges, `-`, and
string/number/boolean literals. Anything else excludes the model.

Encoding into the decide contract: `slice` holds the model's input data,
`event` is the empty record, and a non-null result is one command
`{decision, result}`; a null result (no hit) is no command.
"""
import json
import re
import sys
import subprocess
import xml.etree.ElementTree as ET
from pathlib import Path

XSI = "{http://www.w3.org/2001/XMLSchema-instance}"
FIRST_LIKE = {"UNIQUE", "ANY", "FIRST"}
COLLECT_LIKE = {"RULE ORDER", "COLLECT"}
AGGREGATES = {"SUM": "sum", "MIN": "min", "MAX": "max", "COUNT": "len"}
TYPES = {"number": "number", "string": "string", "boolean": "boolean"}
SIMPLE_NAME = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*$")

# Reachable no-hit gaps at the pinned XML, not blanket hit-policy exclusions.
# Other default tables cover the declared non-null inputValues domain; the
# single-input RULE/OUTPUT ORDER tables cover every numeric Age.
KNOWN_GAPS = {
    "0108-first-hitpolicy": (2929, "declared no-hit defaults are discarded", "FIRST", True),
    "0109-ruleOrder-hitpolicy": (2929, "declared no-hit defaults are discarded", "RULE ORDER", True),
    "0110-outputOrder-hitpolicy": (2930, "empty nonaggregate hits produce [] instead of null", "OUTPUT ORDER", False),
    "0119-multi-collect-hitpolicy": (2930, "empty nonaggregate hits produce [] instead of null", "COLLECT", False),
}


def gap_witness(model_dir):
    """Derive defaults from the pinned XML; leave translated semantics intact."""
    issue, reason, hit_policy, has_defaults = KNOWN_GAPS[model_dir.name]
    root = ET.parse(next(model_dir.glob("*.dmn"))).getroot()
    ns = root.tag[1:].split("}")[0]
    decision = root.find(q(ns, "decision"))
    table = decision.find(q(ns, "decisionTable"))
    assert table.get("hitPolicy") == hit_policy and not table.get("aggregation")
    defaults = [o.find(q(ns, "defaultOutputEntry")) for o in table.findall(q(ns, "output"))]
    assert all(d is not None for d in defaults) if has_defaults else all(d is None for d in defaults)
    name = decision.get("name")
    expected_result = {
        o.get("name"): json.loads(literal(d.find(q(ns, "text")).text))
        for o, d in zip(table.findall(q(ns, "output")), defaults)
    } if has_defaults else None
    # Valid declared-domain inputs that match none of these models' rows.
    slice_value = {"Age": 19, "RiskCategory": "High", "isAffordable": True} if has_defaults else {
        "Age": 19, "RiskCategory": "Medium", "isAffordable": False,
    }
    expected_commands = [] if expected_result is None else [{"decision": name, "result": expected_result}]
    known_commands = [] if hit_policy == "FIRST" else [{"decision": name, "result": []}]
    return {
        "model": model_dir.name, "issue": issue, "reason": reason,
        "source": f"TestCases/compliance-level-2/{model_dir.name}/{model_dir.name}.dmn",
        "semantics": "DMN 1.5 section 10.3.2.10 step 2: no match returns the default context, otherwise null",
        "slice": slice_value, "event": {},
        "expect": {"facts": [], "commands": expected_commands, "timers": []},
        "knownActual": {"facts": [], "commands": known_commands, "timers": []},
    }


class Excluded(Exception):
    pass


def q(ns, tag):
    return "{%s}%s" % (ns, tag)


def literal_list(text):
    """`"a", "b"` -> ["a", "b"] for string enumerations."""
    return [part.strip().strip('"') for part in text.split(",") if part.strip()]


def unary(text):
    text = (text or "").strip()
    if text in ("", "-"):
        return ""
    for part in split_top(text):
        if not re.fullmatch(
            r'(?:(?:<=|>=|<|>|=|!=)\s*)?(?:-?\d+(?:\.\d+)?|"[^"]*"|true|false)'
            r'|[\[\(\]]\s*-?\d+(?:\.\d+)?\s*\.\.\s*-?\d+(?:\.\d+)?\s*[\]\)\[]',
            part,
        ):
            raise Excluded(f"input entry `{text}` is not a simple unary test")
    return ", ".join(split_top(text))


def split_top(text):
    parts, depth, current, quoted = [], 0, "", False
    for ch in text:
        if ch == '"':
            quoted = not quoted
        if not quoted and ch in "[(":
            depth += 1
        if not quoted and ch in "])":
            depth -= 1
        if ch == "," and depth == 0 and not quoted:
            parts.append(current.strip())
            current = ""
        else:
            current += ch
    parts.append(current.strip())
    return parts


def literal(text):
    text = (text or "").strip()
    if re.fullmatch(r'-?\d+(?:\.\d+)?|"[^"]*"|true|false|null', text):
        return text
    raise Excluded(f"output entry `{text}` is not a literal")


def closed(properties, required=None):
    return {
        "type": "object",
        "properties": properties,
        "required": list(properties) if required is None else required,
        "additionalProperties": False,
    }


def tck_value(node, ns):
    """A TCK expected/input value as JSON (numbers as JSON numbers)."""
    if node.find(q(ns, "list")) is not None:
        return [tck_value(item, ns) for item in node.find(q(ns, "list")).findall(q(ns, "item"))]
    components = node.findall(q(ns, "component"))
    if components:
        return {c.get("name"): tck_value(c, ns) for c in components}
    value = node.find(q(ns, "value"))
    if value is None or value.get(XSI + "nil") == "true":
        return None
    kind = value.get(XSI + "type")
    text = value.text or ""
    if kind == "xsd:decimal":
        number = float(text) if "." in text else int(text)
        return number
    if kind == "xsd:boolean":
        return text == "true"
    if kind == "xsd:string":
        return text
    raise Excluded(f"unsupported TCK value type {kind}")


def translate(model_dir):
    dmn_path = next(model_dir.glob("*.dmn"))
    root = ET.parse(dmn_path).getroot()
    ns = root.tag[1:].split("}")[0]
    decisions = root.findall(q(ns, "decision"))
    if len(decisions) != 1:
        raise Excluded(f"{len(decisions)} decisions; the subset covers one decision table per model")
    if root.findall(q(ns, "businessKnowledgeModel")):
        raise Excluded("business knowledge model invocation")
    decision = decisions[0]
    table = decision.find(q(ns, "decisionTable"))
    if table is None:
        raise Excluded("decision logic is not a decision table (FEEL literal expression or invocation)")
    name = decision.get("name")
    hit = table.get("hitPolicy", "UNIQUE")
    aggregation = table.get("aggregation")

    inputs = []
    slice_props = {}
    for inp in table.findall(q(ns, "input")):
        expr = inp.find(q(ns, "inputExpression"))
        field = (expr.find(q(ns, "text")).text or "").strip()
        kind = TYPES.get(expr.get("typeRef"))
        if not SIMPLE_NAME.match(field) or kind is None:
            raise Excluded(f"input expression `{field}` ({expr.get('typeRef')}) is not a simple typed name")
        inputs.append(field)
        slice_props[field] = {"type": kind}

    outputs = table.findall(q(ns, "output"))
    multi = len(outputs) > 1
    out_names = [o.get("name") for o in outputs] if multi else ["v"]
    out_types = []
    priorities = []
    for o in outputs:
        type_ref = o.get("typeRef") or decision.find(q(ns, "variable")).get("typeRef")
        out_types.append(TYPES.get(type_ref, "string"))
        values = o.find(q(ns, "outputValues"))
        priorities.append(literal_list(values.find(q(ns, "text")).text) if values is not None else None)

    rows = []
    for index, rule in enumerate(table.findall(q(ns, "rule"))):
        ins = [unary(e.find(q(ns, "text")).text) for e in rule.findall(q(ns, "inputEntry"))]
        outs = [literal(e.find(q(ns, "text")).text) for e in rule.findall(q(ns, "outputEntry"))]
        rows.append((index, ins, outs))

    if hit in ("PRIORITY", "OUTPUT ORDER"):
        if not any(priorities):
            raise Excluded(f"{hit} without outputValues")

        def priority(row):
            key = []
            for column, values in enumerate(priorities):
                if values:
                    text = row[2][column].strip('"')
                    key.append(values.index(text) if text in values else len(values))
            return key

        rows = sorted(rows, key=priority)  # stable: ties keep rule order

    if hit in FIRST_LIKE or hit == "PRIORITY":
        policy, shape = "first", "row"
    elif hit in COLLECT_LIKE and not aggregation or hit == "OUTPUT ORDER":
        policy, shape = "collect", "rows"
    elif hit == "COLLECT" and aggregation in AGGREGATES:
        policy, shape = "collect", AGGREGATES[aggregation]
    else:
        raise Excluded(f"hit policy {hit} {aggregation or ''}".strip())

    columns_in = [{"id": f"i{n}", "name": f, "field": f"slice.{f}"} for n, f in enumerate(inputs)]
    columns_out = [{"id": f"o{n}", "name": o, "field": o} for n, o in enumerate(out_names)]
    table_rows = []
    for index, ins, outs in rows:
        row = {"_id": f"r{index + 1}"}
        row.update({f"i{n}": cell for n, cell in enumerate(ins)})
        row.update({f"o{n}": cell for n, cell in enumerate(outs)})
        table_rows.append(row)

    record = closed({o: {"type": t} for o, t in zip(out_names, out_types)})
    if shape == "row":
        result = "hit == null or len(keys(hit)) == 0 ? null : " + ("hit" if multi else "hit.v")
        result_schema = record if multi else {"type": out_types[0]}
    elif shape == "rows":
        result = "hit" if multi else "map(hit, #.v)"
        result_schema = {"type": "array", "items": record if multi else {"type": out_types[0]}}
    else:
        if multi:
            raise Excluded("aggregation over multiple outputs")
        result = f"len(hit) == 0 ? null : {shape}(map(hit, #.v))"
        result_schema = {"type": "number"}

    rule_json = {
        "ruleId": "dmn_tck." + model_dir.name,
        "ruleVersion": "2.0",
        "contract": "decide",
        "inputSchema": closed({"slice": closed(slice_props, []), "event": closed({})}),
        "outputSchema": closed({
            "facts": {"type": "array", "items": closed({"op": {"type": "string", "enum": ["set"]}})},
            "commands": {"type": "array", "items": closed({"decision": {"type": "string"}, "result": result_schema})},
            "timers": {"type": "array", "items": closed({"timerId": {"type": "string"}})},
        }),
        "scopeRequired": [],
        "graph": {
            "nodes": [
                {"id": "in", "name": "in", "type": "inputNode"},
                {"id": "table", "name": name, "type": "decisionTableNode", "content": {
                    "hitPolicy": policy, "inputs": columns_in, "outputs": columns_out,
                    "rules": table_rows, "outputPath": "hit", "passThrough": True}},
                {"id": "shape", "name": "shape", "type": "expressionNode", "content": {
                    "expressions": [{"id": "x_result", "key": "result", "value": result}]}},
                {"id": "emit", "name": "emit", "type": "expressionNode", "content": {"expressions": [
                    {"id": "x_facts", "key": "facts", "value": "[]"},
                    {"id": "x_commands", "key": "commands",
                     "value": f"result == null ? [] : [{{decision: {json.dumps(name)}, result: result}}]"},
                    {"id": "x_timers", "key": "timers", "value": "[]"}]}},
                {"id": "out", "name": "out", "type": "outputNode"},
            ],
            "edges": [
                {"id": "e1", "sourceId": "in", "targetId": "table"},
                {"id": "e2", "sourceId": "table", "targetId": "shape"},
                {"id": "e3", "sourceId": "shape", "targetId": "emit"},
                {"id": "e4", "sourceId": "emit", "targetId": "out"},
            ],
        },
    }

    cases = []
    for test_path in sorted(model_dir.glob("*test*.xml")):
        troot = ET.parse(test_path).getroot()
        tns = troot.tag[1:].split("}")[0]
        for case in troot.findall(q(tns, "testCase")):
            slice_value = {}
            for node in case.findall(q(tns, "inputNode")):
                value = tck_value(node, tns)
                if value is None:
                    raise Excluded("a null input datum (closed slice schemas have no null)")
                slice_value[node.get("name")] = value
            results = [r for r in case.findall(q(tns, "resultNode")) if r.get("name") == name]
            if len(results) != 1:
                raise Excluded("test case without exactly one result for the decision")
            expected = tck_value(results[0].find(q(tns, "expected")), tns)
            commands = [] if expected is None else [{"decision": name, "result": expected}]
            cases.append({
                "name": f"{test_path.stem}#{case.get('id')}",
                "slice": slice_value,
                "event": {},
                "expect": {"facts": [], "commands": commands, "timers": []},
            })
    return {
        "description": f"DMN TCK {model_dir.name}: {hit}{' ' + aggregation if aggregation else ''} -> JDM {policy} ({shape}).",
        "rule": rule_json,
        "cases": cases,
    }


def main():
    tck, out = Path(sys.argv[1]), Path(sys.argv[2])
    manifest = json.loads((out / "manifest.json").read_text())
    actual_commit = subprocess.check_output(["git", "-C", str(tck), "rev-parse", "HEAD"], text=True).strip()
    if actual_commit != manifest["commit"]:
        raise SystemExit(f"TCK commit {actual_commit} differs from pinned {manifest['commit']}")
    (out / "cases").mkdir(exist_ok=True)
    (out / "witnesses").mkdir(exist_ok=True)
    included, excluded, known_gaps = [], {}, []
    level2 = tck / "TestCases" / "compliance-level-2"
    for model_dir in sorted(p for p in level2.iterdir() if p.is_dir()):
        try:
            fixture = translate(model_dir)
        except Excluded as reason:
            excluded[model_dir.name] = str(reason)
            continue
        (out / "cases" / f"{model_dir.name}.json").write_text(json.dumps(fixture, indent=2) + "\n")
        entry = {"model": model_dir.name, "cases": [c["name"] for c in fixture["cases"]]}
        if model_dir.name in KNOWN_GAPS:
            witness = gap_witness(model_dir)
            witness_name = f"{model_dir.name}.json"
            (out / "witnesses" / witness_name).write_text(json.dumps(witness, indent=2) + "\n")
            entry.update({"issue": witness["issue"], "reason": witness["reason"], "witness": witness_name})
            known_gaps.append(entry)
            excluded[model_dir.name] = f"#{witness['issue']}: {witness['reason']} (guarded by a no-hit witness)"
        else:
            included.append(entry)
    level3 = sorted(p.name for p in (tck / "TestCases" / "compliance-level-3").iterdir() if p.is_dir())
    active_witnesses = {entry["witness"] for entry in known_gaps}
    for path in (out / "witnesses").glob("*.json"):
        if path.name not in active_witnesses:
            path.unlink()
    manifest["included"] = included
    manifest["known_gaps"] = known_gaps
    manifest["excluded"] = {
        "compliance-level-2": excluded,
        "compliance-level-3": {
            "count": len(level3),
            "reason": "full FEEL (functions, contexts, date/time, BKMs, invocations): outside the FEEL-free decision-table subset",
        },
    }
    (out / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")


if __name__ == "__main__":
    main()
