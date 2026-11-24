"""Typed symbolic-tracing expressions for Norn (`norn-expr` / ZEN).

Every expression node is backed by a closed immutable AST with zero surviving callbacks
and zero raw-ZEN construction hatches.

Soundness guardrails:
- `Expr` cannot be constructed directly from arbitrary strings or subclassed.
- Pre-tracing AST validation (`validate_callback_ast`) inspects every authoring callback
  BEFORE execution and rejects Python `if`/`if`-expr, `and`/`or`/`not`, `is`/`in`,
  chained comparisons, loops, comprehensions, nested lambdas, and calls outside the
  closed expression operator allowlist.
- `Expr.__bool__`, `__len__`, `__iter__`, `__contains__`, `__index__` ALWAYS raise
  `NornTracingError` with actionable fixes.
- Bare `float` is refused everywhere (`NornFloatRefusalError`).
- Structured ZEN rendering matches `tools/norn-ts/src/builder/expr.ts` parenthesization
  exactly (no spurious outer parentheses around top-level expressions).
"""

from __future__ import annotations

import ast
import inspect
import linecache
import re
from collections.abc import Callable, Sequence
from dataclasses import dataclass
from decimal import Decimal
from pathlib import Path
from typing import Any, Literal, NoReturn, final, overload

from .canonical import OMIT, NornFloatRefusalError, SourceSpan

_IDENT_RE = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*$")
_CONTROL_OR_ESCAPE_RE = re.compile(r"[\\\n\r\x00-\x1f]")

_EXPR_TOKEN = object()


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


# ------------------------------------------------------------------ Concrete Domain Value Types


class Timestamp:
    """Marker type for UTC RFC-3339 Timestamp expressions (`Expr[Timestamp]`)."""


class Date:
    """Marker type for ISO-8601 calendar Date expressions (`Expr[Date]`)."""


class Duration:
    """Marker type for exact Duration expressions (`Expr[Duration]`)."""


class Quantity[U]:
    """Marker type for exact Decimal quantities tagged with unit `U` (e.g. `Quantity[Literal["h"]]`)."""


class Money[C]:
    """Marker type for exact Monetary amounts bound to currency `C`."""


class Rate[C, U]:
    """Marker type for exact Monetary rates (`currency C per unit U`)."""


class Link[Target]:
    """Marker type for typed object links (`Link[Employee]`)."""


# Concrete static type aliases (preserve backwards-compatible names while mapping to real types).
type Sym[T] = T
type BoolTag = bool
type StrTag = str
type IntTag = int
type DateTag = Date
type TimestampTag = Timestamp
type DurationTag = Duration
type DecimalTag[U, S] = Quantity[U]


# ------------------------------------------------------------------ Closed Expression AST


type _CompareOp = Literal["==", "!=", "<", "<=", ">", ">="]
type _BinaryOp = Literal["+", "-", "*", "/"]
type _LogicalOp = Literal["and", "or"]


@dataclass(frozen=True, slots=True)
class _LiteralNode:
    value: str | int | bool | Decimal | None


@dataclass(frozen=True, slots=True)
class _RefNode:
    root: str
    path: tuple[str, ...]


@dataclass(frozen=True, slots=True)
class _CompareNode:
    op: _CompareOp
    left: _ExprNode
    right: _ExprNode


@dataclass(frozen=True, slots=True)
class _BinaryNode:
    op: _BinaryOp
    left: _ExprNode
    right: _ExprNode


@dataclass(frozen=True, slots=True)
class _LogicalNode:
    op: _LogicalOp
    left: _ExprNode
    right: _ExprNode


@dataclass(frozen=True, slots=True)
class _NotNode:
    operand: _ExprNode


@dataclass(frozen=True, slots=True)
class _ConditionalNode:
    condition: _ExprNode
    yes: _ExprNode
    no: _ExprNode


@dataclass(frozen=True, slots=True)
class _CallNode:
    fn: str
    args: tuple[_ExprNode, ...]


type _ExprNode = (
    _LiteralNode
    | _RefNode
    | _CompareNode
    | _BinaryNode
    | _LogicalNode
    | _NotNode
    | _ConditionalNode
    | _CallNode
)


# ------------------------------------------------------------------ ZEN rendering (exact TS parity)


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


def _temporal_kind(node: _ExprNode) -> Literal["date", "timestamp"] | None:
    if not isinstance(node, _CallNode):
        return None
    if node.fn in ("date", "add_days"):
        return "date"
    if node.fn in ("timestamp", "add_seconds", "resolve_local_timestamp"):
        return "timestamp"
    return None


def _as_date(node: _ExprNode) -> _ExprNode:
    return node if _temporal_kind(node) == "date" else _CallNode("date", (node,))


def _as_timestamp(node: _ExprNode) -> _ExprNode:
    return node if _temporal_kind(node) == "timestamp" else _CallNode("timestamp", (node,))


def _is_atomic(node: _ExprNode) -> bool:
    """Matches `operand()` atomicity rules in `tools/norn-ts/src/builder/expr.ts`."""
    if isinstance(node, (_LiteralNode, _RefNode)):
        return True
    if isinstance(node, _CallNode):
        return node.fn not in ("dateBefore", "timestampBefore", "timestampAtOrBefore")
    return False


def _operand(node: _ExprNode, span: SourceSpan | None = None) -> str:
    text = _render_node(node, span)
    return text if _is_atomic(node) else f"({text})"


