"""Typed Python authoring schema & lowering to canonical Norn IR (`irVersion: 0.1`).

Emits byte-identical `EntityIR v2`, `Markings`, and `LawIR` artifacts matching
`tools/norn-ts/src/builder/{entity,law}.ts` and `crates/norn-ir/src/entity_v2`.

Design guarantees:
- Annotations are parsed directly with `ast.parse(..., mode="eval")` against a closed
  allowlist of Norn types/primitives/locally defined classes — `eval()` and
  `typing.get_type_hints()` are NEVER called.
- `PropertyTypeSpec` and `MaskSpec` are closed (cannot be constructed from raw IR dicts).
- `Annotated[base_type, meta...]` consistency is validated at elaboration time.
- Object `invariants` are emitted into `objectType.invariants`; interface `lifecycles`
  are explicitly rejected (no constraint is ever silently dropped).
- Every guard, invariant, and law callback is AST-validated BEFORE tracing and must
  return a closed `norn_py.Expr` node.
"""

from __future__ import annotations

import ast
import contextvars
import re
import types
from collections.abc import Callable, Mapping, Sequence
from dataclasses import dataclass, field
from decimal import Decimal, InvalidOperation
from enum import StrEnum
from types import MappingProxyType
from typing import (
    TYPE_CHECKING,
    Annotated,
    Any,
    Literal,
    NoReturn,
    Optional,
    Self,
    TypeVar,
    Union,
    final,
    get_args,
    get_origin,
)
from urllib.parse import urlsplit

from .canonical import (
    OMIT,
    DependencyPin,
    EnvelopeOptions,
    FinalOmit,
    JsonValue,
    NornFloatRefusalError,
    SourceSpan,
    build_envelope,
    omit_empty,
)
from .expr import (
    Date,
    Duration,
    Expr,
    Field,
    Link,
    Money,
    NornTracingError,
    Quantity,
    Rate,
    Sym,
    Timestamp,
    capture_caller_span,
    format_exact_decimal,
    trace_bool_callback,
)

T = TypeVar("T")
S = TypeVar("S", bound=StrEnum)

OBJECT_TYPE_NAME_RE = re.compile(r"^[a-z][a-z0-9_]*(\.[a-z][a-z0-9_]*)*\.[A-Z][A-Za-z0-9]*$")

Rounding = Literal["refuse", "toward_zero", "half_even", "half_away_from_zero"]
TextFormat = Literal["email", "uri", "uuid"]
TimezonePolicy = Literal["utc", "offset"]
Multiplicity = Literal["one", "many"]
RoleKind = Literal["thing", "happening"]
MarkingSeverity = Literal["low", "moderate", "high", "critical"]

_SPEC_TOKEN = object()
_MASK_TOKEN = object()


# ------------------------------------------------------------------ Value & Marking Annotations


@dataclass(frozen=True, slots=True)
class UnitRef:
    system: Literal["ucum", "qudt"]
    code: str

    def to_ir(self) -> dict[str, object]:
        return {"code": self.code, "system": self.system}


@dataclass(frozen=True, slots=True)
class Marking:
    """Attach a classification marking via `Annotated[Field[str], Marking("pii.demographic")]`."""

    id: str


@dataclass(frozen=True, slots=True)
class _TextSpec:
    fmt: TextFormat | None = None

    def to_ir(self) -> dict[str, object]:
        return {"kind": "text"} if self.fmt is None else {"format": self.fmt, "kind": "text"}


@dataclass(frozen=True, slots=True)
class _IntegerSpec:
    def to_ir(self) -> dict[str, object]:
        return {"kind": "integer"}


@dataclass(frozen=True, slots=True)
class _BooleanSpec:
    def to_ir(self) -> dict[str, object]:
        return {"kind": "boolean"}


@dataclass(frozen=True, slots=True)
class _DateSpec:
    def to_ir(self) -> dict[str, object]:
        return {"kind": "date"}


@dataclass(frozen=True, slots=True)
class _TimestampSpec:
    timezone: TimezonePolicy = "utc"

    def to_ir(self) -> dict[str, object]:
        return {"kind": "timestamp", "timezone": self.timezone}


@dataclass(frozen=True, slots=True)
class _DurationSpec:
    def to_ir(self) -> dict[str, object]:
        return {"kind": "duration"}


@dataclass(frozen=True, slots=True)
class _RefSpec:
    name: str

    def to_ir(self) -> dict[str, object]:
        return {"kind": "ref", "name": self.name}


@dataclass(frozen=True, slots=True)
class _DecimalSpec:
    scale: int
    unit: UnitRef | None = None
    currency: str | None = None
    per_unit: UnitRef | None = None
    rounding: Rounding | None = None

    def to_ir(self) -> dict[str, object]:
        body: dict[str, object] = {"kind": "decimal", "scale": self.scale}
        if self.unit is not None:
            body["unit"] = self.unit.to_ir()
        if self.currency is not None:
            body["currencySource"] = {"links": [], "property": self.currency}
        if self.per_unit is not None:
            body["perUnit"] = self.per_unit.to_ir()
        if self.rounding is not None:
            body["rounding"] = self.rounding
        return body


@dataclass(frozen=True, slots=True)
class _EnumSpec:
    enum_cls: type[StrEnum]

    def to_ir(self) -> dict[str, object]:
        return {"kind": "enum", "values": [{"value": member.value} for member in self.enum_cls]}


type _InnerPropertySpec = (
    _TextSpec
    | _IntegerSpec
    | _BooleanSpec
    | _DateSpec
    | _TimestampSpec
    | _DurationSpec
    | _RefSpec
    | _DecimalSpec
    | _EnumSpec
)


@final
class PropertyTypeSpec:
    """Closed EntityIR v2 PropertyType specification (cannot be built from raw IR dicts)."""

    __slots__ = ("_inner",)
    _inner: _InnerPropertySpec

    def __init_subclass__(cls, **kwargs: object) -> NoReturn:
        raise NornTracingError("NORN-PY-RAW-IR", "PropertyTypeSpec cannot be subclassed")

    def __init__(self, *, _inner: _InnerPropertySpec | None = None, _token: object = None) -> None:
        if _token is not _SPEC_TOKEN or _inner is None:
            raise NornTracingError(
                "NORN-PY-RAW-IR",
                "PropertyTypeSpec cannot be constructed directly from raw IR dicts; "
                "use text(), integer(), boolean(), decimal_type(), rate_type(), date_type(), "
                "timestamp_type(), duration_type(), enum_type(), or ref_type()",
                capture_caller_span(),
            )
        object.__setattr__(self, "_inner", _inner)

    def __setattr__(self, _name: str, _value: object) -> NoReturn:
        raise AttributeError("PropertyTypeSpec instances are immutable")

    def __delattr__(self, _name: str) -> NoReturn:
        raise AttributeError("PropertyTypeSpec instances are immutable")

    @property
    def ir(self) -> dict[str, object]:
        return self._inner.to_ir()

    def __repr__(self) -> str:
        return f"PropertyTypeSpec({self._inner!r})"


def _make_spec(inner: _InnerPropertySpec) -> PropertyTypeSpec:
    return PropertyTypeSpec(_inner=inner, _token=_SPEC_TOKEN)


def text(fmt: TextFormat | None = None) -> PropertyTypeSpec:
    if fmt is not None and fmt not in ("email", "uri", "uuid"):
        raise NornTracingError("NORN-PY-SCHEMA", f"unsupported text format {fmt!r}")
    return _make_spec(_TextSpec(fmt))


def integer() -> PropertyTypeSpec:
    return _make_spec(_IntegerSpec())


def boolean() -> PropertyTypeSpec:
    return _make_spec(_BooleanSpec())


def date_type() -> PropertyTypeSpec:
    return _make_spec(_DateSpec())


def timestamp_type(timezone: TimezonePolicy = "utc") -> PropertyTypeSpec:
    if timezone not in ("utc", "offset"):
        raise NornTracingError("NORN-PY-SCHEMA", f"unsupported timezone policy {timezone!r}")
    return _make_spec(_TimestampSpec(timezone))


def duration_type() -> PropertyTypeSpec:
    return _make_spec(_DurationSpec())


def ref_type(name: str) -> PropertyTypeSpec:
    if not isinstance(name, str) or not name:
        raise NornTracingError("NORN-PY-SCHEMA", f"ref_type name must be a non-empty string, got {name!r}")
    return _make_spec(_RefSpec(name))


_VALID_ROUNDING: frozenset[str] = frozenset({"refuse", "toward_zero", "half_even", "half_away_from_zero"})


def _require_non_negative_int(value: int, label: str) -> None:
    if isinstance(value, bool) or isinstance(value, float) or not isinstance(value, int) or value < 0:
        raise NornTracingError("NORN-PY-SCHEMA", f"{label} must be a non-negative int, got {value!r}")


