"""Canonical JSON serialization and artifact envelopes matching tools/norn-ts & norn-canonical.

Provides:
- `canonical_json(value)`: human-readable golden-file text matching `tools/norn-ts/src/canonical.ts`
  (UTF-16 sorted keys, two-space indent, trailing newline).
- `canonical_norn1_bytes(value)`: compact Norn1 canonical JSON bytes matching
  `crates/norn-canonical` (Unicode NFC normalization, UTF-8/code-point sorted keys,
  duplicate-after-NFC rejection, zero whitespace).
- `canonical_norn1_sha256(value)`: domain-separated SHA-256 hex digest matching
  `norn_canonical::HashProfile::Norn1`.
"""

from __future__ import annotations

import hashlib
import json
import math
import unicodedata
from collections.abc import Mapping, Sequence
from dataclasses import dataclass

type JsonScalar = None | bool | int | str
type JsonValue = JsonScalar | list["JsonValue"] | dict[str, "JsonValue"]

NORN1_DOMAIN_TAG: bytes = b"norn.canonical.json/norn1\0"


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


def _normalize_json(
    value: object,
    *,
    norn1: bool,
    path: str = "$",
    seen: set[int] | None = None,
) -> JsonValue:
    if seen is None:
        seen = set()

    if value is None or isinstance(value, bool):
        return value
    if isinstance(value, str):
        return unicodedata.normalize("NFC", value) if norn1 else value
    if isinstance(value, int):
        return value
    if isinstance(value, float):
        if not math.isfinite(value):
            raise NonJsonValueError(path, f"non-finite number {value}")
        label = "Norn1 canonical JSON" if norn1 else "canonical JSON"
        raise NornFloatRefusalError(value, context=f"{label} at {path}")

    obj_id = id(value)
    if obj_id in seen:
        raise NonJsonValueError(path, "cycle")

    if isinstance(value, (list, tuple)):
        seen.add(obj_id)
        try:
            return [_normalize_json(item, norn1=norn1, path=f"{path}[{idx}]", seen=seen) for idx, item in enumerate(value)]
        finally:
            seen.remove(obj_id)

    if isinstance(value, dict):
        seen.add(obj_id)
        try:
            entries: dict[str, object] = {}
            for raw_key, member in value.items():
                if not isinstance(raw_key, str):
                    raise NonJsonValueError(path, f"non-string object key {raw_key!r}")
                if raw_key == "__proto__":
                    raise NonJsonValueError(
                        f"{path}.{raw_key}", "__proto__ is not a portable JSON member name"
                    )
                if member is OMIT:
                    continue
                if norn1:
                    key = unicodedata.normalize("NFC", raw_key)
                    if key in entries:
                        raise NonJsonValueError(
                            f"{path}.{key}",
                            f"duplicate JSON key {key!r} after NFC normalization",
                        )
                else:
                    key = raw_key
                entries[key] = member
            ordered_keys = sorted(entries.keys()) if norn1 else sorted(entries.keys(), key=_utf16_sort_key)
            return {key: _normalize_json(entries[key], norn1=norn1, path=f"{path}.{key}", seen=seen) for key in ordered_keys}
        finally:
            seen.remove(obj_id)

    raise NonJsonValueError(path, f"instance of {type(value).__name__} is not plain JSON data")


def to_json(value: object, path: str = "$", seen: set[int] | None = None) -> JsonValue:
    """Return a UTF-16 sorted-key, closure-free plain JSON copy of `value`.

    Rejects floats (VAL-EXACT-1), non-finite numbers, callables, custom class instances,
    cycles, and `__proto__`. Dict entries whose value is `None` are kept; use
    `OMIT` to elide optional members matching JS `undefined`.
    """
    return _normalize_json(value, norn1=False, path=path, seen=seen)


def _to_norn1_json(value: object, path: str = "$", seen: set[int] | None = None) -> JsonValue:
    """Normalize `value` into Norn1 canonical structure (NFC strings/keys, code-point key sort)."""
    return _normalize_json(value, norn1=True, path=path, seen=seen)


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


def canonical_norn1_bytes(value: object) -> bytes:
    """Compact Norn1 canonical JSON bytes (zero whitespace, NFC strings, code-point sorted keys).

    Matches `crates/norn-canonical::serialize` / `HashProfile::Norn1`.
    """
    plain = _to_norn1_json(value)
    return json.dumps(plain, separators=(",", ":"), ensure_ascii=False).encode("utf-8")


def norn1_sha256_bytes(norn1_bytes: bytes) -> str:
    """Lower-case hex SHA-256 of `NORN1_DOMAIN_TAG || norn1_bytes`."""
    return hashlib.sha256(NORN1_DOMAIN_TAG + norn1_bytes).hexdigest()


def canonical_norn1_sha256(value: object) -> str:
    """Lower-case hex SHA-256 of `NORN1_DOMAIN_TAG || canonical_norn1_bytes(value)`."""
    return norn1_sha256_bytes(canonical_norn1_bytes(value))


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