def _render_node(node: _ExprNode, span: SourceSpan | None = None) -> str:
    """Render a closed AST node to canonical ZEN text matching `tools/norn-ts/src/builder/expr.ts`."""
    if isinstance(node, _LiteralNode):
        return _literal_zen(node.value, span)
    if isinstance(node, _RefNode):
        return ".".join((node.root, *node.path))
    if isinstance(node, (_CompareNode, _BinaryNode, _LogicalNode)):
        return f"{_operand(node.left, span)} {node.op} {_operand(node.right, span)}"
    if isinstance(node, _NotNode):
        return f"not {_operand(node.operand, span)}"
    if isinstance(node, _ConditionalNode):
        return f"{_operand(node.condition, span)} ? {_operand(node.yes, span)} : {_operand(node.no, span)}"
    if isinstance(node, _CallNode):
        if node.fn == "dateBefore":
            return f"{_render_node(node.args[0], span)} < {_render_node(node.args[1], span)}"
        if node.fn == "timestampBefore":
            return f"{_render_node(node.args[0], span)} < {_render_node(node.args[1], span)}"
        if node.fn == "timestampAtOrBefore":
            return f"{_render_node(node.args[0], span)} <= {_render_node(node.args[1], span)}"
        rendered_args = ", ".join(_render_node(arg, span) for arg in node.args)
        return f"{node.fn}({rendered_args})"
    raise NornTracingError("NORN-PY-EXPRESSION", f"unexpected AST node {type(node).__name__}", span)


# ------------------------------------------------------------------ Closed Expr wrapper


