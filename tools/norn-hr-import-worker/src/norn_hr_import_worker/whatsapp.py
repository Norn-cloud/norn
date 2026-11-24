"""Deterministic WhatsApp shift/leave/attendance chat export parser (Arabic & English)."""

from __future__ import annotations

import re
from dataclasses import dataclass, replace
from datetime import UTC, datetime

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
    duration_hours_exact,
    parse_exact_decimal,
    sha256_bytes,
    sha256_prefixed,
    validate_iso_date,
)

# Normalize Arabic-Indic (U+0660..U+0669) and Eastern Arabic-Indic (U+06F0..U+06F9) digits.
_DIGIT_TRANSLATION = str.maketrans("٠١٢٣٤٥٦٧٨٩۰۱۲۳۴۵۶۷۸۹", "01234567890123456789")

# Supports:
#   [22/11/2026, 18:05] Sender: body
#   [2026-11-22, 18:05:10] Sender: body
#   22/11/2026, 6:05 PM - Sender: body
#   22/11/2026, 06:05 م - Sender: body
_HEADER_RE = re.compile(
    r"^(?:\[(?P<b_date>\d{2}/\d{2}/\d{4}|\d{4}-\d{2}-\d{2}),\s*"
    r"(?P<b_time>\d{1,2}:\d{2}(?::\d{2})?)(?:\s*(?P<b_ampm>AM|PM|am|pm|ص|م))?\]\s*"
    r"(?P<b_sender>[^:]+?):\s*(?P<b_body>.*)|"
    r"(?P<d_date>\d{2}/\d{2}/\d{4}|\d{4}-\d{2}-\d{2}),\s*"
    r"(?P<d_time>\d{1,2}:\d{2}(?::\d{2})?)(?:\s*(?P<d_ampm>AM|PM|am|pm|ص|م))?\s*-\s*"
    r"(?P<d_sender>[^:]+?):\s*(?P<d_body>.*))$"
)

_TIME_RANGE_RE = re.compile(r"\b(\d{1,2}:\d{2})\s*[-–—]\s*(\d{1,2}:\d{2})\b")
_ISO_DATE_INLINE_RE = re.compile(r"\b(\d{4}-\d{2}-\d{2})\b")
_DMY_DATE_INLINE_RE = re.compile(r"\b(\d{1,2})/(\d{1,2})(?:/(\d{4}))?\b")

_SHIFT_ROLES = (
    "cashier",
    "supervisor",
    "barista",
    "clerk",
    "driver",
    "nurse",
    "security",
    "كاشير",
    "مشرف",
    "سائق",
)
_ROLE_CANONICAL: dict[str, str] = {
    "cashier": "cashier",
    "supervisor": "supervisor",
    "barista": "barista",
    "clerk": "clerk",
    "driver": "driver",
    "nurse": "nurse",
    "security": "security",
    "كاشير": "cashier",
    "مشرف": "supervisor",
    "سائق": "driver",
}

_LEAVE_KIND_KEYWORDS: tuple[tuple[str, str], ...] = (
    ("sick", "sick"),
    ("مرضي", "sick"),
    ("مرضية", "sick"),
    ("annual", "annual"),
    ("سنوي", "annual"),
    ("سنوية", "annual"),
    ("emergency", "emergency"),
    ("عارضة", "emergency"),
    ("طارئة", "emergency"),
    ("unpaid", "unpaid"),
    ("بدون أجر", "unpaid"),
)
_LEAVE_TRIGGER_WORDS = ("leave", "إجازة", "اجازة", "اجازه", "إجازه")
_ATTENDANCE_TRIGGER_WORDS = ("clocked in", "clock in", "check-in", "attendance", "حضور", "انصراف")
_SHIFT_TRIGGER_WORDS = ("shift", "شيفت", "وردية", "morning", "evening", "مساء", "صباح")
_SELF_ASSIGNMENT_MARKERS = (
    "i will",
    "i'll",
    "i am",
    "i'm",
    "my shift",
    "i can take",
    "i'll take",
    "أنا",
    "انا",
    "هاخد",
    "سأغطي",
    "شيفتي",
    "مناوبتي",
)
_DAYS_RE = re.compile(r"\b(\d+(?:\.\d+)?)\s*(?:days?|day|d|أيام|ايام|يوم)\b", re.IGNORECASE)
_NAMED_SHIFT_RE = re.compile(
    r"(?:^|[—\-:,]\s*)(?P<person>[A-Z][a-z]+(?:\s+[A-Z][a-z]+)+)\s*(?::\s*|\s+)"
    r"(?:(?:[Mm]orning|[Ee]vening|[Nn]ight)\s+)?"
    r"(?P<role>[Cc]ashier|[Ss]upervisor|[Bb]arista|[Cc]lerk|[Dd]river|[Nn]urse|[Ss]ecurity)"
)


