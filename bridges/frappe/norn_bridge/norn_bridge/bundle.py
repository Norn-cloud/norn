"""The installed projection bundle of the current site (data, never code)."""

from __future__ import annotations

import hashlib
import json
import os

FORMAT = "norn.frappe-bundle/1"
# Must equal norn_projections::FRAPPE_BUNDLE_DOMAIN.
DOMAIN = b"norn.frappe-bundle/1\n"
_cache: dict[str, tuple[float, dict]] = {}


def site_dir() -> str:
    import frappe

    return frappe.get_site_path("norn")


def path(kind: str) -> str:
    """`staged` (pulled, not yet applied) or `installed` (applied by migrate)."""
    return os.path.join(site_dir(), f"{kind}.bundle.json")


def load(kind: str = "installed") -> dict | None:
    file = path(kind)
    try:
        mtime = os.path.getmtime(file)
    except OSError:
        return None
    hit = _cache.get(file)
    if hit and hit[0] == mtime:
        return hit[1]
    with open(file, encoding="utf-8") as handle:
        document = json.load(handle)
    if document.get("format") != FORMAT:
        raise ValueError(f"{file}: not a {FORMAT} document")
    _cache[file] = (mtime, document)
    return document


def digest(document: dict) -> str:
    """`bundleHash`: SHA-256 of DOMAIN then the canonical JSON of the bundle
    without its hash, as `norn_projections::project_frappe_bundle` computes it.
    Sorted keys and compact separators match Norn's canonical form for
    bundles, whose keys are ASCII and whose numbers are integers."""
    body = {k: v for k, v in document.items() if k != "bundleHash"}
    raw = json.dumps(body, sort_keys=True, separators=(",", ":"), ensure_ascii=False)
    return hashlib.sha256(DOMAIN + raw.encode()).hexdigest()


def object_type(doctype: str) -> dict:
    """The Norn object type a generated DocType projects, from the bundle."""
    document = load() or {}
    entry = document.get("objectTypes", {}).get(doctype)
    if entry is None:
        raise LookupError(f"{doctype} is not in the installed Norn bundle")
    return entry