@final
class Expr[T]:
    """Immutable symbolic expression backed by a closed AST (no raw ZEN construction)."""

    __slots__ = ("_node", "_span")

    _node: _ExprNode
    _span: SourceSpan | None

    def __init_subclass__(cls, **kwargs: object) -> NoReturn:
        raise NornTracingError("NORN-PY-RAW-EXPR", "Expr cannot be subclassed")

    def __init__(
        self,
        *,
        _node: _ExprNode | None = None,
        _span: SourceSpan | None = None,
        _token: object = None,
    ) -> None:
        if _token is not _EXPR_TOKEN or _node is None:
            raise NornTracingError(
                "NORN-PY-RAW-EXPR",
                "Expr cannot be constructed directly from raw ZEN strings; "
                "use typed field references, lit(), and expression combinators",
                capture_caller_span(),
            )
        object.__setattr__(self, "_node", _node)
        object.__setattr__(self, "_span", _span)

    def __setattr__(self, _name: str, _value: object) -> NoReturn:
        raise AttributeError("Expr instances are immutable")

    def __delattr__(self, _name: str) -> NoReturn:
        raise AttributeError("Expr instances are immutable")

    @property
    def zen(self) -> str:
        return _render_node(self._node, self._span)

    @property
    def span(self) -> SourceSpan | None:
        return self._span

    def __repr__(self) -> str:
        return f"Expr({self.zen!r})"

    # --- Hard Astra soundness refusals: never let Python silently collapse symbolic branches ---

    def __bool__(self) -> NoReturn:
        raise NornTracingError(
            "NORN-PY-BOOL-COERCION",
            "Cannot evaluate symbolic Expr as a Python bool (this happens in `if expr:`, "
            "`a and b`, `a or b`, `not a`, or chained `0 < x < 10`). "
            "Use `all_of(a, b)`, `any_of(a, b)`, `not_(a)`, `when(cond, yes, no)`, "
            "`between(x, lo, hi)`, or parenthesized `(a) & (b)` instead.",
            self._span,
        )

    def __len__(self) -> NoReturn:
        raise NornTracingError(
            "NORN-PY-LEN-COERCION",
            "Cannot call Python `len()` on a symbolic Expr. Use `norn_len(expr)` instead.",
            self._span,
        )

    def __iter__(self) -> NoReturn:
        raise NornTracingError(
            "NORN-PY-ITER-COERCION",
            "Cannot iterate over a symbolic Expr in Python. Use bounded declarative combinators instead.",
            self._span,
        )

    def __contains__(self, _item: object) -> NoReturn:
        raise NornTracingError(
            "NORN-PY-CONTAINS-COERCION",
            "Cannot use Python `in` on a symbolic Expr (it coerces to bool). Use `contains(a, b)` instead.",
            self._span,
        )

    def __index__(self) -> NoReturn:
        raise NornTracingError(
            "NORN-PY-INDEX-COERCION",
            "Cannot use a symbolic Expr as a Python integer index or `range()` bound.",
            self._span,
        )

    # --- Strictly typed comparisons & operators (never return Python bool) ---

    @overload
    def eq[U](self: Expr[Quantity[U]], other: Expr[Quantity[U]] | Decimal) -> Expr[bool]: ...
    @overload
    def eq[C](self: Expr[Money[C]], other: Expr[Money[C]] | Decimal) -> Expr[bool]: ...
    @overload
    def eq(self, other: Expr[T] | T) -> Expr[bool]: ...
    def eq(self, other: Any) -> Expr[bool]:
        return _compare("==", self, other)

    @overload
    def ne[U](self: Expr[Quantity[U]], other: Expr[Quantity[U]] | Decimal) -> Expr[bool]: ...
    @overload
    def ne[C](self: Expr[Money[C]], other: Expr[Money[C]] | Decimal) -> Expr[bool]: ...
    @overload
    def ne(self, other: Expr[T] | T) -> Expr[bool]: ...
    def ne(self, other: Any) -> Expr[bool]:
        return _compare("!=", self, other)

    def __eq__(self, other: object) -> Expr[bool]:  # type: ignore[override]
        return _compare("==", self, other)

    def __ne__(self, other: object) -> Expr[bool]:  # type: ignore[override]
        return _compare("!=", self, other)

    @overload
    def __lt__(self: Expr[int], other: Expr[int] | int) -> Expr[bool]: ...
    @overload
    def __lt__(self: Expr[Decimal], other: Expr[Decimal] | Decimal) -> Expr[bool]: ...
    @overload
    def __lt__[U](self: Expr[Quantity[U]], other: Expr[Quantity[U]] | Decimal) -> Expr[bool]: ...
    @overload
    def __lt__[C](self: Expr[Money[C]], other: Expr[Money[C]] | Decimal) -> Expr[bool]: ...
    def __lt__(self, other: Any) -> Expr[bool]:
        return _compare("<", self, other)

    @overload
    def __le__(self: Expr[int], other: Expr[int] | int) -> Expr[bool]: ...
    @overload
    def __le__(self: Expr[Decimal], other: Expr[Decimal] | Decimal) -> Expr[bool]: ...
    @overload
    def __le__[U](self: Expr[Quantity[U]], other: Expr[Quantity[U]] | Decimal) -> Expr[bool]: ...
    @overload
    def __le__[C](self: Expr[Money[C]], other: Expr[Money[C]] | Decimal) -> Expr[bool]: ...
    def __le__(self, other: Any) -> Expr[bool]:
        return _compare("<=", self, other)

    @overload
    def __gt__(self: Expr[int], other: Expr[int] | int) -> Expr[bool]: ...
    @overload
    def __gt__(self: Expr[Decimal], other: Expr[Decimal] | Decimal) -> Expr[bool]: ...
    @overload
    def __gt__[U](self: Expr[Quantity[U]], other: Expr[Quantity[U]] | Decimal) -> Expr[bool]: ...
    @overload
    def __gt__[C](self: Expr[Money[C]], other: Expr[Money[C]] | Decimal) -> Expr[bool]: ...
    def __gt__(self, other: Any) -> Expr[bool]:
        return _compare(">", self, other)

    @overload
    def __ge__(self: Expr[int], other: Expr[int] | int) -> Expr[bool]: ...
    @overload
    def __ge__(self: Expr[Decimal], other: Expr[Decimal] | Decimal) -> Expr[bool]: ...
    @overload
    def __ge__[U](self: Expr[Quantity[U]], other: Expr[Quantity[U]] | Decimal) -> Expr[bool]: ...
    @overload
    def __ge__[C](self: Expr[Money[C]], other: Expr[Money[C]] | Decimal) -> Expr[bool]: ...
    def __ge__(self, other: Any) -> Expr[bool]:
        return _compare(">=", self, other)

    @overload
    def __add__(self: Expr[int], other: Expr[int] | int) -> Expr[int]: ...
    @overload
    def __add__(self: Expr[Decimal], other: Expr[Decimal] | Decimal) -> Expr[Decimal]: ...
    @overload
    def __add__[U](self: Expr[Quantity[U]], other: Expr[Quantity[U]] | Decimal) -> Expr[Quantity[U]]: ...
    @overload
    def __add__[C](self: Expr[Money[C]], other: Expr[Money[C]] | Decimal) -> Expr[Money[C]]: ...
    def __add__(self, other: Any) -> Expr[Any]:
        return _arith("+", self, other)

    @overload
    def __sub__(self: Expr[int], other: Expr[int] | int) -> Expr[int]: ...
    @overload
    def __sub__(self: Expr[Decimal], other: Expr[Decimal] | Decimal) -> Expr[Decimal]: ...
    @overload
    def __sub__[U](self: Expr[Quantity[U]], other: Expr[Quantity[U]] | Decimal) -> Expr[Quantity[U]]: ...
    @overload
    def __sub__[C](self: Expr[Money[C]], other: Expr[Money[C]] | Decimal) -> Expr[Money[C]]: ...
    def __sub__(self, other: Any) -> Expr[Any]:
        return _arith("-", self, other)

    @overload
    def __mul__(self: Expr[int], other: Expr[int] | int) -> Expr[int]: ...
    @overload
    def __mul__(self: Expr[Decimal], other: Expr[Decimal] | Decimal) -> Expr[Decimal]: ...
    @overload
    def __mul__[U](self: Expr[Quantity[U]], other: Expr[Decimal] | Decimal | int) -> Expr[Quantity[U]]: ...
    @overload
    def __mul__[C](self: Expr[Money[C]], other: Expr[Decimal] | Decimal | int) -> Expr[Money[C]]: ...
    def __mul__(self, other: Any) -> Expr[Any]:
        return _arith("*", self, other)

    @overload
    def __truediv__(self: Expr[int], other: Expr[int] | int) -> Expr[int]: ...
    @overload
    def __truediv__(self: Expr[Decimal], other: Expr[Decimal] | Decimal) -> Expr[Decimal]: ...
    @overload
    def __truediv__[U](self: Expr[Quantity[U]], other: Expr[Decimal] | Decimal | int) -> Expr[Quantity[U]]: ...
    @overload
    def __truediv__[C](self: Expr[Money[C]], other: Expr[Decimal] | Decimal | int) -> Expr[Money[C]]: ...
    def __truediv__(self, other: Any) -> Expr[Any]:
        return _arith("/", self, other)

    def __and__(self: Expr[bool], other: Expr[bool]) -> Expr[bool]:
        return all_of(self, other)

    def __or__(self: Expr[bool], other: Expr[bool]) -> Expr[bool]:
        return any_of(self, other)

    def __invert__(self: Expr[bool]) -> Expr[bool]:
        return not_(self)

    def is_null(self) -> Expr[bool]:
        return _make_expr(_CompareNode("==", self._node, _LiteralNode(None)), capture_caller_span())

    def is_not_null(self) -> Expr[bool]:
        return _make_expr(_CompareNode("!=", self._node, _LiteralNode(None)), capture_caller_span())


def _make_expr[T](node: _ExprNode, span: SourceSpan | None = None) -> Expr[T]:
    return Expr(_node=node, _span=span, _token=_EXPR_TOKEN)


