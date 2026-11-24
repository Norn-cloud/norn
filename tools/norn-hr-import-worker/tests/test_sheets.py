"""Tests for deterministic CSV/TSV HR sheet extraction and strict schema/decimal checks."""

from __future__ import annotations

from pathlib import Path

import pytest

from norn_hr_import_worker.cli import main as cli_main
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
    assert bundle.candidates[0].dispute_state == "disputed"
    assert bundle.candidates[0].status == "quarantined"
    assert bundle.candidates[1].dispute_state == "disputed"
    assert bundle.candidates[1].status == "quarantined"
    assert len(bundle.findings) == 2
    by_cand = {f.candidate_id: f for f in bundle.findings}
    assert set(by_cand) == {bundle.candidates[0].id, bundle.candidates[1].id}
    assert all(f.finding_kind == "feed_conflict" for f in bundle.findings)
    c0, c1 = bundle.candidates[0], bundle.candidates[1]
    assert by_cand[c0.id].conflicting_payload_digest == c1.payload_digest
    assert by_cand[c1.id].conflicting_payload_digest == c0.payload_digest


def test_sheet_validates_intervals_even_when_hours_present_and_sanitizes_findings() -> None:
    csv_att = (
        b"branch_code,employee_code,work_date,clock_in,clock_out,hours\n"
        b"BR-01,EMP-1,2026-11-25,17:00,09:00,8.00\n"
        b"BR-01,EMP-2,2026-11-25,09:00,17:00,10.00\n"
    )
    bundle_att = extract_sheet(
        raw_bytes=csv_att,
        document_uri="evidence://sheets/att-bad-interval.csv",
        batch_id="batch_att_int",
        profile="attendance",
        captured_at="2026-11-25T20:00:00Z",
        extracted_at="2026-11-25T21:00:00Z",
    )
    assert all(c.dispute_state == "quarantined" for c in bundle_att.candidates)
    codes_by_idx = [
        {f.code for f in bundle_att.findings if f.candidate_id == c.id}
        for c in bundle_att.candidates
    ]
    assert "invalid_time_interval" in codes_by_idx[0]
    assert "hours_interval_mismatch" in codes_by_idx[1]

    csv_emp = (
        b"employee_code,display_name,effective_from,currency,base_salary,max_weekly_hours,primary_skill\n"
        b"EMP-9,Canary Person,2026-01-01,USD,SECRET_SALARY_99999,40,cashier\n"
    )
    bundle_emp = extract_sheet(
        raw_bytes=csv_emp,
        document_uri="evidence://sheets/emp-pii.csv",
        batch_id="batch_emp_pii",
        profile="employee",
        captured_at="2026-11-25T20:00:00Z",
        extracted_at="2026-11-25T21:00:00Z",
    )
    assert bundle_emp.candidates[0].dispute_state == "quarantined"
    assert all("SECRET_SALARY_99999" not in f.message for f in bundle_emp.findings)


def test_sheet_unknown_header_error_redacts_raw_pii_in_library_and_cli(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    pii_canary = "PII_HEADERLESS_ROW1_SECRET_98765"
    bad_csv = (
        f"employee_code,leave_kind,starts_on,ends_on,days,{pii_canary}\n"
        "E1,annual,2026-12-01,2026-12-02,2,val\n"
    ).encode()
    with pytest.raises(SheetSchemaError) as exc_info:
        extract_sheet(
            raw_bytes=bad_csv,
            document_uri="evidence://sheets/headerless.csv",
            batch_id="b_hdr",
            profile="leave",
            captured_at="2026-11-22T10:00:00Z",
            extracted_at="2026-11-22T11:00:00Z",
        )
    err_msg = str(exc_info.value)
    assert pii_canary not in err_msg
    assert "count=1" in err_msg and "indices=[5]" in err_msg

    csv_file = tmp_path / "headerless.csv"
    csv_file.write_bytes(bad_csv)
    rc = cli_main(
        [
            "--channel",
            "sheet",
            "--profile",
            "leave",
            "--input",
            str(csv_file),
            "--batch-id",
            "b_hdr_cli",
            "--document-uri",
            "evidence://sheets/headerless.csv",
            "--captured-at",
            "2026-11-22T10:00:00Z",
            "--extracted-at",
            "2026-11-22T11:00:00Z",
        ]
    )
    assert rc == 2
    captured = capsys.readouterr()
    assert pii_canary not in captured.err
    assert pii_canary not in captured.out
    assert "indices=[5]" in captured.err
