#!/usr/bin/env python3
"""Generate one guarded/unguarded create writer per benchmark law and run the real proof driver."""
import argparse
import ast
from decimal import Decimal
import itertools
import json
import operator
from pathlib import Path
import re
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
UNIT = {"system": "ucum", "code": "1"}
TYPES = {"text": {"kind": "text"}, "qty": {"kind": "decimal", "scale": 0, "unit": UNIT},
         "money": {"kind": "decimal", "scale": 2, "currencySource": {"links": [], "property": "currency"}}}
BOUND = (-3, 4)
MAX_PRODUCT_STATES = 100_000
_BIN_OPS = {ast.Add: operator.add, ast.Sub: operator.sub, ast.Mult: operator.mul}
_CMP_OPS = {ast.Eq: operator.eq, ast.NotEq: operator.ne, ast.Lt: operator.lt,
            ast.LtE: operator.le, ast.Gt: operator.gt, ast.GtE: operator.ge}


def envelope(artifact, kind, payload, dependencies):
    return {"artifactId": artifact, "dependencies": dependencies, "irVersion": "0.1", "kind": kind,
            "lifecycle": "draft", "name": artifact, "namespace": "bench", "payload": payload,
            "provenance": {"source": "authoring_sdk", "sourcePath": "scripts/bench_proofs.py"},
            "scope": {"environmentId": "dev", "orgId": "org_demo"}, "version": "1.0.0"}


def declarations(law, props, guarded):
    subject = "bench.Subject"
    fields = [{"id": name, "type": TYPES[kind], **({"default": "0"} if kind == "qty" else {})}
              for name, kind in props.items()]
    units = [{"artifactId": "core.units", "kind": "entity", "required": True, "version": "1.0.0"}]
    entity = {"artifactId": f"{subject}.entity", "kind": "entity", "required": True, "version": "1.0.0"}
    schema = {name: ({"type": "string"} if kind == "text" else {"type": "number", "nornType": TYPES[kind]})
              for name, kind in props.items()}
    action = {"actionId": "bench.create", "title": "create", "idempotent": True, "ledgered": True,
              "scopeRequired": ["orgId", "environmentId"],
              "inputSchema": {"type": "object", "additionalProperties": False, "required": list(props), "properties": schema},
              "outputSchema": {"type": "object", "additionalProperties": False, "required": ["id"],
                               "properties": {"id": {"type": "string"}}},
              "effectProgram": {"kind": "upsert_input_object", "objectIdField": "id", "objectKind": subject,
                                "entityArtifactId": f"{subject}.entity", "entityArtifactVersion": "1.0.0",
                                "writeMode": {"kind": "create_only"}},
              "writes": [{"change": "created", "entityKind": subject}]}
    if guarded:
        action["preconditions"] = [law["expression"].replace("self.", "input.")]
    return [
        envelope(f"{subject}.entity", "entity", {"entityVersion": "2.0", "objectType": {
            "name": subject, "identity": [{"name": "primary", "properties": ["id"]}], "properties": fields}}, units),
        envelope("bench.create.action", "action", action, units + [entity]),
        envelope(f"bench.{law['id']}.law", "law", {
            "id": law["id"], "lawVersion": "0.1", "modality": {"kind": "always"}, "owner": "bench",
            "provenance": {"source": "authoring_sdk", "sourcePath": "scripts/bench_proofs.py"},
            "rationale": law["requirement"], "response": {"kind": "predicate", "expression": law["expression"]},
            "scope": {"objectTypes": [subject], "root": {"kind": "entity", "id": subject}},
            "severity": "error", "subject": subject}, [entity]),
    ]


def _eval_node(node, env):
    if isinstance(node, ast.Expression):
        return _eval_node(node.body, env)
    if isinstance(node, ast.Constant):
        if isinstance(node.value, bool) or isinstance(node.value, str):
            return node.value
        if isinstance(node.value, (int, float)):
            return Decimal(str(node.value))
        raise TypeError(f"unsupported constant: {node.value!r}")
    if isinstance(node, ast.Name):
        if node.id not in env:
            raise NameError(node.id)
        return env[node.id]
    if isinstance(node, ast.UnaryOp):
        operand = _eval_node(node.operand, env)
        if isinstance(node.op, ast.Not):
            return not operand
        if isinstance(node.op, ast.USub):
            return -operand
        if isinstance(node.op, ast.UAdd):
            return +operand
        raise TypeError(f"unsupported unary op: {type(node.op).__name__}")
    if isinstance(node, ast.BinOp):
        fn = _BIN_OPS.get(type(node.op))
        if fn is None:
            raise TypeError(f"unsupported binary op: {type(node.op).__name__}")
        return fn(_eval_node(node.left, env), _eval_node(node.right, env))
    if isinstance(node, ast.BoolOp):
        if isinstance(node.op, ast.And):
            result = True
            for value in node.values:
                result = _eval_node(value, env)
                if not result:
                    return result
            return result
        if isinstance(node.op, ast.Or):
            result = False
            for value in node.values:
                result = _eval_node(value, env)
                if result:
                    return result
            return result
        raise TypeError(f"unsupported bool op: {type(node.op).__name__}")
    if isinstance(node, ast.Compare):
        left = _eval_node(node.left, env)
        for op, comparator in zip(node.ops, node.comparators, strict=True):
            fn = _CMP_OPS.get(type(op))
            if fn is None:
                raise TypeError(f"unsupported compare op: {type(op).__name__}")
            right = _eval_node(comparator, env)
            if not fn(left, right):
                return False
            left = right
        return True
    raise TypeError(f"unsupported AST node: {type(node).__name__}")