def _require_expr(value: object, context: str, span: SourceSpan | None) -> Expr[Any]:
    if type(value) is not Expr:
        raise NornTracingError(
            "NORN-PY-RAW-EXPR",
            f"{context} must be a closed norn_py Expr node, got {type(value).__name__}",
            span,
        )
    return value


def _coerce_operand(value: object, span: SourceSpan | None) -> _ExprNode:
    if type(value) is Expr:
        return value._node
    if isinstance(value, float):
        raise NornFloatRefusalError(value, context="expression operand")
    if isinstance(value, (str, int, bool, Decimal)) or value is None:
        # Validate literal eagerly so invalid strings/Decimals fail at call site.
        _literal_zen(value, span)
        return _LiteralNode(value)
    raise NornTracingError(
        "NORN-PY-EXPRESSION",
        f"unsupported expression operand of type {type(value).__name__}",
        span,
    )


def _compare(op: _CompareOp, left: Expr[Any], right: object) -> Expr[bool]:
    span = capture_caller_span(skip=3)
    left_expr = _require_expr(left, "comparison operand", span)
    right_node = _coerce_operand(right, span)
    return _make_expr(_CompareNode(op, left_expr._node, right_node), span)


def _arith[T](op: _BinaryOp, left: Expr[T], right: object) -> Expr[T]:
    span = capture_caller_span(skip=3)
    left_expr = _require_expr(left, "arithmetic operand", span)
    right_node = _coerce_operand(right, span)
    return _make_expr(_BinaryNode(op, left_expr._node, right_node), span)


# ------------------------------------------------------------------ Typed Field Descriptor


class Field[T]:
    """Descriptor exposing a declared Concept field as `Expr[T]` on symbolic instances.

    Usage:
        class Shift(Concept, name="hr.Shift", title="Shift", version="0.1.0"):
            starts_at: Annotated[Field[Timestamp], timestamp_type("utc")]
            hours: Annotated[Field[Quantity[Literal["h"]]], decimal_type(2, unit=HOURS)]
    """

    __slots__ = ("_default", "_name")

    def __init__(self, *, name: str = "", default: object = OMIT) -> None:
        self._name = name
        self._default = default

    def __set_name__(self, owner: type[object], name: str) -> None:
        if not self._name:
            self._name = name

    @overload
    def __get__(self, instance: None, owner: type[object]) -> Field[T]: ...

    @overload
    def __get__(self, instance: object, owner: type[object]) -> Expr[T]: ...

    def __get__(self, instance: object | None, owner: type[object] | None = None) -> Field[T] | Expr[T]:
        if instance is None:
            return self
        root: str = object.__getattribute__(instance, "_norn_sym_root")
        span = capture_caller_span(skip=2)
        return _make_expr(_RefNode(root=_check_ident(root, span), path=(_check_ident(self._name, span),)), span)


def field_default[T](value: T) -> Field[T]:
    """Declare a typed literal default on a `Field[T]` attribute without losing `Field[T]` typing."""
    if isinstance(value, float):
        raise NornFloatRefusalError(value, context="field_default()")
    return Field(default=value)


# ------------------------------------------------------------------ Friendly combinators


def field_ref(root: str, *path: str, span: SourceSpan | None = None) -> Expr[Any]:
    """Reference `root.a.b` in ZEN expression scope."""
    checked_root = _check_ident(root, span)
    checked_path = tuple(_check_ident(seg, span) for seg in path)
    return _make_expr(_RefNode(checked_root, checked_path), span or capture_caller_span())


def lit[T: (str, int, bool, Decimal, None)](value: T) -> Expr[T]:
    """Explicit exact literal node (refuses binary float)."""
    span = capture_caller_span()
    _literal_zen(value, span)
    return _make_expr(_LiteralNode(value), span)


def all_of(*conditions: Expr[bool]) -> Expr[bool]:
    """Conjoin one or more Boolean expressions with ZEN `and` (left-associative, matching TS)."""
    span = capture_caller_span()
    if not conditions:
        raise NornTracingError("NORN-PY-EXPRESSION", "all_of() requires at least one condition", span)
    nodes = [_require_expr(c, "all_of() argument", span)._node for c in conditions]
    acc = nodes[0]
    for nxt in nodes[1:]:
        acc = _LogicalNode("and", acc, nxt)
    return _make_expr(acc, span)


def any_of(*conditions: Expr[bool]) -> Expr[bool]:
    """Disjoin one or more Boolean expressions with ZEN `or` (left-associative, matching TS)."""
    span = capture_caller_span()
    if not conditions:
        raise NornTracingError("NORN-PY-EXPRESSION", "any_of() requires at least one condition", span)
    nodes = [_require_expr(c, "any_of() argument", span)._node for c in conditions]
    acc = nodes[0]
    for nxt in nodes[1:]:
        acc = _LogicalNode("or", acc, nxt)
    return _make_expr(acc, span)


def not_(condition: Expr[bool]) -> Expr[bool]:
    """Negate a Boolean expression with ZEN `not`."""
    span = capture_caller_span()
    cond_expr = _require_expr(condition, "not_() argument", span)
    return _make_expr(_NotNode(cond_expr._node), span)


def when[T](condition: Expr[bool], yes: Expr[T], no: Expr[T]) -> Expr[T]:
    """Declarative conditional expression `cond ? yes : no` (replaces Python `if`)."""
    span = capture_caller_span()
    cond_expr = _require_expr(condition, "when() condition", span)
    return _make_expr(_ConditionalNode(cond_expr._node, _coerce_operand(yes, span), _coerce_operand(no, span)), span)


if_else = when


