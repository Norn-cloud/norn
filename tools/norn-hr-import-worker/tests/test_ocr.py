"""Tests for pluggable clean-room OCR/VLM adapter boundary and fail-closed review quarantine."""

from __future__ import annotations

import json
from pathlib import Path

from norn_hr_import_worker.cli import main as cli_main
from norn_hr_import_worker.ocr import (
    DeterministicFixtureOcrAdapter,
    OcrFieldObservation,
    OcrRawResponse,
    OcrRecordObservation,
    extract_from_ocr,
)


class _CustomStubAdapter:
    """Verifies any custom adapter satisfying `OcrAdapter` protocol plugs in cleanly."""

    @property
    def adapter_name(self) -> str:
        return "custom-stub-ocr"

    @property
    def adapter_version(self) -> str:
        return "1.2.3"

    def extract(self, document_bytes: bytes, document_uri: str, media_type: str) -> OcrRawResponse:
        del document_bytes, document_uri, media_type
        return OcrRawResponse(
            records=(
                OcrRecordObservation(
                    record_key="photo/shift/emp-1/2026-11-26",
                    target_kind="shift_commitment",
                    effective_on="2026-11-26",
                    fields=(
                        OcrFieldObservation(
                            field_name="employee_code",
                            value="EMP-1",
                            text_span="EMP-1",
                            page=1,
                            bbox_x="0.1000",
                            bbox_y="0.2000",
                            bbox_w="0.1500",
                            bbox_h="0.0400",
                            confidence_bp=9700,
                        ),
                        OcrFieldObservation(
                            field_name="quantity_hours",
                            value="8",
                            text_span="8h",
                            page=1,
                            bbox_x="0.3000",
                            bbox_y="0.2000",
                            bbox_w="0.1000",
                            bbox_h="0.0400",
                            confidence_bp=9600,
                        ),
                    ),
                ),
            )
        )


def test_custom_ocr_adapter_accepts_anchored_high_confidence_record() -> None:
    bundle = extract_from_ocr(
        raw_bytes=b"synthetic-image-bytes",
        document_uri="evidence://photos/roster-w48.png",
        media_type="image/png",
        batch_id="batch_photo_1",
        captured_at="2026-11-22T12:00:00Z",
        extracted_at="2026-11-22T12:05:00Z",
        adapter=_CustomStubAdapter(),
    )

    assert bundle.batch.extractor == "custom-stub-ocr"
    assert bundle.batch.extractor_version == "1.2.3"
    assert len(bundle.candidates) == 1
    assert bundle.candidates[0].dispute_state == "clean"
    assert bundle.candidates[0].status == "staged"
    assert bundle.candidates[0].proposal["quantity_hours"] == "8.00"
    assert len(bundle.evidence) == 2
    assert len(bundle.findings) == 0


