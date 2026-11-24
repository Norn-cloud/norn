"""Tests for the experimental typed Python symbolic-tracing spike (`tools/norn-py`).

Covers all 6 Sol blocking review findings:
1. Host isolation boundary (closed AST annotation resolver with zero `eval()`, restricted
   minimal `__builtins__`, closed stdlib module proxies, dunder/metaclass/builtin blocking,
   and POSIX `resource`-limited clean subprocesses).
2. Pre-tracing callback AST validation (rejects Python `if`/`if`-expr, `and`/`or`/`not`,
   `is`/`in`, chained comparisons, loops, comprehensions, nested lambdas, and non-allowlisted
   calls BEFORE tracing).
3. Never silently drop constraints or accept raw IR (object `invariants` emitted into
   `objectType.invariants`, interface `lifecycles` explicitly rejected, closed `Expr`,
   closed `PropertyTypeSpec`, closed `MaskSpec`, duck-typed `.zen` objects rejected).
4. Real determinism (subprocess without `-I` honors `PYTHONHASHSEED=0` verified by cross-process
   `hash("norn-determinism-probe")` equality, plus deterministic elaboration gate).
5. Admission & exact TS/Python parity (`hr_pilot.py` pins `core.units@1.0.0`, avoids colliding
   with `core.TimeInterval.interface@0.1.0`, exact ZEN parenthesization, compact Norn1
   canonical JSON bytes + SHA-256, and live same-pack TS/Python parity test covering
   markings + interface + invariant + entity + lifecycle + guard).
6. Real static typing (`Field[T]` concrete types, `eq`/`ne` accepting `T | Expr[T]`,
   elaboration-time `Annotated[base, meta...]` consistency checks, and positive + negative
   `mypy --strict` subprocess assertions).
"""

from __future__ import annotations

import ast
import json
import os
import shutil
import subprocess
import sys
import tempfile
import types
import unittest
from decimal import Decimal
from enum import StrEnum
from pathlib import Path
from typing import Annotated, Any, Literal
from unittest import mock

from norn_py import (
    Concept,
    Date,
    DeploymentSpec,
    DynamicSym,
    EnvelopeOptions,
    Expr,
    Field,
    FieldMeta,
    Link,
    Marking,
    MarkingDecl,
    Money,
    NonJsonValueError,
    NornFloatRefusalError,
    NornTracingError,
    PackManifest,
    PropertyTypeSpec,
    Quantity,
    Rate,
    SecretRef,
    Sym,
    Timestamp,
    UnitRef,
    all_of,
    between,
    canonical_json,
    canonical_norn1_bytes,
    canonical_norn1_sha256,
    compose_packs,
    date_before,
    date_type,
    decimal_type,
    elaborate_file,
    field_default,
    fn_ref,
    integer,
    interval_gap_seconds,
    intervals_overlap,
    invariant,
    lifecycle,
    link,
    mask_last_n,
    mask_redacted,
    parse_date,
    preflight_source,
    rate_type,
    resolve_annotation,
    sandbox_subprocess_cmd,
    sandbox_subprocess_env,
    timestamp_before,
    timestamp_type,
    trace_bool_callback,
    transition,
    validate_callback_ast,
    verify_deterministic_elaboration,
)
from norn_py.expr import _find_callback_node
from norn_py.sandbox import (
    MAX_SOURCE_BYTES,
    _apply_posix_resource_limits,
    _compute_clamped_rlimit,
    _read_source_bounded,
)

REPO_ROOT = Path(__file__).resolve().parents[3]
NORN_PY_ROOT = Path(__file__).resolve().parents[1]
NORN_TS_ROOT = REPO_ROOT / "tools" / "norn-ts"
HR_PILOT = NORN_PY_ROOT / "examples" / "hr_pilot.py"
CORE_AGENT_GOLDEN = REPO_ROOT / "registry" / "core" / "artifacts" / "core.Agent.interface.json"
CORE_TIME_INTERVAL_GOLDEN = REPO_ROOT / "registry" / "core" / "artifacts" / "core.TimeInterval.interface.json"


# ------------------------------------------------------------------ 1. Host Isolation Boundary