@overload
def between(value: Expr[int], low: Expr[int] | int, high: Expr[int] | int, *, inclusive: bool = True) -> Expr[bool]: ...
@overload
def between(
    value: Expr[Decimal], low: Expr[Decimal] | Decimal, high: Expr[Decimal] | Decimal, *, inclusive: bool = True
) -> Expr[bool]: ...
@overload
def between[U](
    value: Expr[Quantity[U]],
    low: Expr[Quantity[U]] | Decimal,
    high: Expr[Quantity[U]] | Decimal,
    *,
    inclusive: bool = True,
) -> Expr[bool]: ...
@overload
def between[C](
    value: Expr[Money[C]],
    low: Expr[Money[C]] | Decimal,
    high: Expr[Money[C]] | Decimal,
    *,
    inclusive: bool = True,
) -> Expr[bool]: ...
def between(value: Any, low: Any, high: Any, *, inclusive: bool = True) -> Expr[bool]:
    """Declarative range check replacing Python chained comparisons (`lo <= x <= hi`)."""
    span = capture_caller_span()
    val_node = _require_expr(value, "between() value", span)._node
    lo_node = _coerce_operand(low, span)
    hi_node = _coerce_operand(high, span)
    lo_op: _CompareOp = ">=" if inclusive else ">"
    hi_op: _CompareOp = "<=" if inclusive else "<"
    return _make_expr(
        _LogicalNode("and", _CompareNode(lo_op, val_node, lo_node), _CompareNode(hi_op, val_node, hi_node)),
        span,
    )


def norn_len(value: Expr[str]) -> Expr[int]:
    span = capture_caller_span()
    val_expr = _require_expr(value, "norn_len() argument", span)
    return _make_expr(_CallNode("len", (val_expr._node,)), span)


def contains(haystack: Expr[str], needle: Expr[str] | str) -> Expr[bool]:
    span = capture_caller_span()
    hay_expr = _require_expr(haystack, "contains() haystack", span)
    return _make_expr(_CallNode("contains", (hay_expr._node, _coerce_operand(needle, span))), span)


# ------------------------------------------------------------------ Temporal builtins (norn-zen/6 & norn-zen/7)


def parse_date(value: Expr[Date] | Expr[str] | str) -> Expr[Date]:
    span = capture_caller_span()
    return _make_expr(_CallNode("date", (_coerce_operand(value, span),)), span)


def parse_timestamp(value: Expr[Timestamp] | Expr[str] | str) -> Expr[Timestamp]:
    span = capture_caller_span()
    return _make_expr(_CallNode("timestamp", (_coerce_operand(value, span),)), span)


def date_before(left: Expr[Date], right: Expr[Date]) -> Expr[bool]:
    span = capture_caller_span()
    l_expr = _require_expr(left, "date_before() left", span)
    r_expr = _require_expr(right, "date_before() right", span)
    return _make_expr(_CallNode("dateBefore", (_as_date(l_expr._node), _as_date(r_expr._node))), span)


def timestamp_before(left: Expr[Timestamp], right: Expr[Timestamp]) -> Expr[bool]:
    span = capture_caller_span()
    l_expr = _require_expr(left, "timestamp_before() left", span)
    r_expr = _require_expr(right, "timestamp_before() right", span)
    return _make_expr(_CallNode("timestampBefore", (_as_timestamp(l_expr._node), _as_timestamp(r_expr._node))), span)


def timestamp_at_or_before(left: Expr[Timestamp], right: Expr[Timestamp]) -> Expr[bool]:
    span = capture_caller_span()
    l_expr = _require_expr(left, "timestamp_at_or_before() left", span)
    r_expr = _require_expr(right, "timestamp_at_or_before() right", span)
    return _make_expr(
        _CallNode("timestampAtOrBefore", (_as_timestamp(l_expr._node), _as_timestamp(r_expr._node))),
        span,
    )


def days_between(start: Expr[Date], end: Expr[Date]) -> Expr[int]:
    span = capture_caller_span()
    s_expr = _require_expr(start, "days_between() start", span)
    e_expr = _require_expr(end, "days_between() end", span)
    return _make_expr(_CallNode("days_between", (_as_date(s_expr._node), _as_date(e_expr._node))), span)


def add_days(start: Expr[Date], days: Expr[int] | int) -> Expr[Date]:
    span = capture_caller_span()
    s_expr = _require_expr(start, "add_days() start", span)
    return _make_expr(_CallNode("add_days", (_as_date(s_expr._node), _coerce_operand(days, span))), span)


def seconds_between(start: Expr[Timestamp], end: Expr[Timestamp]) -> Expr[Quantity[Literal["s"]]]:
    span = capture_caller_span()
    s_expr = _require_expr(start, "seconds_between() start", span)
    e_expr = _require_expr(end, "seconds_between() end", span)
    return _make_expr(_CallNode("seconds_between", (_as_timestamp(s_expr._node), _as_timestamp(e_expr._node))), span)


def add_seconds(start: Expr[Timestamp], seconds: Expr[int] | int) -> Expr[Timestamp]:
    span = capture_caller_span()
    s_expr = _require_expr(start, "add_seconds() start", span)
    return _make_expr(_CallNode("add_seconds", (_as_timestamp(s_expr._node), _coerce_operand(seconds, span))), span)


def intervals_overlap(
    a_start: Expr[Timestamp],
    a_end: Expr[Timestamp],
    b_start: Expr[Timestamp],
    b_end: Expr[Timestamp],
) -> Expr[bool]:
    """norn-zen/7 fail-closed half-open interval overlap check."""
    span = capture_caller_span()
    nodes = tuple(
        _as_timestamp(_require_expr(arg, "intervals_overlap() arg", span)._node)
        for arg in (a_start, a_end, b_start, b_end)
    )
    return _make_expr(_CallNode("intervals_overlap", nodes), span)