def _require_rounding(rounding: Rounding | None) -> None:
    if rounding is not None and rounding not in _VALID_ROUNDING:
        raise NornTracingError("NORN-PY-SCHEMA", f"unsupported rounding mode {rounding!r}")


def decimal_type(
    scale: int,
    *,
    unit: UnitRef | None = None,
    currency: str | None = None,
    rounding: Rounding | None = None,
) -> PropertyTypeSpec:
    _require_non_negative_int(scale, "decimal scale")
    if unit is not None and not isinstance(unit, UnitRef):
        raise NornTracingError("NORN-PY-SCHEMA", f"decimal unit must be a UnitRef, got {type(unit).__name__}")
    if unit is not None and currency is not None:
        raise NornTracingError("NORN-PY-SCHEMA", "decimal cannot specify both unit and currency")
    _require_rounding(rounding)
    return _make_spec(_DecimalSpec(scale=scale, unit=unit, currency=currency, rounding=rounding))


def rate_type(
    scale: int,
    *,
    currency: str,
    per_unit: UnitRef,
    rounding: Rounding | None = None,
) -> PropertyTypeSpec:
    _require_non_negative_int(scale, "rate scale")
    if not isinstance(per_unit, UnitRef):
        raise NornTracingError("NORN-PY-SCHEMA", f"rate per_unit must be a UnitRef, got {type(per_unit).__name__}")
    _require_rounding(rounding)
    return _make_spec(_DecimalSpec(scale=scale, currency=currency, per_unit=per_unit, rounding=rounding))


def enum_type(enum_cls: type[StrEnum]) -> PropertyTypeSpec:
    if not (isinstance(enum_cls, type) and issubclass(enum_cls, StrEnum)):
        raise NornTracingError("NORN-PY-SCHEMA", f"enum_type expects a StrEnum subclass, got {enum_cls!r}")
    return _make_spec(_EnumSpec(enum_cls))


@dataclass(frozen=True, slots=True)
class FieldMeta:
    """Optional description or exact default on a Concept field."""

    description: str | None = None
    default: str | bool | Decimal | StrEnum | int | FinalOmit | None = OMIT

    def __post_init__(self) -> None:
        if isinstance(self.default, float):
            raise NornFloatRefusalError(self.default, context="FieldMeta.default")
        if isinstance(self.default, Decimal):
            format_exact_decimal(self.default)


# ------------------------------------------------------------------ Links, Keys, Lifecycles, Invariants


@dataclass(frozen=True, slots=True)
class LinkSpec:
    """Typed link annotation: `employee: Annotated[Field[Link[Employee]], link(Employee, ...)]`."""

    target: str
    to: Multiplicity = "one"
    from_: Multiplicity = "many"
    required: bool = False
    inverse: str | None = None
    acyclic: bool = False
    description: str | None = None

    def to_ir(
        self,
        link_id: str,
        *,
        for_interface: bool = False,
        span: SourceSpan | None = None,
    ) -> dict[str, object]:
        if for_interface:
            if self.acyclic:
                raise NornTracingError(
                    "NORN-PY-SCHEMA",
                    f"interface link {link_id!r} cannot declare `acyclic=True` "
                    "(EntityIR v2 InterfaceLink does not support acyclic)",
                    span,
                )
            if self.inverse is not None:
                raise NornTracingError(
                    "NORN-PY-SCHEMA",
                    f"interface link {link_id!r} cannot declare `inverse={self.inverse!r}` "
                    "(EntityIR v2 InterfaceLink does not support inverse)",
                    span,
                )
        card: dict[str, object] = {
            "from": self.from_,
            "required": True if self.required else OMIT,
            "to": self.to,
        }
        base: dict[str, object] = {
            "cardinality": card,
            "description": self.description if self.description is not None else OMIT,
            "id": link_id,
            "target": self.target,
        }
        if not for_interface:
            base["acyclic"] = True if self.acyclic else OMIT
            base["inverse"] = self.inverse if self.inverse is not None else OMIT
        return base


def link(
    target: type[Concept] | str,
    *,
    to: Multiplicity = "one",
    from_: Multiplicity = "many",
    required: bool = False,
    inverse: str | None = None,
    acyclic: bool = False,
    description: str | None = None,
) -> LinkSpec:
    if isinstance(target, str):
        target_name = target
    elif isinstance(target, type) and issubclass(target, Concept) and target.__norn_name__:
        target_name = target.__norn_name__
    else:
        raise NornTracingError("NORN-PY-SCHEMA", f"link target must be a named Concept subclass or str, got {target!r}")
    return LinkSpec(
        target=target_name,
        to=to,
        from_=from_,
        required=required,
        inverse=inverse,
        acyclic=acyclic,
        description=description,
    )


@dataclass(frozen=True, slots=True)
class KeySpec:
    name: str
    properties: tuple[str, ...]

    def to_ir(self) -> dict[str, object]:
        return {"name": self.name, "properties": list(self.properties)}


@dataclass(frozen=True, slots=True)
class TransitionSpec[S: StrEnum]:
    id: str
    from_: tuple[S, ...]
    to: S
    guards: tuple[Callable[[Any], Expr[bool]], ...] = ()
    description: str | None = None
    guard_refusal_code: str | None = None


@dataclass(frozen=True, slots=True)
class LifecycleSpec[S: StrEnum]:
    property: str
    enum_cls: type[S]
    initial: tuple[S, ...]
    transitions: tuple[TransitionSpec[S], ...]
    terminal: tuple[S, ...] = ()
    immutable: tuple[S, ...] = ()
    description: str | None = None


def transition[S: StrEnum](
    id: str,
    *,
    from_: Sequence[S],
    to: S,
    guards: Sequence[Callable[[Any], Expr[bool]]] = (),
    description: str | None = None,
    guard_refusal_code: str | None = None,
) -> TransitionSpec[S]:
    return TransitionSpec(
        id=id,
        from_=tuple(from_),
        to=to,
        guards=tuple(guards),
        description=description,
        guard_refusal_code=guard_refusal_code,
    )


def lifecycle[S: StrEnum](
    property: str,
    enum_cls: type[S],
    *,
    initial: Sequence[S],
    transitions: Sequence[TransitionSpec[S]],
    terminal: Sequence[S] = (),
    immutable: Sequence[S] = (),
    description: str | None = None,
) -> LifecycleSpec[S]:
    return LifecycleSpec(
        property=property,
        enum_cls=enum_cls,
        initial=tuple(initial),
        transitions=tuple(transitions),
        terminal=tuple(terminal),
        immutable=tuple(immutable),
        description=description,
    )


@dataclass(frozen=True, slots=True)
class InvariantSpec:
    id: str
    message: str
    fn: Callable[[Any], Expr[bool]]
    description: str | None = None
    classification: Literal["integrity", "quality"] = "integrity"
    span: SourceSpan | None = None


def invariant(
    id: str,
    *,
    message: str,
    description: str | None = None,
    classification: Literal["integrity", "quality"] = "integrity",
) -> Callable[[Callable[[Any], Expr[bool]]], InvariantSpec]:
    """Decorator attaching a symbolically traced per-record invariant to a Concept/Interface."""
    span = capture_caller_span()

    def wrap(fn: Callable[[Any], Expr[bool]]) -> InvariantSpec:
        return InvariantSpec(
            id=id,
            message=message,
            fn=fn,
            description=description,
            classification=classification,
            span=span,
        )

    return wrap


# ------------------------------------------------------------------ Markings Catalog


@dataclass(frozen=True, slots=True)
class _RedactedMask:
    def to_ir(self) -> dict[str, object]:
        return {"kind": "redacted"}


@dataclass(frozen=True, slots=True)
class _NullMask:
    def to_ir(self) -> dict[str, object]:
        return {"kind": "null"}


@dataclass(frozen=True, slots=True)
class _RefuseMask:
    def to_ir(self) -> dict[str, object]:
        return {"kind": "refuse"}


@dataclass(frozen=True, slots=True)
class _LastNMask:
    count: int

    def to_ir(self) -> dict[str, object]:
        return {"count": self.count, "kind": "last-n"}


@dataclass(frozen=True, slots=True)
class _PseudonymMask:
    subject: str

    def to_ir(self) -> dict[str, object]:
        return {"kind": "pseudonym", "subject": self.subject}


type _InnerMaskSpec = _RedactedMask | _NullMask | _RefuseMask | _LastNMask | _PseudonymMask