@dataclass(frozen=True, slots=True)
class ParsedChatMessage:
    """One parsed multi-line WhatsApp message with exact 1-based line bounds."""

    index: int
    start_line: int
    end_line: int
    observed_at: str
    observed_date: str
    sender: str
    body: str
    message_digest: str


def _normalize_digits(text: str) -> str:
    return text.translate(_DIGIT_TRANSLATION)


def _parse_chat_timestamp(date_str: str, time_str: str, ampm: str | None) -> tuple[str, str]:
    date_norm = _normalize_digits(date_str)
    time_norm = _normalize_digits(time_str)
    if "/" in date_norm:
        day_s, month_s, year_s = date_norm.split("/")
        iso_date = validate_iso_date(f"{int(year_s):04d}-{int(month_s):02d}-{int(day_s):02d}")
    else:
        iso_date = validate_iso_date(date_norm)

    parts = time_norm.split(":")
    hour = int(parts[0])
    minute = int(parts[1])
    second = int(parts[2]) if len(parts) == 3 else 0
    if ampm is not None:
        marker = ampm.lower()
        if marker in {"pm", "م"} and hour < 12:
            hour += 12
        elif marker in {"am", "ص"} and hour == 12:
            hour = 0
    if hour > 23 or minute > 59 or second > 59:
        raise ExactValueError("Out-of-range chat timestamp")
    dt = datetime(
        int(iso_date[0:4]),
        int(iso_date[5:7]),
        int(iso_date[8:10]),
        hour,
        minute,
        second,
        tzinfo=UTC,
    )
    return dt.strftime("%Y-%m-%dT%H:%M:%SZ"), iso_date


def _parse_chat_messages(text: str) -> list[ParsedChatMessage]:
    raw_lines = text.splitlines()
    grouped: list[tuple[int, int, str, str, str, list[str]]] = []

    for line_no, raw_line in enumerate(raw_lines, start=1):
        stripped = raw_line.lstrip("\ufeff")
        norm = _normalize_digits(stripped)
        match = _HEADER_RE.match(norm)
        if match is not None:
            if match.group("b_date") is not None:
                d_s, t_s, ap = match.group("b_date"), match.group("b_time"), match.group("b_ampm")
                sender = match.group("b_sender").strip()
                body = match.group("b_body").strip()
            else:
                d_s, t_s, ap = match.group("d_date"), match.group("d_time"), match.group("d_ampm")
                sender = match.group("d_sender").strip()
                body = match.group("d_body").strip()
            observed_at, observed_date = _parse_chat_timestamp(d_s, t_s, ap)
            grouped.append((line_no, line_no, observed_at, observed_date, sender, [body]))
        elif grouped:
            s_ln, _, obs_at, obs_d, snd, bodies = grouped[-1]
            bodies.append(stripped.strip())
            grouped[-1] = (s_ln, line_no, obs_at, obs_d, snd, bodies)

    messages: list[ParsedChatMessage] = []
    for idx, (start_ln, end_ln, obs_at, obs_d, sender, body_lines) in enumerate(grouped, start=1):
        full_body = "\n".join(b for b in body_lines if b)
        msg_digest = sha256_prefixed(
            {
                "body": full_body,
                "end_line": end_ln,
                "observed_at": obs_at,
                "sender": sender,
                "start_line": start_ln,
            }
        )
        messages.append(
            ParsedChatMessage(
                index=idx,
                start_line=start_ln,
                end_line=end_ln,
                observed_at=obs_at,
                observed_date=obs_d,
                sender=sender,
                body=full_body,
                message_digest=msg_digest,
            )
        )
    return messages


def _extract_effective_date(body: str, default_date: str) -> str:
    norm = _normalize_digits(body)
    iso_match = _ISO_DATE_INLINE_RE.search(norm)
    if iso_match is not None:
        return validate_iso_date(iso_match.group(1))
    dmy_match = _DMY_DATE_INLINE_RE.search(norm)
    if dmy_match is not None:
        day = int(dmy_match.group(1))
        month = int(dmy_match.group(2))
        year = int(dmy_match.group(3)) if dmy_match.group(3) else int(default_date[0:4])
        return validate_iso_date(f"{year:04d}-{month:02d}-{day:02d}")
    return default_date