def interval_gap_seconds(
    a_start: Expr[Timestamp],
    a_end: Expr[Timestamp],
    b_start: Expr[Timestamp],
    b_end: Expr[Timestamp],
) -> Expr[Quantity[Literal["s"]]]:
    """norn-zen/7 fail-closed gap in exact Decimal seconds between two half-open intervals."""
    span = capture_caller_span()
    nodes = tuple(
        _as_timestamp(_require_expr(arg, "interval_gap_seconds() arg", span)._node)
        for arg in (a_start, a_end, b_start, b_end)
    )
    return _make_expr(_CallNode("interval_gap_seconds", nodes), span)


def interval_intersection_seconds(
    a_start: Expr[Timestamp],
    a_end: Expr[Timestamp],
    b_start: Expr[Timestamp],
    b_end: Expr[Timestamp],
) -> Expr[Quantity[Literal["s"]]]:
    """norn-zen/7 fail-closed overlap duration in exact Decimal seconds."""
    span = capture_caller_span()
    nodes = tuple(
        _as_timestamp(_require_expr(arg, "interval_intersection_seconds() arg", span)._node)
        for arg in (a_start, a_end, b_start, b_end)
    )
    return _make_expr(_CallNode("interval_intersection_seconds", nodes), span)


# ------------------------------------------------------------------ Dynamic Symbolic Record Proxy


class DynamicSym:
    """Untyped runtime symbolic record proxy for ad-hoc field sets (tests / internal fallback)."""

    def __init__(self, root: str, allowed_fields: Sequence[str], concept_name: str) -> None:
        object.__setattr__(self, "_root", root)
        object.__setattr__(self, "_allowed", frozenset(allowed_fields))
        object.__setattr__(self, "_concept", concept_name)

    def __getattr__(self, name: str) -> Expr[Any]:
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


# ------------------------------------------------------------------ Pre-tracing Callback AST Inspection


ALLOWED_CALLBACK_FUNCS: frozenset[str] = frozenset(
    {
        "Decimal",
        "add_days",
        "add_seconds",
        "all_of",
        "any_of",
        "between",
        "contains",
        "date_before",
        "days_between",
        "if_else",
        "interval_gap_seconds",
        "interval_intersection_seconds",
        "intervals_overlap",
        "lit",
        "norn_len",
        "not_",
        "parse_date",
        "parse_timestamp",
        "seconds_between",
        "timestamp_at_or_before",
        "timestamp_before",
        "when",
    }
)

ALLOWED_CALLBACK_METHODS: frozenset[str] = frozenset(
    {
        "eq",
        "is_not_null",
        "is_null",
        "ne",
    }
)

_FORBIDDEN_INTERNAL_ATTRS: frozenset[str] = frozenset(
    {
        "ag_frame",
        "co_code",
        "cr_frame",
        "f_back",
        "f_builtins",
        "f_globals",
        "f_locals",
        "func_globals",
        "gi_frame",
        "tb_frame",
    }
)

_SOURCE_REGISTRY: dict[str, tuple[str, ast.Module]] = {}


def register_source(filename: str, source: str, tree: ast.Module | None = None) -> None:
    """Register authored source in-memory so callback AST inspection needs no disk `open()`."""
    parsed = tree if tree is not None else ast.parse(source, filename=filename)
    _SOURCE_REGISTRY[filename] = (source, parsed)
    lines = source.splitlines(keepends=True)
    linecache.cache[filename] = (len(source), None, lines, filename)


def _load_module_ast_for_callable(fn: Callable[..., object], span: SourceSpan | None) -> tuple[str, ast.Module]:
    code = getattr(fn, "__code__", None)
    if code is None:
        raise NornTracingError(
            "NORN-PY-CALLBACK-SOURCE",
            "authoring callback must be a plain Python function or lambda with inspectable AST",
            span,
        )
    filename: str = code.co_filename
    cached = _SOURCE_REGISTRY.get(filename)
    if cached is not None:
        return filename, cached[1]

    # Fall back to linecache / source file on disk when invoked outside sandbox.
    lines = linecache.getlines(filename)
    if lines:
        source = "".join(lines)
        tree = ast.parse(source, filename=filename)
        _SOURCE_REGISTRY[filename] = (source, tree)
        return filename, tree

    src_file = inspect.getsourcefile(fn)
    if src_file is not None:
        p = Path(src_file)
        if p.is_file():
            source = p.read_text(encoding="utf-8")
            tree = ast.parse(source, filename=filename)
            _SOURCE_REGISTRY[filename] = (source, tree)
            return filename, tree

    raise NornTracingError(
        "NORN-PY-CALLBACK-SOURCE",
        f"cannot retrieve source AST for callback {getattr(fn, '__qualname__', repr(fn))!r}; "
        "refusing to trace uninspected callable",
        span,
    )


def _find_callback_node(fn: Callable[..., object], span: SourceSpan | None) -> tuple[str, ast.FunctionDef | ast.Lambda]:
    filename, tree = _load_module_ast_for_callable(fn, span)
    code = fn.__code__
    first_line = code.co_firstlineno
    fn_name = code.co_name

    if fn_name == "<lambda>":
        candidates = [
            node
            for node in ast.walk(tree)
            if isinstance(node, ast.Lambda) and node.lineno == first_line
        ]
        if len(candidates) == 1:
            return filename, candidates[0]
        if len(candidates) > 1:
            # Disambiguate multiple lambdas on one line using Python 3.11+ co_positions.
            pos_cols = {
                col
                for lineno, _end_line, col, _end_col in code.co_positions()
                if lineno == first_line and col is not None
            }
            for cand in candidates:
                if cand.col_offset in pos_cols or any(
                    cand.col_offset <= col <= (cand.end_col_offset or cand.col_offset)
                    for col in pos_cols
                ):
                    return filename, cand
            return filename, candidates[0]
    else:
        for node in ast.walk(tree):
            if isinstance(node, ast.FunctionDef) and node.name == fn_name:
                dec_line = node.decorator_list[0].lineno if node.decorator_list else None
                if node.lineno == first_line or dec_line == first_line:
                    return filename, node

    raise NornTracingError(
        "NORN-PY-CALLBACK-SOURCE",
        f"could not locate AST definition for callback {fn_name!r} at {filename}:{first_line}",
        span or SourceSpan(filename, first_line),
    )


