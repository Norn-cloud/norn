"""Pluggable OCR/VLM adapter protocol, deterministic fixture adapter, and fail-closed validator."""

from __future__ import annotations

import json
from dataclasses import dataclass, replace
from decimal import Decimal
from typing import Protocol

from norn_hr_import_worker.models import (
    EXTRACTOR_NAME,
    EXTRACTOR_VERSION,
    CandidateStatus,
    DataQualityFindingRecord,
    DisputeState,
    ExactValueError,
    ExtractedCandidateProposal,
    FieldEvidenceRecord,
    ImportBundleResult,
    JsonValue,
    SafeWorkerLogger,
    SourceDocumentBatch,
    TargetKind,
    decimal_to_canonical_str,
    parse_exact_decimal,
    parse_exact_int,
    sha256_bytes,
    sha256_prefixed,
    validate_iso_date,
)

_ALLOWED_TARGET_KINDS: frozenset[str] = frozenset(
    {"person", "leave_request", "shift_commitment", "attendance_event"}
)
_ALLOWED_FIELDS_BY_KIND: dict[TargetKind, frozenset[str]] = {
    "shift_commitment": frozenset(
        {
            "branch_code",
            "employee_code",
            "employee_label",
            "role_code",
            "shift_date",
            "start_time",
            "end_time",
            "quantity_hours",
            "unit",
        }
    ),
    "attendance_event": frozenset(
        {
            "branch_code",
            "employee_code",
            "employee_label",
            "work_date",
            "clock_in",
            "clock_out",
            "quantity_hours",
            "unit",
        }
    ),
    "leave_request": frozenset(
        {
            "employee_code",
            "employee_label",
            "leave_kind",
            "starts_on",
            "ends_on",
            "days",
            "unit",
        }
    ),
    "person": frozenset(
        {
            "employee_code",
            "display_name",
            "jurisdiction_code",
            "effective_from",
            "effective_to",
            "currency",
            "base_salary",
            "hourly_overtime_rate",
            "max_weekly_hours",
            "primary_skill",
        }
    ),
}
_NUMERIC_FIELDS: frozenset[str] = frozenset(
    {"quantity_hours", "days", "base_salary", "hourly_overtime_rate", "max_weekly_hours"}
)
_DATE_FIELDS: frozenset[str] = frozenset(
    {
        "effective_on",
        "effective_from",
        "effective_to",
        "shift_date",
        "work_date",
        "starts_on",
        "ends_on",
    }
)


@dataclass(frozen=True, slots=True)
class OcrFieldObservation:
    """One field extracted from a document region by an untrusted OCR/VLM backend."""

    field_name: str
    value: str | None
    text_span: str
    page: int | None
    bbox_x: str | None
    bbox_y: str | None
    bbox_w: str | None
    bbox_h: str | None
    confidence_bp: int | None


@dataclass(frozen=True, slots=True)
class OcrRecordObservation:
    """One proposed HR record extracted from a schedule photo or scanned HR form."""

    record_key: str
    target_kind: str
    effective_on: str | None
    fields: tuple[OcrFieldObservation, ...]


@dataclass(frozen=True, slots=True)
class OcrRawResponse:
    """Raw untrusted output returned by an `OcrAdapter`."""

    records: tuple[OcrRecordObservation, ...]


class OcrAdapter(Protocol):
    """Clean-room pluggable interface for OCR / VLM schedule or form extractors."""

    @property
    def adapter_name(self) -> str: ...

    @property
    def adapter_version(self) -> str: ...

    def extract(
        self, document_bytes: bytes, document_uri: str, media_type: str
    ) -> OcrRawResponse: ...