@final
class MaskSpec:
    """Closed immutable masking strategy specification backed by frozen typed variants."""

    __slots__ = ("_inner",)
    _inner: _InnerMaskSpec

    def __init_subclass__(cls, **kwargs: object) -> NoReturn:
        raise NornTracingError("NORN-PY-RAW-IR", "MaskSpec cannot be subclassed")

    def __init__(self, *, _inner: _InnerMaskSpec | None = None, _token: object = None) -> None:
        if _token is not _MASK_TOKEN or _inner is None:
            raise NornTracingError(
                "NORN-PY-RAW-IR",
                "MaskSpec cannot be constructed from raw dicts; use mask_redacted(), mask_null(), "
                "mask_last_n(), mask_refuse(), or mask_pseudonym()",
                capture_caller_span(),
            )
        object.__setattr__(self, "_inner", _inner)

    def __setattr__(self, _name: str, _value: object) -> NoReturn:
        raise AttributeError("MaskSpec instances are immutable")

    def __delattr__(self, _name: str) -> NoReturn:
        raise AttributeError("MaskSpec instances are immutable")

    def to_ir(self) -> dict[str, object]:
        return self._inner.to_ir()

    def __eq__(self, other: object) -> bool:
        return type(other) is MaskSpec and self._inner == other._inner

    def __hash__(self) -> int:
        return hash(self._inner)

    def __repr__(self) -> str:
        return f"MaskSpec({self._inner!r})"


def _make_mask(inner: _InnerMaskSpec) -> MaskSpec:
    return MaskSpec(_inner=inner, _token=_MASK_TOKEN)


def mask_redacted() -> MaskSpec:
    return _make_mask(_RedactedMask())


def mask_null() -> MaskSpec:
    return _make_mask(_NullMask())


def mask_refuse() -> MaskSpec:
    return _make_mask(_RefuseMask())


def mask_last_n(count: int) -> MaskSpec:
    _require_non_negative_int(count, "mask_last_n count")
    return _make_mask(_LastNMask(count=count))


def mask_pseudonym(subject: str) -> MaskSpec:
    if not isinstance(subject, str) or not subject:
        raise NornTracingError("NORN-PY-SCHEMA", f"mask_pseudonym subject must be a non-empty str, got {subject!r}")
    return _make_mask(_PseudonymMask(subject=subject))


@dataclass(frozen=True, slots=True)
class MarkingDecl:
    id: str
    label: str
    severity: MarkingSeverity
    category: str
    mask: MaskSpec

    def __post_init__(self) -> None:
        if type(self.mask) is not MaskSpec:
            raise NornTracingError(
                "NORN-PY-RAW-IR",
                f"MarkingDecl.mask must be a closed MaskSpec (from mask_redacted/mask_null/mask_last_n), "
                f"got {type(self.mask).__name__}",
            )

    def to_ir(self) -> dict[str, object]:
        return {
            "category": self.category,
            "id": self.id,
            "label": self.label,
            "mask": self.mask.to_ir(),
            "severity": self.severity,
        }


@dataclass(frozen=True, slots=True)
class MarkingsCatalog:
    id: str
    title: str
    version: str
    markings: tuple[MarkingDecl, ...]
    span: SourceSpan | None = field(default_factory=capture_caller_span)

    def lower(self, options: EnvelopeOptions) -> JsonValue:
        namespace = self.id.split(".")[0]
        return build_envelope(
            kind="entity",
            artifact_id=self.id,
            namespace=namespace,
            name=self.title,
            version=self.version,
            payload={
                "entityVersion": "2.0",
                "markings": [m.to_ir() for m in self.markings],
            },
            options=options,
        )


# ------------------------------------------------------------------ Function Handle (spike prototype)


@dataclass(frozen=True, slots=True)
class FnRef:
    """Spike prototype (not collected by `collect_artifacts`): handle for an external worker behind `perform`."""

    id: str
    version: str
    input_type: str
    output_type: str
    span: SourceSpan | None = field(default_factory=capture_caller_span)


def fn_ref(*, id: str, version: str, input_type: str, output_type: str) -> FnRef:
    """Spike prototype (outside `collect_artifacts`): declare an external worker handle."""
    return FnRef(
        id=id,
        version=version,
        input_type=input_type,
        output_type=output_type,
        span=capture_caller_span(),
    )


# ------------------------------------------------------------------ LawIR Declaration


@dataclass(frozen=True, slots=True)
class LawDecl:
    id: str
    title: str
    version: str
    owner: str
    subject: type[Concept]
    rationale: str
    predicate: Callable[[Any], Expr[bool]]
    severity: Literal["error", "warning"] = "error"
    modality: Literal["always", "never", "eventually"] = "always"
    scope_root_kind: Literal["company", "app", "cell", "entity"] = "entity"
    scope_root_id: str | None = None
    dependencies: tuple[DependencyPin, ...] = ()
    span: SourceSpan | None = field(default_factory=capture_caller_span)

    def lower(self, options: EnvelopeOptions) -> JsonValue:
        sym = self.subject.sym("self")
        expr = trace_bool_callback(
            self.predicate,
            sym,
            context=f"law {self.id!r} predicate",
            span=self.span,
        )
        subj_name = self.subject.__norn_name__
        deps = (
            list(self.dependencies)
            if self.dependencies
            else [DependencyPin(f"{subj_name}.entity", self.subject.__norn_version__, kind="entity")]
        )
        law_payload: dict[str, object] = {
            "id": self.id,
            "lawVersion": "0.1",
            "modality": {"kind": self.modality},
            "owner": self.owner,
            "provenance": {
                "source": "authoring_sdk",
                "sourcePath": options.source_path,
            },
            "rationale": self.rationale,
            "response": {"expression": expr.zen, "kind": "predicate"},
            "scope": {
                "objectTypes": [subj_name],
                "root": {
                    "id": self.scope_root_id or subj_name,
                    "kind": self.scope_root_kind,
                },
            },
            "severity": self.severity,
            "subject": subj_name,
        }
        return build_envelope(
            kind="law",
            artifact_id=f"{self.id}.law",
            namespace=self.id.split(".")[0],
            name=self.title,
            version=self.version,
            payload=law_payload,
            options=options,
            dependencies=[d.to_ir() for d in deps],
        )


def law[C: Concept](
    *,
    id: str,
    title: str,
    version: str,
    owner: str,
    subject: type[C],
    rationale: str,
    severity: Literal["error", "warning"] = "error",
    modality: Literal["always", "never", "eventually"] = "always",
    scope_root_kind: Literal["company", "app", "cell", "entity"] = "entity",
    scope_root_id: str | None = None,
    dependencies: Sequence[DependencyPin] = (),
) -> Callable[[Callable[[Sym[C]], Expr[bool]]], LawDecl]:
    """Decorator tracing a Python predicate into canonical LawIR."""
    span = capture_caller_span()

    def wrap(fn: Callable[[Sym[C]], Expr[bool]]) -> LawDecl:
        return LawDecl(
            id=id,
            title=title,
            version=version,
            owner=owner,
            subject=subject,
            rationale=rationale,
            predicate=fn,
            severity=severity,
            modality=modality,
            scope_root_kind=scope_root_kind,
            scope_root_id=scope_root_id,
            dependencies=tuple(dependencies),
            span=span,
        )

    return wrap


# ------------------------------------------------------------------ Closed Safe Annotation AST Resolver (NO eval!)


_ANNOTATION_ALLOWLIST_NAMES: dict[str, object] = {
    "Annotated": Annotated,
    "Date": Date,
    "Decimal": Decimal,
    "Duration": Duration,
    "Field": Field,
    "FieldMeta": FieldMeta,
    "Link": Link,
    "Literal": Literal,
    "Marking": Marking,
    "Money": Money,
    "None": None,
    "Optional": Optional,
    "Quantity": Quantity,
    "Rate": Rate,
    "StrEnum": StrEnum,
    "Timestamp": Timestamp,
    "Union": Union,
    "UnitRef": UnitRef,
    "bool": bool,
    "boolean": boolean,
    "date_type": date_type,
    "decimal_type": decimal_type,
    "duration_type": duration_type,
    "enum_type": enum_type,
    "float": float,
    "int": int,
    "integer": integer,
    "link": link,
    "rate_type": rate_type,
    "ref_type": ref_type,
    "str": str,
    "text": text,
    "timestamp_type": timestamp_type,
}

_ANNOTATION_ALLOWED_CALLABLES: frozenset[object] = frozenset(
    {
        Decimal,
        FieldMeta,
        Marking,
        UnitRef,
        boolean,
        date_type,
        decimal_type,
        duration_type,
        enum_type,
        integer,
        link,
        rate_type,
        ref_type,
        text,
        timestamp_type,
    }
)


def _is_safe_namespace_binding(val: object) -> bool:
    if val is None or isinstance(val, (str, int, bool, Decimal, UnitRef, Marking, FieldMeta, PropertyTypeSpec, LinkSpec)):
        return True
    if isinstance(val, type) and issubclass(val, (StrEnum, Concept)):
        return True
    if val in _ANNOTATION_ALLOWLIST_NAMES.values():
        return True
    origin = get_origin(val)
    return origin in (Annotated, Link, Field, Quantity, Money, Rate, Literal, Union, types.UnionType)