def states(expression, fields, step):
    tree = ast.parse(expression.replace("self.", ""), mode="eval")
    lower, upper = BOUND
    domain = [Decimal(value) / Decimal(step) for value in range(lower * step, upper * step + 1)]
    if len(domain) ** len(fields) > MAX_PRODUCT_STATES:
        raise ValueError(f"state space {len(domain) ** len(fields)} exceeds {MAX_PRODUCT_STATES}")
    enabled = sum(bool(_eval_node(tree, dict(zip(fields, values, strict=True))))
                  for values in itertools.product(domain, repeat=len(fields)))
    return enabled + 1


def laws(root):
    rules = json.loads((root / "fixtures/bench/erp-rules.json").read_text())["rules"]
    for rule in rules:
        if "expression" in rule:
            fields = sorted(set(re.findall(r"self\.(\w+)", rule["expression"])))
            yield ({"id": rule["id"], "expression": rule["expression"], "requirement": rule["requirement"]},
                   {"id": "text", **dict.fromkeys(fields, "qty")}, fields, 1)
    cases = json.loads((root / "fixtures/bench/clause-cases.json").read_text())["cases"]
    for case in cases:
        for law in case.get("laws", []):
            entity = next(e for e in case["entities"] if e["name"] == law["on"])
            fields = sorted(set(re.findall(r"self\.(\w+)", law["expression"])))
            keep = {"id", "currency", "status", *fields}
            props = {name: kind for name, kind in entity["props"].items() if name in keep}
            step = 100 if "money" in props.values() else 1
            yield ({"id": law["id"], "expression": law["expression"], "requirement": law["text"]}, props, fields, step)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary")
    parser.add_argument("tools")
    parser.add_argument("work", type=Path)
    parser.add_argument("only", nargs="*")
    args = parser.parse_args(argv)
    work = args.work.resolve()
    if work == ROOT or ROOT in work.parents:
        parser.error("work directory must remain outside the source checkout")
    work.mkdir(parents=True, exist_ok=True)
    base = [json.loads((ROOT / f"registry/core/artifacts/{n}.json").read_text()) for n in ("core.Process.interface", "core.units")]
    (work / "base.json").write_text(json.dumps(base))
    only = set(args.only)
    runner = ROOT / "scripts/run-generated-proofs.py"
    failed = False
    for law, props, fields, step in laws(ROOT):
        if only and law["id"] not in only:
            continue
        if not re.fullmatch(r"[A-Za-z0-9._-]+", law["id"]) or law["id"] in (".", ".."):
            raise ValueError(f"unsafe law id: {law['id']!r}")
        directory = work / law["id"]
        directory.mkdir(parents=True, exist_ok=True)
        (directory / "original.json").write_text(json.dumps(declarations(law, props, True)))
        (directory / "mutant.json").write_text(json.dumps(declarations(law, props, False)))
        numeric = [f for f in fields if props.get(f) != "text"]
        try:
            expected, computed = states(law["expression"], numeric, step), True
        except (NameError, TypeError, SyntaxError, ValueError):
            # The driver needs an integer; 0 is a placeholder, reported as "not computed".
            expected, computed = 0, False
        profile = {"profile": "norn.bench.one-create/1", "action": "bench.create.action", "identity": "sample",
                   "inputs": {f: [str(BOUND[0]), str(BOUND[1])] for f in numeric}, "seed": 0,
                   "expectedStates": expected, "exclusions": {"authorization": "pre-authorized; outside profile"}}
        reported = expected if computed else "not computed"
        (directory / "bounds.json").write_text(json.dumps(profile))
        try:
            run = subprocess.run([sys.executable, "-I", "-B", str(runner), args.binary, str(directory / "original.json"),
                                  str(directory / "mutant.json"), str(work / "base.json"), str(directory / "bounds.json"),
                                  str(directory / "jobs"), args.tools], capture_output=True, text=True, timeout=500, check=False)
        except subprocess.TimeoutExpired:
            failed = True
            print("BENCH-PROOF " + json.dumps({"id": law["id"], "returncode": None, "expectedStates": reported,
                                                "report": None, "stderr": "proof driver timed out"}), flush=True)
            continue
        if run.returncode != 0:
            failed = True
        report = None
        for line in reversed(run.stdout.splitlines()):
            try:
                report = json.loads(line)
                break
            except json.JSONDecodeError:
                continue
        print("BENCH-PROOF " + json.dumps({"id": law["id"], "returncode": run.returncode, "expectedStates": reported,
                                            "report": report, "stderr": run.stderr[-400:] if report is None else ""}), flush=True)
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
