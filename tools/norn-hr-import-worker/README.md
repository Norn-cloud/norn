# `norn-hr-import-worker`

Clean-room deterministic Python extraction worker for `registry/packs/hr-import/` aligned with Core 07 (`norn.SourceDocument`, `norn.SourceEvidence`, `OBS-EVIDENCE-1`, `FEED-CONFLICT`, `MAP-DECIMAL-TEXT-1`).

## Capabilities

1. **WhatsApp shift / leave / attendance chat exports** (`whatsapp.py`): parses Arabic and English timestamps (including Arabic-Indic numerals `٠-٩` and `ص`/`م`), multi-line messages, shift assignments/swaps, leave requests, and attendance check-ins into closed-allowlist JSON proposals anchored with exact line-range and message-hash `SourceEvidence`.
2. **CSV / TSV HR sheets** (`sheets.py`): strict schema validation for `attendance`, `roster`, `leave`, and `employee` sheets, replay-idempotent dedup, `FEED-CONFLICT` detection on conflicting duplicate keys, and exact `Decimal` / `int` parsing with **zero IEEE-754 floats**.
3. **Pluggable OCR / VLM adapter boundary** (`ocr.py`): clean-room `OcrAdapter` protocol + `DeterministicFixtureOcrAdapter` + fail-closed schema/anchor/confidence validator that quarantines unanchored or low-confidence fields into `needs_review` findings rather than inventing data.

## Safety & Determinism Guarantees

- **Zero IEEE-754 floats** in canonical output: `canonical_json_bytes` refuses any `float` value.
- **Canonical SHA-256 digests** (`sort_keys=True`, compact UTF-8 JSON) for every source document, extracted candidate, and field evidence record.
- **Zero PII leakage in logs**: `SafeWorkerLogger` emits only batch IDs, digests, line/row indices, counts, and finding codes — never raw text, names, phones, national IDs, or salaries.

## Running Checks

```bash
cd tools/norn-hr-import-worker
OPENBLAS_NUM_THREADS=1 uv run --no-project --with pytest pytest
mypy --strict src tests
ruff check src tests
```