class TestHostIsolationBoundary(unittest.TestCase):
    def test_annotation_resolver_never_evals_arbitrary_code(self) -> None:
        malicious_annotations = [
            "__import__('os').system('echo pwned')",
            "(lambda: 1)()",
            "open('/etc/passwd')",
            "eval('1 + 1')",
            "[x for x in (1, 2)]",
            "().__class__.__bases__[0].__subclasses__()",
        ]
        for ann in malicious_annotations:
            with self.subTest(annotation=ann):
                with self.assertRaises(NornTracingError) as ctx:
                    resolve_annotation(ann, {}, None)
                self.assertEqual(ctx.exception.code, "NORN-PY-UNSAFE-ANNOTATION")

    def test_preflight_blocks_dunder_escapes_builtins_and_transitive_imports(self) -> None:
        snippets = [
            ("import os\n", "NORN-PY-FORBIDDEN-IMPORT"),
            ("from typing import sys\n", "NORN-PY-FORBIDDEN-IMPORT"),
            ("from enum import sys\n", "NORN-PY-FORBIDDEN-IMPORT"),
            ("from norn_py import elaborate_file\n", "NORN-PY-FORBIDDEN-IMPORT"),
            ("from norn_py import *\n", "NORN-PY-FORBIDDEN-IMPORT"),
            ("x = ().__class__.__bases__[0].__subclasses__()\n", "NORN-PY-FORBIDDEN-ATTR"),
            ("x = eval('1+1')\n", "NORN-PY-FORBIDDEN-CALL"),
            ("f = eval\n", "NORN-PY-FORBIDDEN-CALL"),
            ("x = getattr(str, 'upper')\n", "NORN-PY-FORBIDDEN-CALL"),
            ("x = type('Evil', (), {})\n", "NORN-PY-FORBIDDEN-CALL"),
            ("x = globals()\n", "NORN-PY-FORBIDDEN-CALL"),
            ("x = dir()\n", "NORN-PY-FORBIDDEN-CALL"),
            ("open('/etc/passwd')\n", "NORN-PY-FORBIDDEN-CALL"),
            ("x = 1.25\n", "NORN-PY-FLOAT-LITERAL"),
            ("try:\n  x = 1\nexcept Exception:\n  pass\n", "NORN-PY-FORBIDDEN-SYNTAX"),
            ("async def f():\n  pass\n", "NORN-PY-FORBIDDEN-SYNTAX"),
            ("class Evil(metaclass=int):\n  pass\n", "NORN-PY-FORBIDDEN-SYNTAX"),
            ("class Bad:\n  def __init__(self):\n    pass\n", "NORN-PY-FORBIDDEN-SYNTAX"),
        ]
        for snippet, expected_code in snippets:
            with self.subTest(snippet=snippet):
                with self.assertRaises(NornTracingError) as ctx:
                    preflight_source(snippet, "bad.py")
                self.assertEqual(ctx.exception.code, expected_code)

    def test_runtime_sandbox_blocks_transitive_typing_sys_and_arbitrary_base_class(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            # Even if someone imports `typing` as a module, `typing.sys` does NOT exist on the closed proxy!
            bad_typing = Path(tmp) / "bad_typing.py"
            bad_typing.write_text("import typing\nX = typing.sys\n", encoding="utf-8")
            with self.assertRaises(NornTracingError) as ctx:
                elaborate_file(bad_typing, scope={"orgId": "o", "environmentId": "dev"})
            self.assertEqual(ctx.exception.code, "NORN-PY-ELABORATION")
            self.assertIn("AttributeError", str(ctx.exception))

            # Inheriting from an arbitrary builtin class (not Concept/StrEnum/Protocol) fails closed!
            bad_base = Path(tmp) / "bad_base.py"
            bad_base.write_text("class Evil(dict):\n    pass\n", encoding="utf-8")
            with self.assertRaises(NornTracingError) as ctx2:
                elaborate_file(bad_base, scope={"orgId": "o", "environmentId": "dev"})
            self.assertEqual(ctx2.exception.code, "NORN-PY-ELABORATION")
            self.assertIn("NORN-PY-FORBIDDEN-SYNTAX", str(ctx2.exception))

    def test_bounded_source_read_refuses_oversized_file_without_unbounded_read(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            small_cap_file = Path(tmp) / "oversized_small.py"
            small_cap_file.write_bytes(b"x = 1\n" * 20)
            with self.assertRaises(NornTracingError) as ctx_small:
                _read_source_bounded(small_cap_file, "oversized_small.py", max_bytes=32)
            self.assertEqual(ctx_small.exception.code, "NORN-PY-UNBOUNDED")

            big_file = Path(tmp) / "oversized_max.py"
            big_file.write_bytes(b"#" * (MAX_SOURCE_BYTES + 16))
            with self.assertRaises(NornTracingError) as ctx_big:
                elaborate_file(big_file, scope={"orgId": "o", "environmentId": "dev"})
            self.assertEqual(ctx_big.exception.code, "NORN-PY-UNBOUNDED")

    def test_posix_rlimit_preserves_stricter_inherited_soft_limit_and_fails_closed(self) -> None:
        if os.name != "posix":
            self.skipTest("POSIX resource limits test requires POSIX")

        # Pure clamping check: stricter inherited soft limit (2) MUST NOT be raised to desired (15).
        self.assertEqual(_compute_clamped_rlimit(2, 60, 15, -1), (2, 15))
        self.assertEqual(_compute_clamped_rlimit(-1, -1, 15, -1), (15, 15))
        self.assertEqual(_compute_clamped_rlimit(5, 10, 20, -1), (5, 10))

        # Live child subprocess check: inherit stricter RLIMIT_CPU soft=2, hard=30, then apply target=15.
        probe = (
            "import json, resource\n"
            "from norn_py.sandbox import _apply_posix_resource_limits\n"
            "resource.setrlimit(resource.RLIMIT_CPU, (2, 30))\n"
            "_apply_posix_resource_limits(15, 1024 * 1024 * 1024)\n"
            "soft, hard = resource.getrlimit(resource.RLIMIT_CPU)\n"
            "print(json.dumps({'soft': soft, 'hard': hard}))\n"
        )
        out = subprocess.check_output(
            [sys.executable, "-s", "-S", "-B", "-P", "-c", probe],
            env=sandbox_subprocess_env(NORN_PY_ROOT),
            text=True,
        )
        limits = json.loads(out)
        self.assertEqual(limits["soft"], 2)
        self.assertEqual(limits["hard"], 15)

        # Fail-closed check: setrlimit errors MUST propagate, never be silently swallowed.
        import resource

        with mock.patch.object(resource, "setrlimit", side_effect=OSError("rlimit denied")):
            with self.assertRaises(OSError):
                _apply_posix_resource_limits(10, 1024 * 1024 * 1024)


# ------------------------------------------------------------------ 2. Pre-tracing Callback AST Validation


class TestPreTracingCallbackValidation(unittest.TestCase):
    def test_rejects_ternary_if_exp_and_is_none_before_tracing(self) -> None:
        accessed = False

        class _SpySym:
            def __getattr__(self, _name: str) -> Any:
                nonlocal accessed
                accessed = True
                return None

        bad_ternary = lambda s: s.ends_at if s.ends_at is None else s.starts_at  # noqa: E731
        with self.assertRaises(NornTracingError) as ctx:
            trace_bool_callback(bad_ternary, _SpySym(), context="test ternary")  # type: ignore[arg-type]
        self.assertEqual(ctx.exception.code, "NORN-PY-FORBIDDEN-CONTROL-FLOW")
        self.assertIn("when(cond, yes, no)", str(ctx.exception))
        self.assertFalse(accessed, "callback MUST be rejected by AST check before any attribute access runs")

    def test_rejects_python_and_or_not_before_tracing(self) -> None:
        sym = DynamicSym("self", ("a", "b"), "hr.Test")

        # Even if `s.a` would never be evaluated, AST inspection rejects BEFORE calling the lambda!
        bad_and = lambda s: s.a and s.b  # noqa: E731
        with self.assertRaises(NornTracingError) as ctx_and:
            validate_callback_ast(bad_and)
        self.assertEqual(ctx_and.exception.code, "NORN-PY-FORBIDDEN-CONTROL-FLOW")
        self.assertIn("all_of", str(ctx_and.exception))

        bad_or = lambda s: s.a or s.b  # noqa: E731
        with self.assertRaises(NornTracingError) as ctx_or:
            validate_callback_ast(bad_or)
        self.assertEqual(ctx_or.exception.code, "NORN-PY-FORBIDDEN-CONTROL-FLOW")
        self.assertIn("any_of", str(ctx_or.exception))

        bad_not = lambda s: not s.a  # noqa: E731
        with self.assertRaises(NornTracingError) as ctx_not:
            validate_callback_ast(bad_not)
        self.assertEqual(ctx_not.exception.code, "NORN-PY-FORBIDDEN-CONTROL-FLOW")
        self.assertIn("not_", str(ctx_not.exception))

        bad_is = lambda s: s.a is None  # noqa: E731
        with self.assertRaises(NornTracingError) as ctx_is:
            validate_callback_ast(bad_is)
        self.assertEqual(ctx_is.exception.code, "NORN-PY-FORBIDDEN-CONTROL-FLOW")
        self.assertIn("is_null", str(ctx_is.exception))

        bad_in = lambda s: "x" in s.a  # noqa: E731
        with self.assertRaises(NornTracingError) as ctx_in:
            validate_callback_ast(bad_in)
        self.assertEqual(ctx_in.exception.code, "NORN-PY-FORBIDDEN-CONTROL-FLOW")
        self.assertIn("contains", str(ctx_in.exception))

        bad_chain = lambda s: 0 < s.a <= 12  # noqa: E731
        with self.assertRaises(NornTracingError) as ctx_chain:
            validate_callback_ast(bad_chain)
        self.assertEqual(ctx_chain.exception.code, "NORN-PY-FORBIDDEN-CONTROL-FLOW")

        # Verify `trace_bool_callback` rejects BEFORE calling the callback body at all.
        def bad_if_stmt(s: Any) -> Expr[bool]:
            if s.a:
                res1: Expr[bool] = s.a.eq("x")
                return res1
            res2: Expr[bool] = s.b.eq("y")
            return res2

        with self.assertRaises(NornTracingError) as ctx_if:
            trace_bool_callback(bad_if_stmt, sym, context="test if")
        self.assertEqual(ctx_if.exception.code, "NORN-PY-FORBIDDEN-CONTROL-FLOW")

        bad_call = lambda s: len(s.a) > 0  # noqa: E731
        with self.assertRaises(NornTracingError) as ctx_call:
            validate_callback_ast(bad_call)
        self.assertEqual(ctx_call.exception.code, "NORN-PY-FORBIDDEN-CALL")

        bad_comp = lambda s: [x for x in (1, 2)]  # noqa: E731
        with self.assertRaises(NornTracingError) as ctx_comp:
            validate_callback_ast(bad_comp)
        self.assertEqual(ctx_comp.exception.code, "NORN-PY-FORBIDDEN-CONTROL-FLOW")

        bad_nested_lambda = lambda s: (lambda x: x)(s.a)  # noqa: E731
        with self.assertRaises(NornTracingError) as ctx_lam:
            validate_callback_ast(bad_nested_lambda)
        self.assertIn(ctx_lam.exception.code, ("NORN-PY-AMBIGUOUS-CALLBACK", "NORN-PY-FORBIDDEN-CALL"))

    def test_ambiguous_multi_lambda_line_fails_closed(self) -> None:
        # Multiline outer lambda (whose closing line col < opening col) + inner lambda on line 1:
        # Full (line, col) span check MUST resolve `outer` to the outer AST Lambda (not inner!),
        # and then reject the nested lambda inside `outer`'s body.
        outer_multiline = (  # noqa: E731
            lambda s: (lambda y: y)(
                s.a
            )
        )
        _fname, outer_node = _find_callback_node(outer_multiline, None)
        assert isinstance(outer_node, ast.Lambda)
        self.assertGreater(outer_node.end_lineno or 0, outer_node.lineno)
        with self.assertRaises(NornTracingError) as ctx_outer:
            validate_callback_ast(outer_multiline)
        self.assertEqual(ctx_outer.exception.code, "NORN-PY-FORBIDDEN-CALL")

        # Inner lambda returned from a multiline outer lambda starting on the SAME line:
        # inner's span is contained in BOTH outer and inner candidates -> fails closed as ambiguous!
        inner_builder: Any = (  # noqa: E731
            lambda _s: (lambda y: y)
            if True
            else None
        )
        inner_fn = inner_builder(None)
        assert callable(inner_fn)
        with self.assertRaises(NornTracingError) as ctx_inner:
            validate_callback_ast(inner_fn)
        self.assertEqual(ctx_inner.exception.code, "NORN-PY-AMBIGUOUS-CALLBACK")

        # Two distinct sibling lambdas on one line disambiguate cleanly via full (line, col) spans,
        # while stripping bytecode positions fails closed with NORN-PY-AMBIGUOUS-CALLBACK.
        f1, f2 = (lambda s: s.a > 0, lambda s: s.a < 10)  # noqa: E731
        validate_callback_ast(f1)
        validate_callback_ast(f2)
        stripped_code = f1.__code__.replace(co_linetable=b"")
        f1_no_pos = types.FunctionType(stripped_code, f1.__globals__, f1.__name__)
        with self.assertRaises(NornTracingError) as ctx_no_pos:
            validate_callback_ast(f1_no_pos)
        self.assertEqual(ctx_no_pos.exception.code, "NORN-PY-AMBIGUOUS-CALLBACK")

    def test_runtime_expr_coercion_refusals_still_enforced(self) -> None:
        s = DynamicSym("self", ("hours", "name", "starts_at", "ends_at"), "hr.Shift")
        with self.assertRaises(NornTracingError) as c_bool:
            if s.hours > Decimal("0.00"):
                pass
        self.assertEqual(c_bool.exception.code, "NORN-PY-BOOL-COERCION")

        with self.assertRaises(NornTracingError) as c_len:
            len(s.name)
        self.assertEqual(c_len.exception.code, "NORN-PY-LEN-COERCION")

        with self.assertRaises(NornTracingError) as c_iter:
            iter(s.name)
        self.assertEqual(c_iter.exception.code, "NORN-PY-ITER-COERCION")

        with self.assertRaises(NornTracingError) as c_in:
            _ = "a" in s.name
        self.assertEqual(c_in.exception.code, "NORN-PY-CONTAINS-COERCION")

        with self.assertRaises(NornTracingError) as c_idx:
            range(s.hours)
        self.assertEqual(c_idx.exception.code, "NORN-PY-INDEX-COERCION")

        with self.assertRaises(NornFloatRefusalError):
            _ = s.hours > 8.5  # type: ignore[operator]

        with self.assertRaises(NornTracingError) as c_unk:
            _ = s.nonexistent_field
        self.assertEqual(c_unk.exception.code, "NORN-PY-UNKNOWN-FIELD")


# ------------------------------------------------------------------ 3. No Silent Drops / No Raw IR


class TestNoSilentDropsAndClosedIR(unittest.TestCase):
    def test_object_invariants_emitted_into_object_type_ir(self) -> None:
        class _Order(Concept, name="sales.TestOrder", title="Test order", version="0.1.0"):
            id: Field[str]
            total: Annotated[Field[Decimal], decimal_type(2)]

            @invariant("non_negative_total", message="total must be non-negative")
            def _non_neg(s: Sym[_Order]) -> Expr[bool]:
                return s.total >= Decimal("0.00")

            @invariant("large_order_warn", message="order unusually large", classification="quality")
            def _warn_large(s: Sym[_Order]) -> Expr[bool]:
                return s.total <= Decimal("100000.00")

        opts = EnvelopeOptions(scope={"orgId": "o", "environmentId": "dev"}, source_path="test.py")
        artifact = _Order.lower(opts)
        assert isinstance(artifact, dict)
        payload = artifact["payload"]
        assert isinstance(payload, dict)
        obj = payload["objectType"]
        assert isinstance(obj, dict)
        self.assertEqual(
            obj["invariants"],
            [
                {
                    "expression": "self.total >= 0.00",
                    "id": "non_negative_total",
                    "message": "total must be non-negative",
                },
                {
                    "classification": "quality",
                    "expression": "self.total <= 100000.00",
                    "id": "large_order_warn",
                    "message": "order unusually large",
                },
            ],
        )

    def test_interface_lifecycle_explicitly_rejected(self) -> None:
        class _St(StrEnum):
            A = "a"
            B = "b"

        class _BadIface(
            Concept,
            name="test.BadLifecycleIface",
            title="Bad interface",
            version="0.1.0",
            interface=True,
        ):
            status: Field[_St]
            state = lifecycle("status", _St, initial=(_St.A,), transitions=(transition("go", from_=(_St.A,), to=_St.B),))

        opts = EnvelopeOptions(scope={"orgId": "o", "environmentId": "dev"}, source_path="test.py")
        with self.assertRaises(NornTracingError) as ctx:
            _BadIface.lower(opts, globalns={"_St": _St})
        self.assertEqual(ctx.exception.code, "NORN-PY-INTERFACE-LIFECYCLE")

    def test_raw_expr_and_raw_ir_spec_and_duck_typed_zen_rejected(self) -> None:
        # Public Expr() constructor cannot accept arbitrary ZEN strings
        with self.assertRaises(NornTracingError) as ctx_expr:
            Expr()
        self.assertEqual(ctx_expr.exception.code, "NORN-PY-RAW-EXPR")

        # Subclassing Expr is forbidden
        with self.assertRaises(NornTracingError) as ctx_sub:
            class _FakeExpr(Expr[bool]):  # type: ignore[misc]
                pass
        self.assertEqual(ctx_sub.exception.code, "NORN-PY-RAW-EXPR")

        # Duck-typed object with `.zen` attribute is rejected in guard/invariant lowering
        fake_zen = types.SimpleNamespace(zen="1 == 1")
        sym = DynamicSym("self", ("x",), "test.Obj")
        bad_guard = lambda s: fake_zen  # noqa: E731
        with self.assertRaises(NornTracingError) as ctx_duck:
            trace_bool_callback(bad_guard, sym, context="test guard")  # type: ignore[arg-type]
        self.assertEqual(ctx_duck.exception.code, "NORN-PY-RAW-EXPR")

        # Raw PropertyTypeSpec dict smuggling is forbidden
        with self.assertRaises(NornTracingError) as ctx_spec:
            PropertyTypeSpec()
        self.assertEqual(ctx_spec.exception.code, "NORN-PY-RAW-IR")

        # Raw dict in MarkingDecl.mask is forbidden
        with self.assertRaises(NornTracingError) as ctx_mask:
            MarkingDecl(
                id="pii.x",
                label="X",
                severity="high",
                category="pii",
                mask={"kind": "redacted"},  # type: ignore[arg-type]
            )
        self.assertEqual(ctx_mask.exception.code, "NORN-PY-RAW-IR")

    def test_mask_spec_is_immutable_and_isolated_from_to_ir_mutation(self) -> None:
        m = mask_last_n(4)
        with self.assertRaises(AttributeError):
            m._inner = mask_redacted()._inner
        ir_copy = m.to_ir()
        ir_copy["kind"] = "tampered"
        ir_copy["count"] = 999
        self.assertEqual(m.to_ir(), {"count": 4, "kind": "last-n"})

    def test_link_unsupported_metadata_is_explicitly_rejected(self) -> None:
        opts = EnvelopeOptions(scope={"orgId": "o", "environmentId": "dev"}, source_path="test.py")

        class _Node(Concept, name="test.LinkNode", title="Node", version="0.1.0"):
            id: Field[str]

        # Interface link with `acyclic=True` MUST fail closed (never silently dropped)
        class _BadIfaceAcyclic(
            Concept,
            name="test.BadIfaceAcyclic",
            title="Bad iface",
            version="0.1.0",
            interface=True,
        ):
            parent: Annotated[Field[Link[_Node]], link(_Node, acyclic=True)]

        with self.assertRaises(NornTracingError) as c_acyc:
            _BadIfaceAcyclic.lower(opts, globalns={"_Node": _Node})
        self.assertEqual(c_acyc.exception.code, "NORN-PY-SCHEMA")

        # Interface link with `inverse=...` MUST fail closed
        class _BadIfaceInverse(
            Concept,
            name="test.BadIfaceInverse",
            title="Bad iface",
            version="0.1.0",
            interface=True,
        ):
            parent: Annotated[Field[Link[_Node]], link(_Node, inverse="children")]

        with self.assertRaises(NornTracingError) as c_inv:
            _BadIfaceInverse.lower(opts, globalns={"_Node": _Node})
        self.assertEqual(c_inv.exception.code, "NORN-PY-SCHEMA")

        # Link with property Marking MUST fail closed
        class _BadLinkMarking(Concept, name="test.BadLinkMarking", title="Bad link", version="0.1.0"):
            parent: Annotated[Field[Link[_Node]], link(_Node), Marking("pii.x")]

        with self.assertRaises(NornTracingError) as c_mark:
            _BadLinkMarking.lower(opts, globalns={"_Node": _Node})
        self.assertEqual(c_mark.exception.code, "NORN-PY-SCHEMA")

        # Link with default MUST fail closed
        class _BadLinkDefault(Concept, name="test.BadLinkDefault", title="Bad link", version="0.1.0"):
            parent: Annotated[Field[Link[_Node]], link(_Node)] = field_default("x")  # type: ignore[arg-type]

        with self.assertRaises(NornTracingError) as c_def:
            _BadLinkDefault.lower(opts, globalns={"_Node": _Node})
        self.assertEqual(c_def.exception.code, "NORN-PY-SCHEMA")

        # Link with FieldMeta(description=...) MUST fail closed (must pass description to link(...))
        class _BadLinkMeta(Concept, name="test.BadLinkMeta", title="Bad link", version="0.1.0"):
            parent: Annotated[Field[Link[_Node]], link(_Node), FieldMeta(description="ignored")]

        with self.assertRaises(NornTracingError) as c_meta:
            _BadLinkMeta.lower(opts, globalns={"_Node": _Node})
        self.assertEqual(c_meta.exception.code, "NORN-PY-SCHEMA")

        # Duplicate FieldMeta on a property (e.g. trailing empty FieldMeta()) MUST fail closed
        class _BadDupPropMeta(Concept, name="test.BadDupPropMeta", title="Bad dup meta", version="0.1.0"):
            name: Annotated[Field[str], FieldMeta(description="first", default="a"), FieldMeta()]

        with self.assertRaises(NornTracingError) as c_dup_prop:
            _BadDupPropMeta.lower(opts, globalns={"_Node": _Node})
        self.assertEqual(c_dup_prop.exception.code, "NORN-PY-SCHEMA")

        # Duplicate FieldMeta on a link MUST also fail closed
        class _BadDupLinkMeta(Concept, name="test.BadDupLinkMeta", title="Bad dup link", version="0.1.0"):
            parent: Annotated[Field[Link[_Node]], link(_Node), FieldMeta(description="x"), FieldMeta()]

        with self.assertRaises(NornTracingError) as c_dup_link:
            _BadDupLinkMeta.lower(opts, globalns={"_Node": _Node})
        self.assertEqual(c_dup_link.exception.code, "NORN-PY-SCHEMA")


# ------------------------------------------------------------------ 4. Real Determinism


class TestRealDeterminism(unittest.TestCase):
    def test_sandbox_subprocess_honors_pythonhashseed_zero(self) -> None:
        probe = 'print(hash("norn-determinism-probe"))'
        cmd = sandbox_subprocess_cmd(probe)
        self.assertNotIn("-I", cmd, "-I ignores PYTHONHASHSEED=0 in CPython 3.12+")
        env = sandbox_subprocess_env(NORN_PY_ROOT)
        h1 = subprocess.check_output(cmd, env=env, text=True).strip()
        h2 = subprocess.check_output(cmd, env=env, text=True).strip()
        self.assertEqual(h1, h2)

    def test_hr_pilot_deterministic_admissible_and_no_core_collision(self) -> None:
        result = verify_deterministic_elaboration(
            HR_PILOT,
            scope={"environmentId": "dev", "orgId": "org_demo"},
            source_path="tools/norn-py/examples/hr_pilot.py",
        )
        ids = [a["artifactId"] for a in result.artifacts if isinstance(a, dict)]
        self.assertNotIn("core.TimeInterval.interface", ids)
        self.assertEqual(
            ids,
            [
                "hr.privacy.markings",
                "hr_pilot.TimeSpan.interface",
                "hr.Employee.entity",
                "hr.LeaveRequest.entity",
                "hr.Shift.entity",
                "hr.shift.valid_window_and_hours.law",
            ],
        )

        # Verify unit admission dependency (`core.units@1.0.0`, kind="entity", required=True)
        # is pinned on every entity declaring UCUM units (LeaveRequest & Shift).
        by_id: dict[str, dict[str, Any]] = {
            str(a["artifactId"]): a for a in result.artifacts if isinstance(a, dict)
        }
        expected_units_pin = {
            "artifactId": "core.units",
            "kind": "entity",
            "required": True,
            "version": "1.0.0",
        }
        self.assertIn(expected_units_pin, by_id["hr.LeaveRequest.entity"]["dependencies"])
        self.assertIn(expected_units_pin, by_id["hr.Shift.entity"]["dependencies"])

        # Verify exact ZEN rendering (no spurious outer parens)
        ts_invariants = by_id["hr_pilot.TimeSpan.interface"]["payload"]["interface"]["invariants"]
        self.assertEqual(
            ts_invariants[0]["expression"],
            "timestamp(self.starts_at) < timestamp(self.ends_at)",
        )
        leave_guards = by_id["hr.LeaveRequest.entity"]["payload"]["objectType"]["lifecycles"][0]["transitions"][0]["guards"]
        self.assertEqual(leave_guards, ["self.days > 0.00"])

        # Confirm PII markings lowered cleanly onto Employee properties.
        props: dict[str, dict[str, Any]] = {
            str(p["id"]): p
            for p in by_id["hr.Employee.entity"]["payload"]["objectType"]["properties"]
        }
        self.assertEqual(props["gender"]["markings"], ["pii.demographic"])
        self.assertEqual(props["national_id"]["markings"], ["pii.gov_id"])
        self.assertEqual(props["base_salary"]["markings"], ["pii.compensation"])

        self.assertIn("Employee", result.source_map)


# ------------------------------------------------------------------ 5. Exact ZEN, Norn1 & TS/Python Parity


class TestCanonicalNorn1AndTsParity(unittest.TestCase):
    def test_rust_norn1_golden_pins_and_unicode_order_conformance(self) -> None:
        """Cross-verify Python Norn1 canonical bytes & hashes directly against Rust golden pins.

        Checks:
        1. Every Rust-generated Norn1 pin in `crates/norn-ir/tests/golden/law-pins.json` (all 22).
        2. NFD -> NFC + control-character normalization vector from `crates/norn-canonical/src/lib.rs`.
        3. UTF-16 (`canonical_json`, matching TS) vs Unicode code-point (`canonical_norn1_bytes`,
           matching Rust `norn-canonical`) ordering divergence between U+E000 and U+10000.
        """
        law_pins_path = REPO_ROOT / "crates" / "norn-ir" / "tests" / "golden" / "law-pins.json"
        pins: dict[str, str] = json.loads(law_pins_path.read_text(encoding="utf-8"))
        self.assertGreaterEqual(len(pins), 20)

        for ref, expected_pin in pins.items():
            rel_file, pointer = ref.split("#", 1)
            doc: Any = json.loads((REPO_ROOT / rel_file).read_text(encoding="utf-8"))
            cur: Any = doc
            for seg in pointer.lstrip("/").split("/"):
                cur = cur[int(seg)] if isinstance(cur, list) else cur[seg]
            actual_pin = f"sha256:{canonical_norn1_sha256(cur)}"
            self.assertEqual(actual_pin, expected_pin, f"Rust Norn1 golden pin mismatch for {ref}")

        # Exact NFD -> NFC + C0 control escape vector from `crates/norn-canonical/src/lib.rs:2468`
        rust_vec = {
            "z": 0,
            "e": 123,
            "decomposed": "e\u0301",
            "control": "\u0000\n\t",
            "nested": {"b": 1, "a": 0},
        }
        self.assertEqual(
            canonical_norn1_bytes(rust_vec),
            '{"control":"\\u0000\\n\\t","decomposed":"\u00e9","e":123,"nested":{"a":0,"b":1},"z":0}'.encode(),
        )

        # UTF-16 vs Unicode scalar value ordering: U+E000 (0xE000) vs U+10000 (surrogate 0xD800 0xDC00).
        # Rust Norn1 sorts by Unicode code point (U+E000 < U+10000);
        # TS `canonicalJson` sorts by UTF-16 code unit (0xD800 < 0xE000).
        pair = {"\U00010000": "supp", "\ue000": "bmp"}
        self.assertEqual(
            canonical_norn1_bytes(pair),
            '{"\ue000":"bmp","\U00010000":"supp"}'.encode(),
        )
        ts_pretty = canonical_json(pair)
        self.assertLess(ts_pretty.index("\U00010000"), ts_pretty.index("\ue000"))

    def test_norn1_compact_bytes_nfc_and_duplicate_rejection(self) -> None:
        # Zero whitespace, code-point sorted keys, NFC string normalization
        nfd = "e\u0301"  # decomposed é
        nfc = "\u00e9"   # composed é
        b1 = canonical_norn1_bytes({"b": 1, "a": nfd})
        b2 = canonical_norn1_bytes({"a": nfc, "b": 1})
        self.assertEqual(b1, b2)
        self.assertEqual(b1, f'{{"a":"{nfc}","b":1}}'.encode())
        self.assertEqual(canonical_norn1_sha256({"b": 1, "a": nfd}), canonical_norn1_sha256({"a": nfc, "b": 1}))

        # Duplicate keys after NFC normalization MUST be refused
        with self.assertRaises(NonJsonValueError):
            canonical_norn1_bytes({nfd: 1, nfc: 2})

    def test_exact_zen_parenthesization_and_temporal_combinators(self) -> None:
        class _Shift(Concept, name="test.ZenShift", title="Zen shift", version="0.1.0"):
            hours: Annotated[Field[Quantity[Literal["h"]]], decimal_type(2, unit=UnitRef("ucum", "h"))]
            starts_at: Annotated[Field[Timestamp], timestamp_type("utc")]
            ends_at: Annotated[Field[Timestamp], timestamp_type("utc")]
            start_date: Annotated[Field[Date], date_type()]
            end_date: Annotated[Field[Date], date_type()]

        s = _Shift.sym("self")
        self.assertEqual((s.hours > Decimal("0")).zen, "self.hours > 0")
        self.assertEqual(
            timestamp_before(s.starts_at, s.ends_at).zen,
            "timestamp(self.starts_at) < timestamp(self.ends_at)",
        )
        self.assertEqual(
            date_before(s.start_date, s.end_date).zen,
            "date(self.start_date) < date(self.end_date)",
        )
        # Matches tools/norn-ts/src/builder/expr.ts:406 exactly: dateBefore always wraps operands in date(...).
        self.assertEqual(
            date_before(parse_date(s.start_date), s.end_date).zen,
            "date(date(self.start_date)) < date(self.end_date)",
        )
        self.assertEqual(
            between(s.hours, Decimal("1.00"), Decimal("12.00")).zen,
            "(self.hours >= 1.00) and (self.hours <= 12.00)",
        )
        expr = all_of(
            intervals_overlap(s.starts_at, s.ends_at, s.starts_at, s.ends_at),
            interval_gap_seconds(s.starts_at, s.ends_at, s.starts_at, s.ends_at) >= Decimal("39600"),
        )
        self.assertEqual(
            expr.zen,
            "intervals_overlap(timestamp(self.starts_at), timestamp(self.ends_at), "
            "timestamp(self.starts_at), timestamp(self.ends_at)) and "
            "(interval_gap_seconds(timestamp(self.starts_at), timestamp(self.ends_at), "
            "timestamp(self.starts_at), timestamp(self.ends_at)) >= 39600)",
        )

    def test_byte_identical_parity_with_checked_in_core_goldens(self) -> None:
        """Exact byte-for-byte parity against checked-in core.Agent & core.TimeInterval artifacts."""
        with tempfile.TemporaryDirectory() as tmp:
            agent_py = Path(tmp) / "core_agent.py"
            agent_py.write_text(
                '''from __future__ import annotations

from enum import StrEnum
from typing import Annotated

from norn_py import Concept, Field, ref_type


class AgentKind(StrEnum):
    PERSON = "person"
    ORGANIZATION = "organization"
    SYSTEM = "system"


class Agent(
    Concept,
    name="core.Agent",
    title="Agent",
    version="0.1.0",
    interface=True,
    role="thing",
    description=(
        "An actor that holds resources, takes part in economic events and "
        "makes commitments: a person, an organisation or a system (REA agent)."
    ),
):
    display_name: Annotated[Field[str], ref_type("core.display_name")]
    kind: Field[AgentKind]
''',
                encoding="utf-8",
            )
            res_agent = verify_deterministic_elaboration(
                agent_py,
                scope={},
                source_path="registry/core/src/physics.ts",
                registry_package_path="registry/core",
            )
            expected_agent = CORE_AGENT_GOLDEN.read_text(encoding="utf-8")
            self.assertEqual(canonical_json(res_agent.artifacts[0]), expected_agent)
            self.assertEqual(
                canonical_norn1_sha256(res_agent.artifacts[0]),
                canonical_norn1_sha256(json.loads(expected_agent)),
            )

            ti_py = Path(tmp) / "core_time_interval.py"
            ti_py.write_text(
                '''from __future__ import annotations

from typing import Annotated

from norn_py import Concept, Expr, Field, Sym, Timestamp, invariant, ref_type, timestamp_before


class TimeInterval(
    Concept,
    name="core.TimeInterval",
    title="Time interval",
    version="0.1.0",
    interface=True,
    role="happening",
    description=(
        "A proper half-open temporal interval [starts_at, ends_at) in UTC "
        "with an optional canonical IANA timezone label."
    ),
):
    starts_at: Annotated[Field[Timestamp], ref_type("core.time.instant")]
    ends_at: Annotated[Field[Timestamp], ref_type("core.time.instant")]
    timezone: Annotated[Field[str | None], ref_type("core.time.iana_timezone")]

    @invariant("proper_interval", message="starts_at must be strictly before ends_at")
    def _proper_interval(s: Sym[TimeInterval]) -> Expr[bool]:
        return timestamp_before(s.starts_at, s.ends_at)
''',
                encoding="utf-8",
            )
            res_ti = verify_deterministic_elaboration(
                ti_py,
                scope={},
                source_path="registry/core/src/time.ts",
                registry_package_path="registry/core",
            )
            expected_ti = CORE_TIME_INTERVAL_GOLDEN.read_text(encoding="utf-8")
            self.assertEqual(canonical_json(res_ti.artifacts[0]), expected_ti)
            self.assertEqual(
                canonical_norn1_bytes(res_ti.artifacts[0]),
                canonical_norn1_bytes(json.loads(expected_ti)),
            )
            self.assertEqual(
                canonical_norn1_sha256(res_ti.artifacts[0]),
                canonical_norn1_sha256(json.loads(expected_ti)),
            )

    def test_same_pack_ts_vs_python_norn1_bytes_and_sha256_parity(self) -> None:
        """Paired entity+interface+lifecycle+invariant+marking pack compiled by tools/norn-ts vs tools/norn-py."""
        bun = shutil.which("bun")
        ts_installed = (NORN_TS_ROOT / "node_modules" / "typescript").exists()
        if bun is None or not ts_installed:
            self.skipTest("tools/norn-ts/node_modules not installed in this checkout")

        with tempfile.TemporaryDirectory() as tmp:
            tmp_dir = Path(tmp)
            ts_file = tmp_dir / "paired_pack.ts"
            ts_out = tmp_dir / "paired_pack.ts.json"
            py_file = tmp_dir / "paired_pack.py"

            ts_file.write_text(
                """import { Schema } from "effect"
import { Norn } from "@norn/authoring"

export const Privacy = Norn.markings({
  id: "pilot.privacy.markings",
  title: "Privacy catalog",
  version: "0.1.0",
  markings: [
    { id: "pii.contact", label: "Contact", severity: "moderate", category: "pii", mask: { kind: "redacted" } },
    { id: "pii.account", label: "Account", severity: "high", category: "pii", mask: { kind: "last-n", count: 4 } },
    { id: "pii.secret", label: "Secret", severity: "critical", category: "pii", mask: { kind: "null" } },
  ],
})

const WindowBounds = Schema.Struct({
  starts_at: Norn.timestamp("utc"),
  ends_at: Norn.timestamp("utc"),
  effective_from: Norn.date(),
  effective_to: Norn.date(),
})

export const TimeWindow = Norn.interfaceType({
  name: "pilot.TimeWindow",
  title: "Time window",
  version: "0.1.0",
  role: "happening",
  description: "Half-open UTC time window.",
  properties: Schema.Struct({
    starts_at: Norn.timestamp("utc"),
    ends_at: Norn.timestamp("utc"),
    effective_from: Norn.date(),
    effective_to: Norn.date(),
    timezone: Schema.NullOr(Schema.String),
  }),
  invariants: [
    {
      id: "proper_window",
      message: "starts_at must be strictly before ends_at",
      expression: Norn.expression(WindowBounds, (self) => Norn.timestampBefore(self.starts_at, self.ends_at)),
    },
    {
      id: "ordered_dates",
      message: "effective_from must precede effective_to",
      expression: Norn.expression(WindowBounds, (self) => Norn.dateBefore(self.effective_from, self.effective_to)),
    },
  ],
})

export const Worker = Norn.type({
  name: "pilot.Worker",
  title: "Worker",
  version: "0.1.0",
  role: "thing",
  properties: Schema.Struct({
    id: Schema.String,
    email: Schema.String,
  }),
  propertyMarkings: {
    email: ["pii.contact"],
  },
  identity: [{ name: "primary", properties: ["id"] }],
})

const BookingProperties = Schema.Struct({
  id: Schema.String,
  starts_at: Norn.timestamp("utc"),
  ends_at: Norn.timestamp("utc"),
  effective_from: Norn.date(),
  effective_to: Norn.date(),
  timezone: Schema.NullOr(Schema.String),
  hours: Norn.decimal(2, { unit: { system: "ucum", code: "h" } }),
  status: Schema.Literals(["draft", "submitted", "approved"]),
})

export const Booking = Norn.type({
  name: "pilot.Booking",
  title: "Booking",
  version: "0.1.0",
  role: "happening",
  implements: [TimeWindow],
  dependencies: [{ artifactId: "core.units", version: "1.0.0", kind: "entity" }],
  properties: BookingProperties,
  defaults: { status: "draft" },
  identity: [{ name: "primary", properties: ["id"] }],
  links: [
    {
      id: "worker",
      target: Worker,
      cardinality: { to: "one", from: "many", required: true },
    },
  ],
  lifecycles: [
    {
      property: "status",
      initial: ["draft"],
      terminal: ["approved"],
      transitions: [
        {
          id: "submit",
          from: ["draft"],
          to: "submitted",
          guards: [Norn.expression(BookingProperties, (self) => self.hours > 0)],
        },
        {
          id: "approve",
          from: ["submitted"],
          to: "approved",
        },
      ],
    },
  ],
})
""",
                encoding="utf-8",
            )

            py_file.write_text(
                """from __future__ import annotations

from decimal import Decimal
from enum import StrEnum
from typing import Annotated, Literal

from norn_py import (
    Concept,
    Date,
    DependencyPin,
    Expr,
    Field,
    KeySpec,
    Link,
    Marking,
    MarkingDecl,
    MarkingsCatalog,
    Quantity,
    Sym,
    Timestamp,
    UnitRef,
    date_before,
    date_type,
    decimal_type,
    field_default,
    invariant,
    lifecycle,
    link,
    mask_last_n,
    mask_null,
    mask_redacted,
    timestamp_before,
    timestamp_type,
    transition,
)

HOURS = UnitRef(system="ucum", code="h")
CORE_UNITS = DependencyPin("core.units", "1.0.0", kind="entity")

Privacy = MarkingsCatalog(
    id="pilot.privacy.markings",
    title="Privacy catalog",
    version="0.1.0",
    markings=(
        MarkingDecl(id="pii.contact", label="Contact", severity="moderate", category="pii", mask=mask_redacted()),
        MarkingDecl(id="pii.account", label="Account", severity="high", category="pii", mask=mask_last_n(4)),
        MarkingDecl(id="pii.secret", label="Secret", severity="critical", category="pii", mask=mask_null()),
    ),
)


class TimeWindow(
    Concept,
    name="pilot.TimeWindow",
    title="Time window",
    version="0.1.0",
    interface=True,
    role="happening",
    description="Half-open UTC time window.",
):
    starts_at: Annotated[Field[Timestamp], timestamp_type("utc")]
    ends_at: Annotated[Field[Timestamp], timestamp_type("utc")]
    effective_from: Annotated[Field[Date], date_type()]
    effective_to: Annotated[Field[Date], date_type()]
    timezone: Field[str | None]

    @invariant("proper_window", message="starts_at must be strictly before ends_at")
    def _proper_window(s: Sym[TimeWindow]) -> Expr[bool]:
        return timestamp_before(s.starts_at, s.ends_at)

    @invariant("ordered_dates", message="effective_from must precede effective_to")
    def _ordered_dates(s: Sym[TimeWindow]) -> Expr[bool]:
        return date_before(s.effective_from, s.effective_to)


class Worker(
    Concept,
    name="pilot.Worker",
    title="Worker",
    version="0.1.0",
    role="thing",
    identity=(KeySpec("primary", ("id",)),),
):
    id: Field[str]
    email: Annotated[Field[str], Marking("pii.contact")]


class BookingStatus(StrEnum):
    DRAFT = "draft"
    SUBMITTED = "submitted"
    APPROVED = "approved"


def _positive_hours(s: Sym[Booking]) -> Expr[bool]:
    return s.hours > Decimal("0")


class Booking(
    Concept,
    name="pilot.Booking",
    title="Booking",
    version="0.1.0",
    role="happening",
    implements=(TimeWindow,),
    identity=(KeySpec("primary", ("id",)),),
    dependencies=(CORE_UNITS,),
):
    id: Field[str]
    starts_at: Annotated[Field[Timestamp], timestamp_type("utc")]
    ends_at: Annotated[Field[Timestamp], timestamp_type("utc")]
    effective_from: Annotated[Field[Date], date_type()]
    effective_to: Annotated[Field[Date], date_type()]
    timezone: Field[str | None]
    hours: Annotated[Field[Quantity[Literal["h"]]], decimal_type(2, unit=HOURS)]
    status: Field[BookingStatus] = field_default(BookingStatus.DRAFT)
    worker: Annotated[Field[Link[Worker]], link(Worker, to="one", from_="many", required=True)]

    state = lifecycle(
        "status",
        BookingStatus,
        initial=(BookingStatus.DRAFT,),
        terminal=(BookingStatus.APPROVED,),
        transitions=(
            transition("submit", from_=(BookingStatus.DRAFT,), to=BookingStatus.SUBMITTED, guards=(_positive_hours,)),
            transition("approve", from_=(BookingStatus.SUBMITTED,), to=BookingStatus.APPROVED),
        ),
    )
""",
                encoding="utf-8",
            )

            proc = subprocess.run(
                [bun, "src/cli.ts", "compile", str(ts_file), "--org", "org_demo", "--env", "dev", "--out", str(ts_out)],
                cwd=NORN_TS_ROOT,
                capture_output=True,
                text=True,
                check=False,
            )
            self.assertEqual(proc.returncode, 0, proc.stderr or proc.stdout)

            # Compute TS `canonicalJson` golden-file text and its SHA-256 on the TypeScript side
            # using `tools/norn-ts/src/canonical.ts` + Node crypto (this checks exact TS `canonicalJson`
            # parity; authoritative Rust Norn1 conformance is verified against `law-pins.json` above).
            ts_hash_script = """
import { readFileSync } from "node:fs"
import { createHash } from "node:crypto"
import { canonicalJson } from "./src/canonical.ts"

const sha = (text: string) => createHash("sha256").update(Buffer.from(text, "utf8")).digest("hex")

const artifacts = JSON.parse(readFileSync(process.env.NORN_TS_OUT!, "utf8"))
const canonicalText = canonicalJson(artifacts)
const perArtifact = artifacts.map((a: unknown) => {
  const text = canonicalJson(a)
  return {
    canonicalText: text,
    canonicalSha256: sha(text),
  }
})
process.stdout.write(JSON.stringify({
  sourcePath: artifacts[0].provenance.sourcePath,
  count: artifacts.length,
  canonicalText,
  canonicalSha256: sha(canonicalText),
  perArtifact,
}))
"""
            ts_hash_proc = subprocess.run(
                [bun, "-e", ts_hash_script],
                cwd=NORN_TS_ROOT,
                env={**os.environ, "NORN_TS_OUT": str(ts_out)},
                capture_output=True,
                text=True,
                check=False,
            )
            self.assertEqual(ts_hash_proc.returncode, 0, ts_hash_proc.stderr or ts_hash_proc.stdout)
            ts_digest = json.loads(ts_hash_proc.stdout)

            py_result = verify_deterministic_elaboration(
                py_file,
                scope={"environmentId": "dev", "orgId": "org_demo"},
                source_path=str(ts_digest["sourcePath"]),
            )

            import hashlib

            self.assertEqual(len(py_result.artifacts), ts_digest["count"])
            self.assertEqual(py_result.canonical_text, ts_digest["canonicalText"])
            self.assertEqual(
                hashlib.sha256(py_result.canonical_text.encode()).hexdigest(),
                ts_digest["canonicalSha256"],
            )

            # Assert per-artifact TS `canonicalJson` byte & SHA-256 parity
            for py_art, ts_entry in zip(py_result.artifacts, ts_digest["perArtifact"], strict=True):
                py_text = canonical_json(py_art)
                self.assertEqual(py_text, ts_entry["canonicalText"])
                self.assertEqual(hashlib.sha256(py_text.encode()).hexdigest(), ts_entry["canonicalSha256"])

    def test_hr_pilot_admitted_by_rust_checker_wasm_when_available(self) -> None:
        """Run real `checker.checkArtifactSet` via Bun/WASM when norn-checker-wasm is installed."""
        bun = shutil.which("bun")
        if bun is None or not (NORN_TS_ROOT / "node_modules" / "typescript").exists():
            self.skipTest("bun or tools/norn-ts/node_modules unavailable")

        candidate_dirs = [
            os.environ.get("NORN_CHECKER_WASM_PKG"),
            str(Path.home() / ".cache" / "norn" / "runtime" / "checker-wasm"),
            str(NORN_TS_ROOT / "checker"),
        ]
        pkg_dir = next(
            (
                d
                for d in candidate_dirs
                if d
                and (Path(d) / "norn_checker_wasm.js").is_file()
                and (Path(d) / "norn_checker_wasm_bg.wasm").is_file()
            ),
            None,
        )
        if pkg_dir is None:
            self.skipTest(
                "norn-checker-wasm package not present locally "
                "(set NORN_CHECKER_WASM_PKG or install ~/.cache/norn/runtime/checker-wasm)"
            )

        py_result = verify_deterministic_elaboration(
            HR_PILOT,
            scope={"environmentId": "dev", "orgId": "org_demo"},
            source_path="tools/norn-py/examples/hr_pilot.py",
        )
        core_units = REPO_ROOT / "registry" / "core" / "artifacts" / "core.units.json"
        with tempfile.TemporaryDirectory() as tmp:
            cand_path = Path(tmp) / "hr_pilot_artifacts.json"
            cand_path.write_text(py_result.canonical_text, encoding="utf-8")
            check_script = """
import { readFileSync } from "node:fs"
import { loadChecker } from "./src/checker-abi.ts"
import { checkPack } from "./src/pack.ts"

const checker = await loadChecker(process.env.NORN_WASM_PKG_DIR!)
if (!checker) throw new Error("failed to load norn-checker-wasm")
const artifacts = JSON.parse(readFileSync(process.env.NORN_CAND_PATH!, "utf8"))
const coreUnits = JSON.parse(readFileSync(process.env.NORN_CORE_UNITS_PATH!, "utf8"))
const report = checkPack(checker, artifacts, [coreUnits])
process.stdout.write(JSON.stringify(report))
"""
            proc = subprocess.run(
                [bun, "-e", check_script],
                cwd=NORN_TS_ROOT,
                env={
                    **os.environ,
                    "NORN_CAND_PATH": str(cand_path),
                    "NORN_CORE_UNITS_PATH": str(core_units),
                    "NORN_WASM_PKG_DIR": pkg_dir,
                },
                capture_output=True,
                text=True,
                check=False,
            )
            self.assertEqual(proc.returncode, 0, proc.stderr or proc.stdout)
            report = json.loads(proc.stdout)
            self.assertTrue(report["admitted"], f"Rust checker refused hr_pilot.py: {report}")


# ------------------------------------------------------------------ 6. Real Static Typing & Consistency


class TestStaticTypingAndAnnotationConsistency(unittest.TestCase):
    def test_annotated_base_and_metadata_mismatches_rejected_at_elaboration(self) -> None:
        opts = EnvelopeOptions(scope={"orgId": "o", "environmentId": "dev"}, source_path="test.py")

        class _TargetA(Concept, name="test.TargetA", title="Target A", version="0.1.0"):
            id: Field[str]

        class _TargetB(Concept, name="test.TargetB", title="Target B", version="0.1.0"):
            id: Field[str]

        # 1. Annotated[str, UnitRef("ucum", "h")] -> unsupported/mismatched metadata
        class _BadUnitOnStr(Concept, name="test.BadUnitOnStr", title="Bad", version="0.1.0"):
            x: Annotated[str, UnitRef("ucum", "h")]

        with self.assertRaises(NornTracingError) as c1:
            _BadUnitOnStr.lower(opts, globalns=locals())
        self.assertEqual(c1.exception.code, "NORN-PY-TYPE-MISMATCH")

        # 2. Annotated[int, Decimal("2")] -> unsupported/mismatched metadata
        class _BadDecLiteralOnInt(Concept, name="test.BadDecLitOnInt", title="Bad", version="0.1.0"):
            x: Annotated[int, Decimal("2")]

        with self.assertRaises(NornTracingError) as c2:
            _BadDecLiteralOnInt.lower(opts, globalns=locals())
        self.assertEqual(c2.exception.code, "NORN-PY-TYPE-MISMATCH")

        # 3. Annotated[int, decimal_type(2)] -> int cannot carry decimal_type
        class _BadDecSpecOnInt(Concept, name="test.BadDecSpecOnInt", title="Bad", version="0.1.0"):
            x: Annotated[int, decimal_type(2)]

        with self.assertRaises(NornTracingError) as c3:
            _BadDecSpecOnInt.lower(opts, globalns=locals())
        self.assertEqual(c3.exception.code, "NORN-PY-TYPE-MISMATCH")

        # 4. Annotated[str, decimal_type(2, unit=HOURS)] -> str cannot carry decimal_type
        class _BadDecSpecOnStr(Concept, name="test.BadDecSpecOnStr", title="Bad", version="0.1.0"):
            x: Annotated[str, decimal_type(2, unit=UnitRef("ucum", "h"))]

        with self.assertRaises(NornTracingError) as c4:
            _BadDecSpecOnStr.lower(opts, globalns=locals())
        self.assertEqual(c4.exception.code, "NORN-PY-TYPE-MISMATCH")

        # 5. Quantity[Literal["h"]] paired with UnitRef("ucum", "d") -> unit mismatch
        class _BadUnitCodeMismatch(Concept, name="test.BadUnitCode", title="Bad", version="0.1.0"):
            x: Annotated[Field[Quantity[Literal["h"]]], decimal_type(2, unit=UnitRef("ucum", "d"))]

        with self.assertRaises(NornTracingError) as c5:
            _BadUnitCodeMismatch.lower(opts, globalns=locals())
        self.assertEqual(c5.exception.code, "NORN-PY-TYPE-MISMATCH")

        # 6. Link[TargetA] paired with link(TargetB) -> target mismatch
        class _BadLinkMismatch(Concept, name="test.BadLink", title="Bad", version="0.1.0"):
            ref: Annotated[Field[Link[_TargetA]], link(_TargetB)]

        with self.assertRaises(NornTracingError) as c6:
            _BadLinkMismatch.lower(opts, globalns={"_TargetA": _TargetA, "_TargetB": _TargetB})
        self.assertEqual(c6.exception.code, "NORN-PY-TYPE-MISMATCH")

        # 7. bool paired with integer() -> bool is NOT admitted as integer
        class _BadBoolAsInt(Concept, name="test.BadBoolInt", title="Bad", version="0.1.0"):
            flag: Annotated[bool, integer()]

        with self.assertRaises(NornTracingError) as c7:
            _BadBoolAsInt.lower(opts, globalns=locals())
        self.assertEqual(c7.exception.code, "NORN-PY-TYPE-MISMATCH")

        # 8. Money[Literal["EUR"]] paired with currency="USD" -> currency mismatch
        class _BadMoneyCurrency(Concept, name="test.BadMoneyCurrency", title="Bad", version="0.1.0"):
            amount: Annotated[Field[Money[Literal["EUR"]]], decimal_type(2, currency="USD")]

        with self.assertRaises(NornTracingError) as c8:
            _BadMoneyCurrency.lower(opts, globalns=locals())
        self.assertEqual(c8.exception.code, "NORN-PY-TYPE-MISMATCH")

        # 9. Money[str] (non-Literal tag) -> refused
        class _BadMoneyNonLit(Concept, name="test.BadMoneyNonLit", title="Bad", version="0.1.0"):
            amount: Annotated[Field[Money[str]], decimal_type(2, currency="currency")]

        with self.assertRaises(NornTracingError) as c9:
            _BadMoneyNonLit.lower(opts, globalns=locals())
        self.assertEqual(c9.exception.code, "NORN-PY-TYPE-MISMATCH")

        # 10. Rate[Literal["USD"], Literal["h"]] paired with per_unit=UnitRef("ucum", "d") -> unit mismatch
        class _BadRateUnit(Concept, name="test.BadRateUnit", title="Bad", version="0.1.0"):
            hourly_rate: Annotated[
                Field[Rate[Literal["USD"], Literal["h"]]],
                rate_type(2, currency="USD", per_unit=UnitRef("ucum", "d")),
            ]

        with self.assertRaises(NornTracingError) as c10:
            _BadRateUnit.lower(opts, globalns=locals())
        self.assertEqual(c10.exception.code, "NORN-PY-TYPE-MISMATCH")

        # 11. Rate[Literal["EUR"], Literal["h"]] paired with currency="USD" -> currency mismatch
        class _BadRateCurrency(Concept, name="test.BadRateCurrency", title="Bad", version="0.1.0"):
            hourly_rate: Annotated[
                Field[Rate[Literal["EUR"], Literal["h"]]],
                rate_type(2, currency="USD", per_unit=UnitRef("ucum", "h")),
            ]

        with self.assertRaises(NornTracingError) as c11:
            _BadRateCurrency.lower(opts, globalns=locals())
        self.assertEqual(c11.exception.code, "NORN-PY-TYPE-MISMATCH")

        # 12. Valid Money and Rate annotations elaborate cleanly
        class _GoodMoneyAndRate(Concept, name="test.GoodMoneyAndRate", title="Good", version="0.1.0"):
            currency: Field[str]
            salary: Annotated[Field[Money[Literal["currency"]]], decimal_type(2, currency="currency")]
            hourly_rate: Annotated[
                Field[Rate[Literal["currency"], Literal["h"]]],
                rate_type(2, currency="currency", per_unit=UnitRef("ucum", "h")),
            ]

        good_ir = _GoodMoneyAndRate.lower(opts, globalns=locals())
        self.assertIsInstance(good_ir, dict)

    def test_mypy_strict_positive_and_negative_typing_fixtures(self) -> None:
        mypy_bin = shutil.which("mypy")
        if mypy_bin is None:
            self.skipTest("mypy not installed on PATH")

        preamble = """from __future__ import annotations

from decimal import Decimal
from enum import StrEnum
from typing import Annotated, Literal

from norn_py import (
    Concept,
    Expr,
    Field,
    Link,
    Money,
    Quantity,
    Sym,
    Timestamp,
    UnitRef,
    all_of,
    decimal_type,
    field_default,
    link,
    timestamp_before,
    timestamp_type,
)

HOURS = UnitRef(system="ucum", code="h")
DAYS = UnitRef(system="ucum", code="d")


class Status(StrEnum):
    DRAFT = "draft"
    APPROVED = "approved"


class OtherStatus(StrEnum):
    OPEN = "open"


class Employee(Concept, name="t.Employee", title="Employee", version="0.1.0"):
    id: Field[str]


class Department(Concept, name="t.Department", title="Department", version="0.1.0"):
    id: Field[str]


class Record(Concept, name="t.Record", title="Record", version="0.1.0"):
    id: Field[str]
    count: Field[int]
    starts_at: Annotated[Field[Timestamp], timestamp_type("utc")]
    ends_at: Annotated[Field[Timestamp], timestamp_type("utc")]
    hours: Annotated[Field[Quantity[Literal["h"]]], decimal_type(2, unit=HOURS)]
    days: Annotated[Field[Quantity[Literal["d"]]], decimal_type(2, unit=DAYS)]
    pay: Annotated[Field[Money[Literal["currency"]]], decimal_type(2, currency="currency")]
    status: Field[Status] = field_default(Status.DRAFT)
    emp: Annotated[Field[Link[Employee]], link(Employee)]
    other_emp: Annotated[Field[Link[Employee]], link(Employee)]
    dept: Annotated[Field[Link[Department]], link(Department)]
"""

        positive_body = """
def valid_predicates(s: Sym[Record]) -> Expr[bool]:
    return all_of(
        s.id.eq("r1"),
        s.count.eq(3),
        s.count > 0,
        timestamp_before(s.starts_at, s.ends_at),
        s.hours > Decimal("0.00"),
        (s.hours + s.hours).eq(Decimal("8.00")),
        s.pay >= Decimal("100.00"),
        s.status.eq(Status.DRAFT),
        s.status.ne(Status.APPROVED),
        s.emp.eq(s.other_emp),
        s.emp.is_not_null(),
    )
"""

        negative_cases: list[tuple[str, str]] = [
            ("unit_compare_mismatch", "def bad(s: Sym[Record]) -> Expr[bool]:\n    return s.hours > s.days\n"),
            (
                "unit_add_mismatch",
                "def bad(s: Sym[Record]) -> Expr[bool]:\n    return (s.hours + s.days).eq(Decimal('1'))\n",
            ),
            ("unit_eq_mismatch", "def bad(s: Sym[Record]) -> Expr[bool]:\n    return s.hours.eq(s.days)\n"),
            ("float_comparison", "def bad(s: Sym[Record]) -> Expr[bool]:\n    return s.hours > 8.5\n"),
            ("timestamp_vs_str", "def bad(s: Sym[Record]) -> Expr[bool]:\n    return timestamp_before(s.starts_at, s.id)\n"),
            ("timestamp_raw_lt", "def bad(s: Sym[Record]) -> Expr[bool]:\n    return s.starts_at < s.ends_at\n"),
            ("state_bare_str", "def bad(s: Sym[Record]) -> Expr[bool]:\n    return s.status.eq('draft')\n"),
            ("state_other_enum", "def bad(s: Sym[Record]) -> Expr[bool]:\n    return s.status.eq(OtherStatus.OPEN)\n"),
            ("link_wrong_target", "def bad(s: Sym[Record]) -> Expr[bool]:\n    return s.emp.eq(s.dept)\n"),
            ("link_bare_str", "def bad(s: Sym[Record]) -> Expr[bool]:\n    return s.emp.eq('emp_1')\n"),
            ("link_ordered_gt", "def bad(s: Sym[Record]) -> Expr[bool]:\n    return s.emp > 0\n"),
            ("unknown_field", "def bad(s: Sym[Record]) -> Expr[bool]:\n    return s.missing_field.eq('x')\n"),
        ]

        with tempfile.TemporaryDirectory() as tmp:
            tmp_dir = Path(tmp)
            env = {"MYPYPATH": str(NORN_PY_ROOT), "PATH": str(Path(mypy_bin).parent)}

            pos_file = tmp_dir / "positive.py"
            pos_file.write_text(preamble + positive_body, encoding="utf-8")
            pos_proc = subprocess.run(
                [mypy_bin, "--strict", "--python-version", "3.12", str(pos_file)],
                capture_output=True,
                text=True,
                env=env,
                check=False,
            )
            self.assertEqual(
                pos_proc.returncode,
                0,
                f"Positive typing fixture failed mypy:\n{pos_proc.stdout}\n{pos_proc.stderr}",
            )

            for name, snippet in negative_cases:
                with self.subTest(negative_case=name):
                    neg_file = tmp_dir / f"neg_{name}.py"
                    neg_file.write_text(preamble + "\n" + snippet, encoding="utf-8")
                    neg_proc = subprocess.run(
                        [mypy_bin, "--strict", "--python-version", "3.12", str(neg_file)],
                        capture_output=True,
                        text=True,
                        env=env,
                        check=False,
                    )
                    self.assertNotEqual(
                        neg_proc.returncode,
                        0,
                        f"Negative typing fixture {name!r} unexpectedly PASSED mypy --strict!",
                    )


# ------------------------------------------------------------------ Spike Prototype Tests


class TestPackAndDeploymentPrototypes(unittest.TestCase):
    def test_pack_composition_and_deployment_spec(self) -> None:
        class _Person(Concept, name="hr.TestPerson", title="Test person", version="0.1.0"):
            id: Field[str]

        class _EgyptProfile(Concept, name="loc_eg.TestProfile", title="Egypt profile", version="0.1.0"):
            id: Field[str]

        hr_pack = PackManifest(
            id="norn.pack.hr",
            version="0.1.0",
            category="domain",
            title="HR Pack",
            entities=(_Person,),
            functions=(
                fn_ref(
                    id="norn.solver.rostering",
                    version="0.1.0",
                    input_type="SolveRequest",
                    output_type="SolveResponse",
                ),
            ),
        )
        loc_eg = PackManifest(
            id="norn.pack.loc-eg",
            version="0.1.0",
            category="localization",
            title="Egypt Localization",
            requires=("norn.pack.hr",),
            entities=(_EgyptProfile,),
        )
        composed = compose_packs(hr_pack, loc_eg)
        self.assertEqual(composed["entities"], ["hr.TestPerson", "loc_eg.TestProfile"])
        self.assertEqual(composed["functions"], ["norn.solver.rostering@0.1.0"])

        dep = DeploymentSpec(
            id="dep_demo_cairo",
            org_id="org_demo",
            environment_id="prod",
            packs=(hr_pack, loc_eg),
            worker_endpoints={"norn.solver.rostering@0.1.0": "https://solver.example.internal/solve"},
            secret_refs=(SecretRef(name="WHATSAPP_VERIFY_TOKEN"),),
            initial_settings={"weekly_overtime_budget_egp": Decimal("1500.00"), "avoid_split_shifts": True},
        )
        wire = dep.to_ir()
        self.assertEqual(wire["initialSettings"], {"avoid_split_shifts": True, "weekly_overtime_budget_egp": "1500.00"})
        self.assertTrue(canonical_json(wire))

    def test_pack_missing_dep_and_raw_secret_refused(self) -> None:
        orphan = PackManifest(
            id="norn.pack.loc-eg",
            version="0.1.0",
            category="localization",
            title="Orphan Egypt Pack",
            requires=("norn.pack.hr",),
        )
        with self.assertRaises(NornTracingError) as ctx:
            compose_packs(orphan)
        self.assertEqual(ctx.exception.code, "NORN-PY-PACK-MISSING-DEP")

        base = PackManifest(id="norn.pack.hr", version="0.1.0", category="domain", title="HR")
        with self.assertRaises(NornTracingError) as sec_ctx:
            DeploymentSpec(
                id="dep_bad",
                org_id="org_demo",
                environment_id="dev",
                packs=(base,),
                initial_settings={"whatsapp_api_token": "raw-secret-never-allowed"},
            )
        self.assertEqual(sec_ctx.exception.code, "NORN-PY-RAW-SECRET")

        with self.assertRaises(NornFloatRefusalError):
            DeploymentSpec(
                id="dep_float",
                org_id="org_demo",
                environment_id="dev",
                packs=(base,),
                initial_settings={"budget": 12.5},  # type: ignore[dict-item]
            )


if __name__ == "__main__":
    unittest.main()
