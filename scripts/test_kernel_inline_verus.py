"""Regression cases for statement approval and Verus's per-function evidence."""
from pathlib import Path
import runpy
import unittest

HERE = Path(__file__).resolve().parent
gate = runpy.run_path(str(HERE / "kernel_inline_verus.py"))
coverage = runpy.run_path(str(HERE / "coverage_manifest.py"))


class VersionContracts(unittest.TestCase):
    def test_approved_statement_rejects_weaker_exhaustion_law(self):
        source = (HERE.parent / "crates/norn-kernel/src/version_check.rs").read_text()
        symbols = coverage["load"]()["inlineVerus"]["version_check"]
        gate["approved_statements"](source, symbols)
        weakened = source.replace("None => version == u64::MAX,", "None => true,")
        self.assertNotEqual(source, weakened)
        with self.assertRaisesRegex(SystemExit, "approved statement changed"):
            gate["approved_statements"](weakened, symbols)

    def test_verifier_outcomes(self):
        symbols = {"checked_next_version": {"postcondition": "successor"},
                   "has_committed_deletion": {"postcondition": "deleted"}}
        for mutant in (None, *symbols):
            with self.subTest(mutant=mutant):
                code = int(mutant is not None)
                summary = {"is-verifying-entire-crate": True, "encountered-vir-error": False,
                           "success": mutant is None, "errors": code, "verified": 3 - code}
                rows, notes = [], {}
                for name, spec in symbols.items():
                    symbol = f'norn_kernel_version::{name}'
                    labels = [spec["postcondition"]]
                    rows.append({"mode:": "exec", "function": symbol, "success": name != mutant})
                    notes[symbol] = {"obligation_proof_notes": labels,
                                     "failed_proof_notes": labels if name == mutant else []}
                data = {"verification-results": summary, "func-details": notes,
                        "times-ms": {"smt": {"smt-run-module-times": [{"function-breakdown": rows}]}}}
                check = gate["check_verdict"]
                check(data, code, symbols, mutant)
                for key, bad in (("verified", 0), ("encountered-vir-error", True),
                                 ("success", mutant is not None)):
                    with self.assertRaises(SystemExit):
                        check({**data, "verification-results": {**summary, key: bad}}, code, symbols, mutant)
                if mutant:
                    failed = notes[f'norn_kernel_version::{mutant}']["failed_proof_notes"]
                    failed[:] = ["wrong obligation"]
                    with self.assertRaisesRegex(SystemExit, "incorrect postcondition"):
                        check(data, code, symbols, mutant)
                rows.pop()
                with self.assertRaisesRegex(SystemExit, "unexpected executable"):
                    check(data, code, symbols, mutant)


if __name__ == "__main__":
    unittest.main()
