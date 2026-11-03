#!/usr/bin/env python3
"""One-shot conversion of the shared diagnostic manifest and its provenance."""
import hashlib
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SOURCE = ROOT / "crates/norn-checker/conformance/diagnostic-codes.json"
TARGET = SOURCE.with_suffix("")
RECORDS = ROOT / ".github/ci/provenance/artifacts"


def main():
    document = json.loads(SOURCE.read_text())
    entries = {item["code"]: item for item in document["codes"]}
    if len(entries) != len(document["codes"]):
        raise ValueError("duplicate diagnostic identity")
    if any(not all(part and all(c.isascii() and (c.islower() or c.isdigit() or c == "_") for c in part)
                   for part in code.split(".")) or len(code.split(".")) != 3 for code in entries):
        raise ValueError("invalid dotted diagnostic identity")
    template = json.loads((RECORDS / "crates/norn-checker/src/codes.rs.json").read_text())
    TARGET.mkdir()
    for code, value in entries.items():
        path = TARGET / f"{code}.json"
        content = (json.dumps(value, indent=2) + "\n").encode()
        path.write_bytes(content)
        record = RECORDS / (path.relative_to(ROOT).as_posix() + ".json")
        record.parent.mkdir(parents=True, exist_ok=True)
        record.write_text(json.dumps({**template, "sha256": hashlib.sha256(content).hexdigest()}, indent=2) + "\n")
    SOURCE.unlink()
    (RECORDS / (SOURCE.relative_to(ROOT).as_posix() + ".json")).unlink()


if __name__ == "__main__":
    main()
