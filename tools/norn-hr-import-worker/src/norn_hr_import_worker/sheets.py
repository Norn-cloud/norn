"""Deterministic CSV/TSV HR sheet extractor with strict schema validation and zero floats."""

from __future__ import annotations

import csv
import io
from collections.abc import Mapping
from dataclasses import replace
from decimal import Decimal
from typing import Literal

from norn_hr_import_worker.models import (
    EXTRACTOR_NAME,
    EXTRACTOR_VERSION,
    DataQualityFindingRecord,
    ExactValueError,
    ExtractedCandidateProposal,
    FieldEvidenceRecord,
    ImportBundleResult,
    JsonValue,
    SafeWorkerLogger,
    SourceDocumentBatch,
    TargetKind,
    decimal_to_canonical_str,
    duration_hours_exact,
    parse_exact_decimal,
    parse_hhmm_minutes,
    sha256_bytes,
    sha256_prefixed,
    validate_iso_date,
)

SheetProfile = Literal["attendance", "roster", "leave", "employee"]

_REQUIRED_COLUMNS: dict[SheetProfile, frozenset[str]] = {
    "attendance": frozenset({"branch_code", "employee_code", "work_date"}),
    "roster": frozenset(
        {"branch_code", "employee_code", "role_code", "shift_date", "start_time", "end_time"}
    ),
    "leave": frozenset({"employee_code", "leave_kind", "starts_on", "ends_on", "days"}),
    "employee": frozenset(
        {
            "employee_code",
            "display_name",
            "effective_from",
            "currency",
            "base_salary",
            "max_weekly_hours",
            "primary_skill",
        }
    ),
}

