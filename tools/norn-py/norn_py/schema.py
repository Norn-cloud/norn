"""Typed Python authoring schema & lowering to canonical Norn IR (`irVersion: 0.1`).

Emits byte-identical `EntityIR v2`, `Markings`, `LawIR`, and `FnRef` declarations
matching `tools/norn-ts/src/builder/{entity,law}.ts`.
"""

from __future__ import annotations

import re
from collections.abc import Callable, Sequence
from dataclasses import dataclass, field
from decimal import Decimal
from enum import StrEnum
from typing import Annotated, Any, Literal, TypeVar, get_args, get_origin, get_type_hints

from .canonical import (
    OMIT,
    DependencyPin,
    EnvelopeOptions,
    JsonValue,
    NornFloatRefusalError,
    SourceSpan,
    build_envelope,
    omit_empty,
)
from .expr import BoolTag, Expr, NornTracingError, Sym, capture_caller_span

T = TypeVar("T")
S = TypeVar("S", bound=StrEnum)

OBJECT_TYPE_NAME_RE = re.compile(r"^[a-z][a-z0-9_]*(\.[a-z][a-z0-9_]*)*\.[A-Z][A-Za-z0-9]*$")

Rounding = Literal["refuse", "toward_zero", "half_even", "half_away_from_zero"]
TextFormat = Literal["email", "uri", "uuid"]
TimezonePolicy = Literal["utc", "offset"]
Multiplicity = Literal["one", "many"]
RoleKind = Literal["thing", "happening"]
MarkingSeverity = Literal["low", "moderate", "high", "critical"]


# ------------------------------------------------------------------ Value & Marking Annotations


@dataclass(frozen=True, slots=True)
class UnitRef:
    system: Literal["ucum", "qudt"]
    code: str

    def to_ir(self) -> dict[str, object]:
        return {"code": self.code, "system": self.system}


@dataclass(frozen=True, slots=True)
class Marking:
    """Attach a classification marking via `Annotated[str, Marking("pii.demographic")]`."""

    id: str


@dataclass(frozen=True, slots=True)
class PropertyTypeSpec:
    """Canonical EntityIR v2 PropertyType wire representation."""

    ir: dict[str, object]


def text(fmt: TextFormat | None = None) -> PropertyTypeSpec:
    return PropertyTypeSpec({"kind": "text"} if fmt is None else {"kind": "text", "format": fmt})


def integer() -> PropertyTypeSpec:
    return PropertyTypeSpec({"kind": "integer"})


def boolean() -> PropertyTypeSpec:
    return PropertyTypeSpec({"kind": "boolean"})


def date_type() -> PropertyTypeSpec:
    return PropertyTypeSpec({"kind": "date"})


def timestamp_type(timezone: TimezonePolicy = "utc") -> PropertyTypeSpec:
    return PropertyTypeSpec({"kind": "timestamp", "timezone": timezone})


def duration_type() -> PropertyTypeSpec:
    return PropertyTypeSpec({"kind": "duration"})


def ref_type(name: str) -> PropertyTypeSpec:
    return PropertyTypeSpec({"kind": "ref", "name": name})


def decimal_type(
    scale: int,
    *,
    unit: UnitRef | None = None,
    currency: str | None = None,
    rounding: Rounding | None = None,
) -> PropertyTypeSpec:
    if isinstance(scale, float) or scale < 0:
        raise NornTracingError("NORN-PY-SCHEMA", f"decimal scale must be a non-negative int, got {scale!r}")
    if unit is not None and currency is not None:
        raise NornTracingError("NORN-PY-SCHEMA", "decimal cannot specify both unit and currency")
    body: dict[str, object] = {"kind": "decimal", "scale": scale}
    if unit is not None:
        body["unit"] = unit.to_ir()
    if currency is not None:
        body["currencySource"] = {"links": [], "property": currency}
    if rounding is not None:
        body["rounding"] = rounding
    return PropertyTypeSpec(body)