def test_ocr_quarantines_unanchored_low_confidence_and_malformed_fields_into_needs_review() -> None:
    fixture = {
        "records": [
            {
                "record_key": "photo/row/1",
                "target_kind": "shift_commitment",
                "effective_on": "2026-11-27",
                "fields": [
                    {
                        "field_name": "employee_code",
                        "value": "EMP-9",
                        "text_span": "EMP-9",
                        "page": 1,
                        "bbox_x": "0.10",
                        "bbox_y": "0.10",
                        "bbox_w": "0.20",
                        "bbox_h": "0.05",
                        "confidence_bp": 6100,
                    },
                    {
                        "field_name": "role_code",
                        "value": "cashier",
                        "text_span": "",
                        "page": 0,
                        "bbox_x": "1.50",
                        "bbox_y": "0.10",
                        "bbox_w": "0.00",
                        "bbox_h": "0.05",
                        "confidence_bp": 9500,
                    },
                    {
                        "field_name": "quantity_hours",
                        "value": "8e0",
                        "text_span": "8e0",
                        "page": 1,
                        "bbox_x": "0.40",
                        "bbox_y": "0.10",
                        "bbox_w": "0.15",
                        "bbox_h": "0.05",
                        "confidence_bp": 9200,
                    },
                ],
            }
        ]
    }
    raw = json.dumps(fixture).encode("utf-8")
    adapter = DeterministicFixtureOcrAdapter()

    bundle_a = extract_from_ocr(
        raw_bytes=raw,
        document_uri="evidence://photos/bad-roster.json",
        media_type="application/json",
        batch_id="batch_ocr_bad",
        captured_at="2026-11-22T12:00:00Z",
        extracted_at="2026-11-22T12:05:00Z",
        adapter=adapter,
    )
    bundle_b = extract_from_ocr(
        raw_bytes=raw,
        document_uri="evidence://photos/bad-roster.json",
        media_type="application/json",
        batch_id="batch_ocr_bad",
        captured_at="2026-11-22T12:00:00Z",
        extracted_at="2026-11-22T12:05:00Z",
        adapter=adapter,
    )

    assert bundle_a.to_canonical_json() == bundle_b.to_canonical_json()
    assert len(bundle_a.candidates) == 1
    cand = bundle_a.candidates[0]
    assert cand.dispute_state == "quarantined"
    assert cand.status == "quarantined"
    # Unanchored field and malformed decimal must never be silently invented into proposal.
    assert "role_code" not in cand.proposal
    assert "quantity_hours" not in cand.proposal
    kinds = {f.finding_kind for f in bundle_a.findings}
    assert kinds == {"low_confidence", "missing_anchor", "schema_violation"}
    assert all(f.code == "needs_review" for f in bundle_a.findings)


def test_cli_outputs_canonical_json_file(tmp_path: Path) -> None:
    inp = tmp_path / "ocr.json"
    out = tmp_path / "out.json"
    inp.write_text(
        json.dumps(
            {
                "records": [
                    {
                        "record_key": "photo/row/ok",
                        "target_kind": "leave_request",
                        "effective_on": "2026-12-01",
                        "fields": [
                            {
                                "field_name": "days",
                                "value": "2",
                                "text_span": "2 days",
                                "page": 1,
                                "bbox_x": "0.10",
                                "bbox_y": "0.10",
                                "bbox_w": "0.20",
                                "bbox_h": "0.05",
                                "confidence_bp": 9400,
                            }
                        ],
                    }
                ]
            }
        ),
        encoding="utf-8",
    )
    rc = cli_main(
        [
            "--channel",
            "ocr",
            "--input",
            str(inp),
            "--batch-id",
            "batch_cli",
            "--document-uri",
            "evidence://cli/ocr.json",
            "--captured-at",
            "2026-11-22T12:00:00Z",
            "--extracted-at",
            "2026-11-22T12:05:00Z",
            "--out",
            str(out),
        ]
    )
    assert rc == 0
    parsed = json.loads(out.read_text(encoding="utf-8"))
    assert parsed["extraction_digest"].startswith("sha256:")
    assert parsed["candidates"][0]["proposal"]["days"] == "2.00"


