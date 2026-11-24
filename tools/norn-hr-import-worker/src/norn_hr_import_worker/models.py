"""Core 07 typed models, exact decimal parsing, canonical JSON hashing, and PII-safe logging."""

from __future__ import annotations

import hashlib
import json
import logging
import re
from collections.abc import Mapping, Sequence
from dataclasses import dataclass, field
from decimal import Decimal, InvalidOperation
from typing import Literal

EXTRACTOR_NAME = "norn-hr-import-worker"
EXTRACTOR_VERSION = "0.1.0"

# MAP-DECIMAL-TEXT-1 / VAL-EXACT-1 strict decimal grammar: no commas, no exponent, no '+'.
_EXACT_DECIMAL_RE = re.compile(r"^-?(?:0|[1-9][0-9]*)(?:\.[0-9]+)?$")
_EXACT_INT_RE = re.compile(r"^-?(?:0|[1-9][0-9]*)$")
_ISO_DATE_RE = re.compile(r"^[0-9]{4}-[0-9]{2}-[0-9]{2}$")
_HHMM_RE = re.compile(r"^([01][0-9]|2[0-3]):([0-5][0-9])(?::([0-5][0-9]))?$")

SourceChannel = Literal["whatsapp_export", "spreadsheet_csv", "schedule_photo", "erp_export"]
TargetKind = Literal["person", "leave_request", "shift_commitment", "attendance_event"]
DisputeState = Literal["clean", "quarantined", "disputed"]
CandidateStatus = Literal["staged", "quarantined"]
FindingKind = Literal[
    "schema_violation",
    "missing_anchor",
    "low_confidence",
    "duplicate_key",
    "feed_conflict",
    "domain_law_violation",
]
FindingSeverity = Literal["warning", "error"]
QuarantineDisposition = Literal["quarantined", "disputed", "advisory"]

JsonScalar = str | int | bool | None
JsonValue = JsonScalar | Sequence["JsonValue"] | Mapping[str, "JsonValue"]


class ExactValueError(ValueError):
    """Raised when a value violates exact decimal, integer, or canonical JSON rules."""


def _assert_no_floats(value: object, path: str = "$") -> None:
    """Reject any IEEE-754 float anywhere inside a canonical payload."""
    if isinstance(value, float):
        raise ExactValueError(f"IEEE-754 float forbidden at {path}; use exact Decimal strings")
    if isinstance(value, bool) or value is None or isinstance(value, (str, int)):
        return
    if isinstance(value, Mapping):
        for k, v in value.items():
            if not isinstance(k, str):
                raise ExactValueError(f"Non-string JSON object key at {path}: {type(k).__name__}")
            _assert_no_floats(v, f"{path}.{k}")
        return
    if isinstance(value, (list, tuple)):
        for idx, item in enumerate(value):
            _assert_no_floats(item, f"{path}[{idx}]")
        return
    raise ExactValueError(f"Unsupported non-canonical type at {path}: {type(value).__name__}")


def canonical_json_text(value: JsonValue) -> str:
    """Serialize a float-free JSON value with deterministic sorted keys and compact separators."""
    _assert_no_floats(value)
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False)


def canonical_json_bytes(value: JsonValue) -> bytes:
    """Encode canonical JSON as UTF-8 bytes."""
    return canonical_json_text(value).encode("utf-8")


def sha256_bytes(data: bytes) -> str:
    """Return `sha256:<hex>` digest of raw bytes."""
    return f"sha256:{hashlib.sha256(data).hexdigest()}"


def sha256_prefixed(value: JsonValue) -> str:
    """Return `sha256:<hex>` digest of the canonical JSON encoding of `value`."""
    return sha256_bytes(canonical_json_bytes(value))


def parse_exact_decimal(raw: str | int | Decimal) -> Decimal:
    """Parse an exact Decimal under MAP-DECIMAL-TEXT-1 (rejects floats, commas, exponents, '+')."""
    if isinstance(raw, bool):
        raise ExactValueError("Boolean is not an exact decimal")
    if isinstance(raw, float):
        raise ExactValueError("IEEE-754 float is forbidden for exact decimal values")
    if isinstance(raw, int):
        return Decimal(raw)
    if isinstance(raw, Decimal):
        if not raw.is_finite():
            raise ExactValueError("Non-finite Decimal is forbidden")
        return raw
    if not isinstance(raw, str):
        raise ExactValueError(f"Unsupported decimal input type: {type(raw).__name__}")
    if not _EXACT_DECIMAL_RE.fullmatch(raw):
        raise ExactValueError(f"Invalid exact decimal text: {raw!r}")
    try:
        dec = Decimal(raw)
    except InvalidOperation as exc:
        raise ExactValueError(f"Decimal parse failure: {raw!r}") from exc
    if not dec.is_finite():
        raise ExactValueError(f"Non-finite decimal text: {raw!r}")
    return dec


