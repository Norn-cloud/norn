#!/usr/bin/env python3
"""Deterministically generate Norn's versioned offline IANA tzdb bundles.

Reads exact SHA-256-pinned upstream `jiff-tzdb` crate archives, verifies their
tarball hashes before extraction, strips the non-IANA `Factory` entry,
deduplicates shared alias TZif payloads, and emits deterministic binary bundles
under `crates/norn-expr/data/tzdb/<release>.tzdb` alongside the shipped
`LICENSE-MIT` notice.

Binary format (`NTZDB01\\0`, little-endian):
  - magic:       8 bytes   `NTZDB01\\0`
  - release_len: u16       length of UTF-8 release identifier (e.g. "2025b")
  - reserved:    u16       must be 0
  - zone_count:  u32       number of sorted unique case-sensitive zone entries
  - names_len:   u32       byte length of the concatenated UTF-8 zone names
  - payload_len: u32       byte length of the concatenated deduplicated TZif blobs
  - release:     [u8; release_len]
  - index:       zone_count * (name_offset: u32, name_len: u32, tzif_offset: u32, tzif_len: u32)
  - names:       [u8; names_len]
  - payload:     [u8; payload_len]
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import struct
import sys
import tarfile
import tempfile
import urllib.request
from dataclasses import dataclass
from pathlib import Path

MAGIC = b"NTZDB01\x00"
HEADER_STRUCT = struct.Struct("<8sHHIII")
ENTRY_STRUCT = struct.Struct("<IIII")
FORBIDDEN_ZONES = frozenset({"Factory"})
ROOT = Path(__file__).resolve().parents[1]
OUTPUT_DIR = ROOT / "crates/norn-expr/data/tzdb"


@dataclass(frozen=True)
class UpstreamRelease:
    release: str
    crate_version: str
    crate_url: str
    crate_sha256: str
    expected_zones: int


RELEASES: tuple[UpstreamRelease, ...] = (
    UpstreamRelease(
        release="2025b",
        crate_version="0.1.4",
        crate_url="https://static.crates.io/crates/jiff-tzdb/jiff-tzdb-0.1.4.crate",
        crate_sha256="c1283705eb0a21404d2bfd6eef2a7593d240bc42a0bdb39db0ad6fa2ec026524",
        expected_zones=597,
    ),
    UpstreamRelease(
        release="2026c",
        crate_version="0.1.8",
        crate_url="https://static.crates.io/crates/jiff-tzdb/jiff-tzdb-0.1.8.crate",
        crate_sha256="142bd39932ad231f10513df9ab62661fead8719872150b7ad02a2df79f4e141e",
        expected_zones=597,
    ),
)

VERSION_RE = re.compile(r'pub\(super\)\s+static\s+VERSION:\s+Option<&str>\s*=\s*Some\(r"([^"]+)"\);')
ENTRY_RE = re.compile(r'^\s*\(r"([^"]+)",\s*(\d+)\.\.(\d+)\),\s*$')


def sha256_hex(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


MAX_CRATE_BYTES = 32 * 1024 * 1024


class _HttpsOnlyRedirects(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        if not newurl.startswith("https://"):
            raise ValueError(f"{req.full_url}: refused non-HTTPS redirect to {newurl}")
        return super().redirect_request(req, fp, code, msg, headers, newurl)


def _write_atomic(path: Path, data: bytes) -> None:
    with tempfile.NamedTemporaryFile(dir=path.parent, delete=False) as tmp:
        tmp_path = Path(tmp.name)
        try:
            tmp.write(data)
            tmp.flush()
        except BaseException:
            tmp_path.unlink(missing_ok=True)
            raise
    try:
        os.replace(tmp_path, path)
    except BaseException:
        tmp_path.unlink(missing_ok=True)
        raise


def load_verified_crate(spec: UpstreamRelease, crate_dir: Path, download: bool) -> Path:
    crate_path = crate_dir / f"jiff-tzdb-{spec.crate_version}.crate"
    if not crate_path.is_file():
        if not download:
            raise FileNotFoundError(
                f"missing {crate_path}; pass --download or place the pinned crate in {crate_dir}"
            )
        if not spec.crate_url.startswith("https://"):
            raise ValueError(f"refusing non-HTTPS crate URL: {spec.crate_url!r}")
        crate_dir.mkdir(parents=True, exist_ok=True)
        opener = urllib.request.build_opener(_HttpsOnlyRedirects())
        with opener.open(spec.crate_url, timeout=60) as response:
            data = response.read(MAX_CRATE_BYTES + 1)
        if len(data) > MAX_CRATE_BYTES:
            raise ValueError(f"{crate_path}: download exceeds {MAX_CRATE_BYTES}-byte budget")
        actual = sha256_hex(data)
        if actual != spec.crate_sha256:
            raise ValueError(
                f"{crate_path}: SHA-256 mismatch: expected {spec.crate_sha256}, got {actual}"
            )
        _write_atomic(crate_path, data)
        return crate_path
    actual = sha256_hex(crate_path.read_bytes())
    if actual != spec.crate_sha256:
        raise ValueError(
            f"{crate_path}: SHA-256 mismatch: expected {spec.crate_sha256}, got {actual}"
        )
    return crate_path


def parse_tzname_rs(text: str, expected_release: str) -> list[tuple[str, int, int]]:
    version_match = VERSION_RE.search(text)
    if version_match is None:
        raise ValueError("tzname.rs missing VERSION declaration")
    actual_release = version_match.group(1)
    if actual_release != expected_release:
        raise ValueError(
            f"tzname.rs VERSION mismatch: expected {expected_release!r}, got {actual_release!r}"
        )

    in_table = False
    entries: list[tuple[str, int, int]] = []
    for line_no, line in enumerate(text.splitlines(), start=1):
        stripped = line.strip()
        if stripped.startswith("pub(super) static TZNAME_TO_OFFSET:"):
            in_table = True
            continue
        if not in_table:
            continue
        if stripped == "];":
            break
        match = ENTRY_RE.match(line)
        if match is None:
            raise ValueError(f"tzname.rs:{line_no}: unparsed table line: {line!r}")
        name, start_s, end_s = match.groups()
        start, end = int(start_s), int(end_s)
        if start >= end:
            raise ValueError(f"tzname.rs:{line_no}: invalid range {start}..{end} for {name!r}")
        entries.append((name, start, end))
    else:
        raise ValueError("tzname.rs: unterminated TZNAME_TO_OFFSET table")
    return entries


def encode_bundle(release: str, zone_blobs: list[tuple[str, bytes]]) -> bytes:
    release_bytes = release.encode("ascii")
    if not (1 <= len(release_bytes) <= 64):
        raise ValueError(f"invalid release length: {release!r}")

    sorted_zones = sorted(zone_blobs, key=lambda item: item[0].encode("utf-8"))
    names_buf = bytearray()
    payload_buf = bytearray()
    blob_to_range: dict[bytes, tuple[int, int]] = {}
    index_records: list[tuple[int, int, int, int]] = []

    prev_name: bytes | None = None
    for name, tzif in sorted_zones:
        if name in FORBIDDEN_ZONES:
            raise ValueError(f"forbidden zone {name!r} reached encoder")
        if not tzif.startswith(b"TZif"):
            raise ValueError(f"zone {name!r} payload does not begin with TZif magic")
        name_bytes = name.encode("utf-8")
        if prev_name is not None and name_bytes <= prev_name:
            raise ValueError(f"duplicate or unsorted zone {name!r}")
        prev_name = name_bytes

        name_offset = len(names_buf)
        names_buf.extend(name_bytes)

        tzif_range = blob_to_range.get(tzif)
        if tzif_range is None:
            tzif_offset = len(payload_buf)
            payload_buf.extend(tzif)
            tzif_range = (tzif_offset, len(tzif))
            blob_to_range[tzif] = tzif_range

        index_records.append((name_offset, len(name_bytes), tzif_range[0], tzif_range[1]))

    header = HEADER_STRUCT.pack(
        MAGIC,
        len(release_bytes),
        0,
        len(index_records),
        len(names_buf),
        len(payload_buf),
    )
    out = bytearray(header)
    out.extend(release_bytes)
    for rec in index_records:
        out.extend(ENTRY_STRUCT.pack(*rec))
    out.extend(names_buf)
    out.extend(payload_buf)
    return bytes(out)


def _extract_member(tf: tarfile.TarFile, member_name: str) -> bytes:
    stream = tf.extractfile(member_name)
    if stream is None:
        raise ValueError(f"tar archive missing regular file {member_name!r}")
    with stream:
        return stream.read()


def build_from_crate(spec: UpstreamRelease, crate_path: Path) -> tuple[bytes, bytes, str]:
    prefix = f"jiff-tzdb-{spec.crate_version}/"
    with tarfile.open(crate_path, "r:gz") as tf:
        tzname_text = _extract_member(tf, prefix + "tzname.rs").decode("utf-8")
        concatenated = _extract_member(tf, prefix + "concatenated-zoneinfo.dat")
        license_mit = _extract_member(tf, prefix + "LICENSE-MIT")
        vcs_info = json.loads(_extract_member(tf, prefix + ".cargo_vcs_info.json").decode("utf-8"))
        vcs_commit = vcs_info["git"]["sha1"]

    raw_entries = parse_tzname_rs(tzname_text, spec.release)
    found_forbidden = [name for name, _, _ in raw_entries if name in FORBIDDEN_ZONES]
    if sorted(found_forbidden) != sorted(FORBIDDEN_ZONES):
        raise ValueError(
            f"{spec.release}: expected forbidden zones {sorted(FORBIDDEN_ZONES)}, found {found_forbidden}"
        )

    zone_blobs: list[tuple[str, bytes]] = []
    for name, start, end in raw_entries:
        if name in FORBIDDEN_ZONES:
            continue
        if end > len(concatenated):
            raise ValueError(f"{spec.release}: {name} slice {start}..{end} out of bounds")
        zone_blobs.append((name, concatenated[start:end]))

    if len(zone_blobs) != spec.expected_zones:
        raise ValueError(
            f"{spec.release}: expected {spec.expected_zones} zones after excluding Factory, got {len(zone_blobs)}"
        )

    bundle = encode_bundle(spec.release, zone_blobs)
    return bundle, license_mit, vcs_commit


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--crate-dir",
        type=Path,
        default=Path(tempfile.gettempdir()) / "norn-tzdb-inspect",
        help="Directory holding pinned jiff-tzdb-<version>.crate tarballs",
    )
    parser.add_argument(
        "--download",
        action="store_true",
        help="Download missing pinned .crate archives from static.crates.io",
    )
    parser.add_argument(
        "--check",
        action="store_true",
        help="Verify committed bundles match deterministic regeneration without writing",
    )
    args = parser.parse_args(argv)

    OUTPUT_DIR.mkdir(parents=True, exist_ok=True)
    expected_license: bytes | None = None
    built: list[tuple[UpstreamRelease, bytes, str]] = []
    drift = False

    for spec in RELEASES:
        crate_path = load_verified_crate(spec, args.crate_dir, args.download)
        bundle, license_mit, vcs_commit = build_from_crate(spec, crate_path)
        if expected_license is None:
            expected_license = license_mit
        elif license_mit != expected_license:
            raise ValueError(f"{spec.release}: LICENSE-MIT differs across pinned jiff-tzdb crates")
        built.append((spec, bundle, vcs_commit))

    if expected_license is None:
        raise RuntimeError("no tzdb releases configured")

    for spec, bundle, vcs_commit in built:
        bundle_path = OUTPUT_DIR / f"{spec.release}.tzdb"
        bundle_sha = sha256_hex(bundle)
        if args.check:
            if not bundle_path.is_file() or bundle_path.read_bytes() != bundle:
                print(f"DRIFT {bundle_path.relative_to(ROOT)}", file=sys.stderr)
                drift = True
            else:
                print(f"OK {bundle_path.relative_to(ROOT)} sha256={bundle_sha} bytes={len(bundle)}")
        else:
            _write_atomic(bundle_path, bundle)
            print(
                f"WROTE {bundle_path.relative_to(ROOT)} sha256={bundle_sha} bytes={len(bundle)} vcs={vcs_commit}"
            )

    license_path = OUTPUT_DIR / "LICENSE-MIT"
    license_sha = sha256_hex(expected_license)
    if args.check:
        if not license_path.is_file() or license_path.read_bytes() != expected_license:
            print(f"DRIFT {license_path.relative_to(ROOT)}", file=sys.stderr)
            drift = True
        else:
            print(f"OK {license_path.relative_to(ROOT)} sha256={license_sha}")
    else:
        _write_atomic(license_path, expected_license)
        print(f"WROTE {license_path.relative_to(ROOT)} sha256={license_sha}")

    return 1 if drift else 0


if __name__ == "__main__":
    raise SystemExit(main())