def test_ocr_rejects_unknown_duplicate_missing_effective_on_and_rebinds_conflict_evidence() -> None:
    fixture = {
        "records": [
            {
                "record_key": "photo/row/strict",
                "target_kind": "shift_commitment",
                "effective_on": None,
                "fields": [
                    {
                        "field_name": "injected_unknown",
                        "value": "SECRET_RAW_VALUE",
                        "text_span": "SECRET_RAW_VALUE",
                        "page": 1,
                        "bbox_x": "0.10",
                        "bbox_y": "0.10",
                        "bbox_w": "0.20",
                        "bbox_h": "0.05",
                        "confidence_bp": 9500,
                    },
                    {
                        "field_name": "role_code",
                        "value": "cashier",
                        "text_span": "cashier",
                        "page": 1,
                        "bbox_x": "0.10",
                        "bbox_y": "0.20",
                        "bbox_w": "0.20",
                        "bbox_h": "0.05",
                        "confidence_bp": 9600,
                    },
                    {
                        "field_name": "role_code",
                        "value": "supervisor",
                        "text_span": "supervisor",
                        "page": 1,
                        "bbox_x": "0.40",
                        "bbox_y": "0.20",
                        "bbox_w": "0.20",
                        "bbox_h": "0.05",
                        "confidence_bp": 9600,
                    },
                ],
            },
            {
                "record_key": "photo/row/dup-key",
                "target_kind": "shift_commitment",
                "effective_on": "2026-11-28",
                "fields": [
                    {
                        "field_name": "role_code",
                        "value": "cashier",
                        "text_span": "cashier",
                        "page": 1,
                        "bbox_x": "0.10",
                        "bbox_y": "0.30",
                        "bbox_w": "0.20",
                        "bbox_h": "0.05",
                        "confidence_bp": 9600,
                    }
                ],
            },
            {
                "record_key": "photo/row/dup-key",
                "target_kind": "shift_commitment",
                "effective_on": "2026-11-28",
                "fields": [
                    {
                        "field_name": "role_code",
                        "value": "barista",
                        "text_span": "barista",
                        "page": 1,
                        "bbox_x": "0.10",
                        "bbox_y": "0.40",
                        "bbox_w": "0.20",
                        "bbox_h": "0.05",
                        "confidence_bp": 9600,
                    }
                ],
            },
        ]
    }
    bundle = extract_from_ocr(
        raw_bytes=json.dumps(fixture).encode("utf-8"),
        document_uri="evidence://photos/strict.json",
        media_type="application/json",
        batch_id="batch_ocr_strict",
        captured_at="2026-11-22T12:00:00Z",
        extracted_at="2026-11-22T12:05:00Z",
        adapter=DeterministicFixtureOcrAdapter(),
    )

    cand0 = bundle.candidates[0]
    assert cand0.dispute_state == "disputed"
    assert cand0.status == "quarantined"
    assert cand0.effective_on == ""
    assert "injected_unknown" not in cand0.proposal
    # First value retained; duplicate field did NOT silently overwrite it.
    assert cand0.proposal["role_code"] == "cashier"
    cand0_findings = [f for f in bundle.findings if f.candidate_id == cand0.id]
    assert {"missing_required_field", "needs_review", "feed_conflict"} <= {
        f.code for f in cand0_findings
    }
    assert all("SECRET_RAW_VALUE" not in f.message for f in bundle.findings)

    # Duplicate-key conflict marks BOTH candidates disputed and every evidence links validly.
    cand1, cand2 = bundle.candidates[1], bundle.candidates[2]
    assert cand1.dispute_state == "disputed" and cand1.status == "quarantined"
    assert cand2.dispute_state == "disputed" and cand2.status == "quarantined"
    emitted_ids = {c.id for c in bundle.candidates}
    assert all(ev.candidate_id in emitted_ids for ev in bundle.evidence)
    assert any(ev.candidate_id == cand2.id for ev in bundle.evidence)
    conflict_findings = [
        f
        for f in bundle.findings
        if f.finding_kind == "feed_conflict" and f.candidate_id in {cand1.id, cand2.id}
    ]
    assert {f.candidate_id for f in conflict_findings} == {cand1.id, cand2.id}


