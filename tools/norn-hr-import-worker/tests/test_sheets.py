"""Tests for deterministic CSV/TSV HR sheet extraction and strict schema/decimal checks."""

from __future__ import annotations

from pathlib import Path

import pytest

from norn_hr_import_worker.models import sha256_bytes
from norn_hr_import_worker.sheets import SheetSchemaError, extract_sheet

REPO_ROOT = Path(__file__).resolve().parents[3]
FIXTURE_CSV = REPO_ROOT / "registry/packs/hr-import/fixtures/attendance-sheet-sample.csv"


def test_csv_attendance_repo_fixture_quarantines_missing_clock_out() -> None:
    raw = FIXTURE_CSV.read_bytes()
    bundle = extract_sheet(
        raw_bytes=raw,
        document_uri="evidence://registry/packs/hr-import/fixtures/attendance-sheet-sample.csv",
        batch_id="batch_csv_1",
        profile="attendance",
        captured_at="2026-11-22T20:30:00Z",
        extracted_at="2026-11-23T08:05:00Z",
    )

    assert bundle.batch.document_digest == sha256_bytes(raw)
    assert len(bundle.candidates) == 4
    clean = [c for c in bundle.candidates if c.dispute_state == "clean"]
    quarantined = [c for c in bundle.candidates if c.dispute_state == "quarantined"]
    assert len(clean) == 3
    assert len(quarantined) == 1
    assert quarantined[0].proposal["employee_code"] == "EMP-SYN-003"
    assert len(bundle.findings) == 1
    assert bundle.findings[0].code == "missing_clock_out"
    assert bundle.findings[0].law_pin == "core.TimeInterval.proper_interval"


def test_tsv_employee_and_leave_sheets_exact_decimals_and_refusals() -> None:
    tsv = (
        b"employee_code\tdisplay_name\tjurisdiction_code\teffective_from\tcurrency\t"
        b"base_salary\tmax_weekly_hours\tprimary_skill\n"
        b"EMP-01\tAda Lovelace\tGB\t2026-01-01\tGBP\t5000.25\t40\tengineer\n"
        b"EMP-02\tBad Number\tGB\t2026-01-01\tGBP\t5,000.00\t40e0\tengineer\n"
    )

    bundle = extract_sheet(
        raw_bytes=tsv,
        document_uri="evidence://sheets/employees.tsv",
        batch_id="batch_emp_tsv",
        profile="employee",
        captured_at="2026-11-22T10:00:00Z",
        extracted_at="2026-11-22T11:00:00Z",
    )

    assert bundle.batch.media_type == "text/tab-separated-values"
    assert bundle.candidates[0].dispute_state == "clean"
    assert bundle.candidates[0].proposal["base_salary"] == "5000.25"
    assert bundle.candidates[0].proposal["max_weekly_hours"] == "40.00"

    # Second row has comma-formatted decimal and scientific notation -> both refused!
    assert bundle.candidates[1].dispute_state == "quarantined"
    row2_findings = [f for f in bundle.findings if f.candidate_id == bundle.candidates[1].id]
    assert {f.field_path for f in row2_findings} == {"base_salary", "max_weekly_hours"}
    assert all(f.code == "invalid_decimal" for f in row2_findings)


def test_sheet_rejects_unknown_or_missing_columns_fail_closed() -> None:
    bad_unknown = (
        b"employee_code,leave_kind,starts_on,ends_on,days,injected_col\n"
        b"E1,annual,2026-12-01,2026-12-02,2,x\n"
    )
    with pytest.raises(SheetSchemaError, match="Unknown columns"):
        extract_sheet(
            raw_bytes=bad_unknown,
            document_uri="evidence://sheets/bad.csv",
            batch_id="b_bad1",
            profile="leave",
            captured_at="2026-11-22T10:00:00Z",
            extracted_at="2026-11-22T11:00:00Z",
        )

    bad_missing = b"employee_code,leave_kind,starts_on\nE1,annual,2026-12-01\n"
    with pytest.raises(SheetSchemaError, match="Missing required"):
        extract_sheet(
            raw_bytes=bad_missing,
            document_uri="evidence://sheets/missing.csv",
            batch_id="b_bad2",
            profile="leave",
            captured_at="2026-11-22T10:00:00Z",
            extracted_at="2026-11-22T11:00:00Z",
        )


def test_sheet_duplicate_key_dedup_vs_feed_conflict() -> None:
    csv_bytes = (
        b"branch_code,employee_code,role_code,shift_date,start_time,end_time\n"
        b"BR-01,EMP-1,cashier,2026-11-25,09:00,17:00\n"
        b"BR-01,EMP-1,cashier,2026-11-25,09:00,17:00\n"
        b"BR-01,EMP-1,supervisor,2026-11-25,09:00,17:00\n"
    )

    bundle = extract_sheet(
        raw_bytes=csv_bytes,
        document_uri="evidence://sheets/roster.csv",
        batch_id="batch_ros",
        profile="roster",
        captured_at="2026-11-22T10:00:00Z",
        extracted_at="2026-11-22T11:00:00Z",
    )

    assert bundle.deduplicated_count == 1
    assert len(bundle.candidates) == 2
    assert bundle.candidates[0].dispute_state == "clean"
    assert bundle.candidates[1].dispute_state == "disputed"
    assert len(bundle.findings) == 1
    assert bundle.findings[0].finding_kind == "feed_conflict"
    assert bundle.findings[0].conflicting_payload_digest == bundle.candidates[0].payload_digest