MAX_ANNOTATION_DEPTH: int = 64

_ANNOTATION_PARSE_ACTIVE: contextvars.ContextVar[bool] = contextvars.ContextVar(
    "_norn_annotation_parse_active",
    default=False,
)


def is_annotation_parse_active() -> bool:
    """Return True only while `resolve_annotation` is executing its internal `ast.parse`."""
    return _ANNOTATION_PARSE_ACTIVE.get()


def _eval_annotation_ast(
    node: ast.AST,
    namespace: Mapping[str, object],
    span: SourceSpan | None,
    *,
    depth: int = 0,
) -> Any:
    """Safely evaluate a field annotation AST node without calling `eval()`."""
    if depth > MAX_ANNOTATION_DEPTH:
        raise NornTracingError(
            "NORN-PY-UNSAFE-ANNOTATION",
            f"field annotation exceeds maximum nesting depth ({MAX_ANNOTATION_DEPTH})",
            span,
        )
    if isinstance(node, ast.Constant):
        if isinstance(node.value, float):
            raise NornFloatRefusalError(node.value, context="field annotation")
        if node.value is None or isinstance(node.value, (str, int, bool)):
            return node.value
        raise NornTracingError(
            "NORN-PY-UNSAFE-ANNOTATION",
            f"unsupported constant {node.value!r} in field annotation",
            span,
        )

    if isinstance(node, ast.UnaryOp) and isinstance(node.op, (ast.USub, ast.UAdd)):
        operand = _eval_annotation_ast(node.operand, namespace, span, depth=depth + 1)
        if isinstance(operand, int) and not isinstance(operand, bool):
            return -operand if isinstance(node.op, ast.USub) else +operand
        raise NornTracingError(
            "NORN-PY-UNSAFE-ANNOTATION",
            "unary +/- in annotations is only permitted on integer literals",
            span,
        )

    if isinstance(node, ast.Name):
        if node.id.startswith("__"):
            raise NornTracingError(
                "NORN-PY-UNSAFE-ANNOTATION",
                f"dunder name {node.id!r} is forbidden in field annotations",
                span,
            )
        if node.id in _ANNOTATION_ALLOWLIST_NAMES:
            resolved = _ANNOTATION_ALLOWLIST_NAMES[node.id]
            if resolved is float:
                raise NornFloatRefusalError(0.0, context="field annotation")
            return resolved
        if node.id in namespace:
            local_val = namespace[node.id]
            if _is_safe_namespace_binding(local_val):
                return local_val
            raise NornTracingError(
                "NORN-PY-UNSAFE-ANNOTATION",
                f"symbol {node.id!r} (type {type(local_val).__name__}) is not an admitted annotation binding",
                span,
            )
        raise NornTracingError(
            "NORN-PY-UNSAFE-ANNOTATION",
            f"unknown symbol {node.id!r} in field annotation",
            span,
        )

    if isinstance(node, ast.Attribute):
        if node.attr.startswith("__"):
            raise NornTracingError(
                "NORN-PY-UNSAFE-ANNOTATION",
                f"dunder attribute .{node.attr} is forbidden in field annotations",
                span,
            )
        owner = _eval_annotation_ast(node.value, namespace, span, depth=depth + 1)
        if isinstance(owner, type) and issubclass(owner, StrEnum) and node.attr in owner.__members__:
            return owner.__members__[node.attr]
        raise NornTracingError(
            "NORN-PY-UNSAFE-ANNOTATION",
            f"attribute access .{node.attr} is only allowed on StrEnum members in field annotations",
            span,
        )

    if isinstance(node, ast.BinOp) and isinstance(node.op, ast.BitOr):
        left = _eval_annotation_ast(node.left, namespace, span, depth=depth + 1)
        right = _eval_annotation_ast(node.right, namespace, span, depth=depth + 1)
        union_form: Any = Union
        if left is None:
            return union_form[right, None]
        if right is None:
            return union_form[left, None]
        raise NornTracingError(
            "NORN-PY-UNSAFE-ANNOTATION",
            "union `|` in field annotations is only permitted for nullable types (`T | None`)",
            span,
        )

    if isinstance(node, ast.Subscript):
        base = _eval_annotation_ast(node.value, namespace, span, depth=depth + 1)
        slice_node = node.slice
        args_tuple = (
            tuple(_eval_annotation_ast(elt, namespace, span, depth=depth + 1) for elt in slice_node.elts)
            if isinstance(slice_node, ast.Tuple)
            else (_eval_annotation_ast(slice_node, namespace, span, depth=depth + 1),)
        )
        base_any: Any = base
        if base is Annotated:
            if len(args_tuple) < 2:
                raise NornTracingError("NORN-PY-UNSAFE-ANNOTATION", "Annotated requires a base type and metadata", span)
            return base_any[args_tuple]
        if base is Optional:
            if len(args_tuple) != 1:
                raise NornTracingError("NORN-PY-UNSAFE-ANNOTATION", "Optional expects 1 type argument", span)
            union_form_opt: Any = Union
            return union_form_opt[args_tuple[0], None]
        if base is Union:
            return base_any[args_tuple]
        if base is Literal:
            return base_any[args_tuple if len(args_tuple) > 1 else args_tuple[0]]
        if base in (Field, Link, Quantity, Money):
            if len(args_tuple) != 1:
                raise NornTracingError("NORN-PY-UNSAFE-ANNOTATION", f"{base.__name__} expects 1 type argument", span)
            return base_any[args_tuple[0]]
        if base is Rate:
            if len(args_tuple) != 2:
                raise NornTracingError("NORN-PY-UNSAFE-ANNOTATION", "Rate expects 2 type arguments", span)
            return base_any[args_tuple[0], args_tuple[1]]
        raise NornTracingError(
            "NORN-PY-UNSAFE-ANNOTATION",
            f"subscript on {base!r} is not admitted in field annotations",
            span,
        )

    if isinstance(node, ast.Call):
        for arg in node.args:
            if isinstance(arg, ast.Starred):
                raise NornTracingError("NORN-PY-UNSAFE-ANNOTATION", "starred args forbidden in annotations", span)
        for kw in node.keywords:
            if kw.arg is None:
                raise NornTracingError("NORN-PY-UNSAFE-ANNOTATION", "kwargs unpacking forbidden in annotations", span)
        callee = _eval_annotation_ast(node.func, namespace, span, depth=depth + 1)
        if callee not in _ANNOTATION_ALLOWED_CALLABLES:
            raise NornTracingError(
                "NORN-PY-UNSAFE-ANNOTATION",
                f"call to {callee!r} is not an admitted Norn schema annotation constructor",
                span,
            )
        pos_args = [_eval_annotation_ast(a, namespace, span, depth=depth + 1) for a in node.args]
        kw_args = {
            kw.arg: _eval_annotation_ast(kw.value, namespace, span, depth=depth + 1)
            for kw in node.keywords
            if kw.arg is not None
        }
        try:
            result = callee(*pos_args, **kw_args)
        except (NornTracingError, NornFloatRefusalError):
            raise
        except (InvalidOperation, TypeError, ValueError) as exc:
            raise NornTracingError(
                "NORN-PY-UNSAFE-ANNOTATION",
                f"invalid annotation constructor call {callee!r}: {exc}",
                span,
            ) from exc
        if isinstance(result, Decimal):
            format_exact_decimal(result, span)
        return result

    raise NornTracingError(
        "NORN-PY-UNSAFE-ANNOTATION",
        f"unsupported syntax `{type(node).__name__}` in field annotation",
        span,
    )


def resolve_annotation(raw_annotation: Any, namespace: Mapping[str, object], span: SourceSpan | None) -> Any:
    """Resolve a raw annotation (string or type object) without ever calling `eval()`."""
    if isinstance(raw_annotation, str):
        token = _ANNOTATION_PARSE_ACTIVE.set(True)
        try:
            expr_tree = ast.parse(raw_annotation, filename="<norn-annotation>", mode="eval")
        except SyntaxError as exc:
            raise NornTracingError(
                "NORN-PY-UNSAFE-ANNOTATION",
                f"invalid annotation syntax {raw_annotation!r}: {exc.msg}",
                span,
            ) from exc
        finally:
            _ANNOTATION_PARSE_ACTIVE.reset(token)
        return _eval_annotation_ast(expr_tree.body, namespace, span)
    return raw_annotation


# ------------------------------------------------------------------ Annotation Consistency Validation


def _unwrap_nullable(annotation: Any) -> tuple[Any, bool]:
    origin = get_origin(annotation)
    if origin in (Union, types.UnionType):
        raw_args = get_args(annotation)
        args = [a for a in raw_args if a is not type(None)]
        has_none = len(args) < len(raw_args)
        if has_none and len(args) == 1:
            return args[0], True
    return annotation, False


