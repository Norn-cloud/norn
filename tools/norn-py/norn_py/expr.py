"""Max-strict phantom-typed symbolic tracing expressions for Norn (`norn-expr` / ZEN).

Every expression node is immutable data with zero surviving callbacks.
Soundness guardrails (Astra + Fable rulings, 2026-11-23):
- `Expr.__bool__`, `__len__`, `__iter__`, `__contains__`, `__index__` ALWAYS raise
  `NornTracingError` with an actionable fix so Python control flow can never silently
  collapse symbolic branches.
- Bare `float` is refused everywhere (`NornFloatRefusalError`).
- Zero `unsafeZen` escape hatch.
"""

from __future__ import annotations

import inspect
import re
from collections.abc import Sequence
from dataclasses import dataclass
from decimal import Decimal
from typing import Any, NoReturn, TypeVar

from .canonical import NornFloatRefusalError, SourceSpan

T = TypeVar("T")
U = TypeVar("U")
S = TypeVar("S")

_IDENT_RE = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*$")
_CONTROL_OR_ESCAPE_RE = re.compile(r"[\\\n\r\x00-\x1f]")


class NornTracingError(TypeError):
    """Raised when authored Python attempts a non-declarative or unsound operation."""

    def __init__(self, code: str, message: str, span: SourceSpan | None = None) -> None:
        prefix = f"{span.format()}: " if span is not None else ""
        super().__init__(f"{prefix}[{code}] {message}")
        self.code = code
        self.span = span


def capture_caller_span(skip: int = 2) -> SourceSpan | None:
    """Capture caller `(file, line)` for diagnostics when AST spans are unavailable."""
    frame = inspect.currentframe()
    try:
        cur = frame
        for _ in range(skip):
            if cur is None:
                return None
            cur = cur.f_back
        if cur is None:
            return None
        return SourceSpan(file=cur.f_code.co_filename, line=cur.f_lineno)
    finally:
        del frame


# ------------------------------------------------------------------ phantom type tags


class BoolTag:
    """Phantom tag for Boolean `norn-expr` expressions."""


class StrTag:
    """Phantom tag for Text `norn-expr` expressions."""


class IntTag:
    """Phantom tag for exact Integer `norn-expr` expressions."""


class DecimalTag[U, S]:
    """Phantom tag for exact Decimal `norn-expr` expressions with unit `U` and scale `S`."""


class DateTag:
    """Phantom tag for ISO-8601 calendar Date expressions."""


class TimestampTag:
    """Phantom tag for UTC RFC-3339 Timestamp expressions."""


class DurationTag:
    """Phantom tag for exact Duration expressions."""


# ------------------------------------------------------------------ ZEN rendering


def render_zen_string(value: str, span: SourceSpan | None = None) -> str:
    """Render a ZEN string literal (matches `zenString` in `tools/norn-ts/src/builder/expr.ts`)."""
    if _CONTROL_OR_ESCAPE_RE.search(value) or ("'" in value and '"' in value):
        raise NornTracingError(
            "NORN-PY-EXPRESSION",
            f"string literal {value!r} contains characters not admitted in norn-expr v1",
            span,
        )
    return f'"{value}"' if "'" in value else f"'{value}'"


def _check_ident(name: str, span: SourceSpan | None = None) -> str:
    if not _IDENT_RE.match(name):
        raise NornTracingError("NORN-PY-EXPRESSION", f"invalid identifier {name!r}", span)
    return name


def _literal_zen(value: str | int | bool | Decimal | None, span: SourceSpan | None = None) -> str:
    if isinstance(value, bool):
        return "true" if value else "false"
    if value is None:
        return "null"
    if isinstance(value, float):
        raise NornFloatRefusalError(value, context="expression literal")
    if isinstance(value, int):
        return str(value)
    if isinstance(value, Decimal):
        if not value.is_finite():
            raise NornTracingError("NORN-PY-EXPRESSION", f"non-finite Decimal {value!r}", span)
        return format(value, "f")
    if isinstance(value, str):
        return render_zen_string(value, span)
    raise NornTracingError(
        "NORN-PY-EXPRESSION",
        f"unsupported literal type {type(value).__name__}; use int, Decimal, str, bool, or None",
        span,
    )


# ------------------------------------------------------------------ Expr AST


