"""Tests for WhatsApp Arabic & English shift/leave/attendance extraction."""

from __future__ import annotations

import logging
from pathlib import Path

import pytest

from norn_hr_import_worker.models import SafeWorkerLogger, sha256_bytes
from norn_hr_import_worker.whatsapp import extract_whatsapp_export

REPO_ROOT = Path(__file__).resolve().parents[3]
FIXTURE_WA = REPO_ROOT / "registry/packs/hr-import/fixtures/whatsapp-cairo-shifts.txt"


def test_whatsapp_parses_repo_fixture_with_source_evidence_and_no_pii_logs(
    caplog: pytest.LogCaptureFixture,
) -> None:
    raw = FIXTURE_WA.read_bytes()
    logger = logging.getLogger("test.whatsapp.pii")
    with caplog.at_level(logging.INFO, logger=logger.name):
        bundle = extract_whatsapp_export(
            raw_bytes=raw,
            document_uri="evidence://registry/packs/hr-import/fixtures/whatsapp-cairo-shifts.txt",
            batch_id="batch_wa_1",
            captured_at="2026-11-22T18:10:00Z",
            extracted_at="2026-11-23T08:00:00Z",
            logger=SafeWorkerLogger(logger),
        )

    assert bundle.batch.document_digest == sha256_bytes(raw)
    assert bundle.batch.media_type == "text/plain"
    assert len(bundle.candidates) >= 2
    assert len(bundle.evidence) == len(bundle.candidates)

    salma = next(
        c
        for c in bundle.candidates
        if c.proposal.get("employee_label") == "Salma Tarek" and c.source_span == "line:5-5"
    )
    assert salma.target_kind == "shift_commitment"
    assert salma.effective_on == "2026-11-25"
    assert salma.proposal["quantity_hours"] == "8.00"
    assert salma.proposal["role_code"] == "cashier"
    assert salma.source_span == "line:5-5"

    ev = next(e for e in bundle.evidence if e.candidate_id == salma.id)
    assert ev.document_digest == bundle.batch.document_digest
    assert ev.source_anchor.startswith("line:5-5#")
    assert ev.page == 1

    # Zero PII in logs or findings: employee names and message text must never appear.
    log_text = "\n".join(r.getMessage() for r in caplog.records)
    finding_text = "\n".join(f.message for f in bundle.findings)
    for leaked in ("Salma Tarek", "Karim Adel", "Nour Hassan"):
        assert leaked not in log_text
        assert leaked not in finding_text
    assert bundle.batch.document_digest in log_text
    # Line 4 lacks an explicit role and must never fabricate "general".
    assert all(c.proposal.get("role_code") != "general" for c in bundle.candidates)
    # Line 4 and line 5 share Salma's shift key with differing payloads -> BOTH disputed.
    salma_cands = [
        c for c in bundle.candidates if c.proposal.get("employee_label") == "Salma Tarek"
    ]
    assert len(salma_cands) == 2
    assert all(c.dispute_state == "disputed" and c.status == "quarantined" for c in salma_cands)


def test_whatsapp_handles_arabic_indic_digits_leave_and_attendance() -> None:
    chat = (
        "٢٢/١١/٢٠٢٦, ٠٩:١٥ ص - Sara Ali: طلب إجازة سنوية 3 أيام تبدأ 2026-12-01\n"
        "22/11/2026, 05:30 PM - Omar Rami: attendance 2026-11-22 09:00-17:30\n"
        "[22/11/2026, 19:00] Mona Adel: طلب إجازة مرضية بدون تحديد عدد الأيام\n"
    ).encode()

    bundle = extract_whatsapp_export(
        raw_bytes=chat,
        document_uri="evidence://chat/mixed-ar-en.txt",
        batch_id="batch_ar_en",
        captured_at="2026-11-22T20:00:00Z",
        extracted_at="2026-11-23T08:00:00Z",
    )

    assert len(bundle.candidates) == 3
    leave_ok = bundle.candidates[0]
    assert leave_ok.target_kind == "leave_request"
    assert leave_ok.proposal["leave_kind"] == "annual"
    assert leave_ok.proposal["days"] == "3.00"
    assert leave_ok.effective_on == "2026-12-01"
    assert leave_ok.dispute_state == "clean"

    att_ok = bundle.candidates[1]
    assert att_ok.target_kind == "attendance_event"
    assert att_ok.proposal["quantity_hours"] == "8.50"
    assert att_ok.dispute_state == "clean"

    leave_missing_days = bundle.candidates[2]
    assert leave_missing_days.dispute_state == "quarantined"
    assert leave_missing_days.status == "quarantined"
    assert any(
        f.candidate_id == leave_missing_days.id and f.code == "missing_leave_days"
        for f in bundle.findings
    )