class DeterministicFixtureOcrAdapter:
    """Deterministic fixture parser implementing `OcrAdapter` with zero network or model IO."""

    @property
    def adapter_name(self) -> str:
        return f"{EXTRACTOR_NAME}.fixture_ocr"

    @property
    def adapter_version(self) -> str:
        return EXTRACTOR_VERSION

    def extract(self, document_bytes: bytes, document_uri: str, media_type: str) -> OcrRawResponse:
        del document_uri, media_type
        parsed = json.loads(document_bytes.decode("utf-8"), parse_float=str)
        if not isinstance(parsed, dict) or not isinstance(parsed.get("records"), list):
            raise ExactValueError("OCR fixture payload must be an object with a 'records' list")

        records: list[OcrRecordObservation] = []
        for raw_rec in parsed["records"]:
            if not isinstance(raw_rec, dict):
                raise ExactValueError("OCR fixture record must be a JSON object")
            raw_fields = raw_rec.get("fields")
            if not isinstance(raw_fields, list):
                raise ExactValueError("OCR fixture record must carry a 'fields' list")
            obs_fields: list[OcrFieldObservation] = []
            for raw_field in raw_fields:
                if not isinstance(raw_field, dict):
                    raise ExactValueError("OCR field must be a JSON object")
                page_val = raw_field.get("page")
                conf_val = raw_field.get("confidence_bp")
                obs_fields.append(
                    OcrFieldObservation(
                        field_name=str(raw_field.get("field_name", "")),
                        value=(None if raw_field.get("value") is None else str(raw_field["value"])),
                        text_span=str(raw_field.get("text_span", "")),
                        page=None if page_val is None else parse_exact_int(page_val),
                        bbox_x=(
                            None if raw_field.get("bbox_x") is None else str(raw_field["bbox_x"])
                        ),
                        bbox_y=(
                            None if raw_field.get("bbox_y") is None else str(raw_field["bbox_y"])
                        ),
                        bbox_w=(
                            None if raw_field.get("bbox_w") is None else str(raw_field["bbox_w"])
                        ),
                        bbox_h=(
                            None if raw_field.get("bbox_h") is None else str(raw_field["bbox_h"])
                        ),
                        confidence_bp=None if conf_val is None else parse_exact_int(conf_val),
                    )
                )
            eff = raw_rec.get("effective_on")
            records.append(
                OcrRecordObservation(
                    record_key=str(raw_rec.get("record_key", "")),
                    target_kind=str(raw_rec.get("target_kind", "")),
                    effective_on=None if eff is None else str(eff),
                    fields=tuple(obs_fields),
                )
            )
        return OcrRawResponse(records=tuple(records))


def _validate_bbox(
    field_obs: OcrFieldObservation,
) -> tuple[int, str, str, str, str] | None:
    """Validate Core 07 page >= 1 and strictly positive box inside [0, 1]^2."""
    if (
        field_obs.page is None
        or field_obs.page < 1
        or field_obs.bbox_x is None
        or field_obs.bbox_y is None
        or field_obs.bbox_w is None
        or field_obs.bbox_h is None
    ):
        return None
    try:
        bx = parse_exact_decimal(field_obs.bbox_x)
        by = parse_exact_decimal(field_obs.bbox_y)
        bw = parse_exact_decimal(field_obs.bbox_w)
        bh = parse_exact_decimal(field_obs.bbox_h)
    except ExactValueError:
        return None
    zero = Decimal(0)
    one = Decimal(1)
    if (
        bx < zero
        or bx > one
        or by < zero
        or by > one
        or bw <= zero
        or bw > one
        or bh <= zero
        or bh > one
        or bx + bw > one
        or by + bh > one
    ):
        return None
    return (
        field_obs.page,
        decimal_to_canonical_str(bx, 4),
        decimal_to_canonical_str(by, 4),
        decimal_to_canonical_str(bw, 4),
        decimal_to_canonical_str(bh, 4),
    )