def _pad_hhmm(raw: str) -> str:
    h, m = raw.split(":")
    return f"{int(h):02d}:{int(m):02d}"


def _classify_message(
    msg: ParsedChatMessage,
) -> tuple[TargetKind, dict[str, JsonValue], int, str | None, str] | None:
    """Return (target_kind, proposal, confidence_bp, quarantine_code, effective_on) or None."""
    norm_body = _normalize_digits(msg.body)
    lower = norm_body.lower()
    effective_on = _extract_effective_date(norm_body, msg.observed_date)

    # 1. Leave request detection (Arabic or English)
    if any(trigger in lower for trigger in _LEAVE_TRIGGER_WORDS):
        leave_kind = "annual"
        for kw, canonical in _LEAVE_KIND_KEYWORDS:
            if kw in lower:
                leave_kind = canonical
                break
        days_match = _DAYS_RE.search(norm_body)
        if days_match is None:
            return (
                "leave_request",
                {
                    "employee_label": msg.sender,
                    "leave_kind": leave_kind,
                    "starts_on": effective_on,
                },
                5500,
                "missing_leave_days",
                effective_on,
            )
        days_dec = parse_exact_decimal(days_match.group(1))
        if days_dec <= 0:
            return (
                "leave_request",
                {
                    "days": decimal_to_canonical_str(days_dec, 2),
                    "employee_label": msg.sender,
                    "leave_kind": leave_kind,
                    "starts_on": effective_on,
                },
                5000,
                "non_positive_leave_days",
                effective_on,
            )
        return (
            "leave_request",
            {
                "days": decimal_to_canonical_str(days_dec, 2),
                "employee_label": msg.sender,
                "leave_kind": leave_kind,
                "starts_on": effective_on,
            },
            9100,
            None,
            effective_on,
        )

    # 2. Attendance report detection
    time_match = _TIME_RANGE_RE.search(norm_body)
    if any(trigger in lower for trigger in _ATTENDANCE_TRIGGER_WORDS):
        if time_match is None:
            return (
                "attendance_event",
                {
                    "employee_label": msg.sender,
                    "work_date": effective_on,
                },
                5800,
                "missing_time_window",
                effective_on,
            )
        start_hhmm = _pad_hhmm(time_match.group(1))
        end_hhmm = _pad_hhmm(time_match.group(2))
        try:
            hours = duration_hours_exact(start_hhmm, end_hhmm)
        except ExactValueError:
            return (
                "attendance_event",
                {
                    "clock_in": start_hhmm,
                    "clock_out": end_hhmm,
                    "employee_label": msg.sender,
                    "work_date": effective_on,
                },
                5200,
                "non_positive_duration",
                effective_on,
            )
        return (
            "attendance_event",
            {
                "clock_in": start_hhmm,
                "clock_out": end_hhmm,
                "employee_label": msg.sender,
                "quantity_hours": decimal_to_canonical_str(hours, 2),
                "unit": "h",
                "work_date": effective_on,
            },
            9300,
            None,
            effective_on,
        )

    # 3. Shift assignment / swap detection (requires explicit shift or role keyword).
    has_shift_marker = any(trigger in lower for trigger in _SHIFT_TRIGGER_WORDS) or any(
        role_token in lower for role_token in _SHIFT_ROLES
    )
    if has_shift_marker:
        if time_match is None:
            return None  # Conversational coordination message without a concrete shift window.
        start_hhmm = _pad_hhmm(time_match.group(1))
        end_hhmm = _pad_hhmm(time_match.group(2))
        try:
            hours = duration_hours_exact(start_hhmm, end_hhmm)
        except ExactValueError:
            return (
                "shift_commitment",
                {
                    "employee_label": None,
                    "end_time": end_hhmm,
                    "role_code": None,
                    "shift_date": effective_on,
                    "start_time": start_hhmm,
                },
                5200,
                "non_positive_duration",
                effective_on,
            )

        employee_label: str | None = None
        role_code: str | None = None
        named_match = _NAMED_SHIFT_RE.search(norm_body)
        if named_match is not None:
            employee_label = named_match.group("person").strip()
            role_code = _ROLE_CANONICAL[named_match.group("role").lower()]
        else:
            for role_token in _SHIFT_ROLES:
                if role_token in lower:
                    role_code = _ROLE_CANONICAL[role_token]
                    break
            if any(marker in lower for marker in _SELF_ASSIGNMENT_MARKERS):
                employee_label = msg.sender

        issue_code: str | None = None
        if employee_label is None:
            issue_code = "missing_employee"
            confidence_bp = 5400
        elif role_code is None:
            issue_code = "missing_role"
            confidence_bp = 6000
        else:
            is_confirmation = any(
                kw in lower for kw in ("تم تأكيد", "confirmed", "approved", "موافقة", "cashier")
            )
            confidence_bp = 9450 if is_confirmation else 8700

        return (
            "shift_commitment",
            {
                "employee_label": employee_label,
                "end_time": end_hhmm,
                "quantity_hours": decimal_to_canonical_str(hours, 2),
                "role_code": role_code,
                "shift_date": effective_on,
                "start_time": start_hhmm,
                "unit": "h",
            },
            confidence_bp,
            issue_code,
            effective_on,
        )

    return None


