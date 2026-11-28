"""Regression cases for statement approval and Verus's per-function evidence."""
from pathlib import Path
import runpy
import subprocess
import tempfile
import unittest

HERE = Path(__file__).resolve().parent
gate = runpy.run_path(str(HERE / "kernel_inline_verus.py"))
verus_ir = runpy.run_path(str(HERE / "kernel_verus_ir.py"))
compare_ir = verus_ir["compare"]
compare_artifact_ir = verus_ir["compare_artifact_governance"]
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


class ArtifactGovernanceContracts(unittest.TestCase):
    def test_approved_statement_rejects_weakened_spec_or_enum_change(self):
        source = (HERE.parent / "crates/norn-ir/src/artifact_governance.rs").read_text()
        symbols = coverage["load"]()["inlineVerus"]["artifact_governance"]
        gate["approved_artifact_statements"](source, symbols)
        for label, before, after in (
            ("source_schema_flipped", "ArtifactKind::SourceSchema => false,", "ArtifactKind::SourceSchema => true,"),
            ("law_dropped_from_spec", "            | ArtifactKind::Law\n", ""),
            ("approved_dropped_from_spec", "ArtifactLifecycle::Approved | ArtifactLifecycle::Active => true,",
             "ArtifactLifecycle::Active => true,"),
            ("lemma_weakened", "ensures !expected_direct_write(kind, ArtifactLifecycle::Active),",
             "ensures true,"),
        ):
            with self.subTest(label=label):
                weakened = source.replace(before, after, 1)
                self.assertNotEqual(source, weakened)
                with self.assertRaisesRegex(SystemExit, "approved statement changed"):
                    gate["approved_artifact_statements"](weakened, symbols)

    def test_seeded_mutants_patch_only_executable_body(self):
        source_path = HERE.parent / "crates/norn-ir/src/artifact_governance.rs"
        original = source_path.read_text()
        symbols = coverage["load"]()["inlineVerus"]["artifact_governance"]
        patches = HERE.parent / "verification/mutants/artifact_governance"
        declared = set(symbols["direct_artifact_write_allowed"]["mutants"])
        self.assertEqual({p.stem for p in patches.glob("*.patch")}, declared)
        body = next(m for m in gate["ARTIFACT_BODY"].finditer(original) if m["name"] == "direct_artifact_write_allowed")
        start, end = body.span("body")
        tail = len(original) - end
        for mutant_id in sorted(declared):
            with self.subTest(mutant=mutant_id), tempfile.TemporaryDirectory() as tmp:
                copy = Path(tmp) / source_path.name
                copy.write_text(original)
                subprocess.run(
                    ["patch", "--batch", "--forward", "--fuzz=0", copy, patches / f"{mutant_id}.patch"],
                    cwd=tmp, check=True, stdout=subprocess.DEVNULL,
                )
                mutated = copy.read_text()
                self.assertNotEqual(mutated, original)
                self.assertEqual(mutated[:start], original[:start])
                self.assertEqual(mutated[len(mutated) - tail:], original[end:])
                # Statement digest must stay identical because only the exec body changed.
                gate["approved_artifact_statements"](mutated, symbols)

    def test_artifact_verifier_outcomes(self):
        symbols = {
            "direct_artifact_write_allowed": {
                "postcondition": "direct_artifact_write_truth_table",
            }
        }
        crate = gate["ARTIFACT_CRATE"]
        lemmas = sorted(gate["ARTIFACT_LEMMAS"])
        for mutant in (None, "direct_artifact_write_allowed"):
            with self.subTest(mutant=mutant):
                code = int(mutant is not None)
                summary = {"is-verifying-entire-crate": True, "encountered-vir-error": False,
                           "success": mutant is None, "errors": code, "verified": 6 - code}
                symbol = f"{crate}::direct_artifact_write_allowed"
                label = "direct_artifact_write_truth_table"
                rows = [{"mode:": "exec", "function": symbol, "success": mutant is None}]
                rows.extend({"mode:": "proof", "function": f"{crate}::{lemma}", "success": True} for lemma in lemmas)
                notes = {symbol: {"obligation_proof_notes": [label],
                                  "failed_proof_notes": [label] if mutant else []}}
                data = {"verification-results": summary, "func-details": notes,
                        "times-ms": {"smt": {"smt-run-module-times": [{"function-breakdown": rows}]}}}
                check = gate["check_artifact_verdict"]
                check(data, code, symbols, mutant)
                rows[-1]["success"] = False
                with self.assertRaisesRegex(SystemExit, "missing or failed lemmas"):
                    check(data, code, symbols, mutant)
                rows[-1]["success"] = True
                if mutant:
                    notes[symbol]["failed_proof_notes"] = ["wrong"]
                    with self.assertRaisesRegex(SystemExit, "incorrect postcondition"):
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