def test_whatsapp_dedups_identical_and_flags_feed_conflict_on_divergence() -> None:
    chat = (
        b"[22/11/2026, 18:00] Manager: Karim Adel morning cashier 2026-11-24 09:00-17:00\n"
        b"[22/11/2026, 18:01] Manager: Karim Adel morning cashier 2026-11-24 09:00-17:00\n"
        b"[22/11/2026, 18:02] Manager: Karim Adel evening cashier 2026-11-24 14:00-22:00\n"
    )

    bundle1 = extract_whatsapp_export(
        raw_bytes=chat,
        document_uri="evidence://chat/conflict.txt",
        batch_id="batch_fc",
        captured_at="2026-11-22T18:05:00Z",
        extracted_at="2026-11-23T08:00:00Z",
    )
    bundle2 = extract_whatsapp_export(
        raw_bytes=chat,
        document_uri="evidence://chat/conflict.txt",
        batch_id="batch_fc",
        captured_at="2026-11-22T18:05:00Z",
        extracted_at="2026-11-23T08:00:00Z",
    )

    assert bundle1.deduplicated_count == 1
    assert len(bundle1.candidates) == 2
    assert bundle1.candidates[0].dispute_state == "disputed"
    assert bundle1.candidates[0].status == "quarantined"
    conflict_cand = bundle1.candidates[1]
    assert conflict_cand.dispute_state == "disputed"
    assert conflict_cand.status == "quarantined"
    assert len(bundle1.findings) == 2
    by_cand = {f.candidate_id: f for f in bundle1.findings}
    assert set(by_cand) == {bundle1.candidates[0].id, conflict_cand.id}
    assert all(
        f.finding_kind == "feed_conflict" and f.quarantine_disposition == "disputed"
        for f in bundle1.findings
    )
    first_cand = bundle1.candidates[0]
    assert by_cand[conflict_cand.id].conflicting_payload_digest == first_cand.payload_digest
    assert by_cand[first_cand.id].conflicting_payload_digest == conflict_cand.payload_digest
    assert all("Karim Adel" not in f.message for f in bundle1.findings)
    assert bundle1.to_canonical_json() == bundle2.to_canonical_json()
    assert bundle1.extraction_digest == bundle2.extraction_digest


def test_whatsapp_refuses_non_shift_ranges_and_quarantines_missing_employee_or_role() -> None:
    chat = (
        b"[22/11/2026, 12:00] Manager: Lunch break 13:00-14:00 today\n"
        b"[22/11/2026, 12:05] Manager: Evening shift 2026-11-26 14:00-22:00 cashier\n"
        b"[22/11/2026, 12:10] Sara Ali: I will take the morning shift 2026-11-27 09:00-17:00\n"
    )
    bundle = extract_whatsapp_export(
        raw_bytes=chat,
        document_uri="evidence://chat/strict.txt",
        batch_id="batch_strict",
        captured_at="2026-11-22T12:15:00Z",
        extracted_at="2026-11-22T12:20:00Z",
    )
    # First line has no shift/role keyword -> ignored (not misclassified as a shift).
    assert len(bundle.candidates) == 2
    # Second line has no named employee and is not self-assignment -> quarantined missing_employee.
    missing_emp = bundle.candidates[0]
    assert missing_emp.dispute_state == "quarantined"
    assert missing_emp.proposal["employee_label"] is None
    assert any(
        f.candidate_id == missing_emp.id and f.code == "missing_employee"
        for f in bundle.findings
    )
    # Third line is self-assignment without role -> quarantined missing_role (never "general").
    missing_role = bundle.candidates[1]
    assert missing_role.dispute_state == "quarantined"
    assert missing_role.proposal["employee_label"] == "Sara Ali"
    assert missing_role.proposal["role_code"] is None
    assert any(
        f.candidate_id == missing_role.id and f.code == "missing_role"
        for f in bundle.findings
    )
