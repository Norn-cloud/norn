#!/usr/bin/env python3
"""PROOF-QUINT-RUN-1: result availability never overrides timeout/exit policy."""
import importlib.util
import copy
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("generated_proofs", Path(__file__).with_name("check-generated-quint.py"))
runner = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runner)


class VerdictPolicy(unittest.TestCase):
    def test_success_requires_complete_agreeing_results(self):
        primary = {"status": "success", "distinctStates": 78}
        oracle = {"status": "ok", "errors": []}
        self.assertEqual(runner.verdict(primary, oracle, 0, False, 78), "ok")
        for code in [-9, 1, 2, 124, 125]:
            self.assertEqual(runner.verdict(primary, oracle, code, False, 78), "unknown")
        self.assertEqual(runner.verdict(primary, oracle, 0, False, 77), "unknown")
        self.assertEqual(runner.verdict(primary, {**oracle, "errors": ["failure"]}, 0, False, 78), "unknown")

    def test_violation_requires_both_traces_and_the_verdict_exit(self):
        primary = {"status": "invariant-violation", "violation": {"trace": [{"state": {}}]}}
        oracle = {"status": "violation", "errors": [], "trace": [{"state": {}}]}
        self.assertEqual(runner.verdict(primary, oracle, 1, False, 78), "violation")
        for code in [-9, 0, 2, 124, 125]:
            self.assertEqual(runner.verdict(primary, oracle, code, False, 78), "unknown")
        self.assertEqual(runner.verdict(primary, {**oracle, "trace": []}, 1, False, 78), "unknown")
        self.assertEqual(runner.verdict({**primary, "violation": {}}, oracle, 1, False, 78), "unknown")

    def test_timeout_dominates_success_and_counterexample_files(self):
        for primary, oracle in [
            ({"status": "success", "distinctStates": 78}, {"status": "ok", "errors": []}),
            ({"status": "invariant-violation", "violation": {"trace": [1]}}, {"status": "violation", "errors": [], "trace": [1]}),
        ]:
            for code in [0, 1, 124]:
                self.assertEqual(runner.verdict(primary, oracle, code, True, 78), "unknown")

    def test_oracle_trace_decodes_exact_scalars_and_refuses_other_values(self):
        def record(value):
            return [{"kind": "app", "opcode": "Rec", "args": [{"kind": "str", "value": "v1"}, value]}]
        for kind, value in [("int", 2), ("bool", False)]:
            self.assertEqual(runner.oracle_trace(record({"kind": kind, "value": value})), [{"state": {"v1": value}}])
        for value in [{"kind": "int", "value": 0.5}, {"kind": "int", "value": True}, {"kind": "int", "value": 2**31}, {"kind": "str", "value": "2"}, {"kind": "bool", "value": 0}]:
            with self.assertRaises(ValueError):
                runner.oracle_trace(record(value))
        duplicate = record({"kind": "int", "value": 2})
        duplicate[0]["args"] *= 2
        for trace in [None, [], [None], [{"kind": "app", "opcode": "Rec", "args": [1]}], duplicate]:
            with self.assertRaises(ValueError):
                runner.oracle_trace(trace)

    def test_arithmetic_qualification_covers_both_expression_layers(self):
        self.assertTrue(runner.arithmetic_qualified("(2).to(12).oneOf()"))
        self.assertTrue(runner.arithmetic_qualified("-127"))
        self.assertFalse(runner.arithmetic_qualified("128"))
        self.assertTrue(runner.arithmetic_qualified("v999999999999"))


class ConstantInitializerPredicate(unittest.TestCase):
    def fixture(self, coefficient=0):
        return {"modules": [{"name": "generated", "declarations": [
            {"kind": "var", "name": "present"}, {"kind": "var", "name": "amount"},
            {"kind": "def", "name": "init", "qualifier": "action", "expr": {
                "kind": "app", "opcode": "actionAll", "args": [
                    {"kind": "app", "opcode": "assign", "args": [{"kind": "name", "name": "present"}, {"kind": "bool", "value": False}]},
                    {"kind": "app", "opcode": "assign", "args": [{"kind": "name", "name": "amount"}, {"kind": "int", "value": coefficient}]},
                ]}},
            {"kind": "def", "name": "q::init", "qualifier": "action", "expr": {"kind": "name", "name": "init"}},
            {"kind": "def", "name": "step", "qualifier": "action", "expr": {
                "kind": "app", "opcode": "assign", "args": [{"kind": "name", "name": "amount"}, {"kind": "int", "value": 1}]}},
        ]}], "table": {"retained": 1}, "types": {"retained": 2}, "effects": {"retained": 3}}

    def test_constant_initializer_predicate_preserves_every_other_byte_value(self):
        for coefficient in range(-127, 128):
            source = self.fixture(coefficient)
            before = copy.deepcopy(source)
            expected = copy.deepcopy(source)
            for assignment in expected["modules"][0]["declarations"][2]["expr"]["args"]:
                assignment["opcode"] = "eq"
            self.assertEqual(runner.initializer_predicate(source), expected)
            self.assertEqual(source, before)

    def test_constant_initializer_predicate_refuses_partial_opaque_or_reused_init(self):
        mutations = [
            lambda ds: ds[2]["expr"].update(opcode="actionAny"),
            lambda ds: ds[2]["expr"]["args"].pop(),
            lambda ds: ds[2]["expr"]["args"].append(copy.deepcopy(ds[2]["expr"]["args"][0])),
            lambda ds: ds[2]["expr"]["args"][1]["args"][1].update(kind="name", name="amount"),
            lambda ds: ds[2]["expr"]["args"][1]["args"][1].update(value=True),
            lambda ds: ds[3]["expr"].update(name="other"),
            lambda ds: ds[4].update(expr={"kind": "name", "name": "init"}),
            lambda ds: ds[4].update(expr={"kind": "name", "name": "q::init"}),
            lambda ds: ds[4].update(qualifier="run"),
            lambda ds: ds.append(copy.deepcopy(ds[0])),
        ]
        for mutate in mutations:
            source = self.fixture()
            mutate(source["modules"][0]["declarations"])
            with self.assertRaises(ValueError):
                runner.initializer_predicate(source)


if __name__ == "__main__":
    unittest.main()