def _validate_callback_expr_ast(node: ast.AST, filename: str) -> None:
    """Recursively validate that an expression AST stays strictly inside the closed Norn fragment."""
    span = SourceSpan(filename, getattr(node, "lineno", 1), getattr(node, "col_offset", 0))

    if isinstance(node, ast.IfExp):
        raise NornTracingError(
            "NORN-PY-FORBIDDEN-CONTROL-FLOW",
            "Python conditional expression (`a if cond else b`) is forbidden in Norn callbacks; "
            "use `when(cond, yes, no)` / `if_else(cond, yes, no)` instead",
            span,
        )
    if isinstance(node, ast.BoolOp):
        op_name = "and" if isinstance(node.op, ast.And) else "or"
        replacement = "all_of(...)" if op_name == "and" else "any_of(...)"
        raise NornTracingError(
            "NORN-PY-FORBIDDEN-CONTROL-FLOW",
            f"Python `{op_name}` short-circuits and is forbidden in Norn callbacks; use `{replacement}` instead",
            span,
        )
    if isinstance(node, ast.UnaryOp):
        if isinstance(node.op, ast.Not):
            raise NornTracingError(
                "NORN-PY-FORBIDDEN-CONTROL-FLOW",
                "Python `not` coerces to bool and is forbidden in Norn callbacks; use `not_(...)` instead",
                span,
            )
        if not isinstance(node.op, (ast.Invert, ast.USub, ast.UAdd)):
            raise NornTracingError(
                "NORN-PY-FORBIDDEN-CONTROL-FLOW",
                f"unsupported unary operator {type(node.op).__name__} in Norn callback",
                span,
            )
        _validate_callback_expr_ast(node.operand, filename)
        return
    if isinstance(node, ast.Compare):
        if len(node.ops) != 1:
            raise NornTracingError(
                "NORN-PY-FORBIDDEN-CONTROL-FLOW",
                "chained comparisons are forbidden in Norn callbacks; use `between(...)` or `all_of(...)` instead",
                span,
            )
        op = node.ops[0]
        if isinstance(op, (ast.Is, ast.IsNot)):
            raise NornTracingError(
                "NORN-PY-FORBIDDEN-CONTROL-FLOW",
                "Python `is` / `is not` is forbidden in Norn callbacks; use `.is_null()` / `.is_not_null()` instead",
                span,
            )
        if isinstance(op, (ast.In, ast.NotIn)):
            raise NornTracingError(
                "NORN-PY-FORBIDDEN-CONTROL-FLOW",
                "Python `in` / `not in` is forbidden in Norn callbacks; use `contains(...)` instead",
                span,
            )
        _validate_callback_expr_ast(node.left, filename)
        _validate_callback_expr_ast(node.comparators[0], filename)
        return
    if isinstance(node, ast.BinOp):
        if not isinstance(node.op, (ast.Add, ast.Sub, ast.Mult, ast.Div, ast.BitAnd, ast.BitOr)):
            raise NornTracingError(
                "NORN-PY-FORBIDDEN-CONTROL-FLOW",
                f"binary operator {type(node.op).__name__} is not admitted in Norn callbacks",
                span,
            )
        _validate_callback_expr_ast(node.left, filename)
        _validate_callback_expr_ast(node.right, filename)
        return
    if isinstance(node, ast.Call):
        for arg in node.args:
            if isinstance(arg, ast.Starred):
                raise NornTracingError(
                    "NORN-PY-FORBIDDEN-CONTROL-FLOW",
                    "starred `*args` unpacking is forbidden in Norn callbacks",
                    span,
                )
        for kw in node.keywords:
            if kw.arg is None:
                raise NornTracingError(
                    "NORN-PY-FORBIDDEN-CONTROL-FLOW",
                    "`**kwargs` unpacking is forbidden in Norn callbacks",
                    span,
                )
        if isinstance(node.func, ast.Name):
            if node.func.id not in ALLOWED_CALLBACK_FUNCS:
                raise NornTracingError(
                    "NORN-PY-FORBIDDEN-CALL",
                    f"call to {node.func.id}() is outside the closed Norn expression operator set "
                    f"(allowed: {sorted(ALLOWED_CALLBACK_FUNCS)})",
                    span,
                )
        elif isinstance(node.func, ast.Attribute):
            if node.func.attr.startswith("__") or node.func.attr in _FORBIDDEN_INTERNAL_ATTRS:
                raise NornTracingError(
                    "NORN-PY-FORBIDDEN-ATTR",
                    f"attribute access .{node.func.attr} is forbidden in Norn callbacks",
                    span,
                )
            if node.func.attr not in ALLOWED_CALLBACK_METHODS:
                raise NornTracingError(
                    "NORN-PY-FORBIDDEN-CALL",
                    f"method .{node.func.attr}() is outside the closed Norn expression method set "
                    f"(allowed: {sorted(ALLOWED_CALLBACK_METHODS)})",
                    span,
                )
            _validate_callback_expr_ast(node.func.value, filename)
        else:
            raise NornTracingError(
                "NORN-PY-FORBIDDEN-CALL",
                "dynamic call targets are forbidden in Norn callbacks",
                span,
            )
        for arg in node.args:
            _validate_callback_expr_ast(arg, filename)
        for kw in node.keywords:
            _validate_callback_expr_ast(kw.value, filename)
        return
    if isinstance(node, ast.Attribute):
        if node.attr.startswith("__") or node.attr in _FORBIDDEN_INTERNAL_ATTRS:
            raise NornTracingError(
                "NORN-PY-FORBIDDEN-ATTR",
                f"attribute access .{node.attr} is forbidden in Norn callbacks",
                span,
            )
        _validate_callback_expr_ast(node.value, filename)
        return
    if isinstance(node, ast.Name):
        if node.id.startswith("__"):
            raise NornTracingError(
                "NORN-PY-FORBIDDEN-NAME",
                f"dunder name {node.id!r} is forbidden in Norn callbacks",
                span,
            )
        return
    if isinstance(node, ast.Constant):
        if isinstance(node.value, float):
            raise NornTracingError(
                "NORN-PY-FLOAT-LITERAL",
                f"float literal {node.value!r} is refused (VAL-EXACT-1); use Decimal({str(node.value)!r})",
                span,
            )
        if node.value is None or isinstance(node.value, (str, int, bool)):
            return
        raise NornTracingError(
            "NORN-PY-FORBIDDEN-CONTROL-FLOW",
            f"constant of type {type(node.value).__name__} is not admitted in Norn callbacks",
            span,
        )
    if isinstance(node, ast.Lambda):
        raise NornTracingError(
            "NORN-PY-FORBIDDEN-CONTROL-FLOW",
            "nested `lambda` expressions are forbidden in Norn callbacks",
            span,
        )
    if isinstance(node, (ast.ListComp, ast.SetComp, ast.DictComp, ast.GeneratorExp)):
        raise NornTracingError(
            "NORN-PY-FORBIDDEN-CONTROL-FLOW",
            "comprehensions and generator expressions are forbidden in Norn callbacks",
            span,
        )
    raise NornTracingError(
        "NORN-PY-FORBIDDEN-CONTROL-FLOW",
        f"unsupported syntax `{type(node).__name__}` in Norn callback",
        span,
    )