@dataclass(frozen=True, slots=True)
class Expr[T]:
    """Immutable symbolic expression node lowered to canonical `norn-expr` (ZEN) text."""

    zen: str
    kind: str
    span: SourceSpan | None = None

    # --- Hard Astra soundness refusals: never let Python silently collapse symbolic branches ---

    def __bool__(self) -> NoReturn:
        raise NornTracingError(
            "NORN-PY-BOOL-COERCION",
            "Cannot evaluate symbolic Expr as a Python bool (this happens in `if expr:`, "
            "`a and b`, `a or b`, `not a`, or chained `0 < x < 10`). "
            "Use `all_of(a, b)`, `any_of(a, b)`, `not_(a)`, `when(cond, yes, no)`, "
            "`between(x, lo, hi)`, or parenthesized `(a) & (b)` instead.",
            self.span,
        )

    def __len__(self) -> NoReturn:
        raise NornTracingError(
            "NORN-PY-LEN-COERCION",
            "Cannot call Python `len()` on a symbolic Expr. Use `norn_len(expr)` instead.",
            self.span,
        )

    def __iter__(self) -> NoReturn:
        raise NornTracingError(
            "NORN-PY-ITER-COERCION",
            "Cannot iterate over a symbolic Expr in Python. Use bounded declarative combinators instead.",
            self.span,
        )

    def __contains__(self, _item: object) -> NoReturn:
        raise NornTracingError(
            "NORN-PY-CONTAINS-COERCION",
            "Cannot use Python `in` on a symbolic Expr (it coerces to bool). Use `contains(a, b)` instead.",
            self.span,
        )

    def __index__(self) -> NoReturn:
        raise NornTracingError(
            "NORN-PY-INDEX-COERCION",
            "Cannot use a symbolic Expr as a Python integer index or `range()` bound.",
            self.span,
        )

    # --- Typed comparisons & operators (return new Expr nodes, never Python bool) ---

    def eq(self, other: Expr[T] | T) -> Expr[BoolTag]:
        return _compare("==", self, other)

    def ne(self, other: Expr[T] | T) -> Expr[BoolTag]:
        return _compare("!=", self, other)

    def __eq__(self, other: object) -> Expr[BoolTag]:  # type: ignore[override]
        return _compare("==", self, other)

    def __ne__(self, other: object) -> Expr[BoolTag]:  # type: ignore[override]
        return _compare("!=", self, other)

    def __lt__(self, other: Expr[T] | int | Decimal) -> Expr[BoolTag]:
        return _compare("<", self, other)

    def __le__(self, other: Expr[T] | int | Decimal) -> Expr[BoolTag]:
        return _compare("<=", self, other)

    def __gt__(self, other: Expr[T] | int | Decimal) -> Expr[BoolTag]:
        return _compare(">", self, other)

    def __ge__(self, other: Expr[T] | int | Decimal) -> Expr[BoolTag]:
        return _compare(">=", self, other)

    def __add__(self, other: Expr[T] | int | Decimal) -> Expr[T]:
        return _arith("+", self, other)

    def __sub__(self, other: Expr[T] | int | Decimal) -> Expr[T]:
        return _arith("-", self, other)

    def __mul__(self, other: Expr[T] | int | Decimal) -> Expr[T]:
        return _arith("*", self, other)

    def __truediv__(self, other: Expr[T] | int | Decimal) -> Expr[T]:
        return _arith("/", self, other)

    def __and__(self: Expr[BoolTag], other: Expr[BoolTag]) -> Expr[BoolTag]:
        return all_of(self, other)

    def __or__(self: Expr[BoolTag], other: Expr[BoolTag]) -> Expr[BoolTag]:
        return any_of(self, other)

    def __invert__(self: Expr[BoolTag]) -> Expr[BoolTag]:
        return not_(self)

    def is_null(self) -> Expr[BoolTag]:
        return Expr(f"({self.zen} == null)", "compare", capture_caller_span())

    def is_not_null(self) -> Expr[BoolTag]:
        return Expr(f"({self.zen} != null)", "compare", capture_caller_span())


def _coerce_operand(value: object, span: SourceSpan | None) -> str:
    if isinstance(value, Expr):
        return value.zen
    if isinstance(value, float):
        raise NornFloatRefusalError(value, context="expression operand")
    if isinstance(value, (str, int, bool, Decimal)) or value is None:
        return _literal_zen(value, span)
    raise NornTracingError(
        "NORN-PY-EXPRESSION",
        f"unsupported expression operand of type {type(value).__name__}",
        span,
    )


def _compare(op: str, left: Expr[Any], right: object) -> Expr[BoolTag]:
    span = capture_caller_span(skip=3)
    right_zen = _coerce_operand(right, span)
    return Expr(f"({left.zen} {op} {right_zen})", "compare", span)


def _arith[T](op: str, left: Expr[T], right: object) -> Expr[T]:
    span = capture_caller_span(skip=3)
    right_zen = _coerce_operand(right, span)
    return Expr(f"({left.zen} {op} {right_zen})", "binary", span)


