#!/usr/bin/env python3
"""PROOF-QUINT-RUN-1: result availability never overrides timeout/exit policy."""
import importlib.util
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


if __name__ == "__main__":
    unittest.main()