def rate_type(
    scale: int,
    *,
    currency: str,
    per_unit: UnitRef,
    rounding: Rounding | None = None,
) -> PropertyTypeSpec:
    if isinstance(scale, float) or scale < 0:
        raise NornTracingError("NORN-PY-SCHEMA", f"rate scale must be a non-negative int, got {scale!r}")
    body: dict[str, object] = {
        "kind": "decimal",
        "scale": scale,
        "currencySource": {"links": [], "property": currency},
        "perUnit": per_unit.to_ir(),
    }
    if rounding is not None:
        body["rounding"] = rounding
    return PropertyTypeSpec(body)


def enum_type(enum_cls: type[StrEnum]) -> PropertyTypeSpec:
    return PropertyTypeSpec({"kind": "enum", "values": [{"value": member.value} for member in enum_cls]})


@dataclass(frozen=True, slots=True)
class FieldMeta:
    """Optional description or exact default on a Concept field."""

    description: str | None = None
    default: str | bool | Decimal | None = None


# ------------------------------------------------------------------ Links, Keys, Lifecycles, Invariants


@dataclass(frozen=True, slots=True)
class LinkSpec:
    """Typed link annotation: `employee: Annotated[Link[Employee], link(to="one", from_="many", required=True)]`."""

    target: str
    to: Multiplicity = "one"
    from_: Multiplicity = "many"
    required: bool = False
    inverse: str | None = None
    acyclic: bool = False
    description: str | None = None

    def to_ir(self, link_id: str, *, for_interface: bool = False) -> dict[str, object]:
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


class Link[T]:
    """Phantom marker for typed object links."""


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
    target_name = target if isinstance(target, str) else target.__norn_name__
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
    guards: tuple[Callable[[Sym[Any]], Expr[BoolTag]], ...] = ()
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
    guards: Sequence[Callable[[Sym[Any]], Expr[BoolTag]]] = (),
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
    fn: Callable[[Sym[Any]], Expr[BoolTag]]
    description: str | None = None
    classification: Literal["integrity", "quality"] = "integrity"
    span: SourceSpan | None = None


def invariant(
    id: str,
    *,
    message: str,
    description: str | None = None,
    classification: Literal["integrity", "quality"] = "integrity",
) -> Callable[[Callable[[Sym[Any]], Expr[BoolTag]]], InvariantSpec]:
    """Decorator attaching a symbolically traced per-record invariant to a Concept/Interface."""
    span = capture_caller_span()

    def wrap(fn: Callable[[Sym[Any]], Expr[BoolTag]]) -> InvariantSpec:
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
class MarkingDecl:
    id: str
    label: str
    severity: MarkingSeverity
    category: str
    mask: dict[str, object]

    def to_ir(self) -> dict[str, object]:
        return {
            "category": self.category,
            "id": self.id,
            "label": self.label,
            "mask": dict(self.mask),
            "severity": self.severity,
        }


def mask_redacted() -> dict[str, object]:
    return {"kind": "redacted"}


def mask_null() -> dict[str, object]:
    return {"kind": "null"}


def mask_last_n(count: int) -> dict[str, object]:
    if isinstance(count, float) or count < 0:
        raise NornTracingError("NORN-PY-SCHEMA", f"mask_last_n count must be a non-negative int, got {count!r}")
    return {"count": count, "kind": "last-n"}


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


# ------------------------------------------------------------------ Function Handle (perform)


@dataclass(frozen=True, slots=True)
class FnRef:
    """Declarative handle for an external worker / solver behind `perform` (never runs here)."""

    id: str
    version: str
    input_type: str
    output_type: str
    span: SourceSpan | None = field(default_factory=capture_caller_span)


def fn_ref(*, id: str, version: str, input_type: str, output_type: str) -> FnRef:
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
    predicate: Callable[[Sym[Any]], Expr[BoolTag]]
    severity: Literal["error", "warning"] = "error"
    modality: Literal["always", "never", "eventually"] = "always"
    scope_root_kind: Literal["company", "app", "cell", "entity"] = "entity"
    scope_root_id: str | None = None
    dependencies: tuple[DependencyPin, ...] = ()
    span: SourceSpan | None = field(default_factory=capture_caller_span)

    def lower(self, options: EnvelopeOptions) -> JsonValue:
        subj_fields = self.subject.declared_field_names()
        sym: Sym[Any] = Sym("self", subj_fields, self.subject.__norn_name__)
        expr = self.predicate(sym)
        if not isinstance(expr, Expr):
            raise NornTracingError("NORN-PY-LAW", "law predicate must return a symbolic Expr[BoolTag]", self.span)
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


