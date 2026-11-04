#!/usr/bin/env python3
"""PROOF-QUINT-RUN-1: result availability never overrides timeout/exit policy."""
import importlib.util
import copy
import json
import os
import subprocess
import sys
import tempfile
import time
from unittest import mock
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("generated_proofs", Path(__file__).with_name("check-generated-quint.py"))
runner = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runner)
outer_spec = importlib.util.spec_from_file_location("proof_host", Path(__file__).with_name("run-generated-proofs.py"))
outer = importlib.util.module_from_spec(outer_spec)
outer_spec.loader.exec_module(outer)


class VerdictPolicy(unittest.TestCase):
    def test_tool_asset_pins_ignore_installation_paths_and_commit_content(self):
        with tempfile.TemporaryDirectory() as directory:
            left, right = [Path(directory) / name for name in ["checkout-a", "checkout-b"]]
            left.write_bytes(b"same tool bytes")
            right.write_bytes(left.read_bytes())
            name = "tool/1.2.3/binary"
            self.assertEqual(outer.asset_pins([(name, left)]), outer.asset_pins([(name, right)]))
            self.assertNotIn(directory, json.dumps(outer.asset_pins([(name, left)])))
            right.write_bytes(b"changed tool bytes")
            self.assertNotEqual(outer.asset_pins([(name, left)]), outer.asset_pins([(name, right)]))
            for assets in [[("/host/tool", left)], [("../tool", left)], [(name, left), (name, right)]]:
                with self.assertRaises(ValueError):
                    outer.asset_pins(assets)

    def test_oracle_record_ignores_run_metadata_and_retains_exact_values(self):
        trace = {"#meta": {"description": "Created on 12:00 PID 41 /home/a"},
                 "vars": ["present", "mass"],
                 "states": [{"#meta": {"index": 0}, "present": True, "mass": {"#bigint": "2"}}]}
        first = runner.oracle_record("violation", json.dumps(trace))
        trace["#meta"]["description"] = "Created on 13:00 PID 99 /home/b"
        self.assertEqual(first, runner.oracle_record("violation", json.dumps(trace)))
        self.assertEqual(json.loads(first), {"status": "violation", "trace": [{"state": {"present": True, "mass": 2}}]})
        trace["states"][0]["mass"]["#bigint"] = "999"
        self.assertNotEqual(first, runner.oracle_record("violation", json.dumps(trace)))
        for private in ["Created", "12:00", "PID", "/home/"]:
            self.assertNotIn(private, first)

    def test_success_requires_complete_agreeing_results(self):
        primary = {"status": "success", "distinctStates": 78}
        oracle = {"status": "ok", "errors": []}
        self.assertEqual(runner.verdict(primary, oracle, 0, False, 78), "ok")
        for code in [-9, 1, 2, 124, 125]:
            self.assertEqual(runner.verdict(primary, oracle, code, False, 78), "unknown")
        self.assertEqual(runner.verdict(primary, oracle, 0, False, 77), "unknown")
        self.assertEqual(runner.verdict(primary, {**oracle, "errors": ["failure"]}, 0, False, 78), "unknown")

    def test_violation_requires_both_traces_and_the_verdict_exit(self):
        trace = [{"state": {"present": False, "v": 0}}, {"state": {"present": True, "v": 1}}]
        primary = {"status": "invariant-violation", "violation": {"trace": trace}}
        oracle = {"status": "violation", "errors": [], "trace": trace}
        self.assertEqual(runner.verdict(primary, oracle, 12, False, 78), "violation")
        for code in [-9, 0, 1, 2, 75, 120, 124, 125, 150, 255]:
            self.assertEqual(runner.verdict(primary, oracle, code, False, 78), "unknown")
        self.assertEqual(runner.verdict(primary, {**oracle, "trace": []}, 12, False, 78), "unknown")
        self.assertEqual(runner.verdict({**primary, "violation": {}}, oracle, 12, False, 78), "unknown")

    def test_timeout_dominates_success_and_counterexample_files(self):
        for primary, oracle in [
            ({"status": "success", "distinctStates": 78}, {"status": "ok", "errors": []}),
            ({"status": "invariant-violation", "violation": {"trace": [1]}}, {"status": "violation", "errors": [], "trace": [1]}),
        ]:
            for code in [0, 1, 124]:
                self.assertEqual(runner.verdict(primary, oracle, code, True, 78), "unknown")

    def test_oracle_trace_decodes_exact_scalars_and_refuses_other_values(self):
        def record(value):
            return {"vars": ["v"], "states": [{"#meta": {"index": 0}, "v": value}]}
        for value, expected in [({"#bigint": "2"}, 2), (False, False)]:
            self.assertEqual(runner.oracle_trace(record(value)), [{"state": {"v": expected}}])
        for value in [0.5, 2, "2", {"#bigint": "1.25"}, {"#bigint": "+1"}, {"#bigint": "01"}, {"#bigint": "-0"}, {"#bigint": str(2**31)}]:
            with self.assertRaises(ValueError):
                runner.oracle_trace(record(value))
        for trace in [None, {}, {"vars": ["v", "v"], "states": []}, {"vars": ["v"], "states": [{"#meta": {"index": False}, "v": True}]}]:
            with self.assertRaises(ValueError):
                runner.oracle_trace(trace)

    def test_cli_outcome_exit_and_trace_shape_must_agree(self):
        success = "The outcome is: NoError I@00:00\n"
        violation = "State 1: state invariant 0 violated. I@00:00\nThe outcome is: Error I@00:00\n"
        self.assertEqual(runner.oracle_verdict(0, success), "ok")
        self.assertEqual(runner.oracle_verdict(12, violation), "violation")
        for code, output in [(1, violation), (12, success), (0, violation), (75, success), (255, success), (12, "The outcome is: Error"), (0, success + success)]:
            self.assertEqual(runner.oracle_verdict(code, output), "unknown")
        primary = {"status": "invariant-violation", "violation": {"trace": [{"state": {"present": True, "v": 2}}]}}
        for trace in [[{"state": {"present": True, "other": 2}}], [{"state": {"present": False, "v": 2}}], [{"state": {"present": True, "v": False}}]]:
            self.assertEqual(runner.verdict(primary, {"status": "violation", "errors": [], "trace": trace}, 12, False, 78), "unknown")

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


    def test_malformed_compiler_shapes_have_visible_refusal_reasons(self):
        malformed = [None, [], 1, {}, {"modules": None}, {"modules": {}}, {"modules": [None]},
                     {"modules": [{"name": "generated"}]},
                     {"modules": [{"name": "generated", "declarations": None}]},
                     {"modules": [{"name": "generated", "declarations": [None]}]}]
        mutations = [
            lambda ds: ds[0].update(name=[]),
            lambda ds: ds[0].update(kind={}),
            lambda ds: ds[2].update(expr=None),
            lambda ds: ds[3].update(expr=[]),
            lambda ds: ds[2]["expr"].update(args=None),
            lambda ds: ds[2]["expr"].update(args=[None]),
            lambda ds: ds[2]["expr"]["args"][0].update(args=[None, {}]),
            lambda ds: ds[2]["expr"]["args"][0]["args"][0].update(name=[]),
            lambda ds: ds[4].update(expr={"kind": "name", "name": {}}),
        ]
        for mutate in mutations:
            value = self.fixture()
            mutate(value["modules"][0]["declarations"])
            malformed.append(value)
        for value in malformed:
            with self.subTest(value=value), self.assertRaises(ValueError) as refusal:
                runner.initializer_predicate(value)
            self.assertTrue(str(refusal.exception))


class ProcessBudget(unittest.TestCase):
    def test_term_ignoring_child_is_reaped_inside_the_shared_budget(self):
        command = [sys.executable, "-c", "import signal,time; signal.signal(signal.SIGTERM,signal.SIG_IGN); print('ready',flush=True); time.sleep(30)"]
        owned = []
        real_popen = subprocess.Popen
        def spawn(*args, **kwargs):
            child = real_popen(*args, **kwargs)
            owned.append(child)
            return child
        with tempfile.TemporaryDirectory() as work, mock.patch.object(runner, "CLEANUP_RESERVE_SECONDS", 0.1), mock.patch.object(runner.subprocess, "Popen", side_effect=spawn):
            began = time.monotonic()
            with self.assertRaises(subprocess.TimeoutExpired) as refusal:
                runner.compile_process(command, os.environ.copy(), Path(work), began + 0.5)
            self.assertLess(time.monotonic() - began, 1.0)
            self.assertIn(b"ready", refusal.exception.output)
            self.assertEqual(len(owned), 1)
            self.assertIsNotNone(owned[0].poll())


if __name__ == "__main__":
    unittest.main()