def _unwrap_field_wrapper(annotation: Any) -> tuple[Any, bool]:
    """Unwrap `Field[T]` (and nullable `T | None` inside `Field[...]`)."""
    if get_origin(annotation) is Field:
        inner_args = get_args(annotation)
        if len(inner_args) == 1:
            return _unwrap_nullable(inner_args[0])
    return annotation, False


def _extract_literal_tag(
    type_arg: Any,
    kind_label: str,
    field_name: str,
    span: SourceSpan | None,
) -> str:
    if get_origin(type_arg) is Literal:
        lit_args = get_args(type_arg)
        if len(lit_args) == 1 and isinstance(lit_args[0], str) and lit_args[0]:
            return lit_args[0]
    raise NornTracingError(
        "NORN-PY-TYPE-MISMATCH",
        f"field {field_name!r} {kind_label} type argument must be a single non-empty string Literal[...], "
        f"got {type_arg!r}",
        span,
    )


_SIMPLE_SPEC_BASE_TYPES: dict[type[object], tuple[tuple[type[object], ...], str, str]] = {
    _TextSpec: ((str,), "text() metadata", "str"),
    _IntegerSpec: ((int,), "integer() metadata", "int"),
    _BooleanSpec: ((bool,), "boolean() metadata", "bool"),
    _TimestampSpec: ((Timestamp, str), "timestamp_type()", "Timestamp or str"),
    _DateSpec: ((Date, str), "date_type()", "Date or str"),
    _DurationSpec: ((Duration, str), "duration_type()", "Duration or str"),
}


def _validate_base_matches_spec(
    field_name: str,
    base: Any,
    spec: PropertyTypeSpec,
    span: SourceSpan | None,
) -> None:
    """Validate `Annotated[base_type, spec]` consistency at elaboration time."""
    if base is float:
        raise NornFloatRefusalError(0.0, context=f"field annotation {field_name!r}")

    inner = spec._inner
    origin = get_origin(base)
    args = get_args(base)

    if isinstance(inner, _DecimalSpec):
        if base is Decimal:
            return
        if origin is Quantity:
            if inner.unit is None or inner.currency is not None or inner.per_unit is not None:
                raise NornTracingError(
                    "NORN-PY-TYPE-MISMATCH",
                    f"field {field_name!r} has base Quantity[...] but decimal spec has no matching unit",
                    span,
                )
            if len(args) != 1:
                raise NornTracingError(
                    "NORN-PY-TYPE-MISMATCH",
                    f"field {field_name!r} Quantity[...] requires exactly 1 unit type argument",
                    span,
                )
            unit_tag = _extract_literal_tag(args[0], "Quantity[U]", field_name, span)
            if unit_tag != inner.unit.code:
                raise NornTracingError(
                    "NORN-PY-TYPE-MISMATCH",
                    f"field {field_name!r} declares Quantity[Literal[{unit_tag!r}]] "
                    f"which conflicts with UnitRef code {inner.unit.code!r}",
                    span,
                )
            return
        if origin is Money:
            if inner.currency is None or inner.per_unit is not None or inner.unit is not None:
                raise NornTracingError(
                    "NORN-PY-TYPE-MISMATCH",
                    f"field {field_name!r} has base Money[...] but decimal spec must declare currency "
                    "(without unit/per_unit)",
                    span,
                )
            if len(args) != 1:
                raise NornTracingError(
                    "NORN-PY-TYPE-MISMATCH",
                    f"field {field_name!r} Money[...] requires exactly 1 currency type argument",
                    span,
                )
            currency_tag = _extract_literal_tag(args[0], "Money[C]", field_name, span)
            if currency_tag != inner.currency:
                raise NornTracingError(
                    "NORN-PY-TYPE-MISMATCH",
                    f"field {field_name!r} declares Money[Literal[{currency_tag!r}]] "
                    f"which conflicts with currency {inner.currency!r}",
                    span,
                )
            return
        if origin is Rate:
            if inner.currency is None or inner.per_unit is None or inner.unit is not None:
                raise NornTracingError(
                    "NORN-PY-TYPE-MISMATCH",
                    f"field {field_name!r} has base Rate[...] but spec must use rate_type(currency=..., per_unit=...)",
                    span,
                )
            if len(args) != 2:
                raise NornTracingError(
                    "NORN-PY-TYPE-MISMATCH",
                    f"field {field_name!r} Rate[C, U] requires currency and unit type arguments",
                    span,
                )
            currency_tag = _extract_literal_tag(args[0], "Rate[C, U] currency", field_name, span)
            unit_tag = _extract_literal_tag(args[1], "Rate[C, U] unit", field_name, span)
            if currency_tag != inner.currency:
                raise NornTracingError(
                    "NORN-PY-TYPE-MISMATCH",
                    f"field {field_name!r} declares Rate[Literal[{currency_tag!r}], ...] "
                    f"which conflicts with currency {inner.currency!r}",
                    span,
                )
            if unit_tag != inner.per_unit.code:
                raise NornTracingError(
                    "NORN-PY-TYPE-MISMATCH",
                    f"field {field_name!r} declares Rate[..., Literal[{unit_tag!r}]] "
                    f"which conflicts with per_unit code {inner.per_unit.code!r}",
                    span,
                )
            return
        raise NornTracingError(
            "NORN-PY-TYPE-MISMATCH",
            f"field {field_name!r} has base type {base!r} which is incompatible with decimal/rate metadata; "
            "use Decimal, Quantity[...], Money[...], or Rate[..., ...]",
            span,
        )

    simple_spec = _SIMPLE_SPEC_BASE_TYPES.get(type(inner))
    if simple_spec is not None:
        allowed_bases, spec_label, expected_hint = simple_spec
        if base not in allowed_bases:
            raise NornTracingError(
                "NORN-PY-TYPE-MISMATCH",
                f"field {field_name!r} has base type {base!r} which is incompatible with {spec_label}; use {expected_hint}",
                span,
            )
        return

    if isinstance(inner, _EnumSpec):
        if base is not inner.enum_cls:
            raise NornTracingError(
                "NORN-PY-TYPE-MISMATCH",
                f"field {field_name!r} has base type {base!r} which does not match enum_type({inner.enum_cls.__name__})",
                span,
            )
        return

    if isinstance(inner, _RefSpec):
        if origin is Link:
            raise NornTracingError(
                "NORN-PY-TYPE-MISMATCH",
                f"field {field_name!r} cannot combine Link[...] with ref_type(); use link(...)",
                span,
            )
        return


def _validate_base_matches_link(
    field_name: str,
    base: Any,
    spec: LinkSpec,
    span: SourceSpan | None,
) -> None:
    if get_origin(base) is not Link:
        raise NornTracingError(
            "NORN-PY-TYPE-MISMATCH",
            f"field {field_name!r} has link(...) metadata but base type is {base!r}; expected Link[Target]",
            span,
        )
    link_args = get_args(base)
    if len(link_args) == 1:
        target_arg = link_args[0]
        if isinstance(target_arg, type) and issubclass(target_arg, Concept) and target_arg.__norn_name__:
            if target_arg.__norn_name__ != spec.target:
                raise NornTracingError(
                    "NORN-PY-TYPE-MISMATCH",
                    f"field {field_name!r} declares Link[{target_arg.__name__}] ({target_arg.__norn_name__!r}) "
                    f"which conflicts with link target {spec.target!r}",
                    span,
                )