# ------------------------------------------------------------------ Friendly combinators


def field_ref(root: str, *path: str, span: SourceSpan | None = None) -> Expr[object]:
    """Reference `root.a.b` in ZEN expression scope."""
    checked = [_check_ident(root, span), *(_check_ident(seg, span) for seg in path)]
    return Expr(".".join(checked), "ref", span or capture_caller_span())


def lit(value: str | int | bool | Decimal | None) -> Expr[object]:
    """Explicit exact literal node (refuses binary float)."""
    span = capture_caller_span()
    return Expr(_literal_zen(value, span), "literal", span)


def all_of(*conditions: Expr[BoolTag]) -> Expr[BoolTag]:
    """Conjoin one or more Boolean expressions with ZEN `and`."""
    span = capture_caller_span()
    if not conditions:
        raise NornTracingError("NORN-PY-EXPRESSION", "all_of() requires at least one condition", span)
    for c in conditions:
        if not isinstance(c, Expr):
            raise NornTracingError("NORN-PY-EXPRESSION", "all_of() arguments must be symbolic Expr[BoolTag]", span)
    if len(conditions) == 1:
        return conditions[0]
    joined = " and ".join(c.zen for c in conditions)
    return Expr(f"({joined})", "logical", span)


def any_of(*conditions: Expr[BoolTag]) -> Expr[BoolTag]:
    """Disjoin one or more Boolean expressions with ZEN `or`."""
    span = capture_caller_span()
    if not conditions:
        raise NornTracingError("NORN-PY-EXPRESSION", "any_of() requires at least one condition", span)
    for c in conditions:
        if not isinstance(c, Expr):
            raise NornTracingError("NORN-PY-EXPRESSION", "any_of() arguments must be symbolic Expr[BoolTag]", span)
    if len(conditions) == 1:
        return conditions[0]
    joined = " or ".join(c.zen for c in conditions)
    return Expr(f"({joined})", "logical", span)


def not_(condition: Expr[BoolTag]) -> Expr[BoolTag]:
    """Negate a Boolean expression with ZEN `not`."""
    span = capture_caller_span()
    if not isinstance(condition, Expr):
        raise NornTracingError("NORN-PY-EXPRESSION", "not_() argument must be a symbolic Expr[BoolTag]", span)
    return Expr(f"(not {condition.zen})", "not", span)


def when[T](condition: Expr[BoolTag], yes: Expr[T], no: Expr[T]) -> Expr[T]:
    """Declarative conditional expression `cond ? yes : no` (replaces Python `if`)."""
    span = capture_caller_span()
    if not isinstance(condition, Expr):
        raise NornTracingError("NORN-PY-EXPRESSION", "when() condition must be a symbolic Expr[BoolTag]", span)
    return Expr(f"({condition.zen} ? {_coerce_operand(yes, span)} : {_coerce_operand(no, span)})", "conditional", span)


def between[T](
    value: Expr[T],
    low: Expr[T] | int | Decimal,
    high: Expr[T] | int | Decimal,
    *,
    inclusive: bool = True,
) -> Expr[BoolTag]:
    """Declarative range check replacing Python chained comparisons (`lo <= x <= hi`)."""
    span = capture_caller_span()
    lo_zen = _coerce_operand(low, span)
    hi_zen = _coerce_operand(high, span)
    op = "<=" if inclusive else "<"
    return Expr(f"(({value.zen} {op.replace('<', '>')} {lo_zen}) and ({value.zen} {op} {hi_zen}))", "logical", span)


def norn_len(value: Expr[StrTag]) -> Expr[IntTag]:
    span = capture_caller_span()
    return Expr(f"len({value.zen})", "call", span)


def contains(haystack: Expr[StrTag], needle: Expr[StrTag] | str) -> Expr[BoolTag]:
    span = capture_caller_span()
    return Expr(f"contains({haystack.zen}, {_coerce_operand(needle, span)})", "call", span)


# ------------------------------------------------------------------ Temporal builtins (norn-zen/6 & norn-zen/7)


def parse_date(value: Expr[DateTag] | Expr[StrTag] | str) -> Expr[DateTag]:
    span = capture_caller_span()
    return Expr(f"date({_coerce_operand(value, span)})", "call", span)


def parse_timestamp(value: Expr[TimestampTag] | Expr[StrTag] | str) -> Expr[TimestampTag]:
    span = capture_caller_span()
    return Expr(f"timestamp({_coerce_operand(value, span)})", "call", span)


def date_before(left: Expr[DateTag], right: Expr[DateTag]) -> Expr[BoolTag]:
    span = capture_caller_span()
    return Expr(f"(date({left.zen}) < date({right.zen}))", "compare", span)


