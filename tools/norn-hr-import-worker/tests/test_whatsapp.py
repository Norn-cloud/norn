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

    # Zero PII in logs: employee names and message text must never appear.
    log_text = "\n".join(r.getMessage() for r in caplog.records)
    assert "Salma Tarek" not in log_text
    assert "Karim Adel" not in log_text
    assert "Nour Hassan" not in log_text
    assert bundle.batch.document_digest in log_text


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
    conflict_cand = bundle1.candidates[1]
    assert conflict_cand.dispute_state == "disputed"
    assert conflict_cand.status == "quarantined"
    assert len(bundle1.findings) == 1
    finding = bundle1.findings[0]
    assert finding.finding_kind == "feed_conflict"
    assert finding.quarantine_disposition == "disputed"
    assert finding.conflicting_payload_digest == bundle1.candidates[0].payload_digest
    assert bundle1.to_canonical_json() == bundle2.to_canonical_json()
    assert bundle1.extraction_digest == bundle2.extraction_digest
