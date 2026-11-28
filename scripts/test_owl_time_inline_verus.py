"""Regression cases for OWL-Time inline Verus verdicts, mutants, and D107 comparison."""
from pathlib import Path
import runpy
import subprocess
import tempfile
import unittest

HERE = Path(__file__).resolve().parent
REPO = HERE.parent
gate = runpy.run_path(str(HERE / "owl_time_inline_verus.py"))


class OwlTimeVerusTests(unittest.TestCase):
    def test_mutant_inventory_and_scoped_patches(self):
        source_path = REPO / gate["SOURCE"]
        patches_dir = REPO / gate["PATCHES"]
        original = source_path.read_text(encoding="utf-8")
        self.assertIn("#![deny(clippy::default_numeric_fallback)]", original.splitlines())

        owners = {m: name for name, cfg in gate["SYMBOLS"].items() for m in cfg["mutants"]}
        self.assertEqual({p.stem for p in patches_dir.glob("*.patch")}, owners.keys())

        with tempfile.TemporaryDirectory() as tmp:
            work = Path(tmp)
            for mutant, name in sorted(owners.items()):
                with self.subTest(mutant=mutant, symbol=name):
                    case = work / mutant
                    case.mkdir()
                    copy = case / source_path.name
                    copy.write_text(original, encoding="utf-8")
                    subprocess.run(
                        ["patch", "--batch", "--forward", "--fuzz=0", copy, patches_dir / f"{mutant}.patch"],
                        cwd=case,
                        check=True,
                        stdout=subprocess.DEVNULL,
                    )
                    start, end = gate["function_body"](original, name)
                    mutated = copy.read_text(encoding="utf-8")
                    tail = len(original) - end
                    self.assertNotEqual(mutated, original)
                    self.assertEqual(mutated[:start], original[:start])
                    self.assertEqual(mutated[len(mutated) - tail :], original[end:])

    def test_verifier_outcomes(self):
        symbols = gate["SYMBOLS"]
        lemmas = gate["LEMMAS"]
        crate = gate["CRATE"]
        check = gate["check_verdict"]

        for mutant in (None, *symbols):
            with self.subTest(mutant=mutant):
                code = int(mutant is not None)
                summary = {
                    "is-verifying-entire-crate": True,
                    "encountered-vir-error": False,
                    "success": mutant is None,
                    "errors": code,
                    "verified": len(symbols) + len(lemmas) - code,
                }
                rows, notes = [], {}
                for name, spec in symbols.items():
                    symbol = f"{crate}::{name}"
                    label = [spec["postcondition"]]
                    rows.append({"mode:": "exec", "function": symbol, "success": name != mutant})
                    notes[symbol] = {
                        "obligation_proof_notes": label,
                        "failed_proof_notes": label if name == mutant else [],
                    }
                for lemma in lemmas:
                    rows.append({"mode:": "proof", "function": f"{crate}::{lemma}", "success": True})
                data = {
                    "verification-results": summary,
                    "func-details": notes,
                    "times-ms": {"smt": {"smt-run-module-times": [{"function-breakdown": rows}]}},
                }
                check(data, code, mutant)

                for key, bad in (("verified", 0), ("encountered-vir-error", True), ("success", mutant is not None)):
                    with self.assertRaises(SystemExit):
                        check({**data, "verification-results": {**summary, key: bad}}, code, mutant)

                if mutant:
                    failed = notes[f"{crate}::{mutant}"]["failed_proof_notes"]
                    failed[:] = ["wrong"]
                    with self.assertRaisesRegex(SystemExit, "incorrect postcondition"):
                        check(data, code, mutant)
                    failed[:] = [symbols[mutant]["postcondition"]]

                missing_lemma = [r for r in rows if r["function"] != f"{crate}::pairwise_disjoint"]
                with self.assertRaisesRegex(SystemExit, "missing or failed lemmas"):
                    check(
                        {
                            **data,
                            "times-ms": {
                                "smt": {"smt-run-module-times": [{"function-breakdown": missing_lemma}]}
                            },
                        },
                        code,
                        mutant,
                    )

    def test_compare_vir_and_thir_for_enum_and_bool(self):
        vir = """(@ "allen_core.rs:1:1: 1:2" (Function
 (Fun :path allen_core::converse) :mode Exec :params (
  (@ "allen_core.rs:1:20: 1:28" (Param :name (VarIdent "relation" (VarIdentDisambiguate RustcId 1))
   :typ (Typ Datatype (Dt Path allen_core::t%AllenRelation) () ()))))
 :body (@@ "allen_core.rs:2:5: 2:13" (> ReadPlace (@@ "allen_core.rs:2:5: 2:13" (Place Local (VarIdent "relation" (VarIdentDisambiguate RustcId 1))) (Typ Datatype (Dt Path allen_core::t%AllenRelation) () ())) (UnfinalizedReadKind :preliminary_kind (ReadKind Copy) :id 1)) (Typ Datatype (Dt Path allen_core::t%AllenRelation) () ()))))
(@ "allen_core.rs:5:1: 5:2" (Function
 (Fun :path allen_core::shares_instant) :mode Exec :params (
  (@ "allen_core.rs:5:20: 5:28" (Param :name (VarIdent "relation" (VarIdentDisambiguate RustcId 1))
   :typ (Typ Datatype (Dt Path allen_core::t%AllenRelation) () ()))))
 :body (@@ "allen_core.rs:6:5: 6:9" (> Const (Constant Bool true)) (Typ Bool))))
(@ "allen_core.rs:9:1: 9:2" (Function
 (Fun :path allen_core::holds) :mode Exec :params (
  (@ "allen_core.rs:9:20: 9:28" (Param :name (VarIdent "relation" (VarIdentDisambiguate RustcId 1))
   :typ (Typ Datatype (Dt Path allen_core::t%AllenRelation) () ()))))
 :body (@@ "allen_core.rs:10:5: 10:10" (> Const (Constant Bool false)) (Typ Bool))))
(@ "allen_core.rs:13:1: 13:2" (Function
 (Fun :path allen_core::classify) :mode Exec :params (
  (@ "allen_core.rs:13:20: 13:29" (Param :name (VarIdent "start_end" (VarIdentDisambiguate RustcId 1))
   :typ (Typ Datatype (Dt Path allen_core::t%EndpointOrder) () ()))))
 :body (@@ "allen_core.rs:14:5: 14:9" (> Ctor (Dt Path core::t%option::t%Option) "None" ()) (Typ Datatype (Dt Path core::t%option::t%Option) ((Typ Datatype (Dt Path allen_core::t%AllenRelation) () ())) ()))))
"""
        thir = """DefId(0:1 ~ norn_expr[abcd]::temporal::allen_core::converse):
Thir {
    params: [
        Param {
            pat: Some(Pat {
                ty: temporal::allen_core::AllenRelation,
                span: crates/norn-expr/src/temporal/allen_core.rs:1:20: 1:28,
                kind: PatKind {
                    Binding {
                        name: "relation"
                        mode: ByValue
                        var: LocalVarId(1)
                        ty: temporal::allen_core::AllenRelation
                    }
                }
            })
        }
    ]
    exprs: [
        Expr {
            ty: temporal::allen_core::AllenRelation
            temp_scope_id: 1
            span: crates/norn-expr/src/temporal/allen_core.rs:2:5: 2:13
            kind:
                VarRef { id: 1 }
        }
    ]
}
DefId(0:2 ~ norn_expr[abcd]::temporal::allen_core::shares_instant):
Thir {
    params: [
        Param {
            pat: Some(Pat {
                ty: temporal::allen_core::AllenRelation,
                span: crates/norn-expr/src/temporal/allen_core.rs:5:20: 5:28,
                kind: PatKind {
                    Binding {
                        name: "relation"
                        mode: ByValue
                        var: LocalVarId(1)
                        ty: temporal::allen_core::AllenRelation
                    }
                }
            })
        }
    ]
    exprs: [
        Expr {
            ty: bool
            temp_scope_id: 1
            span: crates/norn-expr/src/temporal/allen_core.rs:6:5: 6:9
            kind:
                Literal { lit: true }
        }
    ]
}
DefId(0:3 ~ norn_expr[abcd]::temporal::allen_core::holds):
Thir {
    params: [
        Param {
            pat: Some(Pat {
                ty: temporal::allen_core::AllenRelation,
                span: crates/norn-expr/src/temporal/allen_core.rs:9:20: 9:28,
                kind: PatKind {
                    Binding {
                        name: "relation"
                        mode: ByValue
                        var: LocalVarId(1)
                        ty: temporal::allen_core::AllenRelation
                    }
                }
            })
        }
    ]
    exprs: [
        Expr {
            ty: bool
            temp_scope_id: 1
            span: crates/norn-expr/src/temporal/allen_core.rs:10:5: 10:10
            kind:
                Literal { lit: false }
        }
    ]
}
DefId(0:4 ~ norn_expr[abcd]::temporal::allen_core::classify):
Thir {
    params: [
        Param {
            pat: Some(Pat {
                ty: temporal::allen_core::EndpointOrder,
                span: crates/norn-expr/src/temporal/allen_core.rs:13:20: 13:29,
                kind: PatKind {
                    Binding {
                        name: "start_end"
                        mode: ByValue
                        var: LocalVarId(1)
                        ty: temporal::allen_core::EndpointOrder
                    }
                }
            })
        }
    ]
    exprs: [
        Expr {
            ty: std::option::Option<temporal::allen_core::AllenRelation>
            temp_scope_id: 1
            span: crates/norn-expr/src/temporal/allen_core.rs:14:5: 14:9
            kind:
                Adt { variant: None }
        }
    ]
}
"""
        counts = gate["compare"](vir, thir)
        self.assertEqual(set(counts), {"converse", "shares_instant", "holds", "classify"})
        with self.assertRaises(SystemExit):
            gate["compare"](vir, thir.replace("ty: bool", "ty: u8", 1))


if __name__ == "__main__":
    unittest.main()