def timestamp_before(left: Expr[TimestampTag], right: Expr[TimestampTag]) -> Expr[BoolTag]:
    span = capture_caller_span()
    return Expr(f"(timestamp({left.zen}) < timestamp({right.zen}))", "compare", span)


def timestamp_at_or_before(left: Expr[TimestampTag], right: Expr[TimestampTag]) -> Expr[BoolTag]:
    span = capture_caller_span()
    return Expr(f"(timestamp({left.zen}) <= timestamp({right.zen}))", "compare", span)


def days_between(start: Expr[DateTag], end: Expr[DateTag]) -> Expr[IntTag]:
    span = capture_caller_span()
    return Expr(f"days_between(date({start.zen}), date({end.zen}))", "call", span)


def add_days(start: Expr[DateTag], days: Expr[IntTag] | int) -> Expr[DateTag]:
    span = capture_caller_span()
    return Expr(f"add_days(date({start.zen}), {_coerce_operand(days, span)})", "call", span)


def seconds_between(start: Expr[TimestampTag], end: Expr[TimestampTag]) -> Expr[DecimalTag[object, object]]:
    span = capture_caller_span()
    return Expr(f"seconds_between(timestamp({start.zen}), timestamp({end.zen}))", "call", span)


def add_seconds(start: Expr[TimestampTag], seconds: Expr[IntTag] | int | Decimal) -> Expr[TimestampTag]:
    span = capture_caller_span()
    return Expr(f"add_seconds(timestamp({start.zen}), {_coerce_operand(seconds, span)})", "call", span)


def intervals_overlap(
    a_start: Expr[TimestampTag],
    a_end: Expr[TimestampTag],
    b_start: Expr[TimestampTag],
    b_end: Expr[TimestampTag],
) -> Expr[BoolTag]:
    """norn-zen/7 fail-closed half-open interval overlap check."""
    span = capture_caller_span()
    return Expr(
        f"intervals_overlap(timestamp({a_start.zen}), timestamp({a_end.zen}), "
        f"timestamp({b_start.zen}), timestamp({b_end.zen}))",
        "call",
        span,
    )


def interval_gap_seconds(
    a_start: Expr[TimestampTag],
    a_end: Expr[TimestampTag],
    b_start: Expr[TimestampTag],
    b_end: Expr[TimestampTag],
) -> Expr[DecimalTag[object, object]]:
    """norn-zen/7 fail-closed gap in exact Decimal seconds between two half-open intervals."""
    span = capture_caller_span()
    return Expr(
        f"interval_gap_seconds(timestamp({a_start.zen}), timestamp({a_end.zen}), "
        f"timestamp({b_start.zen}), timestamp({b_end.zen}))",
        "call",
        span,
    )


def interval_intersection_seconds(
    a_start: Expr[TimestampTag],
    a_end: Expr[TimestampTag],
    b_start: Expr[TimestampTag],
    b_end: Expr[TimestampTag],
) -> Expr[DecimalTag[object, object]]:
    """norn-zen/7 fail-closed overlap duration in exact Decimal seconds."""
    span = capture_caller_span()
    return Expr(
        f"interval_intersection_seconds(timestamp({a_start.zen}), timestamp({a_end.zen}), "
        f"timestamp({b_start.zen}), timestamp({b_end.zen}))",
        "call",
        span,
    )


# ------------------------------------------------------------------ Symbolic Record Proxy


class Sym[T]:
    """Symbolic record proxy passed into traced invariant/law/guard functions.

    Validates every field access against the declared property/link set so typos fail
    immediately at tracing time with exact source spans.
    """

    def __init__(self, root: str, allowed_fields: Sequence[str], concept_name: str) -> None:
        object.__setattr__(self, "_root", root)
        object.__setattr__(self, "_allowed", frozenset(allowed_fields))
        object.__setattr__(self, "_concept", concept_name)

    def __getattr__(self, name: str) -> Expr[object]:
        allowed: frozenset[str] = object.__getattribute__(self, "_allowed")
        root: str = object.__getattribute__(self, "_root")
        concept: str = object.__getattribute__(self, "_concept")
        span = capture_caller_span(skip=2)
        if name not in allowed:
            raise NornTracingError(
                "NORN-PY-UNKNOWN-FIELD",
                f"{concept} has no declared property or link {name!r} (declared: {sorted(allowed)})",
                span,
            )
        return field_ref(root, name, span=span)

    def __bool__(self) -> NoReturn:
        raise NornTracingError(
            "NORN-PY-BOOL-COERCION",
            "Cannot evaluate symbolic record proxy as a Python bool.",
            capture_caller_span(),
        )
