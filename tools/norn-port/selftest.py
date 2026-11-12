"""Real-evaluator harness self-test; run only in a disposable build sandbox."""
from __future__ import annotations

import argparse
import ast
import copy
import hashlib
import tempfile
import sys
import unittest
from pathlib import Path

# Isolated mode (-I) drops the script directory; admit only this directory.
sys.path.insert(0, str(Path(__file__).resolve().parent))
import port


def comparison_violations(source, filename):
    """Conservative lint: raw value equality belongs only in json_compare.equal.

    This deliberately needs no fragile JSON taint tracking: aliases, nested
    values, helper parameters and future JSON readers get the same protection.
    The module entrypoint compares interpreter metadata, never wire data.
    """
    tree = ast.parse(source, filename=filename)
    approved = set()
    if port.equal(filename, "json_compare.py"):
        for node in tree.body:
            if isinstance(node, ast.FunctionDef) and port.equal(node.name, "equal"):
                approved.update(ast.walk(node))
    failures = []
    for node in ast.walk(tree):
        if not isinstance(node, ast.Compare) or node in approved:
            continue
        if not any(isinstance(op, (ast.Eq, ast.NotEq)) for op in node.ops):
            continue
        entrypoint = (isinstance(node.left, ast.Name) and port.equal(node.left.id, "__name__")
                      and port.equal(len(node.ops), 1) and isinstance(node.ops[0], ast.Eq)
                      and isinstance(node.comparators[0], ast.Constant)
                      and port.equal(node.comparators[0].value, "__main__"))
        if not entrypoint:
            failures.append(f"{filename}:{node.lineno}: use json_compare.equal for value equality")
    return failures


FIXTURES = Path(__file__).resolve().parent / "selftest-fixtures"
# Every module here is linted unless it is listed below with its reason, so a
# new module is covered by default.
NOT_PARITY_VALUES = {
    "code_ontology.py",       # compares source syntax (node kinds, names), never parity wire values
    "code_ontology_test.py",  # asserts on that tool's facts
    "codegraph.py",           # compares a version string and file digests
}
NORN = ""


class ParitySelfTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.output = Path(self.temp.name)
        self.spec = {
            "id": "selftest:bump", "source": {"path": "original.py", "symbol": "bump"},
            "invocation": {"documents": ["doc"]}, "artifactId": "demo.bump.rule",
            "generatorSchema": {"type": "object", "properties": {"doc": {"type": "object",
                "properties": {"count": {"type": "integer", "minimum": -100, "maximum": 100}},
                "required": ["count"], "additionalProperties": False}},
                "required": ["doc"], "additionalProperties": False},
            "seeds": [{"doc": {"count": -1}}, {"doc": {"count": 0}}],
            "seedEvidence": "selftest seeds", "seed": 20261013, "cases": 1000, "reportName": "bump",
        }
        self.manifest_path = self.output / "manifest.json"
        port.write(self.manifest_path, {"units": [self.spec]})

    def assertJsonEqual(self, left, right, message=None):
        self.assertTrue(port.equal(left, right), message or f"{left!r} differs from {right!r}")

    def test_real_source_mutations_and_refusals_match_rust(self):
        report = port.run_unit(self.spec, FIXTURES, FIXTURES, NORN, self.output, self.manifest_path)
        self.assertJsonEqual(report["parityManifestSha256"], hashlib.sha256(self.manifest_path.read_bytes()).hexdigest())
        self.assertJsonEqual(report["generatedCases"], 1000)
        self.assertJsonEqual(report["agreementRate"], 1)
        self.assertJsonEqual(report["divergences"], [])
        self.assertTrue((self.output / "bump.md").exists())

    def test_boolean_to_number_mutations_are_preserved(self):
        spec = {"source": {"path": "original.py", "symbol": "mutate_boolean"},
                "invocation": {"documents": ["doc"]}}
        cases = [{"doc": {"flag": flag, "nested": [{"flag": flag}]}} for flag in [True, False]]
        _, outcomes = port.original(spec, FIXTURES, cases)
        for case, outcome in zip(cases, outcomes, strict=True):
            self.assertFalse(port.equal(case, outcome["input"]))
            self.assertIs(type(outcome["input"]["doc"]["flag"]), int)
            self.assertIs(type(outcome["input"]["doc"]["nested"][0]["flag"]), int)
        self.assertFalse(port.equal({"x": [True]}, {"x": [1]}))
        self.assertFalse(port.equal({"x": [False]}, {"x": [0]}))

    def test_number_to_boolean_mutations_in_plain_json_are_preserved(self):
        spec = {"source": {"path": "original.py", "symbol": "mutate_number"}}
        cases = [{"data": {"flag": number, "nested": [{"flag": number}]}} for number in [1, 0]]
        _, outcomes = port.original(spec, FIXTURES, cases)
        for case, outcome in zip(cases, outcomes, strict=True):
            self.assertFalse(port.equal(case, outcome["input"]))
            self.assertIs(type(outcome["input"]["data"]["flag"]), bool)
            self.assertIs(type(outcome["input"]["data"]["nested"][0]["flag"]), bool)

    def test_all_harness_equalities_use_the_shared_comparator(self):
        failures = []
        linted = sorted(path.name for path in port.ROOT.glob("*.py") if path.name not in NOT_PARITY_VALUES)
        self.assertTrue({"json_compare.py", "original.py", "port.py", "python_ast.py"} <= set(linted))
        self.assertTrue(all((port.ROOT / name).is_file() for name in NOT_PARITY_VALUES))
        for name in linted:
            failures.extend(comparison_violations((port.ROOT / name).read_text(), name))
        self.assertJsonEqual(failures, [])
        for source in [
            'value = json.loads(line)\nif value == 1: pass',
            'alias = case["nested"]\nif before != alias: pass',
            'def helper(left, right):\n return left == right',
            'if spec.get("seed") == True: pass',
            'if len(cases) == spec["cases"]: pass',
        ]:
            self.assertTrue(comparison_violations(source, "original.py"), source)
        self.assertFalse(comparison_violations('if equal(left, right): pass', "original.py"))
        self.assertFalse(comparison_violations('def equal(a,b): return a == b', "json_compare.py"))
        self.assertTrue(comparison_violations('def other(a,b): return a == b', "json_compare.py"))

    def test_source_annotations_match_unextracted_module(self):
        # Compare to Python's execution of the whole safe module, including eager
        # parameter/return aliases and decorator inspection of annotations.
        body = '''Alias = int
def annotate(function):
    function.annotation_is_int = function.__annotations__["value"] is int
    return function
@annotate
def inspect(value: Alias, *, optional: Alias = 0) -> Alias:
    return {"eager": inspect.annotation_is_int,
            "annotation": str(inspect.__annotations__["value"]), "value": value}
'''
        for future in ["", "from __future__ import annotations\n"]:
            with self.subTest(postponed=bool(future)):
                source = self.output / "annotations.py"
                source.write_text(future + body)
                namespace = {}
                exec(compile(source.read_bytes(), str(source), "exec", dont_inherit=True), namespace)
                case = {"value": 3}
                expected = namespace["inspect"](**case)
                spec = {"source": {"path": source.name, "symbol": "inspect"}}
                _, outcomes = port.original(spec, self.output, [case])
                self.assertJsonEqual(outcomes[0], {"value": expected, "refusal": None, "input": case})

    def test_postponed_annotations_do_not_load_unused_setup(self):
        source = self.output / "postponed.py"
        source.write_text('''from __future__ import annotations
Unneeded = 1 / 0
constant: Unneeded = 4
def inspect(value: Missing) -> Missing:
    return {"annotation": inspect.__annotations__["value"], "constant": constant}
''')
        spec = {"source": {"path": source.name, "symbol": "inspect"}}
        _, outcomes = port.original(spec, self.output, [{"value": 0}])
        self.assertJsonEqual(outcomes[0]["value"], {"annotation": "Missing", "constant": 4})

    def test_scoreboard_requires_current_artifacts_and_original_source(self):
        defs = self.output / "defs"
        defs.mkdir()
        artifact_path = defs / "bump.rule.json"
        artifact_path.write_bytes((FIXTURES / "bump.rule.json").read_bytes())
        source = self.output / "original.py"
        source.write_bytes((FIXTURES / "original.py").read_bytes())
        inventory = {"format": "norn.code-unit-inventory/0.1", "app": "selftest", "sourceLoc": 10,
                     "units": [{"id": self.spec["id"], "source": {**self.spec["source"], "line": 1},
                                "trigger": "call", "reads": [], "writes": [], "externalCalls": [],
                                "loc": 10, "target": "rule", "parityRecipe": "pure",
                                "status": "translated", "nornArtifactIds": [self.spec["artifactId"]]}]}
        inventory_path = self.output / "inventory.json"
        port.write(inventory_path, inventory)
        report = {"format": "norn.port-parity/0.1-draft", "unit": self.spec["id"],
                  "artifactId": self.spec["artifactId"], "artifactHashMode": "exact-file-bytes",
                  "originalSha256": hashlib.sha256(source.read_bytes()).hexdigest(),
                  "nornArtifactSha256": hashlib.sha256(artifact_path.read_bytes()).hexdigest(),
                  "parityManifestSha256": hashlib.sha256(self.manifest_path.read_bytes()).hexdigest(),
                  "manifestHashMode": "exact-file-bytes",
                  "generatedCases": 1000, "caseCount": 1002, "divergences": []}
        reports = self.output / "reports"
        report_path = reports / "bump.json"
        port.write(report_path, report)

        def score():
            return port.scoreboard(inventory_path, defs, reports, self.output / "scoreboard.json",
                                   FIXTURES, self.output, self.manifest_path)

        self.assertJsonEqual(score()["parityPassRate"], 1)
        for key, value in [
            ("generatorSchema", {"type": "object"}),
            ("seeds", [{"doc": {"count": 99}}]),
            ("seed", 42), ("cases", 2000), ("invocation", {}),
            ("seedEvidence", "different seed evidence"),
        ]:
            changed = {**self.spec, key: value}
            port.write(self.manifest_path, {"units": [changed]})
            self.assertJsonEqual(score()["parityPassRate"], 0, key)
            self.assertJsonEqual(score()["unmeasuredUnits"], [self.spec["id"]], key)
        port.write(self.manifest_path, {"units": [self.spec]})
        self.assertJsonEqual(score()["parityPassRate"], 1)
        # Legacy evidence has no manifest binding and must fail closed.
        legacy = {key: value for key, value in report.items() if key not in
                  ("parityManifestSha256", "manifestHashMode")}
        port.write(report_path, legacy)
        self.assertJsonEqual(score()["parityPassRate"], 0)
        port.write(report_path, report)
        artifact_path.write_text(artifact_path.read_text() + "\n")
        self.assertJsonEqual(score()["parityPassRate"], 0)
        self.assertJsonEqual(score()["unmeasuredUnits"], [self.spec["id"]])
        artifact_path.write_bytes((FIXTURES / "bump.rule.json").read_bytes())
        source.write_text(source.read_text() + "\n")
        self.assertJsonEqual(score()["parityPassRate"], 0)
        source.write_bytes((FIXTURES / "original.py").read_bytes())
        for field, value in [("artifactId", "wrong.rule"), ("nornArtifactSha256", "wrong"),
                             ("originalSha256", "wrong"), ("artifactHashMode", "unknown"),
                             ("parityManifestSha256", "wrong"), ("manifestHashMode", "unknown")]:
            port.write(report_path, {**report, field: value})
            self.assertJsonEqual(score()["parityPassRate"], 0, field)
        port.write(report_path, report)
        port.write(reports / "duplicate.json", report)
        self.assertJsonEqual(score()["parityPassRate"], 0)
        (reports / "duplicate.json").unlink()
        inventory["units"][0]["nornArtifactIds"].append("another.rule")
        port.write(inventory_path, inventory)
        self.assertJsonEqual(score()["parityPassRate"], 0)

    def test_jsonlines_preserves_unicode_separators_in_strings(self):
        script = 'import json; print(json.dumps({"text": "\\u0085\\u2028\\u2029"}, ensure_ascii=False))'
        self.assertJsonEqual(port.communicate([sys.executable, "-I", "-c", script], [{}]),
                         [{"text": "\u0085\u2028\u2029"}])

    def test_stream_reuses_process_and_preserves_unicode_json_frames(self):
        script = 'import json, sys\nfor line in sys.stdin: print(json.dumps(json.loads(line), ensure_ascii=False), flush=True)'
        with port.JsonLines([sys.executable, "-I", "-c", script]) as stream:
            for text in ["\u0085\u2028\u2029", "next frame"]:
                self.assertJsonEqual(stream.call({"text": text}), {"text": text})

    def test_generator_is_reproducible(self):
        self.assertJsonEqual(port.generated(self.spec["generatorSchema"], 42, 1000),
                         port.generated(self.spec["generatorSchema"], 42, 1000))

    def test_divergence_is_retained_and_shrunk(self):
        defs = self.output / "defs"
        defs.mkdir()
        for path in FIXTURES.glob("*.json"):
            (defs / path.name).write_bytes(path.read_bytes())
        artifact = port.read(defs / "bump.rule.json")
        expression = artifact["payload"]["graph"]["nodes"][1]["content"]["expressions"][1]
        expression["value"] = expression["value"].replace("count + 1", "count + 2")
        port.write(defs / "bump.rule.json", artifact)
        spec = copy.deepcopy(self.spec)
        spec["cases"] = 2
        port.write(self.manifest_path, {"units": [spec]})
        report = port.run_unit(spec, FIXTURES, defs, NORN, self.output, self.manifest_path)
        self.assertLess(report["agreementRate"], 1)
        for divergence in report["divergences"]:
            self.assertFalse(divergence["accepted"])
            self.assertJsonEqual(divergence["minimized"]["input"], {"doc": {"count": 0}})


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--norn", required=True)
    args = parser.parse_args()
    NORN = args.norn
    unittest.main(argv=[__file__])
