"""Regression cases for statement approval and Verus's per-function evidence."""
from pathlib import Path
import runpy
import unittest

HERE = Path(__file__).resolve().parent
gate = runpy.run_path(str(HERE / "kernel_inline_verus.py"))
compare_ir = runpy.run_path(str(HERE / "kernel_verus_ir.py"))["compare"]
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



class CompilerAgreement(unittest.TestCase):
    # A scalar static call and an unused parameter isolate the three D107 fields.
    # Preserve both pinned printers' syntax at every field read by the decoders.
    vir = """(Function
 (Fun :path norn_kernel_version::probe) :mode Exec :params (
 (Param :name (VarIdent "x" (VarIdentDisambiguate RustcId 2)) :typ (Typ Int (IntRange U 64))))
 :ensure () :body (@@ (> Block () (@@ (> Call :target
 (CallTarget Fun (CallTargetKind Static) (Fun :path core::t%num::impl&%9::min_value) () ()
 (CallTargetAttrs :autospec (AutospecUsage Final) :const_var false :assume_external_allowed false))
 :args () :post_args None :body None) (Typ Int (IntRange U 64)))) (Typ Int (IntRange U 64))))
"""
    thir = """DefId(0:4 ~ norn_kernel[123a]::version_check::probe):
Thir {
    arms: [],
    blocks: [
        Block {
            stmts: [],
            expr: Some(e1,),
        },
    ],
    exprs: [
        Expr {
            kind: ZstLiteral {
                user_ty: None,
            },
            ty: FnDef(DefId(2:11 ~ core[123a]::num::{impl#9}::min_value), Binder { value: [], bound_vars: [], },),
        },
        Expr {
            kind: Call {
                ty: FnDef(DefId(2:11 ~ core[123a]::num::{impl#9}::min_value), Binder { value: [], bound_vars: [], },),
                fun: e0,
                args: [],
            },
            ty: u64,
        },
        Expr {
            kind: Block {
                block: b0,
            },
            ty: u64,
        },
    ],
    stmts: [],
    params: [
        Param {
            pat: Some(Pat {
                    kind: Binding {
                        name: "x",
                        var: LocalVarId(HirId(DefId(0:4 ~ norn_kernel[123a]::version_check::probe).2),),
                        ty: u64,
                    },
                }),
            ty: u64,
        },
    ],
}
"""

    def test_equal_compiler_views(self):
        ir = compare_ir(self.vir, self.thir.splitlines(True), ["probe"])["probe"]
        self.assertEqual(ir["callees"], [(1, "core::num::{impl#9}::min_value")])
        self.assertEqual(ir["expr_types"], [("Block", "u64", 1), ("call", "u64", 0)])
        self.assertEqual(ir["local_types"], [("x", "u64")])

    def test_callee_operand_must_be_a_function_item(self):
        field = next(line for line in self.thir.splitlines() if line.startswith("            ty: FnDef("))
        ty = field.strip().removeprefix("ty: ").removesuffix(",")
        wrapped = self.thir.replace(field, f"            ty: ({ty},),", 1)
        with self.assertRaisesRegex(ValueError, "top-level THIR FnDef"):
            compare_ir(self.vir, wrapped.splitlines(True), ["probe"])

    def test_call_and_operand_must_resolve_to_the_same_crate_instance(self):
        changed = self.thir.replace("core[123a]", "core[123b]", 1)
        with self.assertRaisesRegex(ValueError, "THIR callee mismatch"):
            compare_ir(self.vir, changed.splitlines(True), ["probe"])

    def test_truncated_target_at_record_boundaries(self):
        truncated = self.thir.removesuffix("}\n")
        for boundary in ("", "DefId(0:5 ~ norn_kernel[123a]::other):\n"):
            with self.subTest(boundary=boundary), self.assertRaisesRegex(ValueError, "incomplete THIR"):
                compare_ir(self.vir, (truncated + boundary).splitlines(True), ["probe"])

    def test_unbalanced_nested_record(self):
        broken = self.thir.replace("pat: Some(Pat {", "pat: Some(Pat ")
        with self.assertRaisesRegex(ValueError, "unbalanced THIR"):
            compare_ir(self.vir, broken.splitlines(True), ["probe"])

    def test_rejects_each_compiler_disagreement(self):
        exprs, params = self.thir.split("    params:")
        cases = {
            "callee": self.thir.replace("impl#9", "impl#8"),
            "expression": exprs.replace("ty: u64", "ty: u32") + "    params:" + params,
            "local": exprs + "    params:" + params.replace("ty: u64", "ty: u32"),
            "missing": self.thir.replace("version_check::probe", "unrelated::probe"),
            "unknown expression": self.thir.replace("kind: Call", "kind: Other"),
        }
        for case, dump in cases.items():
            with self.subTest(case=case), self.assertRaises(ValueError):
                compare_ir(self.vir, dump.splitlines(True), ["probe"])
        with self.assertRaisesRegex(ValueError, "incomplete compiler inventory"):
            compare_ir("", self.thir.splitlines(True), ["probe"])


if __name__ == "__main__":
    unittest.main()
