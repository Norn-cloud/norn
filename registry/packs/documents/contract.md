# Scanned documents: source contract v1

`docs.scans.source` describes what an extractor reports for one scanned
document: the values it read and, for each value, where it read them. All
documents in this repository are deterministic synthetic scans rendered by
`demo/documents/generate.py`; no real document, vendor or person appears.

## Records

One `Scanned Document` record per document, with two child tables:

- `lines`: the printed lines (`line_no`, `description`, `amount`).
- `evidence`: one row per extracted value: the source field it supports
  (`total`, `lines.<n>.amount`, ...), the document URI and the SHA-256 of its
  exact bytes, the page, the box as fractions of the page (top-left origin),
  the text span the extractor read, the extractor and its version, and the
  extractor's own confidence in basis points (null when it reports none).

Amounts are exact decimals in USD. `ledger_ref` names the ledger entry a
document is linked to; it is retained in the source document and checked by a
later unit (LAW B, #343).

## What Norn derives and checks

- `lines_total` is the mapping's sum of the printed line amounts; it is null
  when the document prints no lines.
- LAW A (`docs.ScannedDocument.total_matches_lines`, error severity): when a
  document prints lines, its printed total equals their sum. An imported
  document that breaks it is kept as evidence, gets a finding pinned to the
  law and is quarantined from authoritative reads (OBS-EVIDENCE-1).
- The correction action (`docs.scanned_document.correct`) patches the total of
  an existing document, requires a reason and an evidence object, records the
  verified actor, and runs the same law: an inconsistent correction is refused.

## Extractors

The baseline extractor is Tesseract OCR with a layout parser for the synthetic
templates (`demo/documents/extract.py`); its boxes come from the OCR's word
boxes. The seeded "legacy OCR" import is a synthetic export produced by the
generator with documented errors (a dropped decimal point); it stands for an
older system's output and is labelled `legacy-ocr` in every record.