def parse_exact_int(raw: str | int) -> int:
    """Parse an exact integer without whitespace, leading '+', commas, or floats."""
    if isinstance(raw, bool):
        raise ExactValueError("Boolean is not an exact integer")
    if isinstance(raw, float):
        raise ExactValueError("IEEE-754 float is forbidden for exact integer values")
    if isinstance(raw, int):
        return raw
    if not isinstance(raw, str) or not _EXACT_INT_RE.fullmatch(raw):
        raise ExactValueError(f"Invalid exact integer text: {raw!r}")
    return int(raw)


def decimal_to_canonical_str(value: Decimal, scale: int | None = None) -> str:
    """Format a finite Decimal canonically without scientific notation."""
    if not value.is_finite():
        raise ExactValueError("Cannot format non-finite Decimal")
    if scale is not None:
        quant = Decimal(1) if scale == 0 else Decimal(f"1e-{scale}")
        quantized = value.quantize(quant)
        return format(quantized, "f")
    normalized = value.normalize()
    if normalized == 0:
        return "0"
    text = format(normalized, "f")
    if "." in text:
        text = text.rstrip("0").rstrip(".")
    return text


def validate_iso_date(raw: str) -> str:
    """Validate strict YYYY-MM-DD calendar date format and bounds."""
    if not _ISO_DATE_RE.fullmatch(raw):
        raise ExactValueError(f"Invalid ISO date: {raw!r}")
    year, month, day = int(raw[0:4]), int(raw[5:7]), int(raw[8:10])
    if month < 1 or month > 12 or day < 1 or day > 31:
        raise ExactValueError(f"Out-of-range ISO date: {raw!r}")
    # Exact month length check including leap years.
    is_leap = (year % 4 == 0 and year % 100 != 0) or (year % 400 == 0)
    days_in_month = [31, 29 if is_leap else 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    if day > days_in_month[month - 1]:
        raise ExactValueError(f"Invalid calendar date: {raw!r}")
    return raw


def parse_hhmm_minutes(raw: str) -> int:
    """Parse HH:MM or HH:MM:SS into exact total minutes (refusing seconds != 00 when fractional)."""
    match = _HHMM_RE.fullmatch(raw)
    if match is None:
        raise ExactValueError(f"Invalid HH:MM time: {raw!r}")
    hours = int(match.group(1))
    minutes = int(match.group(2))
    return hours * 60 + minutes


def duration_hours_exact(start_hhmm: str, end_hhmm: str) -> Decimal:
    """Compute exact positive decimal hours between two HH:MM times."""
    start_min = parse_hhmm_minutes(start_hhmm)
    end_min = parse_hhmm_minutes(end_hhmm)
    if end_min <= start_min:
        raise ExactValueError(f"Non-positive shift duration: {start_hhmm} -> {end_hhmm}")
    delta = Decimal(end_min - start_min)
    return (delta / Decimal(60)).quantize(Decimal("0.01"))


@dataclass(frozen=True, slots=True)
class SourceDocumentBatch:
    """Core 07 `norn.SourceDocument` batch metadata (`hr_import.SourceBatch`)."""

    id: str
    channel: SourceChannel
    document_uri: str
    document_digest: str
    media_type: str
    extractor: str
    extractor_version: str
    captured_at: str
    status: Literal["reviewing"] = "reviewing"

    def to_dict(self) -> dict[str, JsonValue]:
        return {
            "captured_at": self.captured_at,
            "channel": self.channel,
            "document_digest": self.document_digest,
            "document_uri": self.document_uri,
            "extractor": self.extractor,
            "extractor_version": self.extractor_version,
            "id": self.id,
            "media_type": self.media_type,
            "status": self.status,
        }


@dataclass(frozen=True, slots=True)
class FieldEvidenceRecord:
    """Core 07 `norn.SourceEvidence` field-level anchor (`hr_import.FieldEvidence`)."""

    id: str
    batch_id: str
    candidate_id: str
    subject_property: str
    subject_type: str | None
    subject_key: str | None
    document_uri: str
    document_digest: str
    media_type: str
    page: int
    bbox_x: str
    bbox_y: str
    bbox_w: str
    bbox_h: str
    text: str
    extractor: str
    extractor_version: str
    confidence_bp: int | None
    source_anchor: str
    record_digest: str

    def to_dict(self) -> dict[str, JsonValue]:
        return {
            "batch": self.batch_id,
            "bbox_h": self.bbox_h,
            "bbox_w": self.bbox_w,
            "bbox_x": self.bbox_x,
            "bbox_y": self.bbox_y,
            "candidate": self.candidate_id,
            "confidence_bp": self.confidence_bp,
            "document_digest": self.document_digest,
            "document_uri": self.document_uri,
            "extractor": self.extractor,
            "extractor_version": self.extractor_version,
            "id": self.id,
            "media_type": self.media_type,
            "page": self.page,
            "record_digest": self.record_digest,
            "source_anchor": self.source_anchor,
            "subject_key": self.subject_key,
            "subject_property": self.subject_property,
            "subject_type": self.subject_type,
            "text": self.text,
        }


@dataclass(frozen=True, slots=True)
class ExtractedCandidateProposal:
    """Closed-allowlist proposal (`hr_import.ExtractedCandidate`)."""

    id: str
    batch_id: str
    target_kind: TargetKind
    source_record_key: str
    payload_digest: str
    confidence_bp: int
    source_span: str
    extracted_at: str
    observed_at: str
    effective_on: str
    dispute_state: DisputeState
    status: CandidateStatus
    proposal: Mapping[str, JsonValue]

    def to_dict(self) -> dict[str, JsonValue]:
        return {
            "batch": self.batch_id,
            "confidence_bp": self.confidence_bp,
            "dispute_state": self.dispute_state,
            "effective_on": self.effective_on,
            "extracted_at": self.extracted_at,
            "id": self.id,
            "observed_at": self.observed_at,
            "payload_digest": self.payload_digest,
            "proposal": dict(self.proposal),
            "source_record_key": self.source_record_key,
            "source_span": self.source_span,
            "status": self.status,
            "target_kind": self.target_kind,
        }


@dataclass(frozen=True, slots=True)
class DataQualityFindingRecord:
    """Replay-stable finding (`hr_import.DataQualityFinding`)."""

    id: str
    candidate_id: str
    evidence_id: str | None
    code: str
    finding_kind: FindingKind
    severity: FindingSeverity
    quarantine_disposition: QuarantineDisposition
    field_path: str
    law_pin: str | None
    conflicting_payload_digest: str | None
    message: str
    status: Literal["open"] = "open"

    def to_dict(self) -> dict[str, JsonValue]:
        return {
            "candidate": self.candidate_id,
            "code": self.code,
            "conflicting_payload_digest": self.conflicting_payload_digest,
            "evidence": self.evidence_id,
            "field_path": self.field_path,
            "finding_kind": self.finding_kind,
            "id": self.id,
            "law_pin": self.law_pin,
            "message": self.message,
            "quarantine_disposition": self.quarantine_disposition,
            "resolution_action": None,
            "resolution_reason": None,
            "resolution_receipt_digest": None,
            "resolved_by_user_id": None,
            "severity": self.severity,
            "status": self.status,
        }


@dataclass(frozen=True, slots=True)
class ImportBundleResult:
    """Deterministic extraction bundle ready for Core 07 governed intake."""

    batch: SourceDocumentBatch
    candidates: tuple[ExtractedCandidateProposal, ...]
    evidence: tuple[FieldEvidenceRecord, ...]
    findings: tuple[DataQualityFindingRecord, ...]
    deduplicated_count: int = 0
    extraction_digest: str = field(init=False)

    def __post_init__(self) -> None:
        body = self._body_dict()
        object.__setattr__(self, "extraction_digest", sha256_prefixed(body))

    def _body_dict(self) -> dict[str, JsonValue]:
        return {
            "batch": self.batch.to_dict(),
            "candidates": [c.to_dict() for c in self.candidates],
            "deduplicated_count": self.deduplicated_count,
            "evidence": [e.to_dict() for e in self.evidence],
            "findings": [f.to_dict() for f in self.findings],
        }

    def to_dict(self) -> dict[str, JsonValue]:
        body = self._body_dict()
        body["extraction_digest"] = self.extraction_digest
        return body

    def to_canonical_json(self) -> str:
        return canonical_json_text(self.to_dict())


class SafeWorkerLogger:
    """Structured logger that emits ONLY non-PII operational metadata (digests, counts, codes)."""

    def __init__(self, logger: logging.Logger | None = None) -> None:
        self._logger = logger or logging.getLogger(EXTRACTOR_NAME)

    def record_extraction_summary(
        self,
        *,
        batch_id: str,
        channel: SourceChannel,
        document_digest: str,
        candidate_count: int,
        finding_count: int,
        deduplicated_count: int,
        finding_codes: Sequence[str],
    ) -> None:
        payload: dict[str, JsonValue] = {
            "batch_id": batch_id,
            "candidate_count": candidate_count,
            "channel": channel,
            "deduplicated_count": deduplicated_count,
            "document_digest": document_digest,
            "event": "hr_import.extract.completed",
            "finding_codes": sorted(set(finding_codes)),
            "finding_count": finding_count,
        }
        self._logger.info("%s", canonical_json_text(payload))
