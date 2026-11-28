"""Regression cases for OWL-Time inline Verus verdicts, wrapper pinning, mutants, and D107 comparison."""
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

    def test_wrapper_contract_pins_ordering_all_arguments_and_short_circuit(self):
        wrapper = (REPO / gate["WRAPPER_SOURCE"]).read_text(encoding="utf-8")
        gate["check_wrapper"](wrapper)

        mutations = {
            "swapped Ordering": wrapper.replace(
                "Ordering::Less => EndpointOrder::Less,",
                "Ordering::Less => EndpointOrder::Greater,",
                1,
            ),
            "omitted ALL variant": wrapper.replace("Self::Meets,\n", "", 1),
            "duplicated ALL variant": wrapper.replace("Self::Meets,", "Self::Before,", 1),
            "swapped holds argument order": wrapper.replace(
                "endpoint_order(end.cmp(other_start)),\n            endpoint_order(other_end.cmp(start)),",
                "endpoint_order(other_end.cmp(start)),\n            endpoint_order(end.cmp(other_start)),",
                1,
            ),
            "flipped holds cmp operands": wrapper.replace(
                "endpoint_order(start.cmp(other_start)),",
                "endpoint_order(other_start.cmp(start)),",
                1,
            ),
            "removed short-circuit": wrapper.replace(
                "if !allen_core::is_less(start_end) {\n            return false;\n        }\n",
                "",
                1,
            ),
            "swapped allen_relation argument order": wrapper.replace(
                "endpoint_order(end.cmp(other_start)),\n        endpoint_order(other_end.cmp(start)),",
                "endpoint_order(other_end.cmp(start)),\n        endpoint_order(end.cmp(other_start)),",
                1,
            ),
        }
        for label, mutated in mutations.items():
            with self.subTest(case=label):
                self.assertNotEqual(mutated, wrapper)
                with self.assertRaises(SystemExit):
                    gate["check_wrapper"](mutated)

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

    def test_compare_vir_and_thir_checks_literals_ctors_patterns_and_logical_ops(self):
        vir = """(@ "allen_core.rs:1:1: 1:2" (Function
 (Fun :path allen_core::converse) :mode Exec :params (
  (@ "allen_core.rs:1:20: 1:28" (Param :name (VarIdent "relation" (VarIdentDisambiguate RustcId 1))
   :typ (Typ Datatype (Dt Path allen_core::t%AllenRelation) () ()))))
 :body (@@ "allen_core.rs:2:5: 4:6" (> Match
  (@@ "allen_core.rs:2:11: 2:19" (> ReadPlace (@@ "allen_core.rs:2:11: 2:19" (Place Local (VarIdent "relation" (VarIdentDisambiguate RustcId 1))) (Typ Datatype (Dt Path allen_core::t%AllenRelation) () ())) (UnfinalizedReadKind :preliminary_kind (ReadKind Copy) :id 1)) (Typ Datatype (Dt Path allen_core::t%AllenRelation) () ()))
  ((Arm
    :pattern (@@ "allen_core.rs:3:9: 3:30" (Pattern Constructor (Dt Path allen_core::t%AllenRelation) "Before" ()) (Typ Datatype (Dt Path allen_core::t%AllenRelation) () ()))
    :guard (@@ "allen_core.rs:3:9: 3:30" (> Const (Constant Bool true)) (Typ Bool))
    :body (@@ "allen_core.rs:3:34: 3:54" (> Ctor (Dt Path allen_core::t%AllenRelation) "After" ()) (Typ Datatype (Dt Path allen_core::t%AllenRelation) () ())))))
  (Typ Datatype (Dt Path allen_core::t%AllenRelation) () ()))))
(@ "allen_core.rs:5:1: 5:2" (Function
 (Fun :path allen_core::shares_instant) :mode Exec :params (
  (@ "allen_core.rs:5:20: 5:28" (Param :name (VarIdent "relation" (VarIdentDisambiguate RustcId 1))
   :typ (Typ Datatype (Dt Path allen_core::t%AllenRelation) () ()))))
 :body (@@ "allen_core.rs:6:5: 6:9" (> Const (Constant Bool true)) (Typ Bool))))
(@ "allen_core.rs:9:1: 9:2" (Function
 (Fun :path allen_core::holds) :mode Exec :params (
  (@ "allen_core.rs:9:20: 9:28" (Param :name (VarIdent "relation" (VarIdentDisambiguate RustcId 1))
   :typ (Typ Datatype (Dt Path allen_core::t%AllenRelation) () ()))))
 :body (@@ "allen_core.rs:10:5: 10:18" (> Logical (LogicalOp And)
  (@@ "allen_core.rs:10:5: 10:9" (> Const (Constant Bool true)) (Typ Bool))
  (@@ "allen_core.rs:10:13: 10:18" (> Const (Constant Bool false)) (Typ Bool))) (Typ Bool))))
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
            span: crates/norn-expr/src/temporal/allen_core.rs:2:5: 4:6
            kind:
                Match {
                    scrutinee:
                        Expr {
                            ty: temporal::allen_core::AllenRelation
                            temp_scope_id: 1
                            span: crates/norn-expr/src/temporal/allen_core.rs:2:11: 2:19
                            kind:
                                VarRef { id: 1 }
                        }
                    arms: [
                        Arm {
                            pattern:
                                Pat {
                                    ty: temporal::allen_core::AllenRelation
                                    span: crates/norn-expr/src/temporal/allen_core.rs:3:9: 3:30
                                    kind: PatKind {
                                        Variant {
                                            adt_def: temporal::allen_core::AllenRelation
                                            args: []
                                            variant_index: VariantIdx(0)
                                            subpatterns: []
                                        }
                                    }
                                }
                            body:
                                Expr {
                                    ty: temporal::allen_core::AllenRelation
                                    temp_scope_id: 1
                                    span: crates/norn-expr/src/temporal/allen_core.rs:3:34: 3:54
                                    kind:
                                        Adt {
                                            adt_def: temporal::allen_core::AllenRelation
                                            variant_index: VariantIdx(12)
                                            args: []
                                            user_ty: None
                                            fields: []
                                            base: None
                                        }
                                }
                        }
                    ]
                }
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
                Literal {
                    lit: Spanned { node: Bool(true), span: crates/norn-expr/src/temporal/allen_core.rs:6:5: 6:9 (#0) }
                    neg: false
                }
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
            span: crates/norn-expr/src/temporal/allen_core.rs:10:5: 10:18
            kind:
                LogicalOp {
                    op: And
                    lhs:
                        Expr {
                            ty: bool
                            temp_scope_id: 1
                            span: crates/norn-expr/src/temporal/allen_core.rs:10:5: 10:9
                            kind:
                                Literal {
                                    lit: Spanned { node: Bool(true), span: crates/norn-expr/src/temporal/allen_core.rs:10:5: 10:9 (#0) }
                                    neg: false
                                }
                        }
                    rhs:
                        Expr {
                            ty: bool
                            temp_scope_id: 1
                            span: crates/norn-expr/src/temporal/allen_core.rs:10:13: 10:18
                            kind:
                                Literal {
                                    lit: Spanned { node: Bool(false), span: crates/norn-expr/src/temporal/allen_core.rs:10:13: 10:18 (#0) }
                                    neg: false
                                }
                        }
                }
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
                Adt {
                    adt_def: std::option::Option
                    variant_index: 0
                    args: [temporal::allen_core::AllenRelation]
                    user_ty: None
                    fields: []
                    base: None
                }
        }
    ]
}
DefId(0:5 ~ norn_expr[abcd]::temporal::allen_core::{impl#5}::eq):
Thir {
    params: [
        Param {
            ty: &'{erased} temporal::allen_core::AllenRelation
        }
    ]
}
DefId(0:6 ~ norn_expr[abcd]::temporal::allen_core::{impl#6}::assert_receiver_is_total_eq):
Thir {
    params: [
        Param {
            ty: &'{erased} temporal::allen_core::AllenRelation
        }
    ]
}
"""
        counts = gate["compare"](vir, thir)
        self.assertEqual(set(counts), {"converse", "shares_instant", "holds", "classify"})
        self.assertEqual(counts["converse"]["pattern_variants"], 1)
        self.assertEqual(counts["converse"]["ctors"], 1)
        self.assertEqual(counts["shares_instant"]["literals"], 1)
        self.assertEqual(counts["holds"]["logical_ops"], 1)

        semantic_mutations = {
            "bool literal true->false at same span": (
                thir.replace("Bool(true)", "Bool(false)", 1),
                "shares_instant literals",
            ),
            "enum constructor variant swap at same span": (
                thir.replace("variant_index: VariantIdx(12)", "variant_index: VariantIdx(1)", 1),
                "converse ctors",
            ),
            "Option constructor None->Some at same span": (
                thir.replace("variant_index: 0", "variant_index: 1", 1),
                "classify ctors",
            ),
            "match pattern arm variant swap at same span": (
                thir.replace("variant_index: VariantIdx(0)", "variant_index: VariantIdx(1)", 1),
                "converse pattern_variants",
            ),
            "logical op And->Or at same span": (
                thir.replace("op: And", "op: Or", 1),
                "holds logical_ops",
            ),
            "type mismatch": (
                thir.replace("ty: bool", "ty: u8", 1),
                "shares_instant expressions",
            ),
            "unexpected derived method on non-AllenRelation": (
                thir.replace(
                    "ty: &'{erased} temporal::allen_core::AllenRelation",
                    "ty: &'{erased} temporal::allen_core::EndpointOrder",
                    1,
                ),
                "unexpected derived THIR method",
            ),
        }
        for label, (mutated_thir, expected_error) in semantic_mutations.items():
            with self.subTest(case=label):
                self.assertNotEqual(mutated_thir, thir)
                with self.assertRaisesRegex(SystemExit, expected_error):
                    gate["compare"](vir, mutated_thir)

    def test_compare_vir_and_thir_checks_integer_literals(self):
        vir = """(@ "allen_core.rs:1:1: 1:2" (Function
 (Fun :path allen_core::converse) :mode Exec :params ()
 :body (@@ "allen_core.rs:2:5: 2:6" (> Const (Constant Int 1)) (Typ Int (IntRange U 8)))))
(@ "allen_core.rs:3:1: 3:2" (Function
 (Fun :path allen_core::shares_instant) :mode Exec :params ()
 :body (@@ "allen_core.rs:4:5: 4:9" (> Const (Constant Bool true)) (Typ Bool))))
(@ "allen_core.rs:5:1: 5:2" (Function
 (Fun :path allen_core::holds) :mode Exec :params ()
 :body (@@ "allen_core.rs:6:5: 6:10" (> Const (Constant Bool false)) (Typ Bool))))
(@ "allen_core.rs:7:1: 7:2" (Function
 (Fun :path allen_core::classify) :mode Exec :params ()
 :body (@@ "allen_core.rs:8:5: 8:9" (> Ctor (Dt Path core::t%option::t%Option) "None" ()) (Typ Datatype (Dt Path core::t%option::t%Option) ((Typ Datatype (Dt Path allen_core::t%AllenRelation) () ())) ()))))
"""
        thir = """DefId(0:1 ~ norn_expr[abcd]::temporal::allen_core::converse):
Thir {
    params: []
    exprs: [
        Expr {
            ty: u8
            temp_scope_id: 1
            span: crates/norn-expr/src/temporal/allen_core.rs:2:5: 2:6
            kind:
                Literal {
                    lit: Spanned { node: Int(Pu128(1), Unsuffixed), span: crates/norn-expr/src/temporal/allen_core.rs:2:5: 2:6 (#0) }
                    neg: false
                }
        }
    ]
}
DefId(0:2 ~ norn_expr[abcd]::temporal::allen_core::shares_instant):
Thir {
    params: []
    exprs: [
        Expr {
            ty: bool
            temp_scope_id: 1
            span: crates/norn-expr/src/temporal/allen_core.rs:4:5: 4:9
            kind:
                Literal {
                    lit: Spanned { node: Bool(true), span: crates/norn-expr/src/temporal/allen_core.rs:4:5: 4:9 (#0) }
                    neg: false
                }
        }
    ]
}
DefId(0:3 ~ norn_expr[abcd]::temporal::allen_core::holds):
Thir {
    params: []
    exprs: [
        Expr {
            ty: bool
            temp_scope_id: 1
            span: crates/norn-expr/src/temporal/allen_core.rs:6:5: 6:10
            kind:
                Literal {
                    lit: Spanned { node: Bool(false), span: crates/norn-expr/src/temporal/allen_core.rs:6:5: 6:10 (#0) }
                    neg: false
                }
        }
    ]
}
DefId(0:4 ~ norn_expr[abcd]::temporal::allen_core::classify):
Thir {
    params: []
    exprs: [
        Expr {
            ty: std::option::Option<temporal::allen_core::AllenRelation>
            temp_scope_id: 1
            span: crates/norn-expr/src/temporal/allen_core.rs:8:5: 8:9
            kind:
                Adt {
                    adt_def: std::option::Option
                    variant_index: VariantIdx(0)
                    args: [temporal::allen_core::AllenRelation]
                    user_ty: None
                    fields: []
                    base: None
                }
        }
    ]
}
"""
        gate["compare"](vir, thir)
        with self.assertRaisesRegex(SystemExit, "converse literals"):
            gate["compare"](vir, thir.replace("Pu128(1)", "Pu128(2)", 1))


if __name__ == "__main__":
    unittest.main()