_ALLOWED_COLUMNS: dict[SheetProfile, frozenset[str]] = {
    "attendance": frozenset(
        {
            "branch_code",
            "employee_code",
            "employee_name",
            "work_date",
            "clock_in",
            "clock_out",
            "clock_in_eet",
            "clock_out_eet",
            "clock_in_local",
            "clock_out_local",
            "hours",
            "notes",
        }
    ),
    "roster": frozenset(
        {
            "branch_code",
            "employee_code",
            "employee_name",
            "role_code",
            "shift_date",
            "start_time",
            "end_time",
            "hours",
            "notes",
        }
    ),
    "leave": frozenset(
        {
            "employee_code",
            "employee_name",
            "leave_kind",
            "starts_on",
            "ends_on",
            "days",
            "reason",
        }
    ),
    "employee": frozenset(
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

_ALLOWED_LEAVE_KINDS = frozenset({"annual", "sick", "unpaid", "emergency", "compensatory"})
_PROFILE_TARGET_KIND: dict[SheetProfile, TargetKind] = {
    "attendance": "attendance_event",
    "roster": "shift_commitment",
    "leave": "leave_request",
    "employee": "person",
}


class SheetSchemaError(ValueError):
    """Fail-closed header or framing refusal for HR CSV/TSV sheets."""


def _validate_headers(profile: SheetProfile, headers: list[str]) -> None:
    if not headers:
        raise SheetSchemaError("Sheet is empty or missing header row")
    if len(set(headers)) != len(headers):
        raise SheetSchemaError(f"Duplicate column names in {profile} sheet header")
    header_set = set(headers)
    missing = sorted(_REQUIRED_COLUMNS[profile] - header_set)
    if missing:
        raise SheetSchemaError(f"Missing required {profile} columns: {', '.join(missing)}")
    unknown = sorted(header_set - _ALLOWED_COLUMNS[profile])
    if unknown:
        raise SheetSchemaError(f"Unknown columns refused for {profile} sheet: {', '.join(unknown)}")
    if profile == "attendance":
        has_in = bool(header_set & {"clock_in", "clock_in_eet", "clock_in_local"})
        has_out = bool(header_set & {"clock_out", "clock_out_eet", "clock_out_local"})
        if not (has_in and has_out):
            raise SheetSchemaError("Attendance sheet requires matching clock_in/clock_out columns")


def _first_present(row: Mapping[str, str], keys: tuple[str, ...]) -> tuple[str, str]:
    for key in keys:
        if key in row:
            return key, row[key].strip()
    return keys[0], ""


def _parse_row(
    profile: SheetProfile, row: Mapping[str, str]
) -> tuple[str, str, dict[str, JsonValue], list[tuple[str, str, str, str | None]]]:
    """Return (record_key, effective_on, proposal, errors[(code, field_path, message, law_pin)])."""
    errors: list[tuple[str, str, str, str | None]] = []

    if profile == "attendance":
        branch = row["branch_code"].strip()
        emp = row["employee_code"].strip()
        raw_date = row["work_date"].strip()
        in_col, clock_in = _first_present(row, ("clock_in", "clock_in_eet", "clock_in_local"))
        out_col, clock_out = _first_present(row, ("clock_out", "clock_out_eet", "clock_out_local"))
        effective_on = raw_date
        try:
            effective_on = validate_iso_date(raw_date)
        except ExactValueError as exc:
            errors.append(("invalid_date", "work_date", str(exc), None))
        if not branch:
            errors.append(("missing_branch", "branch_code", "branch_code is blank", None))
        if not emp:
            errors.append(("missing_employee", "employee_code", "employee_code is blank", None))
        if not clock_in:
            errors.append(("missing_clock_in", in_col, "Missing clock-in time", None))
        else:
            try:
                parse_hhmm_minutes(clock_in)
            except ExactValueError as exc:
                errors.append(
                    ("invalid_time_interval", in_col, str(exc), "core.TimeInterval.proper_interval")
                )
        if not clock_out:
            errors.append(
                (
                    "missing_clock_out",
                    out_col,
                    "Missing clock-out time; quarantined pending regularization",
                    "core.TimeInterval.proper_interval",
                )
            )
        else:
            try:
                parse_hhmm_minutes(clock_out)
            except ExactValueError as exc:
                errors.append(
                    (
                        "invalid_time_interval",
                        out_col,
                        str(exc),
                        "core.TimeInterval.proper_interval",
                    )
                )
        interval_hours: Decimal | None = None
        if clock_in and clock_out:
            try:
                interval_hours = duration_hours_exact(clock_in, clock_out)
            except ExactValueError as exc:
                errors.append(
                    (
                        "invalid_time_interval",
                        out_col,
                        str(exc),
                        "core.TimeInterval.proper_interval",
                    )
                )
        hours_str: str | None = None
        if "hours" in row and row["hours"].strip():
            try:
                hours_dec = parse_exact_decimal(row["hours"].strip())
                if hours_dec <= 0:
                    errors.append(
                        (
                            "non_positive_hours",
                            "hours",
                            "Worked hours must be strictly positive",
                            None,
                        )
                    )
                elif interval_hours is not None and abs(hours_dec - interval_hours) > Decimal(
                    "0.01"
                ):
                    errors.append(
                        (
                            "hours_interval_mismatch",
                            "hours",
                            "Explicit hours does not match clock-in/clock-out interval",
                            "core.TimeInterval.proper_interval",
                        )
                    )
                hours_str = decimal_to_canonical_str(hours_dec, 2)
            except ExactValueError as exc:
                errors.append(("invalid_decimal", "hours", str(exc), None))
        elif interval_hours is not None:
            hours_str = decimal_to_canonical_str(interval_hours, 2)
        proposal: dict[str, JsonValue] = {
            "branch_code": branch,
            "clock_in": clock_in or None,
            "clock_out": clock_out or None,
            "employee_code": emp,
            "quantity_hours": hours_str,
            "unit": "h",
            "work_date": effective_on,
        }
        return (
            f"spreadsheet_csv/attendance/{branch}/{emp}/{effective_on}",
            effective_on,
            proposal,
            errors,
        )

    if profile == "roster":
        branch = row["branch_code"].strip()
        emp = row["employee_code"].strip()
        role_code = row["role_code"].strip()
        raw_date = row["shift_date"].strip()
        start_time = row["start_time"].strip()
        end_time = row["end_time"].strip()
        effective_on = raw_date
        try:
            effective_on = validate_iso_date(raw_date)
        except ExactValueError as exc:
            errors.append(("invalid_date", "shift_date", str(exc), None))
        if not branch:
            errors.append(("missing_branch", "branch_code", "branch_code is blank", None))
        if not emp:
            errors.append(("missing_employee", "employee_code", "employee_code is blank", None))
        if not role_code:
            errors.append(("missing_role", "role_code", "role_code is blank", None))
        interval_hours = None
        try:
            interval_hours = duration_hours_exact(start_time, end_time)
        except ExactValueError as exc:
            errors.append(
                (
                    "invalid_time_interval",
                    "end_time",
                    str(exc),
                    "core.TimeInterval.proper_interval",
                )
            )
        hours_str = None
        if "hours" in row and row["hours"].strip():
            try:
                hours_dec = parse_exact_decimal(row["hours"].strip())
                if hours_dec <= 0:
                    errors.append(("non_positive_hours", "hours", "Shift hours must be > 0", None))
                elif interval_hours is not None and abs(hours_dec - interval_hours) > Decimal(
                    "0.01"
                ):
                    errors.append(
                        (
                            "hours_interval_mismatch",
                            "hours",
                            "Explicit hours does not match start_time/end_time interval",
                            "core.TimeInterval.proper_interval",
                        )
                    )
                hours_str = decimal_to_canonical_str(hours_dec, 2)
            except ExactValueError as exc:
                errors.append(("invalid_decimal", "hours", str(exc), None))
        elif interval_hours is not None:
            hours_str = decimal_to_canonical_str(interval_hours, 2)
        proposal = {
            "branch_code": branch,
            "employee_code": emp,
            "end_time": end_time,
            "quantity_hours": hours_str,
            "role_code": role_code,
            "shift_date": effective_on,
            "start_time": start_time,
            "unit": "h",
        }
        return (
            f"spreadsheet_csv/roster/{branch}/{emp}/{effective_on}/{start_time}",
            effective_on,
            proposal,
            errors,
        )

    if profile == "leave":
        emp = row["employee_code"].strip()
        leave_kind = row["leave_kind"].strip()
        starts_on = row["starts_on"].strip()
        ends_on = row["ends_on"].strip()
        raw_days = row["days"].strip()
        effective_on = starts_on
        starts_valid: str | None = None
        ends_valid: str | None = None
        try:
            starts_valid = validate_iso_date(starts_on)
            effective_on = starts_valid
        except ExactValueError as exc:
            errors.append(("invalid_date", "starts_on", str(exc), None))
        try:
            ends_valid = validate_iso_date(ends_on)
        except ExactValueError as exc:
            errors.append(("invalid_date", "ends_on", str(exc), None))
        if starts_valid is not None and ends_valid is not None and starts_valid > ends_valid:
            errors.append(
                (
                    "reversed_date_range",
                    "ends_on",
                    "starts_on must be on or before ends_on",
                    "hr.LeaveRequest.valid_date_range",
                )
            )
        if leave_kind not in _ALLOWED_LEAVE_KINDS:
            errors.append(
                ("invalid_leave_kind", "leave_kind", "Unsupported leave_kind value", None)
            )
        days_str: str | None = None
        try:
            days_dec = parse_exact_decimal(raw_days)
            if days_dec <= 0:
                errors.append(
                    (
                        "non_positive_days",
                        "days",
                        "Leave days must be strictly positive",
                        "hr.LeaveRequest.positive_days",
                    )
                )
            days_str = decimal_to_canonical_str(days_dec, 2)
        except ExactValueError as exc:
            errors.append(("invalid_decimal", "days", str(exc), None))
        proposal = {
            "days": days_str,
            "employee_code": emp,
            "ends_on": ends_on,
            "leave_kind": leave_kind,
            "starts_on": effective_on,
            "unit": "d",
        }
        return (
            f"spreadsheet_csv/leave/{emp}/{effective_on}/{leave_kind}",
            effective_on,
            proposal,
            errors,
        )

    # employee profile
    emp = row["employee_code"].strip()
    display_name = row["display_name"].strip()
    effective_from = row["effective_from"].strip()
    currency = row["currency"].strip()
    primary_skill = row["primary_skill"].strip()
    jurisdiction = row.get("jurisdiction_code", "").strip() or None
    effective_on = effective_from
    try:
        effective_on = validate_iso_date(effective_from)
    except ExactValueError as exc:
        errors.append(("invalid_date", "effective_from", str(exc), None))
    base_salary_str: str | None = None
    try:
        sal = parse_exact_decimal(row["base_salary"].strip())
        if sal < Decimal(0):
            errors.append(("negative_salary", "base_salary", "base_salary must be >= 0", None))
        base_salary_str = decimal_to_canonical_str(sal, 2)
    except ExactValueError as exc:
        errors.append(("invalid_decimal", "base_salary", str(exc), None))
    max_hours_str: str | None = None
    try:
        hrs = parse_exact_decimal(row["max_weekly_hours"].strip())
        if hrs <= Decimal(0):
            errors.append(
                ("non_positive_hours", "max_weekly_hours", "max_weekly_hours must be > 0", None)
            )
        max_hours_str = decimal_to_canonical_str(hrs, 2)
    except ExactValueError as exc:
        errors.append(("invalid_decimal", "max_weekly_hours", str(exc), None))
    if len(currency) != 3 or not currency.isupper():
        errors.append(
            ("invalid_currency", "currency", "currency must be 3-letter uppercase ISO 4217", None)
        )
    proposal = {
        "base_salary": base_salary_str,
        "currency": currency,
        "display_name": display_name,
        "effective_from": effective_on,
        "employee_code": emp,
        "jurisdiction_code": jurisdiction,
        "max_weekly_hours": max_hours_str,
        "primary_skill": primary_skill,
    }
    return f"spreadsheet_csv/employee/{emp}/{effective_on}", effective_on, proposal, errors


def extract_sheet(
    *,
    raw_bytes: bytes,
    document_uri: str,
    batch_id: str,
    profile: SheetProfile,
    captured_at: str,
    extracted_at: str,
    delimiter: Literal[",", "\t"] | None = None,
    logger: SafeWorkerLogger | None = None,
) -> ImportBundleResult:
    """Parse a CSV/TSV HR sheet with strict column checks, FEED-CONFLICT, and exact decimals."""
    document_digest = sha256_bytes(raw_bytes)
    text = raw_bytes.decode("utf-8-sig")
    chosen_delim: Literal[",", "\t"]
    if delimiter is not None:
        chosen_delim = delimiter
    else:
        first_line = text.splitlines()[0] if text.splitlines() else ""
        chosen_delim = "\t" if "\t" in first_line and "," not in first_line else ","

    media_type = "text/tab-separated-values" if chosen_delim == "\t" else "text/csv"
    reader = csv.reader(io.StringIO(text), delimiter=chosen_delim)
    rows = list(reader)
    if not rows:
        raise SheetSchemaError("Sheet contains zero rows")
    headers = [h.strip() for h in rows[0]]
    _validate_headers(profile, headers)

    batch = SourceDocumentBatch(
        id=batch_id,
        channel="spreadsheet_csv",
        document_uri=document_uri,
        document_digest=document_digest,
        media_type=media_type,
        extractor=EXTRACTOR_NAME,
        extractor_version=EXTRACTOR_VERSION,
        captured_at=captured_at,
    )

    target_kind = _PROFILE_TARGET_KIND[profile]
    total_rows = max(len(rows), 1)
    candidates: list[ExtractedCandidateProposal] = []
    evidence_records: list[FieldEvidenceRecord] = []
    findings: list[DataQualityFindingRecord] = []
    seen_keys: dict[str, tuple[int, str, str, str]] = {}
    deduplicated_count = 0

    for row_number, raw_cells in enumerate(rows[1:], start=2):
        if not any(cell.strip() for cell in raw_cells):
            continue
        if len(raw_cells) != len(headers):
            raise SheetSchemaError(
                f"Row {row_number} has {len(raw_cells)} columns; expected {len(headers)}"
            )
        row_map = dict(zip(headers, raw_cells, strict=True))
        raw_row_text = chosen_delim.join(raw_cells)
        row_digest = sha256_prefixed({"row": row_number, "values": raw_cells})
        record_key, effective_on, proposal, row_errors = _parse_row(profile, row_map)
        payload_digest = sha256_prefixed(proposal)
        key_digest = sha256_prefixed(record_key)[7:19]
        source_span = f"row:{row_number}"
        y_frac = decimal_to_canonical_str(
            parse_exact_decimal(row_number - 1) / parse_exact_decimal(total_rows), 4
        )
        h_frac = decimal_to_canonical_str(Decimal(1) / parse_exact_decimal(total_rows), 4)

        if record_key in seen_keys:
            prev_idx, prev_cand_id, prev_ev_id, prev_digest = seen_keys[record_key]
            if prev_digest == payload_digest:
                deduplicated_count += 1
                continue
            # Core 07 FEED-CONFLICT: mark BOTH existing and incoming candidates disputed.
            if candidates[prev_idx].dispute_state != "disputed":
                candidates[prev_idx] = replace(
                    candidates[prev_idx],
                    dispute_state="disputed",
                    status="quarantined",
                )
            cand_id = f"{batch_id}_row_{row_number}_conflict"
            ev_id = f"{cand_id}_ev"
            evidence_records.append(
                FieldEvidenceRecord(
                    id=ev_id,
                    batch_id=batch_id,
                    candidate_id=cand_id,
                    subject_property="payload_digest",
                    subject_type=f"hr.{target_kind}",
                    subject_key=cand_id,
                    document_uri=document_uri,
                    document_digest=document_digest,
                    media_type=media_type,
                    page=1,
                    bbox_x="0.0000",
                    bbox_y=y_frac,
                    bbox_w="1.0000",
                    bbox_h=h_frac,
                    text=raw_row_text,
                    extractor=EXTRACTOR_NAME,
                    extractor_version=EXTRACTOR_VERSION,
                    confidence_bp=9850,
                    source_anchor=f"row:{row_number}#{row_digest[7:19]}",
                    record_digest=row_digest,
                )
            )
            candidates.append(
                ExtractedCandidateProposal(
                    id=cand_id,
                    batch_id=batch_id,
                    target_kind=target_kind,
                    source_record_key=record_key,
                    payload_digest=payload_digest,
                    confidence_bp=9850,
                    source_span=source_span,
                    extracted_at=extracted_at,
                    observed_at=captured_at,
                    effective_on=effective_on if len(effective_on) == 10 else captured_at[0:10],
                    dispute_state="disputed",
                    status="quarantined",
                    proposal=proposal,
                )
            )
            findings.append(
                DataQualityFindingRecord(
                    id=f"dqf_{prev_cand_id}_feed_conflict_row_{row_number}",
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
                        f"Candidate {prev_cand_id} conflicts with row {row_number} "
                        f"({cand_id}) on record key digest {key_digest}."
                    ),
                )
            )
            findings.append(
                DataQualityFindingRecord(
                    id=f"dqf_{cand_id}_feed_conflict",
                    candidate_id=cand_id,
                    evidence_id=ev_id,
                    code="feed_conflict",
                    finding_kind="feed_conflict",
                    severity="error",
                    quarantine_disposition="disputed",
                    field_path="payload_digest",
                    law_pin="core.boundary.import_finding",
                    conflicting_payload_digest=prev_digest,
                    message=(
                        f"Row {row_number} conflicts with prior candidate {prev_cand_id} "
                        f"on record key digest {key_digest}."
                    ),
                )
            )
            continue

        cand_id = f"{batch_id}_row_{row_number}"
        ev_id = f"{cand_id}_ev"
        seen_keys[record_key] = (len(candidates), cand_id, ev_id, payload_digest)
        is_quarantined = bool(row_errors)
        confidence_bp = 6200 if is_quarantined else 9850
        evidence_records.append(
            FieldEvidenceRecord(
                id=ev_id,
                batch_id=batch_id,
                candidate_id=cand_id,
                subject_property=row_errors[0][1] if row_errors else "record",
                subject_type=f"hr.{target_kind}",
                subject_key=cand_id,
                document_uri=document_uri,
                document_digest=document_digest,
                media_type=media_type,
                page=1,
                bbox_x="0.0000",
                bbox_y=y_frac,
                bbox_w="1.0000",
                bbox_h=h_frac,
                text=raw_row_text,
                extractor=EXTRACTOR_NAME,
                extractor_version=EXTRACTOR_VERSION,
                confidence_bp=confidence_bp,
                source_anchor=f"row:{row_number}#{row_digest[7:19]}",
                record_digest=row_digest,
            )
        )
        safe_effective_on = effective_on
        try:
            validate_iso_date(safe_effective_on)
        except ExactValueError:
            safe_effective_on = captured_at[0:10]
        candidates.append(
            ExtractedCandidateProposal(
                id=cand_id,
                batch_id=batch_id,
                target_kind=target_kind,
                source_record_key=record_key,
                payload_digest=payload_digest,
                confidence_bp=confidence_bp,
                source_span=source_span,
                extracted_at=extracted_at,
                observed_at=captured_at,
                effective_on=safe_effective_on,
                dispute_state="quarantined" if is_quarantined else "clean",
                status="quarantined" if is_quarantined else "staged",
                proposal=proposal,
            )
        )
        for err_idx, (code, field_path, message, law_pin) in enumerate(row_errors, start=1):
            findings.append(
                DataQualityFindingRecord(
                    id=f"dqf_{cand_id}_{err_idx}_{code}",
                    candidate_id=cand_id,
                    evidence_id=ev_id,
                    code=code,
                    finding_kind="schema_violation",
                    severity="error",
                    quarantine_disposition="quarantined",
                    field_path=field_path,
                    law_pin=law_pin,
                    conflicting_payload_digest=None,
                    message=f"Row {row_number}: {message}",
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
            channel="spreadsheet_csv",
            document_digest=document_digest,
            candidate_count=len(candidates),
            finding_count=len(findings),
            deduplicated_count=deduplicated_count,
            finding_codes=[f.code for f in findings],
        )
    return result