def extract_whatsapp_export(
    *,
    raw_bytes: bytes,
    document_uri: str,
    batch_id: str,
    captured_at: str,
    extracted_at: str,
    min_confidence_bp: int = 8500,
    logger: SafeWorkerLogger | None = None,
) -> ImportBundleResult:
    """Deterministically parse a WhatsApp chat export into Core 07 proposals & evidence."""
    document_digest = sha256_bytes(raw_bytes)
    text = raw_bytes.decode("utf-8")
    messages = _parse_chat_messages(text)
    total_lines = max(len(text.splitlines()), 1)

    batch = SourceDocumentBatch(
        id=batch_id,
        channel="whatsapp_export",
        document_uri=document_uri,
        document_digest=document_digest,
        media_type="text/plain",
        extractor=EXTRACTOR_NAME,
        extractor_version=EXTRACTOR_VERSION,
        captured_at=captured_at,
    )

    candidates: list[ExtractedCandidateProposal] = []
    evidence_records: list[FieldEvidenceRecord] = []
    findings: list[DataQualityFindingRecord] = []
    seen_records: dict[str, tuple[int, str, str, str]] = {}
    deduplicated_count = 0

    for msg in messages:
        classified = _classify_message(msg)
        if classified is None:
            continue
        target_kind, proposal, confidence_bp, issue_code, effective_on = classified
        raw_emp = proposal.get("employee_label")
        if isinstance(raw_emp, str) and raw_emp.strip():
            employee_key = raw_emp.strip().lower().replace(" ", "_")
        else:
            employee_key = f"unresolved_msg_{msg.index}"
        source_record_key = f"whatsapp_export/{target_kind}/{employee_key}/{effective_on}"
        payload_digest = sha256_prefixed(proposal)
        key_digest = sha256_prefixed(source_record_key)[7:19]
        anchor = f"line:{msg.start_line}-{msg.end_line}#{msg.message_digest[7:19]}"
        source_span = f"line:{msg.start_line}-{msg.end_line}"

        if source_record_key in seen_records:
            prev_idx, prev_cand_id, prev_ev_id, prev_digest = seen_records[source_record_key]
            if prev_digest == payload_digest:
                deduplicated_count += 1
                continue
            # Core 07 FEED-CONFLICT: mark BOTH prior and incoming candidates disputed.
            if candidates[prev_idx].dispute_state != "disputed":
                candidates[prev_idx] = replace(
                    candidates[prev_idx],
                    dispute_state="disputed",
                    status="quarantined",
                )
            cand_id = f"{batch_id}_cand_{msg.index}_conflict"
            ev_id = f"{cand_id}_ev"
            y_frac = decimal_to_canonical_str(
                parse_exact_decimal(msg.start_line - 1) / parse_exact_decimal(total_lines), 4
            )
            h_frac = decimal_to_canonical_str(
                parse_exact_decimal(msg.end_line - msg.start_line + 1)
                / parse_exact_decimal(total_lines),
                4,
            )
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
                    media_type="text/plain",
                    page=1,
                    bbox_x="0.0000",
                    bbox_y=y_frac,
                    bbox_w="1.0000",
                    bbox_h=h_frac,
                    text=msg.body,
                    extractor=EXTRACTOR_NAME,
                    extractor_version=EXTRACTOR_VERSION,
                    confidence_bp=confidence_bp,
                    source_anchor=anchor,
                    record_digest=msg.message_digest,
                )
            )
            candidates.append(
                ExtractedCandidateProposal(
                    id=cand_id,
                    batch_id=batch_id,
                    target_kind=target_kind,
                    source_record_key=source_record_key,
                    payload_digest=payload_digest,
                    confidence_bp=confidence_bp,
                    source_span=source_span,
                    extracted_at=extracted_at,
                    observed_at=msg.observed_at,
                    effective_on=effective_on,
                    dispute_state="disputed",
                    status="quarantined",
                    proposal=proposal,
                )
            )
            findings.append(
                DataQualityFindingRecord(
                    id=f"dqf_{prev_cand_id}_feed_conflict_msg_{msg.index}",
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
                        f"Candidate {prev_cand_id} conflicts with message at {source_span} "
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
                        f"Conflicting message at {source_span} against prior candidate "
                        f"{prev_cand_id} on record key digest {key_digest}."
                    ),
                )
            )
            continue

        cand_id = f"{batch_id}_cand_{msg.index}"
        ev_id = f"{cand_id}_ev"
        seen_records[source_record_key] = (len(candidates), cand_id, ev_id, payload_digest)
        needs_quarantine = issue_code is not None or confidence_bp < min_confidence_bp
        dispute_state: DisputeState = "quarantined" if needs_quarantine else "clean"
        cand_status: CandidateStatus = "quarantined" if needs_quarantine else "staged"

        y_frac = decimal_to_canonical_str(
            parse_exact_decimal(msg.start_line - 1) / parse_exact_decimal(total_lines), 4
        )
        h_frac = decimal_to_canonical_str(
            parse_exact_decimal(msg.end_line - msg.start_line + 1)
            / parse_exact_decimal(total_lines),
            4,
        )
        evidence_records.append(
            FieldEvidenceRecord(
                id=ev_id,
                batch_id=batch_id,
                candidate_id=cand_id,
                subject_property="quantity_hours" if "quantity_hours" in proposal else "days",
                subject_type=f"hr.{target_kind}",
                subject_key=cand_id,
                document_uri=document_uri,
                document_digest=document_digest,
                media_type="text/plain",
                page=1,
                bbox_x="0.0000",
                bbox_y=y_frac,
                bbox_w="1.0000",
                bbox_h=h_frac,
                text=msg.body,
                extractor=EXTRACTOR_NAME,
                extractor_version=EXTRACTOR_VERSION,
                confidence_bp=confidence_bp,
                source_anchor=anchor,
                record_digest=msg.message_digest,
            )
        )
        candidates.append(
            ExtractedCandidateProposal(
                id=cand_id,
                batch_id=batch_id,
                target_kind=target_kind,
                source_record_key=source_record_key,
                payload_digest=payload_digest,
                confidence_bp=confidence_bp,
                source_span=source_span,
                extracted_at=extracted_at,
                observed_at=msg.observed_at,
                effective_on=effective_on,
                dispute_state=dispute_state,
                status=cand_status,
                proposal=proposal,
            )
        )

        if issue_code is not None:
            findings.append(
                DataQualityFindingRecord(
                    id=f"dqf_{cand_id}_{issue_code}",
                    candidate_id=cand_id,
                    evidence_id=ev_id,
                    code=issue_code,
                    finding_kind="schema_violation",
                    severity="error",
                    quarantine_disposition="quarantined",
                    field_path=issue_code,
                    law_pin=None,
                    conflicting_payload_digest=None,
                    message=f"Message at {source_span} requires review ({issue_code}).",
                )
            )
        elif confidence_bp < min_confidence_bp:
            findings.append(
                DataQualityFindingRecord(
                    id=f"dqf_{cand_id}_low_confidence",
                    candidate_id=cand_id,
                    evidence_id=ev_id,
                    code="needs_review",
                    finding_kind="low_confidence",
                    severity="error",
                    quarantine_disposition="quarantined",
                    field_path="confidence_bp",
                    law_pin=None,
                    conflicting_payload_digest=None,
                    message=(
                        f"Message at {source_span} confidence {confidence_bp} bp "
                        f"is below review threshold {min_confidence_bp} bp."
                    ),
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
            channel="whatsapp_export",
            document_digest=document_digest,
            candidate_count=len(candidates),
            finding_count=len(findings),
            deduplicated_count=deduplicated_count,
            finding_codes=[f.code for f in findings],
        )
    return result