def _extract_property_and_meta(
    field_name: str, annotation: Any, span: SourceSpan | None
) -> tuple[PropertyTypeSpec | LinkSpec, bool, tuple[str, ...], FieldMeta]:
    unwrapped, nullable = _unwrap_nullable(annotation)
    markings: list[str] = []
    explicit_meta: FieldMeta | None = None
    explicit_prop: PropertyTypeSpec | None = None
    explicit_link: LinkSpec | None = None

    if get_origin(unwrapped) is Annotated:
        args = get_args(unwrapped)
        base = args[0]
        for extra in args[1:]:
            if isinstance(extra, Marking):
                markings.append(extra.id)
            elif type(extra) is PropertyTypeSpec:
                if explicit_prop is not None:
                    raise NornTracingError(
                        "NORN-PY-SCHEMA",
                        f"field {field_name!r} declares multiple PropertyTypeSpec annotations",
                        span,
                    )
                explicit_prop = extra
            elif isinstance(extra, LinkSpec):
                if explicit_link is not None:
                    raise NornTracingError(
                        "NORN-PY-SCHEMA",
                        f"field {field_name!r} declares multiple link() annotations",
                        span,
                    )
                explicit_link = extra
            elif isinstance(extra, FieldMeta):
                if explicit_meta is not None:
                    raise NornTracingError(
                        "NORN-PY-SCHEMA",
                        f"field {field_name!r} declares multiple FieldMeta annotations",
                        span,
                    )
                explicit_meta = extra
            else:
                raise NornTracingError(
                    "NORN-PY-TYPE-MISMATCH",
                    f"field {field_name!r} has unsupported Annotated metadata {extra!r} "
                    f"(expected Marking, FieldMeta, link(...), or a PropertyTypeSpec constructor)",
                    span,
                )
        base, inner_nullable = _unwrap_nullable(base)
        nullable = nullable or inner_nullable
    else:
        base = unwrapped

    meta = explicit_meta if explicit_meta is not None else FieldMeta()

    base, field_nullable = _unwrap_field_wrapper(base)
    nullable = nullable or field_nullable

    if explicit_link is not None and explicit_prop is not None:
        raise NornTracingError(
            "NORN-PY-SCHEMA",
            f"field {field_name!r} cannot combine both link(...) and a PropertyTypeSpec",
            span,
        )

    if explicit_link is not None:
        _validate_base_matches_link(field_name, base, explicit_link, span)
        return explicit_link, nullable, tuple(markings), meta

    if explicit_prop is not None:
        _validate_base_matches_spec(field_name, base, explicit_prop, span)
        return explicit_prop, nullable, tuple(markings), meta

    if base is float:
        raise NornFloatRefusalError(0.0, context=f"field annotation {field_name!r}")
    if base is str:
        return text(), nullable, tuple(markings), meta
    if base is int:
        return integer(), nullable, tuple(markings), meta
    if base is bool:
        return boolean(), nullable, tuple(markings), meta
    if isinstance(base, type) and issubclass(base, StrEnum):
        return enum_type(base), nullable, tuple(markings), meta

    raise NornTracingError(
        "NORN-PY-SCHEMA",
        f"field {field_name!r} has unsupported type annotation {annotation!r}. "
        "Use str, int, bool, StrEnum, Annotated[..., decimal_type(...)|date_type()|timestamp_type()|ref_type(...)], "
        "or Annotated[Field[Link[Target]], link(Target, ...)].",
        span,
    )


# ------------------------------------------------------------------ Concept / Interface Base Class