class ArtifactGovernanceCompilerAgreement(unittest.TestCase):
    vir = """(@ "crates/norn-ir/src/artifact_governance.rs:187:1: 220:2 (#0)"
 (Function (Fun :path norn_ir_artifact_governance::direct_artifact_write_allowed) :mode Exec :params (
  (@ "crates/norn-ir/src/artifact_governance.rs:188:5: 188:24 (#0)"
   (Param :name (VarIdent "kind" (VarIdentDisambiguate RustcId 2))
          :typ (Typ Decorate (TypDecoration Ref) None
                (Typ Datatype (Dt Path norn_ir_artifact_governance::t%ArtifactKind) () ()))))
  (@ "crates/norn-ir/src/artifact_governance.rs:189:5: 189:34 (#0)"
   (Param :name (VarIdent "lifecycle" (VarIdentDisambiguate RustcId 3))
          :typ (Typ Decorate (TypDecoration Ref) None
                (Typ Datatype (Dt Path norn_ir_artifact_governance::t%ArtifactLifecycle) () ())))))
 :body (@@ "crates/norn-ir/src/artifact_governance.rs:190:10: 220:2 (#0)"
  (> Block (
   (@@ "crates/norn-ir/src/artifact_governance.rs:191:9: 191:26 (#0)"
    (Pattern Var
     (VarBinder :name (VarIdent "authority_bearing" (VarIdentDisambiguate RustcId 4))
      :typ (Typ Bool)))
    (Typ Bool))
   (@@ "crates/norn-ir/src/artifact_governance.rs:191:29: 209:6 (#0)"
    (> Match
     (@@ "crates/norn-ir/src/artifact_governance.rs:191:36: 191:40 (#0)"
      (Place Local (VarIdent "kind" (VarIdentDisambiguate RustcId 2)))
      (Typ Decorate (TypDecoration Ref) None
       (Typ Datatype (Dt Path norn_ir_artifact_governance::t%ArtifactKind) () ())))
     ((Arm
       :pattern ()
       :guard (@@ "crates/norn-ir/src/artifact_governance.rs:191:29: 209:6 (#0)"
               (> Const (Constant Bool true)) (Typ Bool))
       :body (@@ "crates/norn-ir/src/artifact_governance.rs:207:35: 207:39 (#0)"
              (> Const (Constant Bool true)) (Typ Bool)))))
    (Typ Bool)))
  (@@ "crates/norn-ir/src/artifact_governance.rs:219:5: 219:53 (#0)"
   (> Unary (UnaryOp Not)
    (@@ "crates/norn-ir/src/artifact_governance.rs:219:7: 219:52 (#0)"
     (> Logical (LogicalOp And)
      (@@ "crates/norn-ir/src/artifact_governance.rs:219:7: 219:24 (#0)"
       (> Const (Constant Bool true)) (Typ Bool))
      (@@ "crates/norn-ir/src/artifact_governance.rs:219:28: 219:52 (#0)"
       (> Const (Constant Bool false)) (Typ Bool)))
     (Typ Bool)))
   (Typ Bool)))
  (Typ Bool))))
"""
    thir = """DefId(0:12 ~ norn_ir[abcd]::artifact_governance::direct_artifact_write_allowed):
params: [
    Param {
        pat: Some(Pat {
            ty: &'{erased} artifact_governance::ArtifactKind
            span: crates/norn-ir/src/artifact_governance.rs:188:5: 188:24 (#0)
            kind: PatKind {
                Binding {
                    name: "kind"
                    mode: ByValue
                    var: LocalVarId(2)
                    ty: &'{erased} artifact_governance::ArtifactKind
                }
            }
        })
    },
    Param {
        pat: Some(Pat {
            ty: &'{erased} artifact_governance::ArtifactLifecycle
            span: crates/norn-ir/src/artifact_governance.rs:189:5: 189:34 (#0)
            kind: PatKind {
                Binding {
                    name: "lifecycle"
                    mode: ByValue
                    var: LocalVarId(3)
                    ty: &'{erased} artifact_governance::ArtifactLifecycle
                }
            }
        })
    },
]
body:
    Expr {
        ty: bool
        temp_scope_id: 1
        span: crates/norn-ir/src/artifact_governance.rs:190:10: 220:2 (#0)
        kind:
            Block {
                stmts: [
                    Stmt {
                        pattern: Pat {
                            ty: bool
                            span: crates/norn-ir/src/artifact_governance.rs:191:9: 191:26 (#0)
                            kind: PatKind {
                                Binding {
                                    name: "authority_bearing"
                                    mode: ByValue
                                    var: LocalVarId(4)
                                    ty: bool
                                }
                            }
                        }
                        initializer: Some(
                            Expr {
                                ty: bool
                                temp_scope_id: 2
                                span: crates/norn-ir/src/artifact_governance.rs:191:29: 209:6 (#0)
                                kind:
                                    Match {
                                        scrutinee:
                                            Expr {
                                                ty: artifact_governance::ArtifactKind
                                                temp_scope_id: 3
                                                span: crates/norn-ir/src/artifact_governance.rs:191:35: 191:40 (#0)
                                                kind:
                                                    Deref {
                                                        arg:
                                                            Expr {
                                                                ty: &'{erased} artifact_governance::ArtifactKind
                                                                temp_scope_id: 4
                                                                span: crates/norn-ir/src/artifact_governance.rs:191:36: 191:40 (#0)
                                                                kind:
                                                                    VarRef
                                                            }
                                                    }
                                            }
                                        arms: [
                                            Arm {
                                                body:
                                                    Expr {
                                                        ty: bool
                                                        temp_scope_id: 5
                                                        span: crates/norn-ir/src/artifact_governance.rs:207:35: 207:39 (#0)
                                                        kind:
                                                            Literal
                                                    }
                                            }
                                        ]
                                    }
                            }
                        )
                    }
                ]
                expr:
                    Expr {
                        ty: bool
                        temp_scope_id: 6
                        span: crates/norn-ir/src/artifact_governance.rs:219:5: 219:53 (#0)
                        kind:
                            Unary {
                                arg:
                                    Expr {
                                        ty: bool
                                        temp_scope_id: 7
                                        span: crates/norn-ir/src/artifact_governance.rs:219:7: 219:52 (#0)
                                        kind:
                                            LogicalOp {
                                                lhs:
                                                    Expr {
                                                        ty: bool
                                                        temp_scope_id: 8
                                                        span: crates/norn-ir/src/artifact_governance.rs:219:7: 219:24 (#0)
                                                        kind:
                                                            Literal
                                                    }
                                                rhs:
                                                    Expr {
                                                        ty: bool
                                                        temp_scope_id: 9
                                                        span: crates/norn-ir/src/artifact_governance.rs:219:28: 219:52 (#0)
                                                        kind:
                                                            Literal
                                                    }
                                            }
                                    }
                            }
                    }
            }
    }
"""
    thir_flat = """DefId(0:12 ~ norn_ir[abcd]::artifact_governance::direct_artifact_write_allowed):
Thir {
    arms: [],
    blocks: [],
    exprs: [
        Expr {
            kind: VarRef {
                id: LocalVarId(2),
            },
            ty: &'{erased} artifact_governance::ArtifactKind,
            temp_scope_id: Node(4),
            span: crates/norn-ir/src/artifact_governance.rs:191:36: 191:40 (#0),
        },
        Expr {
            kind: Deref {
                arg: e0,
            },
            ty: artifact_governance::ArtifactKind,
            temp_scope_id: Node(3),
            span: crates/norn-ir/src/artifact_governance.rs:191:35: 191:40 (#0),
        },
        Expr {
            kind: Literal {
                lit: Spanned {
                    node: Bool(
                        true,
                    ),
                    span: crates/norn-ir/src/artifact_governance.rs:207:35: 207:39 (#0),
                },
                neg: false,
            },
            ty: bool,
            temp_scope_id: Node(5),
            span: crates/norn-ir/src/artifact_governance.rs:207:35: 207:39 (#0),
        },
        Expr {
            kind: Match {
                scrutinee: e1,
                arms: [a0],
                match_source: Normal,
            },
            ty: bool,
            temp_scope_id: Node(2),
            span: crates/norn-ir/src/artifact_governance.rs:191:29: 209:6 (#0),
        },
        Expr {
            kind: Literal {
                lit: Spanned {
                    node: Bool(
                        true,
                    ),
                    span: crates/norn-ir/src/artifact_governance.rs:219:7: 219:24 (#0),
                },
                neg: false,
            },
            ty: bool,
            temp_scope_id: Node(8),
            span: crates/norn-ir/src/artifact_governance.rs:219:7: 219:24 (#0),
        },
        Expr {
            kind: Literal {
                lit: Spanned {
                    node: Bool(
                        false,
                    ),
                    span: crates/norn-ir/src/artifact_governance.rs:219:28: 219:52 (#0),
                },
                neg: false,
            },
            ty: bool,
            temp_scope_id: Node(9),
            span: crates/norn-ir/src/artifact_governance.rs:219:28: 219:52 (#0),
        },
        Expr {
            kind: LogicalOp {
                op: And,
                lhs: e4,
                rhs: e5,
            },
            ty: bool,
            temp_scope_id: Node(7),
            span: crates/norn-ir/src/artifact_governance.rs:219:7: 219:52 (#0),
        },
        Expr {
            kind: Unary {
                op: Not,
                arg: e6,
            },
            ty: bool,
            temp_scope_id: Node(6),
            span: crates/norn-ir/src/artifact_governance.rs:219:5: 219:53 (#0),
        },
        Expr {
            kind: Scope {
                region_scope: Node(1),
                value: e7,
            },
            ty: bool,
            temp_scope_id: Node(1),
            span: crates/norn-ir/src/artifact_governance.rs:219:5: 219:53 (#0),
        },
        Expr {
            kind: Block {
                block: b0,
            },
            ty: bool,
            temp_scope_id: Node(1),
            span: crates/norn-ir/src/artifact_governance.rs:190:10: 220:2 (#0),
        },
    ],
    stmts: [],
    params: [],
}
"""

    def test_equal_artifact_compiler_views(self):
        counts = compare_artifact_ir(self.vir, self.thir, self.thir_flat, ["direct_artifact_write_allowed"])
        self.assertEqual(
            counts["direct_artifact_write_allowed"],
            {
                "expressions": 8,
                "locals": 3,
                "calls": 0,
                "literal_patterns": 0,
                "bool_literals": 3,
                "operators": 2,
            },
        )
        # Single-line Bool(true)/Bool(false) formatting must also be accepted.
        single_line = self.thir_flat.replace("Bool(\n                        true,\n                    )", "Bool(true)")
        single_line = single_line.replace("Bool(\n                        false,\n                    )", "Bool(false)")
        self.assertEqual(
            compare_artifact_ir(self.vir, self.thir, single_line, ["direct_artifact_write_allowed"]),
            counts,
        )
        # Rustc's thir-tree may print Deref without `{ arg:` braces (indented child Expr directly).
        bare_deref_thir = self.thir.replace(
            "Deref {\n                                                        arg:\n                                                            Expr {",
            "Deref\n                                                        Expr {",
            1,
        )
        self.assertNotEqual(bare_deref_thir, self.thir)
        self.assertEqual(
            compare_artifact_ir(self.vir, bare_deref_thir, self.thir_flat, ["direct_artifact_write_allowed"]),
            counts,
        )
        # Line-streaming iterators (as used when reading rustc stdout) must match string input
        # and discard unrelated DefId sections.
        noise = "DefId(0:1 ~ norn_ir[abcd]::other::big_fn):\n    ignored\n"
        tree_stream = (line for line in (noise + self.thir + noise).splitlines(True))
        flat_stream = (line for line in (noise + self.thir_flat + noise).splitlines(True))
        self.assertEqual(
            compare_artifact_ir(self.vir, tree_stream, flat_stream, ["direct_artifact_write_allowed"]),
            counts,
        )
        # Duplicate target sections must fail closed when streaming.
        with self.assertRaisesRegex(ValueError, "duplicate THIR function"):
            verus_ir["artifact_thir_sections"]((self.thir + self.thir).splitlines(True), ["direct_artifact_write_allowed"])

    def test_rejects_flipped_executable_bool_literals(self):
        # Flip the final executable Boolean literal in VIR while leaving THIR unchanged.
        head, sep, tail = self.vir.rpartition("(Constant Bool false)")
        self.assertTrue(sep)
        flipped_vir = head + "(Constant Bool true)" + tail
        with self.assertRaisesRegex(ValueError, "bool_literals"):
            compare_artifact_ir(flipped_vir, self.thir, self.thir_flat, ["direct_artifact_write_allowed"])

        # Flip an executable Boolean literal in THIR while leaving VIR unchanged.
        flipped_flat = self.thir_flat.replace(
            "node: Bool(\n                        false,\n                    ),",
            "node: Bool(\n                        true,\n                    ),",
            1,
        )
        self.assertNotEqual(flipped_flat, self.thir_flat)
        with self.assertRaisesRegex(ValueError, "bool_literals"):
            compare_artifact_ir(self.vir, self.thir, flipped_flat, ["direct_artifact_write_allowed"])

    def test_rejects_artifact_compiler_disagreements(self):
        cases = {
            "expr_span": (self.thir.replace("207:35: 207:39", "207:35: 207:40"), self.thir_flat),
            "local_name": (self.thir.replace('name: "authority_bearing"', 'name: "other_local"'), self.thir_flat),
            "cast": (self.thir.replace("Literal", "Cast", 1), self.thir_flat),
            "missing_fn": (self.thir.replace("direct_artifact_write_allowed", "other_fn"), self.thir_flat),
            "missing_flat_fn": (self.thir, self.thir_flat.replace("direct_artifact_write_allowed", "other_fn")),
            "flipped_unary_op": (self.thir, self.thir_flat.replace("op: Not,", "op: Neg,", 1)),
            "flipped_logical_op": (self.thir, self.thir_flat.replace("op: And,", "op: Or,", 1)),
            "negative_literal": (self.thir, self.thir_flat.replace("neg: false,", "neg: true,", 1)),
            "mismatched_tree_deref": (
                self.thir.replace(
                    "ty: artifact_governance::ArtifactKind\n                                                temp_scope_id: 3",
                    "ty: artifact_governance::ArtifactLifecycle\n                                                temp_scope_id: 3",
                    1,
                ),
                self.thir_flat,
            ),
            "mismatched_flat_deref": (
                self.thir,
                self.thir_flat.replace(
                    "ty: artifact_governance::ArtifactKind,\n            temp_scope_id: Node(3),",
                    "ty: artifact_governance::ArtifactLifecycle,\n            temp_scope_id: Node(3),",
                    1,
                ),
            ),
            "non_bool_thir_literal": (
                self.thir,
                self.thir_flat.replace(
                    "node: Bool(\n                        false,\n                    ),",
                    "node: Int(\n                        Pu128(\n                            1,\n                        ),\n                        Unsuffixed,\n                    ),",
                    1,
                ),
            ),
        }
        for label, (bad_thir, bad_flat) in cases.items():
            with self.subTest(label=label), self.assertRaises(ValueError):
                compare_artifact_ir(self.vir, bad_thir, bad_flat, ["direct_artifact_write_allowed"])
        bad_guard = self.vir.replace("(Constant Bool true)", "(Constant Bool false)", 1)
        with self.assertRaisesRegex(ValueError, "unexpected match guard"):
            compare_artifact_ir(bad_guard, self.thir, self.thir_flat, ["direct_artifact_write_allowed"])
        non_bool_vir = self.vir.replace("(Constant Bool false)", "(Constant Int 1)", 1)
        with self.assertRaisesRegex(ValueError, "unsupported VIR executable constant"):
            compare_artifact_ir(non_bool_vir, self.thir, self.thir_flat, ["direct_artifact_write_allowed"])
        bad_vir_op = self.vir.replace("(LogicalOp And)", "(LogicalOp Or)", 1)
        with self.assertRaisesRegex(ValueError, "operators"):
            compare_artifact_ir(bad_vir_op, self.thir, self.thir_flat, ["direct_artifact_write_allowed"])


if __name__ == "__main__":
    unittest.main()
