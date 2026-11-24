"""Tests for Core 07 exact numeric parsing, canonical JSON determinism, and PII-safe logging."""

from __future__ import annotations

import logging
from decimal import Decimal

import pytest

from norn_hr_import_worker.models import (
    ExactValueError,
    JsonValue,
    SafeWorkerLogger,
    canonical_json_bytes,
    canonical_json_text,
    decimal_to_canonical_str,
    duration_hours_exact,
    parse_exact_decimal,
    parse_exact_int,
    sha256_prefixed,
    validate_iso_date,
)


def test_parse_exact_decimal_accepts_strict_values() -> None:
    assert parse_exact_decimal("0") == Decimal("0")
    assert parse_exact_decimal("1234.50") == Decimal("1234.50")
    assert parse_exact_decimal("-42.125") == Decimal("-42.125")
    assert parse_exact_decimal(8) == Decimal("8")
    assert decimal_to_canonical_str(Decimal("8.5000"), 2) == "8.50"
    assert decimal_to_canonical_str(Decimal("0.000")) == "0"


@pytest.mark.parametrize(
    "bad",
    ["1,234", "1e3", "+5", " 5", "5 ", "01", ".5", "5.", "NaN", "Infinity", ""],
)
def test_parse_exact_decimal_refuses_non_canonical_text(bad: str) -> None:
    with pytest.raises(ExactValueError):
        parse_exact_decimal(bad)


def test_parse_exact_decimal_and_canonical_json_forbid_floats() -> None:
    with pytest.raises(ExactValueError):
        parse_exact_decimal(1.5)  # type: ignore[arg-type]
    with pytest.raises(ExactValueError):
        parse_exact_int(1.0)  # type: ignore[arg-type]
    with pytest.raises(ExactValueError):
        canonical_json_bytes({"hours": 8.0})  # type: ignore[dict-item]


def test_canonical_json_and_sha256_are_order_independent() -> None:
    a: dict[str, JsonValue] = {"b": "2", "a": "1", "nested": {"z": 0, "y": "x"}}
    b: dict[str, JsonValue] = {"nested": {"y": "x", "z": 0}, "a": "1", "b": "2"}
    assert canonical_json_text(a) == canonical_json_text(b)
    assert sha256_prefixed(a) == sha256_prefixed(b)
    assert sha256_prefixed(a).startswith("sha256:")
    assert len(sha256_prefixed(a)) == 71


def test_iso_date_and_duration_hours_exact() -> None:
    assert validate_iso_date("2024-02-29") == "2024-02-29"
    with pytest.raises(ExactValueError):
        validate_iso_date("2026-02-29")
    assert duration_hours_exact("09:00", "17:30") == Decimal("8.50")
    with pytest.raises(ExactValueError):
        duration_hours_exact("17:00", "09:00")


def test_safe_worker_logger_emits_only_metadata_and_no_pii(
    caplog: pytest.LogCaptureFixture,
) -> None:
    logger = logging.getLogger("test.norn_hr_import_worker.safe")
    safe = SafeWorkerLogger(logger)
    with caplog.at_level(logging.INFO, logger=logger.name):
        safe.record_extraction_summary(
            batch_id="batch_1",
            channel="whatsapp_export",
            document_digest="sha256:abc",
            candidate_count=2,
            finding_count=1,
            deduplicated_count=0,
            finding_codes=["missing_clock_out", "needs_review"],
        )
    assert len(caplog.records) == 1
    msg = caplog.records[0].getMessage()
    assert '"batch_id":"batch_1"' in msg
    assert '"document_digest":"sha256:abc"' in msg
    assert '"finding_codes":["missing_clock_out","needs_review"]' in msg