class Concept:
    """Base class for Norn EntityIR v2 object types and interfaces.

    Subclasses use class keyword arguments to declare canonical metadata:
        class Employee(Concept, name="hr.Employee", title="Employee", version="1.0.0"):
            id: Field[str]
    """

    __norn_name__: str = ""
    __norn_title__: str = ""
    __norn_version__: str = ""
    __norn_is_interface__: bool = False
    __norn_role__: RoleKind | None = None
    __norn_description__: str | None = None
    __norn_extends__: str | None = None
    __norn_implements__: tuple[str, ...] = ()
    __norn_interface_extends__: tuple[str, ...] = ()
    __norn_markings__: tuple[str, ...] = ()
    __norn_identity__: tuple[KeySpec, ...] = ()
    __norn_dependencies__: tuple[DependencyPin, ...] = ()
    __norn_field_defaults__: dict[str, object] = {}
    __norn_span__: SourceSpan | None = None

    def __init_subclass__(
        cls,
        *,
        name: str = "",
        title: str = "",
        version: str = "",
        interface: bool = False,
        role: RoleKind | None = None,
        description: str | None = None,
        extends: type[Concept] | str | None = None,
        implements: Sequence[type[Concept] | str] = (),
        interface_extends: Sequence[type[Concept] | str] = (),
        markings: Sequence[str] = (),
        identity: Sequence[KeySpec] = (),
        dependencies: Sequence[DependencyPin] = (),
    ) -> None:
        super().__init_subclass__()
        if not name:
            # Reset inherited Norn metadata so anonymous helper subclasses are never
            # re-emitted under a parent Concept's artifact id.
            cls.__norn_name__ = ""
            cls.__norn_field_defaults__ = {}
            return
        span = capture_caller_span(skip=2)
        if not OBJECT_TYPE_NAME_RE.match(name):
            raise NornTracingError(
                "NORN-FLOW-LITERAL",
                f"type name {name!r} must be a dotted lower-case namespace and a PascalCase name (e.g. hr.Employee)",
                span,
            )
        cls.__norn_name__ = name
        cls.__norn_title__ = title or name.rsplit(".", 1)[-1]
        cls.__norn_version__ = version
        cls.__norn_is_interface__ = interface
        cls.__norn_role__ = role
        cls.__norn_description__ = description
        cls.__norn_extends__ = (
            None if extends is None else (extends if isinstance(extends, str) else extends.__norn_name__)
        )
        cls.__norn_implements__ = tuple(
            i if isinstance(i, str) else i.__norn_name__ for i in implements
        )
        cls.__norn_interface_extends__ = tuple(
            p if isinstance(p, str) else p.__norn_name__ for p in interface_extends
        )
        cls.__norn_markings__ = tuple(markings)
        cls.__norn_identity__ = tuple(identity)
        cls.__norn_dependencies__ = tuple(dependencies)
        cls.__norn_span__ = span

        # Install Field descriptors on every declared field so `cls.sym("self").<field>`
        # returns a real closed Expr node while capturing literal defaults separately.
        field_defaults: dict[str, object] = {}
        own_annotations = cls.__dict__.get("__annotations__", {})
        for field_name in own_annotations:
            if field_name.startswith("_"):
                continue
            existing = cls.__dict__.get(field_name, OMIT)
            if isinstance(existing, Field):
                if existing._default is not OMIT:
                    field_defaults[field_name] = existing._default
                if not existing._name:
                    existing._name = field_name
            else:
                if existing is not OMIT:
                    field_defaults[field_name] = existing
                descriptor: Field[Any] = Field(name=field_name)
                setattr(cls, field_name, descriptor)
        cls.__norn_field_defaults__ = field_defaults

    @classmethod
    def sym(cls, root: str = "self") -> Self:
        """Create a typed symbolic instance of this Concept for tracing predicates."""
        inst = object.__new__(cls)
        object.__setattr__(inst, "_norn_sym_root", root)
        return inst

    if not TYPE_CHECKING:
        def __getattr__(self, name: str) -> NoReturn:
            span = capture_caller_span(skip=2)
            raise NornTracingError(
                "NORN-PY-UNKNOWN-FIELD",
                f"{type(self).__norn_name__} has no declared property or link {name!r} "
                f"(declared: {sorted(type(self).declared_field_names())})",
                span,
            )

    def __bool__(self) -> NoReturn:
        raise NornTracingError(
            "NORN-PY-BOOL-COERCION",
            "Cannot evaluate symbolic Concept proxy as a Python bool.",
            capture_caller_span(),
        )

    @classmethod
    def declared_field_names(cls) -> tuple[str, ...]:
        names: list[str] = []
        for base in reversed(cls.__mro__):
            if base is object or base is Concept:
                continue
            for field_name in base.__dict__.get("__annotations__", {}):
                if not field_name.startswith("_") and field_name not in names:
                    names.append(field_name)
        return tuple(names)

    @classmethod
    def lower(
        cls,
        options: EnvelopeOptions,
        *,
        globalns: Mapping[str, object] | None = None,
    ) -> JsonValue:
        """Lower this Concept or Interface to canonical EntityIR v2 JSON (zero `eval()`)."""
        span = cls.__norn_span__
        namespace_bindings: dict[str, object] = dict(globalns) if globalns is not None else {}
        own_annotations: dict[str, Any] = dict(cls.__dict__.get("__annotations__", {}))

        # Validate interface vs object-type structural constraints up front (never silently drop!).
        if cls.__norn_is_interface__:
            if cls.__norn_identity__:
                raise NornTracingError(
                    "NORN-PY-SCHEMA",
                    f"interface {cls.__norn_name__!r} cannot declare identity keys",
                    span,
                )
            if cls.__norn_extends__ is not None:
                raise NornTracingError(
                    "NORN-PY-SCHEMA",
                    f"interface {cls.__norn_name__!r} must use `interface_extends=(...)`, not `extends=...`",
                    span,
                )
            if cls.__norn_implements__:
                raise NornTracingError(
                    "NORN-PY-SCHEMA",
                    f"interface {cls.__norn_name__!r} cannot declare `implements`; use `interface_extends`",
                    span,
                )
        else:
            if cls.__norn_interface_extends__:
                raise NornTracingError(
                    "NORN-PY-SCHEMA",
                    f"object type {cls.__norn_name__!r} cannot declare `interface_extends`; use `implements`",
                    span,
                )

        properties_ir: list[dict[str, object]] = []
        links_ir: list[dict[str, object]] = []
        lifecycles_ir: list[dict[str, object]] = []
        invariants_ir: list[dict[str, object]] = []

        sym = cls.sym("self")

        for field_name, raw_ann in own_annotations.items():
            if field_name.startswith("_"):
                continue
            resolved_ann = resolve_annotation(raw_ann, namespace_bindings, span)
            spec, nullable, field_markings, meta = _extract_property_and_meta(field_name, resolved_ann, span)
            if isinstance(spec, LinkSpec):
                if field_markings:
                    raise NornTracingError(
                        "NORN-PY-SCHEMA",
                        f"link {cls.__norn_name__}.{field_name} cannot declare property markings {list(field_markings)!r}",
                        span,
                    )
                class_link_default = cls.__norn_field_defaults__.get(field_name, OMIT)
                if meta.default is not OMIT or class_link_default is not OMIT:
                    raise NornTracingError(
                        "NORN-PY-SCHEMA",
                        f"link {cls.__norn_name__}.{field_name} cannot declare a default value",
                        span,
                    )
                if meta.description is not None:
                    raise NornTracingError(
                        "NORN-PY-SCHEMA",
                        f"link {cls.__norn_name__}.{field_name} must pass description=... to link(...), not FieldMeta",
                        span,
                    )
                if nullable and spec.required:
                    raise NornTracingError(
                        "NORN-PY-SCHEMA",
                        f"link {cls.__norn_name__}.{field_name} cannot be both nullable (`| None`) and `required=True`",
                        span,
                    )
                links_ir.append(spec.to_ir(field_name, for_interface=cls.__norn_is_interface__, span=span))
                continue

            class_default = cls.__norn_field_defaults__.get(field_name, OMIT)
            if meta.default is not OMIT and class_default is not OMIT:
                raise NornTracingError(
                    "NORN-PY-SCHEMA",
                    f"field {cls.__norn_name__}.{field_name} cannot declare both "
                    "FieldMeta(default=...) and a class-body default",
                    span,
                )
            default_val: object = OMIT
            raw_default = meta.default if meta.default is not OMIT else class_default
            if raw_default is not OMIT:
                if cls.__norn_is_interface__:
                    raise NornTracingError(
                        "NORN-FLOW-LITERAL",
                        f"interface property {field_name!r} cannot declare a default",
                        span,
                    )
                if isinstance(raw_default, float):
                    raise NornFloatRefusalError(raw_default, context=f"default on {cls.__norn_name__}.{field_name}")
                if isinstance(raw_default, Decimal):
                    default_val = format_exact_decimal(raw_default, span)
                elif isinstance(raw_default, StrEnum):
                    default_val = raw_default.value
                elif isinstance(raw_default, (str, bool)):
                    default_val = raw_default
                elif isinstance(raw_default, int):
                    default_val = str(raw_default)
                else:
                    raise NornTracingError(
                        "NORN-PY-SCHEMA",
                        f"unsupported default {raw_default!r} on {cls.__norn_name__}.{field_name}",
                        span,
                    )

            properties_ir.append(
                {
                    "constraints": {"nullable": True} if nullable else OMIT,
                    "default": default_val,
                    "description": meta.description if meta.description is not None else OMIT,
                    "id": field_name,
                    "markings": omit_empty(field_markings),
                    "type": spec.ir,
                }
            )

        for member in cls.__dict__.values():
            if isinstance(member, LifecycleSpec):
                if cls.__norn_is_interface__:
                    raise NornTracingError(
                        "NORN-PY-INTERFACE-LIFECYCLE",
                        f"interface {cls.__norn_name__!r} cannot declare lifecycles "
                        "(EntityIR v2 InterfaceDef has no lifecycles slot; declare lifecycles on implementing object types)",
                        span,
                    )
                transitions_ir: list[dict[str, object]] = []
                for tr in member.transitions:
                    guard_zens = [
                        trace_bool_callback(
                            g,
                            sym,
                            context=f"lifecycle transition {cls.__norn_name__}.{member.property}.{tr.id} guard",
                            span=span,
                        ).zen
                        for g in tr.guards
                    ]
                    transitions_ir.append(
                        {
                            "description": tr.description if tr.description is not None else OMIT,
                            "from": [s.value for s in tr.from_],
                            "guardRefusalCode": (
                                tr.guard_refusal_code if tr.guard_refusal_code is not None else OMIT
                            ),
                            "guards": omit_empty(guard_zens),
                            "id": tr.id,
                            "to": tr.to.value,
                        }
                    )
                lifecycles_ir.append(
                    {
                        "description": member.description if member.description is not None else OMIT,
                        "immutable": omit_empty([s.value for s in member.immutable]),
                        "initial": [s.value for s in member.initial],
                        "property": member.property,
                        "terminal": omit_empty([s.value for s in member.terminal]),
                        "transitions": omit_empty(transitions_ir),
                    }
                )
            elif isinstance(member, InvariantSpec):
                inv_expr = trace_bool_callback(
                    member.fn,
                    sym,
                    context=f"invariant {cls.__norn_name__}.{member.id}",
                    span=member.span or span,
                )
                inv_obj: dict[str, object] = {
                    "description": member.description if member.description is not None else OMIT,
                    "expression": inv_expr.zen,
                    "id": member.id,
                    "message": member.message,
                }
                if member.classification != "integrity":
                    inv_obj["classification"] = member.classification
                invariants_ir.append(inv_obj)

        namespace = cls.__norn_name__.rsplit(".", 1)[0]
        if cls.__norn_is_interface__:
            payload: dict[str, object] = {
                "entityVersion": "2.0",
                "interface": {
                    "description": cls.__norn_description__ if cls.__norn_description__ is not None else OMIT,
                    "extends": omit_empty(cls.__norn_interface_extends__),
                    "invariants": omit_empty(invariants_ir),
                    "links": omit_empty(links_ir),
                    "markings": omit_empty(cls.__norn_markings__),
                    "name": cls.__norn_name__,
                    "properties": omit_empty(properties_ir),
                    "role": cls.__norn_role__ if cls.__norn_role__ is not None else OMIT,
                },
            }
            suffix = "interface"
        else:
            payload = {
                "entityVersion": "2.0",
                "objectType": {
                    "description": cls.__norn_description__ if cls.__norn_description__ is not None else OMIT,
                    "extends": cls.__norn_extends__ if cls.__norn_extends__ is not None else OMIT,
                    "identity": omit_empty([k.to_ir() for k in cls.__norn_identity__]),
                    "implements": omit_empty(cls.__norn_implements__),
                    "invariants": omit_empty(invariants_ir),
                    "lifecycles": omit_empty(lifecycles_ir),
                    "links": omit_empty(links_ir),
                    "markings": omit_empty(cls.__norn_markings__),
                    "name": cls.__norn_name__,
                    "properties": omit_empty(properties_ir),
                    "role": cls.__norn_role__ if cls.__norn_role__ is not None else OMIT,
                },
            }
            suffix = "entity"

        return build_envelope(
            kind="entity",
            artifact_id=f"{cls.__norn_name__}.{suffix}",
            namespace=namespace,
            name=cls.__norn_title__,
            version=cls.__norn_version__,
            payload=payload,
            options=options,
            dependencies=[d.to_ir() for d in cls.__norn_dependencies__],
        )


def collect_artifacts(module_ns: dict[str, object], options: EnvelopeOptions) -> list[JsonValue]:
    """Collect and lower every declared Concept, MarkingsCatalog, and LawDecl in definition order.

    Note: `FnRef`, `PackManifest`, and `DeploymentSpec` are spike prototypes outside the
    artifact collector and are not emitted into the EntityIR/LawIR artifact stream.
    """
    artifacts: list[JsonValue] = []
    seen: set[int] = set()
    for value in module_ns.values():
        if id(value) in seen:
            continue
        if isinstance(value, MarkingsCatalog):
            seen.add(id(value))
            artifacts.append(value.lower(options))
        elif isinstance(value, type) and issubclass(value, Concept) and value is not Concept and value.__norn_name__:
            seen.add(id(value))
            artifacts.append(value.lower(options, globalns=module_ns))
        elif isinstance(value, LawDecl):
            seen.add(id(value))
            artifacts.append(value.lower(options))
    return artifacts


# ------------------------------------------------------------------ Composable Packs & Deployments (spike prototypes)


PackCategory = Literal[
    "foundation",
    "domain",
    "localization",
    "integration",
    "views",
    "company_overlay",
]
PackConflictPolicy = Literal["fail_closed", "explicit_precedence"]
CellHostKind = Literal["rivet", "local", "custom"]

_FORBIDDEN_SETTING_SECRET_TOKENS = ("secret", "password", "passwd", "token", "api_key", "private_key")
_ENDPOINT_KEY_RE = re.compile(r"^[a-z][a-z0-9._-]*@[0-9]+\.[0-9]+\.[0-9]+$")