def validate_callback_node(node: ast.FunctionDef | ast.Lambda, filename: str) -> None:
    """Validate a parsed `FunctionDef` or `Lambda` AST node against the closed callback profile."""
    span = SourceSpan(filename, node.lineno, node.col_offset)
    args = node.args
    if args.vararg or args.kwarg or args.kwonlyargs or args.defaults or args.posonlyargs:
        raise NornTracingError(
            "NORN-PY-FORBIDDEN-CONTROL-FLOW",
            "authoring callbacks must take a single plain positional parameter (e.g. `s`)",
            span,
        )
    if len(args.args) != 1:
        raise NornTracingError(
            "NORN-PY-FORBIDDEN-CONTROL-FLOW",
            f"authoring callbacks must take exactly 1 parameter, got {len(args.args)}",
            span,
        )

    if isinstance(node, ast.Lambda):
        _validate_callback_expr_ast(node.body, filename)
        return

    body = list(node.body)
    if (
        body
        and isinstance(body[0], ast.Expr)
        and isinstance(body[0].value, ast.Constant)
        and isinstance(body[0].value.value, str)
    ):
        body = body[1:]

    for stmt in body:
        stmt_span = SourceSpan(filename, getattr(stmt, "lineno", node.lineno), getattr(stmt, "col_offset", 0))
        if isinstance(stmt, ast.If):
            raise NornTracingError(
                "NORN-PY-FORBIDDEN-CONTROL-FLOW",
                "Python `if`/`else` statements are forbidden in Norn callbacks; "
                "use `when(cond, yes, no)` / `if_else(cond, yes, no)` instead",
                stmt_span,
            )
        if isinstance(stmt, (ast.For, ast.AsyncFor, ast.While)):
            raise NornTracingError(
                "NORN-PY-FORBIDDEN-CONTROL-FLOW",
                "loops (`for`/`while`) are forbidden in Norn callbacks",
                stmt_span,
            )
        if isinstance(stmt, (ast.Try, ast.TryStar, ast.With, ast.AsyncWith)):
            raise NornTracingError(
                "NORN-PY-FORBIDDEN-CONTROL-FLOW",
                f"`{type(stmt).__name__}` blocks are forbidden in Norn callbacks",
                stmt_span,
            )
        if isinstance(stmt, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)):
            raise NornTracingError(
                "NORN-PY-FORBIDDEN-CONTROL-FLOW",
                "nested function/class definitions are forbidden in Norn callbacks",
                stmt_span,
            )

    if len(body) != 1 or not isinstance(body[0], ast.Return) or body[0].value is None:
        raise NornTracingError(
            "NORN-PY-FORBIDDEN-CONTROL-FLOW",
            "authoring callback body must be a single `return <expr>` statement",
            span,
        )
    _validate_callback_expr_ast(body[0].value, filename)


def validate_callback_ast(fn: Callable[..., object], span: SourceSpan | None = None) -> None:
    """Inspect a Python authoring callback with `ast.parse` BEFORE tracing it."""
    if not callable(fn):
        raise NornTracingError(
            "NORN-PY-CALLBACK-SOURCE",
            f"expected a callable authoring callback, got {type(fn).__name__}",
            span,
        )
    filename, node = _find_callback_node(fn, span)
    validate_callback_node(node, filename)


def trace_bool_callback(
    fn: Callable[[Any], Expr[bool]],
    sym: object,
    *,
    context: str,
    span: SourceSpan | None = None,
) -> Expr[bool]:
    """Validate callback AST BEFORE tracing, execute symbolically, and require a closed `Expr`."""
    validate_callback_ast(fn, span)
    result = fn(sym)
    if type(result) is not Expr:
        raise NornTracingError(
            "NORN-PY-RAW-EXPR",
            f"{context} must return a closed norn_py Expr[bool] node, got {type(result).__name__}",
            span,
        )
    return result
