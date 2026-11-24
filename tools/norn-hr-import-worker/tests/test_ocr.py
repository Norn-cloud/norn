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