def extract_from_ocr(
    *,
    raw_bytes: bytes,
    document_uri: str,
    media_type: str,
    batch_id: str,
    captured_at: str,
    extracted_at: str,
    adapter: OcrAdapter,
    min_confidence_bp: int = 8500,
    logger: SafeWorkerLogger | None = None,
) -> ImportBundleResult:
    """Run an `OcrAdapter` and fail-closed quarantine unanchored or low-confidence fields."""
    document_digest = sha256_bytes(raw_bytes)
    raw_response = adapter.extract(raw_bytes, document_uri, media_type)

    batch = SourceDocumentBatch(
        id=batch_id,
        channel="schedule_photo",
        document_uri=document_uri,
        document_digest=document_digest,
        media_type=media_type,
        extractor=adapter.adapter_name,
        extractor_version=adapter.adapter_version,
        captured_at=captured_at,
    )

    candidates: list[ExtractedCandidateProposal] = []
    evidence_records: list[FieldEvidenceRecord] = []
    findings: list[DataQualityFindingRecord] = []
    seen_keys: dict[str, tuple[int, str, str | None, str, bool]] = {}
    deduplicated_count = 0

    for rec_idx, record in enumerate(raw_response.records, start=1):
        known_target_kind = record.target_kind in _ALLOWED_TARGET_KINDS
        target_kind: TargetKind = (
            record.target_kind  # type: ignore[assignment]
            if known_target_kind
            else "shift_commitment"
        )
        cand_id = f"{batch_id}_ocr_{rec_idx}"
        proposal: dict[str, JsonValue] = {}
        rec_findings: list[DataQualityFindingRecord] = []
        rec_evidence: list[FieldEvidenceRecord] = []
        confidences: list[int] = []
        seen_field_names: dict[str, str] = {}
        has_internal_conflict = False

        if not known_target_kind:
            rec_findings.append(
                DataQualityFindingRecord(
                    id=f"dqf_{cand_id}_target_kind",
                    candidate_id=cand_id,
                    evidence_id=None,
                    code="needs_review",
                    finding_kind="schema_violation",
                    severity="error",
                    quarantine_disposition="quarantined",
                    field_path="target_kind",
                    law_pin=None,
                    conflicting_payload_digest=None,
                    message=f"Record {rec_idx}: unsupported OCR target_kind; quarantined.",
                )
            )

        effective_on = ""
        if record.effective_on is None or not record.effective_on.strip():
            rec_findings.append(
                DataQualityFindingRecord(
                    id=f"dqf_{cand_id}_missing_effective_on",
                    candidate_id=cand_id,
                    evidence_id=None,
                    code="missing_required_field",
                    finding_kind="schema_violation",
                    severity="error",
                    quarantine_disposition="quarantined",
                    field_path="effective_on",
                    law_pin=None,
                    conflicting_payload_digest=None,
                    message=f"Record {rec_idx}: missing required effective_on date; quarantined.",
                )
            )
        else:
            try:
                effective_on = validate_iso_date(record.effective_on.strip())
            except ExactValueError as exc:
                rec_findings.append(
                    DataQualityFindingRecord(
                        id=f"dqf_{cand_id}_effective_on",
                        candidate_id=cand_id,
                        evidence_id=None,
                        code="needs_review",
                        finding_kind="schema_violation",
                        severity="error",
                        quarantine_disposition="quarantined",
                        field_path="effective_on",
                        law_pin=None,
                        conflicting_payload_digest=None,
                        message=f"Record {rec_idx}: invalid effective_on date ({exc}).",
                    )
                )

        if not record.fields:
            rec_findings.append(
                DataQualityFindingRecord(
                    id=f"dqf_{cand_id}_empty_fields",
                    candidate_id=cand_id,
                    evidence_id=None,
                    code="needs_review",
                    finding_kind="missing_anchor",
                    severity="error",
                    quarantine_disposition="quarantined",
                    field_path="fields",
                    law_pin=None,
                    conflicting_payload_digest=None,
                    message=f"Record {rec_idx}: zero field observations; refusing to invent data.",
                )
            )

        allowed_fields = _ALLOWED_FIELDS_BY_KIND[target_kind]
        first_anchor = "unanchored"
        for f_idx, f_obs in enumerate(record.fields, start=1):
            ev_id = f"{cand_id}_f{f_idx}"
            valid_box = _validate_bbox(f_obs)
            has_text = bool(f_obs.text_span.strip())
            field_digest = sha256_prefixed(
                {
                    "field_name": f_obs.field_name,
                    "text_span": f_obs.text_span,
                    "value": f_obs.value,
                }
            )
            safe_field_path = (
                f_obs.field_name
                if f_obs.field_name in allowed_fields
                else f"fields[{f_idx}]"
            )

            if valid_box is None or not has_text:
                rec_findings.append(
                    DataQualityFindingRecord(
                        id=f"dqf_{cand_id}_f{f_idx}_missing_anchor",
                        candidate_id=cand_id,
                        evidence_id=None,
                        code="needs_review",
                        finding_kind="missing_anchor",
                        severity="error",
                        quarantine_disposition="quarantined",
                        field_path=safe_field_path,
                        law_pin="hr_import.FieldEvidence.valid_anchor",
                        conflicting_payload_digest=None,
                        message=(
                            f"Record {rec_idx} field[{f_idx}] lacks a valid page/bbox/text anchor; "
                            "quarantined as needs_review."
                        ),
                    )
                )
                continue

            page, bx, by, bw, bh = valid_box
            anchor = f"page:{page}:bbox:{bx},{by},{bw},{bh}"
            if first_anchor == "unanchored":
                first_anchor = anchor

            if (
                f_obs.confidence_bp is None
                or f_obs.confidence_bp < 0
                or f_obs.confidence_bp > 10000
            ):
                rec_findings.append(
                    DataQualityFindingRecord(
                        id=f"dqf_{cand_id}_f{f_idx}_missing_conf",
                        candidate_id=cand_id,
                        evidence_id=ev_id,
                        code="needs_review",
                        finding_kind="low_confidence",
                        severity="error",
                        quarantine_disposition="quarantined",
                        field_path=safe_field_path,
                        law_pin="hr_import.FieldEvidence.valid_anchor",
                        conflicting_payload_digest=None,
                        message=(
                            f"Record {rec_idx} field[{f_idx}] has missing or out-of-range "
                            "confidence; quarantined as needs_review."
                        ),
                    )
                )
                safe_conf = None
            else:
                safe_conf = f_obs.confidence_bp
                confidences.append(safe_conf)
                if safe_conf < min_confidence_bp:
                    rec_findings.append(
                        DataQualityFindingRecord(
                            id=f"dqf_{cand_id}_f{f_idx}_low_conf",
                            candidate_id=cand_id,
                            evidence_id=ev_id,
                            code="needs_review",
                            finding_kind="low_confidence",
                            severity="error",
                            quarantine_disposition="quarantined",
                            field_path=safe_field_path,
                            law_pin=None,
                            conflicting_payload_digest=None,
                            message=(
                                f"Record {rec_idx} field[{f_idx}] confidence {safe_conf} bp "
                                f"is below {min_confidence_bp} bp; quarantined as needs_review."
                            ),
                        )
                    )

            rec_evidence.append(
                FieldEvidenceRecord(
                    id=ev_id,
                    batch_id=batch_id,
                    candidate_id=cand_id,
                    subject_property=safe_field_path,
                    subject_type=f"hr.{target_kind}",
                    subject_key=cand_id,
                    document_uri=document_uri,
                    document_digest=document_digest,
                    media_type=media_type,
                    page=page,
                    bbox_x=bx,
                    bbox_y=by,
                    bbox_w=bw,
                    bbox_h=bh,
                    text=f_obs.text_span,
                    extractor=adapter.adapter_name,
                    extractor_version=adapter.adapter_version,
                    confidence_bp=safe_conf,
                    source_anchor=anchor,
                    record_digest=field_digest,
                )
            )

            # Fail-closed closed-allowlist field check (never accept unknown fields into proposal).
            if f_obs.field_name not in allowed_fields:
                rec_findings.append(
                    DataQualityFindingRecord(
                        id=f"dqf_{cand_id}_f{f_idx}_unknown_field",
                        candidate_id=cand_id,
                        evidence_id=ev_id,
                        code="needs_review",
                        finding_kind="schema_violation",
                        severity="error",
                        quarantine_disposition="quarantined",
                        field_path=safe_field_path,
                        law_pin=None,
                        conflicting_payload_digest=None,
                        message=(
                            f"Record {rec_idx} field[{f_idx}] is not in the closed allowlist "
                            f"for {target_kind}; quarantined as needs_review."
                        ),
                    )
                )
                continue

            # Fail-closed duplicate field check within one record (never silently overwrite).
            if f_obs.field_name in seen_field_names:
                has_internal_conflict = True
                rec_findings.append(
                    DataQualityFindingRecord(
                        id=f"dqf_{cand_id}_f{f_idx}_duplicate_field",
                        candidate_id=cand_id,
                        evidence_id=ev_id,
                        code="feed_conflict",
                        finding_kind="feed_conflict",
                        severity="error",
                        quarantine_disposition="disputed",
                        field_path=f_obs.field_name,
                        law_pin="core.boundary.import_finding",
                        conflicting_payload_digest=field_digest,
                        message=(
                            f"Record {rec_idx} contains repeated observations for "
                            f"{f_obs.field_name}; quarantined as feed_conflict."
                        ),
                    )
                )
                continue
            seen_field_names[f_obs.field_name] = field_digest

            # Fail-closed value validation (never silently coerce or invent missing values).
            if f_obs.value is None or not f_obs.value.strip():
                rec_findings.append(
                    DataQualityFindingRecord(
                        id=f"dqf_{cand_id}_f{f_idx}_blank",
                        candidate_id=cand_id,
                        evidence_id=ev_id,
                        code="needs_review",
                        finding_kind="schema_violation",
                        severity="error",
                        quarantine_disposition="quarantined",
                        field_path=f_obs.field_name,
                        law_pin=None,
                        conflicting_payload_digest=None,
                        message=(
                            f"Record {rec_idx} field {f_obs.field_name} extracted a blank value."
                        ),
                    )
                )
                continue

            val = f_obs.value.strip()
            if f_obs.field_name in _NUMERIC_FIELDS:
                try:
                    dec = parse_exact_decimal(val)
                    if dec <= 0:
                        raise ExactValueError("Value must be strictly positive")
                    proposal[f_obs.field_name] = decimal_to_canonical_str(dec, 2)
                except ExactValueError as exc:
                    rec_findings.append(
                        DataQualityFindingRecord(
                            id=f"dqf_{cand_id}_f{f_idx}_decimal",
                            candidate_id=cand_id,
                            evidence_id=ev_id,
                            code="needs_review",
                            finding_kind="schema_violation",
                            severity="error",
                            quarantine_disposition="quarantined",
                            field_path=f_obs.field_name,
                            law_pin="norn.values.property_validity",
                            conflicting_payload_digest=None,
                            message=(
                                f"Record {rec_idx} field {f_obs.field_name} failed exact "
                                f"decimal check ({exc})."
                            ),
                        )
                    )
            elif f_obs.field_name in _DATE_FIELDS:
                try:
                    proposal[f_obs.field_name] = validate_iso_date(val)
                except ExactValueError as exc:
                    rec_findings.append(
                        DataQualityFindingRecord(
                            id=f"dqf_{cand_id}_f{f_idx}_date",
                            candidate_id=cand_id,
                            evidence_id=ev_id,
                            code="needs_review",
                            finding_kind="schema_violation",
                            severity="error",
                            quarantine_disposition="quarantined",
                            field_path=f_obs.field_name,
                            law_pin=None,
                            conflicting_payload_digest=None,
                            message=(
                                f"Record {rec_idx} field {f_obs.field_name} failed ISO date "
                                f"check ({exc})."
                            ),
                        )
                    )
            else:
                proposal[f_obs.field_name] = val

        source_record_key = record.record_key or f"schedule_photo/record:{rec_idx}"
        payload_digest = sha256_prefixed(
            {
                "effective_on": effective_on,
                "proposal": proposal,
                "target_kind": target_kind,
            }
        )
        key_digest = sha256_prefixed(source_record_key)[7:19]
        agg_confidence = min(confidences) if confidences else 0
        incoming_clean = not rec_findings

        if source_record_key in seen_keys:
            prev_idx, prev_cand_id, prev_ev_id, prev_digest, prev_clean = seen_keys[
                source_record_key
            ]
            if prev_clean and incoming_clean and prev_digest == payload_digest:
                deduplicated_count += 1
                continue
            # Core 07 FEED-CONFLICT: mark BOTH prior and incoming candidates disputed,
            # and rebind every incoming evidence/finding record to `conflict_id`.
            if candidates[prev_idx].dispute_state != "disputed":
                candidates[prev_idx] = replace(
                    candidates[prev_idx],
                    dispute_state="disputed",
                    status="quarantined",
                )
            seen_keys[source_record_key] = (
                prev_idx,
                prev_cand_id,
                prev_ev_id,
                prev_digest,
                False,
            )
            conflict_id = f"{cand_id}_conflict"
            rebound_evidence = [
                replace(ev, candidate_id=conflict_id, subject_key=conflict_id)
                for ev in rec_evidence
            ]
            rebound_findings = [
                replace(f, candidate_id=conflict_id) for f in rec_findings
            ]
            candidates.append(
                ExtractedCandidateProposal(
                    id=conflict_id,
                    batch_id=batch_id,
                    target_kind=target_kind,
                    source_record_key=source_record_key,
                    payload_digest=payload_digest,
                    confidence_bp=agg_confidence,
                    source_span=first_anchor,
                    extracted_at=extracted_at,
                    observed_at=captured_at,
                    effective_on=effective_on,
                    dispute_state="disputed",
                    status="quarantined",
                    proposal=proposal,
                )
            )
            evidence_records.extend(rebound_evidence)
            findings.extend(rebound_findings)
            findings.append(
                DataQualityFindingRecord(
                    id=f"dqf_{prev_cand_id}_feed_conflict_rec_{rec_idx}",
                    candidate_id=prev_cand_id,
                    evidence_id=prev_ev_id,
                    code="feed_conflict",
                    finding_kind="feed_conflict",
                    severity="error",
                    quarantine_disposition="disputed",
                    field_path="payload_digest",
                    law_pin="core.boundary.import_finding",
                    conflicting_payload_digest=payload_digest,
                    message=(
                        f"Candidate {prev_cand_id} conflicts with OCR record {rec_idx} "
                        f"({conflict_id}) on record key digest {key_digest}."
                    ),
                )
            )
            findings.append(
                DataQualityFindingRecord(
                    id=f"dqf_{conflict_id}_feed_conflict",
                    candidate_id=conflict_id,
                    evidence_id=rebound_evidence[0].id if rebound_evidence else None,
                    code="feed_conflict",
                    finding_kind="feed_conflict",
                    severity="error",
                    quarantine_disposition="disputed",
                    field_path="payload_digest",
                    law_pin="core.boundary.import_finding",
                    conflicting_payload_digest=prev_digest,
                    message=(
                        f"OCR record {rec_idx} conflicts with prior candidate {prev_cand_id} "
                        f"on record key digest {key_digest}."
                    ),
                )
            )
            continue

        seen_keys[source_record_key] = (
            len(candidates),
            cand_id,
            rec_evidence[0].id if rec_evidence else None,
            payload_digest,
            incoming_clean,
        )
        is_quarantined = bool(rec_findings)
        dispute_state: DisputeState = (
            "disputed"
            if has_internal_conflict
            else ("quarantined" if is_quarantined else "clean")
        )
        cand_status: CandidateStatus = "quarantined" if is_quarantined else "staged"
        evidence_records.extend(rec_evidence)
        findings.extend(rec_findings)
        candidates.append(
            ExtractedCandidateProposal(
                id=cand_id,
                batch_id=batch_id,
                target_kind=target_kind,
                source_record_key=source_record_key,
                payload_digest=payload_digest,
                confidence_bp=agg_confidence,
                source_span=first_anchor,
                extracted_at=extracted_at,
                observed_at=captured_at,
                effective_on=effective_on,
                dispute_state=dispute_state,
                status=cand_status,
                proposal=proposal,
            )
        )

    result = ImportBundleResult(
        batch=batch,
        candidates=tuple(candidates),
        evidence=tuple(evidence_records),
        findings=tuple(findings),
        deduplicated_count=deduplicated_count,
    )
    if logger is not None:
        logger.record_extraction_summary(
            batch_id=batch_id,
            channel="schedule_photo",
            document_digest=document_digest,
            candidate_count=len(candidates),
            finding_count=len(findings),
            deduplicated_count=deduplicated_count,
            finding_codes=[f.code for f in findings],
        )
    return result