@dataclass(frozen=True, slots=True)
class PackManifest:
    """Spike prototype (outside `collect_artifacts`): composable Norn Pack manifest."""

    id: str
    version: str
    category: PackCategory
    title: str
    requires: tuple[str, ...] = ()
    entities: tuple[type[Concept], ...] = ()
    laws: tuple[LawDecl, ...] = ()
    markings: tuple[MarkingsCatalog, ...] = ()
    functions: tuple[FnRef, ...] = ()
    description: str | None = None

    def to_ir(self) -> dict[str, object]:
        return {
            "category": self.category,
            "description": self.description if self.description is not None else OMIT,
            "entities": [e.__norn_name__ for e in self.entities],
            "functions": [f"{fn.id}@{fn.version}" for fn in self.functions],
            "id": self.id,
            "laws": [lw.id for lw in self.laws],
            "markings": [m.id for m in self.markings],
            "requires": list(self.requires),
            "title": self.title,
            "version": self.version,
        }


def compose_packs(
    *packs: PackManifest,
    conflict_policy: PackConflictPolicy = "fail_closed",
) -> dict[str, object]:
    """Spike prototype: compose packs conjunctively (`A ∧ B ∧ C`), failing closed on missing deps, cycles, or conflicts."""
    if conflict_policy not in ("fail_closed", "explicit_precedence"):
        raise NornTracingError(
            "NORN-PY-PACK-CONFLICT",
            f"unsupported pack conflict_policy {conflict_policy!r} (expected 'fail_closed' or 'explicit_precedence')",
        )
    by_id: dict[str, PackManifest] = {}
    for pack in packs:
        if pack.id in by_id:
            raise NornTracingError("NORN-PY-PACK-DUPLICATE", f"duplicate pack id {pack.id!r}")
        by_id[pack.id] = pack

    for pack in packs:
        for req in pack.requires:
            if req not in by_id:
                raise NornTracingError(
                    "NORN-PY-PACK-MISSING-DEP",
                    f"pack {pack.id!r} requires {req!r}, which is not in the composed set",
                )

    # Detect cyclic `requires` edges (iterative 3-color DFS).
    visit_state: dict[str, int] = {}  # 1 = visiting, 2 = visited
    for start_pack in packs:
        if visit_state.get(start_pack.id, 0) != 0:
            continue
        stack: list[tuple[str, int]] = [(start_pack.id, 0)]
        path: list[str] = [start_pack.id]
        visit_state[start_pack.id] = 1
        while stack:
            cur_id, idx = stack[-1]
            reqs = by_id[cur_id].requires
            if idx < len(reqs):
                stack[-1] = (cur_id, idx + 1)
                nxt = reqs[idx]
                nxt_state = visit_state.get(nxt, 0)
                if nxt_state == 1:
                    cycle_start = path.index(nxt)
                    cycle = " -> ".join([*path[cycle_start:], nxt])
                    raise NornTracingError("NORN-PY-PACK-CYCLE", f"cyclic pack dependency detected: {cycle}")
                if nxt_state == 0:
                    visit_state[nxt] = 1
                    path.append(nxt)
                    stack.append((nxt, 0))
            else:
                visit_state[cur_id] = 2
                stack.pop()
                path.pop()

    seen_entities: dict[str, str] = {}
    seen_laws: dict[str, str] = {}
    seen_markings: dict[str, str] = {}
    seen_functions: dict[str, str] = {}

    def _register(table: dict[str, str], item_id: str, owner: str, kind: str) -> None:
        if item_id in table and conflict_policy == "fail_closed":
            raise NornTracingError(
                "NORN-PY-PACK-CONFLICT",
                f"{kind} {item_id!r} declared by both {table[item_id]!r} and {owner!r}",
            )
        table[item_id] = owner

    for pack in packs:
        for entity in pack.entities:
            _register(seen_entities, entity.__norn_name__, pack.id, "entity")
        for law_decl in pack.laws:
            _register(seen_laws, law_decl.id, pack.id, "law")
        for catalog in pack.markings:
            _register(seen_markings, catalog.id, pack.id, "markings")
        for fn in pack.functions:
            _register(seen_functions, f"{fn.id}@{fn.version}", pack.id, "function")

    return {
        "conflictPolicy": conflict_policy,
        "entities": sorted(seen_entities.keys()),
        "functions": sorted(seen_functions.keys()),
        "laws": sorted(seen_laws.keys()),
        "markings": sorted(seen_markings.keys()),
        "packs": [p.to_ir() for p in packs],
    }


@dataclass(frozen=True, slots=True)
class SecretRef:
    """Spike prototype: reference to an external secret slot; never embeds a secret literal."""

    name: str
    description: str | None = None

    def to_ir(self) -> dict[str, object]:
        return {
            "description": self.description if self.description is not None else OMIT,
            "name": self.name,
        }


def _is_valid_worker_endpoint_url(url: str) -> bool:
    if not url or any(ord(ch) <= 0x20 or 0x7F <= ord(ch) <= 0x9F or ch in ("\\", "?", "#") for ch in url):
        return False
    try:
        parsed = urlsplit(url)
        port = parsed.port
    except ValueError:
        return False
    if port == 0 or parsed.query or parsed.fragment:
        return False
    if parsed.username is not None or parsed.password is not None or "@" in parsed.netloc:
        return False
    return (parsed.scheme == "https" and bool(parsed.hostname)) or (
        parsed.scheme == "http" and parsed.hostname in ("127.0.0.1", "localhost", "::1")
    )


@dataclass(frozen=True, slots=True)
class DeploymentSpec:
    """Spike prototype (outside `collect_artifacts`): live Norn Deployment binding packs to a cell host."""

    id: str
    org_id: str
    environment_id: str
    packs: tuple[PackManifest, ...]
    cell_host: CellHostKind = "rivet"
    worker_endpoints: Mapping[str, str] = field(default_factory=dict)
    secret_refs: tuple[SecretRef, ...] = ()
    initial_settings: Mapping[str, str | int | bool | Decimal] = field(default_factory=dict)

    def __post_init__(self) -> None:
        if (
            not isinstance(self.org_id, str)
            or not self.org_id.strip()
            or not isinstance(self.environment_id, str)
            or not self.environment_id.strip()
        ):
            raise NornTracingError(
                "NORN-PY-DEPLOYMENT-SCOPE",
                "DeploymentSpec requires non-empty org_id and environment_id (scope fails closed)",
            )
        compose_packs(*self.packs)
        frozen_endpoints: dict[str, str] = {}
        for endpoint_key, url in sorted(self.worker_endpoints.items()):
            if not isinstance(endpoint_key, str) or not _ENDPOINT_KEY_RE.match(endpoint_key):
                raise NornTracingError(
                    "NORN-PY-DEPLOYMENT-ENDPOINT",
                    f"worker endpoint key {endpoint_key!r} must have form '<fn_id>@<semver>'",
                )
            if not isinstance(url, str) or not _is_valid_worker_endpoint_url(url):
                raise NornTracingError(
                    "NORN-PY-DEPLOYMENT-ENDPOINT",
                    f"worker endpoint {endpoint_key!r} must use https:// (or loopback http:// for local dev), got {url!r}",
                )
            frozen_endpoints[endpoint_key] = url
        frozen_settings: dict[str, str | int | bool | Decimal] = {}
        for key, val in sorted(self.initial_settings.items()):
            lowered_key = key.lower()
            if any(tok in lowered_key for tok in _FORBIDDEN_SETTING_SECRET_TOKENS):
                raise NornTracingError(
                    "NORN-PY-RAW-SECRET",
                    f"setting {key!r} looks like a secret; declare it in secret_refs instead of initial_settings",
                )
            if isinstance(val, float):
                raise NornFloatRefusalError(val, context=f"DeploymentSpec.initial_settings[{key!r}]")
            if isinstance(val, Decimal):
                format_exact_decimal(val)
            elif type(val) not in (str, int, bool):
                raise NornTracingError(
                    "NORN-PY-SCHEMA",
                    f"DeploymentSpec.initial_settings[{key!r}] has unsupported type {type(val).__name__}",
                )
            frozen_settings[key] = val
        object.__setattr__(self, "worker_endpoints", MappingProxyType(frozen_endpoints))
        object.__setattr__(self, "initial_settings", MappingProxyType(frozen_settings))

    def to_ir(self) -> dict[str, object]:
        composed = compose_packs(*self.packs)
        serialized_settings: dict[str, object] = {
            k: (format_exact_decimal(v) if isinstance(v, Decimal) else v)
            for k, v in sorted(self.initial_settings.items())
        }
        return {
            "cellHost": self.cell_host,
            "composition": composed,
            "deploymentVersion": "0.1",
            "id": self.id,
            "initialSettings": serialized_settings,
            "scope": {
                "environmentId": self.environment_id,
                "orgId": self.org_id,
            },
            "secretRefs": [s.to_ir() for s in self.secret_refs],
            "workerEndpoints": dict(sorted(self.worker_endpoints.items())),
        }