def law(
    *,
    id: str,
    title: str,
    version: str,
    owner: str,
    subject: type[Concept],
    rationale: str,
    severity: Literal["error", "warning"] = "error",
    modality: Literal["always", "never", "eventually"] = "always",
    scope_root_kind: Literal["company", "app", "cell", "entity"] = "entity",
    scope_root_id: str | None = None,
    dependencies: Sequence[DependencyPin] = (),
) -> Callable[[Callable[[Sym[Any]], Expr[BoolTag]]], LawDecl]:
    """Decorator tracing a Python predicate into canonical LawIR."""
    span = capture_caller_span()

    def wrap(fn: Callable[[Sym[Any]], Expr[BoolTag]]) -> LawDecl:
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


# ------------------------------------------------------------------ Concept / Interface Base Class


def _unwrap_nullable(annotation: Any) -> tuple[Any, bool]:
    origin = get_origin(annotation)
    if origin is not None and str(origin) in ("typing.Union", "<class 'types.UnionType'>"):
        args = [a for a in get_args(annotation) if a is not type(None)]
        has_none = len(args) < len(get_args(annotation))
        if has_none and len(args) == 1:
            return args[0], True
    return annotation, False


def _extract_property_and_meta(
    field_name: str, annotation: Any, span: SourceSpan | None
) -> tuple[PropertyTypeSpec | LinkSpec, bool, tuple[str, ...], FieldMeta]:
    unwrapped, nullable = _unwrap_nullable(annotation)
    markings: list[str] = []
    meta = FieldMeta()
    explicit_prop: PropertyTypeSpec | None = None
    explicit_link: LinkSpec | None = None

    if get_origin(unwrapped) is Annotated:
        args = get_args(unwrapped)
        base = args[0]
        for extra in args[1:]:
            if isinstance(extra, Marking):
                markings.append(extra.id)
            elif isinstance(extra, PropertyTypeSpec):
                explicit_prop = extra
            elif isinstance(extra, LinkSpec):
                explicit_link = extra
            elif isinstance(extra, FieldMeta):
                meta = extra
            else:
                raise NornTracingError(
                    "NORN-PY-SCHEMA",
                    f"field {field_name!r} has unsupported Annotated metadata {extra!r}",
                    span,
                )
        base, inner_nullable = _unwrap_nullable(base)
        nullable = nullable or inner_nullable
    else:
        base = unwrapped

    if explicit_link is not None:
        return explicit_link, nullable, tuple(markings), meta

    if explicit_prop is not None:
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
        "or Annotated[Link[Target], link(Target, ...)].",
        span,
    )


