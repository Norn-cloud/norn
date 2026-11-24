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


def to_json(value: object, path: str = "$", seen: set[int] | None = None) -> JsonValue:
    """Return a UTF-16 sorted-key, closure-free plain JSON copy of `value`.

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

    if isinstance(value, (list, tuple)):
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


def _to_norn1_json(value: object, path: str = "$", seen: set[int] | None = None) -> JsonValue:
    """Normalize `value` into Norn1 canonical structure (NFC strings/keys, code-point key sort)."""
    if seen is None:
        seen = set()

    if value is None or isinstance(value, bool):
        return value
    if isinstance(value, str):
        return unicodedata.normalize("NFC", value)
    if isinstance(value, int):
        return value
    if isinstance(value, float):
        if not math.isfinite(value):
            raise NonJsonValueError(path, f"non-finite number {value}")
        raise NornFloatRefusalError(value, context=f"Norn1 canonical JSON at {path}")

    obj_id = id(value)
    if obj_id in seen:
        raise NonJsonValueError(path, "cycle")

    if isinstance(value, (list, tuple)):
        seen.add(obj_id)
        try:
            return [_to_norn1_json(item, f"{path}[{idx}]", seen) for idx, item in enumerate(value)]
        finally:
            seen.remove(obj_id)

    if isinstance(value, dict):
        seen.add(obj_id)
        try:
            normalized_entries: dict[str, object] = {}
            for raw_key, member in value.items():
                if not isinstance(raw_key, str):
                    raise NonJsonValueError(path, f"non-string object key {raw_key!r}")
                if raw_key == "__proto__":
                    raise NonJsonValueError(
                        f"{path}.{raw_key}", "__proto__ is not a portable JSON member name"
                    )
                if member is OMIT:
                    continue
                nfc_key = unicodedata.normalize("NFC", raw_key)
                if nfc_key in normalized_entries:
                    raise NonJsonValueError(
                        f"{path}.{nfc_key}",
                        f"duplicate JSON key {nfc_key!r} after NFC normalization",
                    )
                normalized_entries[nfc_key] = member
            out: dict[str, JsonValue] = {}
            for key in sorted(normalized_entries.keys()):
                out[key] = _to_norn1_json(normalized_entries[key], f"{path}.{key}", seen)
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


def canonical_norn1_bytes(value: object) -> bytes:
    """Compact Norn1 canonical JSON bytes (zero whitespace, NFC strings, code-point sorted keys).

    Matches `crates/norn-canonical::serialize` / `HashProfile::Norn1`.
    """
    plain = _to_norn1_json(value)
    return json.dumps(plain, separators=(",", ":"), ensure_ascii=False).encode("utf-8")


def canonical_norn1_sha256(value: object) -> str:
    """Lower-case hex SHA-256 of `NORN1_DOMAIN_TAG || canonical_norn1_bytes(value)`."""
    return hashlib.sha256(NORN1_DOMAIN_TAG + canonical_norn1_bytes(value)).hexdigest()


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