def test_cli_refuses_oversized_symlink_and_directory_without_traceback(
    tmp_path: Path, capsys: object
) -> None:
    del capsys
    big = tmp_path / "big.csv"
    big.write_bytes(b"a" * 2048)
    rc_big = cli_main(
        [
            "--channel",
            "sheet",
            "--input",
            str(big),
            "--batch-id",
            "b1",
            "--document-uri",
            "evidence://big",
            "--captured-at",
            "2026-11-22T12:00:00Z",
            "--extracted-at",
            "2026-11-22T12:05:00Z",
            "--max-input-bytes",
            "1024",
        ]
    )
    assert rc_big == 2

    target = tmp_path / "real.txt"
    target.write_text("hi", encoding="utf-8")
    link = tmp_path / "link.txt"
    link.symlink_to(target)
    rc_link = cli_main(
        [
            "--channel",
            "whatsapp",
            "--input",
            str(link),
            "--batch-id",
            "b2",
            "--document-uri",
            "evidence://link",
            "--captured-at",
            "2026-11-22T12:00:00Z",
            "--extracted-at",
            "2026-11-22T12:05:00Z",
        ]
    )
    assert rc_link == 2

    rc_dir = cli_main(
        [
            "--channel",
            "ocr",
            "--input",
            str(tmp_path),
            "--batch-id",
            "b3",
            "--document-uri",
            "evidence://dir",
            "--captured-at",
            "2026-11-22T12:00:00Z",
            "--extracted-at",
            "2026-11-22T12:05:00Z",
        ]
    )
    assert rc_dir == 2

    # Hard ceiling: --max-input-bytes > MAX_INPUT_BYTES (4 MiB) or <= 0 is refused.
    rc_over_cap = cli_main(
        [
            "--channel",
            "whatsapp",
            "--input",
            str(target),
            "--batch-id",
            "b4",
            "--document-uri",
            "evidence://cap",
            "--captured-at",
            "2026-11-22T12:00:00Z",
            "--extracted-at",
            "2026-11-22T12:05:00Z",
            "--max-input-bytes",
            str((4 * 1024 * 1024) + 1),
        ]
    )
    assert rc_over_cap == 2

    rc_zero_cap = cli_main(
        [
            "--channel",
            "whatsapp",
            "--input",
            str(target),
            "--batch-id",
            "b5",
            "--document-uri",
            "evidence://zero",
            "--captured-at",
            "2026-11-22T12:00:00Z",
            "--extracted-at",
            "2026-11-22T12:05:00Z",
            "--max-input-bytes",
            "0",
        ]
    )
    assert rc_zero_cap == 2


def test_ocr_never_dedups_second_record_with_changed_missing_effective_on_or_repeated_field() -> (
    None
):
    field_cashier = {
        "field_name": "role_code",
        "value": "cashier",
        "text_span": "cashier",
        "page": 1,
        "bbox_x": "0.10",
        "bbox_y": "0.10",
        "bbox_w": "0.20",
        "bbox_h": "0.05",
        "confidence_bp": 9600,
    }
    cases: list[tuple[str, dict[str, object], str]] = [
        (
            "changed_effective_on",
            {
                "record_key": "k/changed",
                "target_kind": "shift_commitment",
                "effective_on": "2026-11-29",
                "fields": [field_cashier],
            },
            "feed_conflict",
        ),
        (
            "missing_effective_on",
            {
                "record_key": "k/missing",
                "target_kind": "shift_commitment",
                "effective_on": None,
                "fields": [field_cashier],
            },
            "missing_required_field",
        ),
        (
            "repeated_field",
            {
                "record_key": "k/repeat",
                "target_kind": "shift_commitment",
                "effective_on": "2026-11-28",
                "fields": [field_cashier, field_cashier],
            },
            "feed_conflict",
        ),
    ]
    for label, second_rec, expected_code in cases:
        first_rec = {
            "record_key": second_rec["record_key"],
            "target_kind": "shift_commitment",
            "effective_on": "2026-11-28",
            "fields": [field_cashier],
        }
        raw = json.dumps({"records": [first_rec, second_rec]}).encode("utf-8")
        bundle = extract_from_ocr(
            raw_bytes=raw,
            document_uri=f"evidence://ocr/{label}.json",
            media_type="application/json",
            batch_id=f"batch_{label}",
            captured_at="2026-11-22T12:00:00Z",
            extracted_at="2026-11-22T12:05:00Z",
            adapter=DeterministicFixtureOcrAdapter(),
        )
        assert bundle.deduplicated_count == 0, label
        assert len(bundle.candidates) == 2, label
        c0, c1 = bundle.candidates
        assert c0.dispute_state == "disputed" and c0.status == "quarantined", label
        assert c1.dispute_state == "disputed" and c1.status == "quarantined", label
        codes = {f.code for f in bundle.findings}
        assert "feed_conflict" in codes and expected_code in codes, label
        assert {f.candidate_id for f in bundle.findings if f.finding_kind == "feed_conflict"} == {
            c0.id,
            c1.id,
        }, label
