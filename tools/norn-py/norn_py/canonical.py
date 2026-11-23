"""Canonical JSON serialization and artifact envelopes matching tools/norn-ts.

Mirrors `tools/norn-ts/src/canonical.ts` and `tools/norn-ts/src/builder/envelope.ts`
so Python and TypeScript authoring emit byte-identical canonical JSON.
"""

from __future__ import annotations

import json
import math
from collections.abc import Mapping, Sequence
from dataclasses import dataclass

type JsonScalar = None | bool | int | str
type JsonValue = JsonScalar | list["JsonValue"] | dict[str, "JsonValue"]


class NonJsonValueError(ValueError):
    """Raised when a non-plain, non-exact, or cyclic value tries to enter IR."""

    def __init__(self, path: str, reason: str) -> None:
        super().__init__(f"{path}: {reason}")
        self.path = path
        self.reason = reason


class NornFloatRefusalError(TypeError):
    """Raised whenever a binary Python float appears in a Norn definition or expression."""

    def __init__(self, value: float, context: str = "definition") -> None:
        super().__init__(
            f"Binary float {value!r} is refused in Norn {context} (VAL-EXACT-1). "
            f"Use exact Decimal({str(value)!r}) or an exact decimal string literal instead."
        )


def _utf16_sort_key(text: str) -> bytes:
    """Sort keys identically to JavaScript's UTF-16 code-unit order."""
    return text.encode("utf-16-be")


def to_json(value: object, path: str = "$", seen: set[int] | None = None) -> JsonValue:
    """Return a sorted-key, closure-free plain JSON copy of `value`.

    Rejects floats (VAL-EXACT-1), non-finite numbers, callables, custom class instances,
    cycles, and `__proto__`. Dict entries whose value is `None` are kept; use
    `OMIT` to elide optional members matching JS `undefined`.
    """
    if seen is None:
        seen = set()

    if value is None or isinstance(value, (bool, str)):
        return value
    if isinstance(value, int):
        return value
    if isinstance(value, float):
        if not math.isfinite(value):
            raise NonJsonValueError(path, f"non-finite number {value}")
        raise NornFloatRefusalError(value, context=f"canonical JSON at {path}")

    obj_id = id(value)
    if obj_id in seen:
        raise NonJsonValueError(path, "cycle")

    if isinstance(value, list) or isinstance(value, tuple):
        seen.add(obj_id)
        try:
            return [to_json(item, f"{path}[{idx}]", seen) for idx, item in enumerate(value)]
        finally:
            seen.remove(obj_id)

    if isinstance(value, dict):
        seen.add(obj_id)
        try:
            out: dict[str, JsonValue] = {}
            for key in sorted(value.keys(), key=_utf16_sort_key):
                if not isinstance(key, str):
                    raise NonJsonValueError(path, f"non-string object key {key!r}")
                if key == "__proto__":
                    raise NonJsonValueError(
                        f"{path}.{key}", "__proto__ is not a portable JSON member name"
                    )
                member = value[key]
                if member is OMIT:
                    continue
                out[key] = to_json(member, f"{path}.{key}", seen)
            return out
        finally:
            seen.remove(obj_id)

    raise NonJsonValueError(path, f"instance of {type(value).__name__} is not plain JSON data")


class _OmitType:
    """Sentinel matching JS `undefined` (member omitted during canonicalization)."""


OMIT: FinalOmit = _OmitType()
type FinalOmit = _OmitType


def omit_empty(items: Sequence[object]) -> object:
    """Return `OMIT` when `items` is empty, matching `tools/norn-ts/src/builder/entity.ts`."""
    return OMIT if len(items) == 0 else list(items)


def canonical_json(value: object) -> str:
    """Canonical golden-file text: UTF-16 sorted keys, two-space indent, trailing newline."""
    plain = to_json(value)
    return f"{json.dumps(plain, indent=2, ensure_ascii=False)}\n"


@dataclass(frozen=True, slots=True)
class SourceSpan:
    """Exact author source span captured from Python AST / call site."""

    file: str
    line: int
    column: int = 0

    def format(self) -> str:
        return f"{self.file}:{self.line}:{self.column}" if self.column > 0 else f"{self.file}:{self.line}"


@dataclass(frozen=True, slots=True)
class EnvelopeOptions:
    """Install-time scope and provenance options (mirrors TS EnvelopeOptions)."""

    scope: Mapping[str, str]
    source_path: str
    registry_package_path: str | None = None


@dataclass(frozen=True, slots=True)
class DependencyPin:
    """Exact artifact dependency pin."""

    artifact_id: str
    version: str
    kind: str = "entity"
    required: bool = True

    def to_ir(self) -> dict[str, object]:
        return {
            "artifactId": self.artifact_id,
            "kind": self.kind,
            "required": self.required,
            "version": self.version,
        }


def build_envelope(
    *,
    kind: str,
    artifact_id: str,
    namespace: str,
    name: str,
    version: str,
    payload: Mapping[str, object],
    options: EnvelopeOptions,
    dependencies: Sequence[Mapping[str, object]] = (),
) -> JsonValue:
    """Emit the exact `irVersion: "0.1"` artifact envelope produced by `tools/norn-ts`."""
    is_registry = options.registry_package_path is not None
    registry_root = (
        options.registry_package_path.replace("\\", "/").rstrip("/")
        if options.registry_package_path is not None
        else None
    )
    source_path = (
        f"{registry_root}/artifacts/{artifact_id}.json"
        if is_registry and registry_root is not None
        else options.source_path
    )
    return to_json(
        {
            "irVersion": "0.1",
            "artifactId": artifact_id,
            "kind": kind,
            "namespace": namespace,
            "name": name,
            "version": version,
            "lifecycle": "active" if is_registry else "draft",
            "scope": {} if is_registry else dict(options.scope),
            "dependencies": [dict(d) for d in dependencies],
            "provenance": {
                "source": "system" if is_registry else "authoring_sdk",
                "sourcePath": source_path,
            },
            "payload": dict(payload),
        }
    )