class Concept:
    """Base class for Norn EntityIR v2 object types and interfaces.

    Subclasses use class keyword arguments to declare canonical metadata:
        class Employee(Concept, name="hr.Employee", title="Employee", version="1.0.0"):
            ...
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
        globalns: dict[str, Any] | None = None,
    ) -> JsonValue:
        """Lower this Concept or Interface to canonical EntityIR v2 JSON."""
        eval_globals: dict[str, Any] = {**globals(), **(globalns or {})}
        hints = get_type_hints(cls, globalns=eval_globals, include_extras=True)
        own_annotations = cls.__dict__.get("__annotations__", {})

        properties_ir: list[dict[str, object]] = []
        links_ir: list[dict[str, object]] = []
        lifecycles_ir: list[dict[str, object]] = []
        invariants_ir: list[dict[str, object]] = []

        sym: Sym[Any] = Sym("self", cls.declared_field_names(), cls.__norn_name__)

        for field_name in own_annotations:
            if field_name.startswith("_"):
                continue
            spec, nullable, field_markings, meta = _extract_property_and_meta(
                field_name, hints[field_name], cls.__norn_span__
            )
            if isinstance(spec, LinkSpec):
                links_ir.append(spec.to_ir(field_name, for_interface=cls.__norn_is_interface__))
                continue

            class_default = cls.__dict__.get(field_name, OMIT)
            default_val: object = OMIT
            raw_default = meta.default if meta.default is not None else class_default
            if raw_default is not OMIT:
                if cls.__norn_is_interface__:
                    raise NornTracingError(
                        "NORN-FLOW-LITERAL",
                        f"interface property {field_name!r} cannot declare a default",
                        cls.__norn_span__,
                    )
                if isinstance(raw_default, float):
                    raise NornFloatRefusalError(raw_default, context=f"default on {cls.__norn_name__}.{field_name}")
                if isinstance(raw_default, Decimal):
                    default_val = format(raw_default, "f")
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
                        cls.__norn_span__,
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
                transitions_ir: list[dict[str, object]] = []
                for tr in member.transitions:
                    guard_zens = [g(sym).zen for g in tr.guards]
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
                inv_expr = member.fn(sym)
                if not isinstance(inv_expr, Expr):
                    raise NornTracingError(
                        "NORN-PY-INVARIANT",
                        f"invariant {member.id!r} must return a symbolic Expr[BoolTag]",
                        member.span,
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
    """Collect and lower every declared Concept, MarkingsCatalog, and LawDecl in definition order."""
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


# ------------------------------------------------------------------ Composable Packs & Deployments


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
    """Composable Norn Pack manifest (UI-free core package with conjunctive laws)."""

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
    """Compose packs conjunctively (`A ∧ B ∧ C`), failing closed on missing deps or conflicts."""
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
    """Reference to an external secret slot; never embeds a secret literal."""

    name: str
    description: str | None = None

    def to_ir(self) -> dict[str, object]:
        return {
            "description": self.description if self.description is not None else OMIT,
            "name": self.name,
        }


@dataclass(frozen=True, slots=True)
class DeploymentSpec:
    """Live Norn Deployment binding composed packs to a cell host, scope, workers, and settings."""

    id: str
    org_id: str
    environment_id: str
    packs: tuple[PackManifest, ...]
    cell_host: CellHostKind = "rivet"
    worker_endpoints: dict[str, str] = field(default_factory=dict)
    secret_refs: tuple[SecretRef, ...] = ()
    initial_settings: dict[str, str | int | bool | Decimal] = field(default_factory=dict)

    def __post_init__(self) -> None:
        if not self.org_id or not self.environment_id:
            raise NornTracingError(
                "NORN-PY-DEPLOYMENT-SCOPE",
                "DeploymentSpec requires non-empty org_id and environment_id (scope fails closed)",
            )
        compose_packs(*self.packs)
        for endpoint_key, url in self.worker_endpoints.items():
            if not _ENDPOINT_KEY_RE.match(endpoint_key):
                raise NornTracingError(
                    "NORN-PY-DEPLOYMENT-ENDPOINT",
                    f"worker endpoint key {endpoint_key!r} must have form '<fn_id>@<semver>'",
                )
            if not (url.startswith("https://") or url.startswith("http://127.0.0.1") or url.startswith("http://localhost")):
                raise NornTracingError(
                    "NORN-PY-DEPLOYMENT-ENDPOINT",
                    f"worker endpoint {endpoint_key!r} must use https:// (or loopback http:// for local dev), got {url!r}",
                )
        for key, val in self.initial_settings.items():
            lowered_key = key.lower()
            if any(tok in lowered_key for tok in _FORBIDDEN_SETTING_SECRET_TOKENS):
                raise NornTracingError(
                    "NORN-PY-RAW-SECRET",
                    f"setting {key!r} looks like a secret; declare it in secret_refs instead of initial_settings",
                )
            if isinstance(val, float):
                raise NornFloatRefusalError(val, context=f"DeploymentSpec.initial_settings[{key!r}]")

    def to_ir(self) -> dict[str, object]:
        composed = compose_packs(*self.packs)
        serialized_settings: dict[str, object] = {
            k: (format(v, "f") if isinstance(v, Decimal) else v)
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
