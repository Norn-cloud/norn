"""CLI entrypoint for `norn-hr-import-worker`."""

from __future__ import annotations

import argparse
from collections.abc import Sequence
from pathlib import Path

from norn_hr_import_worker.models import SafeWorkerLogger
from norn_hr_import_worker.ocr import DeterministicFixtureOcrAdapter, extract_from_ocr
from norn_hr_import_worker.sheets import SheetProfile, extract_sheet
from norn_hr_import_worker.whatsapp import extract_whatsapp_export


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="norn-hr-import-worker",
        description="Deterministic HR historical import extractor emitting Core 07 canonical JSON.",
    )
    parser.add_argument("--channel", choices=("whatsapp", "sheet", "ocr"), required=True)
    parser.add_argument("--input", type=Path, required=True)
    parser.add_argument("--batch-id", required=True)
    parser.add_argument("--document-uri", required=True)
    parser.add_argument("--captured-at", required=True)
    parser.add_argument("--extracted-at", required=True)
    parser.add_argument(
        "--profile",
        choices=("attendance", "roster", "leave", "employee"),
        default="attendance",
    )
    parser.add_argument("--min-confidence-bp", type=int, default=8500)
    parser.add_argument("--out", type=Path, default=None)
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    raw_bytes = args.input.read_bytes()
    logger = SafeWorkerLogger()

    if args.channel == "whatsapp":
        bundle = extract_whatsapp_export(
            raw_bytes=raw_bytes,
            document_uri=args.document_uri,
            batch_id=args.batch_id,
            captured_at=args.captured_at,
            extracted_at=args.extracted_at,
            min_confidence_bp=args.min_confidence_bp,
            logger=logger,
        )
    elif args.channel == "sheet":
        profile: SheetProfile = args.profile
        bundle = extract_sheet(
            raw_bytes=raw_bytes,
            document_uri=args.document_uri,
            batch_id=args.batch_id,
            profile=profile,
            captured_at=args.captured_at,
            extracted_at=args.extracted_at,
            logger=logger,
        )
    else:
        bundle = extract_from_ocr(
            raw_bytes=raw_bytes,
            document_uri=args.document_uri,
            media_type="application/json",
            batch_id=args.batch_id,
            captured_at=args.captured_at,
            extracted_at=args.extracted_at,
            adapter=DeterministicFixtureOcrAdapter(),
            min_confidence_bp=args.min_confidence_bp,
            logger=logger,
        )

    output = bundle.to_canonical_json() + "\n"
    if args.out is not None:
        args.out.write_text(output, encoding="utf-8")
    else:
        print(output, end="")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
