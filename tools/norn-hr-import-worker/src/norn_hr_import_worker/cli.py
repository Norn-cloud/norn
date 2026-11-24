"""CLI entrypoint for `norn-hr-import-worker` with bounded reads and sanitized errors."""

from __future__ import annotations

import argparse
import json
import os
import stat
import sys
from collections.abc import Sequence
from pathlib import Path

from norn_hr_import_worker.models import MAX_INPUT_BYTES, ExactValueError, SafeWorkerLogger
from norn_hr_import_worker.ocr import DeterministicFixtureOcrAdapter, extract_from_ocr
from norn_hr_import_worker.sheets import SheetProfile, SheetSchemaError, extract_sheet
from norn_hr_import_worker.whatsapp import extract_whatsapp_export

_READ_CHUNK_BYTES = 65_536


class InputRefusalError(ValueError):
    """Fail-closed refusal for symlinks, non-regular files, or oversized inputs."""


def read_bounded_input(path: Path, *, max_bytes: int = MAX_INPUT_BYTES) -> bytes:
    """Refuse symlinks, non-regular files, and files exceeding `max_bytes` before loading."""
    if max_bytes <= 0 or max_bytes > MAX_INPUT_BYTES:
        raise InputRefusalError(
            f"max_input_bytes must be within 1..{MAX_INPUT_BYTES} bytes"
        )
    try:
        link_stat = os.lstat(path)
    except OSError as exc:
        raise InputRefusalError("Input file could not be inspected") from exc
    if stat.S_ISLNK(link_stat.st_mode):
        raise InputRefusalError("Input path must not be a symbolic link")
    if not stat.S_ISREG(link_stat.st_mode):
        raise InputRefusalError("Input path must be a regular file")
    if link_stat.st_size > max_bytes:
        raise InputRefusalError(f"Input file exceeds {max_bytes} byte limit")

    flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0)
    try:
        fd = os.open(path, flags)
    except OSError as exc:
        raise InputRefusalError("Input file could not be opened safely") from exc

    chunks: list[bytes] = []
    total = 0
    try:
        fd_stat = os.fstat(fd)
        if not stat.S_ISREG(fd_stat.st_mode):
            raise InputRefusalError("Opened input descriptor is not a regular file")
        if fd_stat.st_size > max_bytes:
            raise InputRefusalError(f"Input file exceeds {max_bytes} byte limit")
        while True:
            remaining = (max_bytes - total) + 1
            chunk = os.read(fd, min(_READ_CHUNK_BYTES, remaining))
            if not chunk:
                break
            total += len(chunk)
            if total > max_bytes:
                raise InputRefusalError(f"Input file exceeds {max_bytes} byte limit")
            chunks.append(chunk)
    finally:
        os.close(fd)
    return b"".join(chunks)


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
    parser.add_argument("--max-input-bytes", type=int, default=MAX_INPUT_BYTES)
    parser.add_argument("--out", type=Path, default=None)
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    logger = SafeWorkerLogger()
    try:
        raw_bytes = read_bounded_input(args.input, max_bytes=args.max_input_bytes)
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
    except (InputRefusalError, ExactValueError, SheetSchemaError) as exc:
        print(f"error ({type(exc).__name__}): {exc}", file=sys.stderr)
        return 2
    except (UnicodeDecodeError, json.JSONDecodeError):
        print(
            "error: input file is not valid UTF-8 or JSON for the selected channel",
            file=sys.stderr,
        )
        return 2
    except OSError:
        print("error: failed to read or write file", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
