#!/usr/bin/env python3
"""Acquire a pinned OWL ontology closure outside the source tree and normalise it to N-Triples.

The read manifest (`registry/standards/imports/<atlas id>.read.json`) pins, file by file, the
`owl:imports` closure of an ontology set's roots in its `admission` (an `OntologySource`): every
original's HTTPS URL and SHA-256, and the licence every one of them must declare. Nothing is
vendored. `fetch` downloads each pinned original (reusing a digest-matching copy), refuses a
digest mismatch or an ontology whose header does not declare the pinned licence, parses every
ontology with the hash-locked rdflib of `verification/ontology/tools/requirements.lock`, and
writes, outside the repository:

    <output>/raw/<host>/<path>  the originals, exactly as pinned
    <output>/graph.nt           the union graph as canonical N-Triples, sorted (rdflib.compare
                                relabels blank nodes, so the bytes depend only on the graph)

and refuses a graph whose SHA-256 is not the manifest's `graph.sha256`. `norn-format-read`
then reads `graph.nt` with the OWL reader (`crates/norn-formats/src/owl.rs`).

A pinned fetch follows no HTTP redirect: the bytes come from the pinned URL itself, or the
fetch is refused before any digest is computed. `pin` is the maintainer's half: it recomputes
the closure of `selection.roots` from the network with `selection.resolve`, checks every
licence, normalises, and rewrites the manifest's `admission.files`, `namespaces` and `graph`;
where a URL redirects, it pins the destination, so the later fetch needs no redirect. Review
its diff like a dependency bump.

Run with the ontology tools interpreter (`scripts/bootstrap-ontology-tools.sh`):

    target/ontology-tools/venv/bin/python -I scripts/importers/owl_sources.py fetch \\
        registry/standards/imports/edmc-fibo-2026q3.read.json --output ~/.cache/<task>/fibo
"""

from __future__ import annotations

import argparse
import hashlib
import http.client
import json
import re
import time
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SIZE_BUDGET = 64 * 1024 * 1024
LICENCE_MARKERS = {"MIT": ("opensource.org/licenses/MIT",)}
OWL_IMPORTS = re.compile(r'<owl:imports\s+rdf:resource="([^"]+)"')
ONTOLOGY = re.compile(r'<owl:Ontology\s+rdf:about="([^"]+)"')
VERSION_IRI = re.compile(r'<owl:versionIRI\s+rdf:resource="([^"]+)"')
ENTITY = re.compile(r'<!ENTITY\s+([A-Za-z][\w.-]*)\s+"([^"]*)"\s*>')


def header(raw: bytes) -> dict:
    """The ontology IRI, version IRI and imports an RDF/XML header declares, with the
    document's internal DTD entities (`&fibo-fnd-pty-pty;`) expanded."""
    text = raw.decode("utf-8", "replace")
    entities = dict(ENTITY.findall(text.split("]>", 1)[0]))
    expand = lambda value: re.sub(r"&([A-Za-z][\w.-]*);", lambda m: entities.get(m.group(1), m.group(0)), value)
    head = text.split("</owl:Ontology>", 1)[0]
    found = ONTOLOGY.search(head)
    version = VERSION_IRI.search(head)
    return {
        "iri": expand(found.group(1)) if found else None,
        "version": expand(version.group(1)) if version else None,
        "imports": [expand(value) for value in OWL_IMPORTS.findall(head)],
    }


def outside_tree(path: Path) -> Path:
    path = path.expanduser().resolve()
    if path == ROOT or ROOT in path.parents:
        raise SystemExit("ontology downloads must remain outside the source checkout")
    return path


# Extra urllib handlers (tests replace the HTTPS transport); empty in use.
TRANSPORT: tuple = ()


class RedirectRefused(SystemExit):
    """A pinned fetch was redirected: the bytes would not come from the pinned URL."""


class _RefuseRedirects(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        raise RedirectRefused(
            f"{req.full_url}: redirected ({code}) to {newurl}; a pinned fetch follows no redirect "
            "(pin the destination with `pin`)")


class _HttpsOnlyRedirects(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        if not newurl.startswith("https://"):
            raise SystemExit(f"{req.full_url}: redirected to {newurl}, which is not HTTPS")
        return super().redirect_request(req, fp, code, msg, headers, newurl)


def fetch_url(url: str, attempts: int = 5, *, follow: bool = False) -> tuple[bytes, str]:
    """The bytes at `url` and the URL that served them. A pinned fetch (`follow=False`)
    refuses any redirect; `pin` follows HTTPS redirects and pins where they end."""
    if not url.startswith("https://"):
        raise SystemExit(f"{url}: only HTTPS originals are pinned")
    opener = urllib.request.build_opener(
        _HttpsOnlyRedirects() if follow else _RefuseRedirects(), *TRANSPORT)
    request = urllib.request.Request(url, headers={
        "User-Agent": "Norn-ontology-import/1.0",
        "Accept": "application/rdf+xml, text/plain;q=0.5",
    })
    for attempt in range(attempts):
        try:
            with opener.open(request, timeout=60) as response:
                raw = response.read(SIZE_BUDGET + 1)
                served = response.geturl()
            if len(raw) > SIZE_BUDGET:
                raise SystemExit(f"{url}: exceeds the {SIZE_BUDGET}-byte budget")
            if not follow and served != url:
                raise RedirectRefused(f"{url}: served from {served}; a pinned fetch follows no redirect")
            return raw, served
        except (OSError, http.client.HTTPException):
            if attempt == attempts - 1:
                raise
            time.sleep(2 ** (attempt + 1))
    raise AssertionError("unreachable")


def fetch_bytes(url: str, attempts: int = 5) -> bytes:
    """The bytes at exactly `url`: a redirect is refused."""
    return fetch_url(url, attempts)[0]


def sha256(raw: bytes) -> str:
    return hashlib.sha256(raw).hexdigest()


def declares_licence(raw: bytes, licence: str) -> bool:
    """Whether an ontology header's `dct:license` names `licence`."""
    header = raw.split(b"</owl:Ontology>", 1)[0]
    match = re.search(rb"<dct:license[^>]*>(.*?)</dct:license>", header, re.S)
    if match is None:
        return False
    text = match.group(1).decode("utf-8", "replace")
    return any(marker in text for marker in LICENCE_MARKERS.get(licence, ()))


def local_path(raw_dir: Path, url: str) -> Path:
    relative = url.removeprefix("https://")
    if not relative or any(part in ("", "..") for part in relative.split("/")):
        raise SystemExit(f"{url}: not a plain HTTPS file URL")
    return raw_dir / relative


def is_ontology(url: str) -> bool:
    return url.endswith(".rdf")


def acquire(url: str, digest: str, raw_dir: Path) -> bytes:
    """Fetch one pinned original (or reuse a digest-matching copy)."""
    target = local_path(raw_dir, url)
    raw = target.read_bytes() if target.exists() else None
    if raw is None or sha256(raw) != digest:
        raw = fetch_bytes(url)
        if sha256(raw) != digest:
            raise SystemExit(f"{url}: digest mismatch (pinned {digest})")
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(raw)
    return raw


def normalise(originals: list[tuple[str, bytes]]) -> tuple[bytes, list[dict]]:
    """The canonical N-Triples of the union graph and each ontology's namespace prefix."""
    from rdflib import Graph  # the hash-locked ontology tools interpreter
    from rdflib.compare import to_canonical_graph

    graph = Graph()
    namespaces = {}
    for url, raw in originals:
        part = Graph()
        part.parse(data=raw, format="xml", publicID=url)
        iri = header(raw)["iri"]
        if iri is None:
            raise SystemExit(f"{url}: declares no owl:Ontology")
        prefix = next((str(p) for p, ns in part.namespaces() if str(ns) == iri), None)
        if prefix is None:
            raise SystemExit(f"{url}: no xmlns prefix names the ontology namespace {iri}")
        namespaces[iri] = prefix
        graph += part
    lines = sorted(line for line in to_canonical_graph(graph).serialize(format="nt").splitlines() if line.strip())
    listed = [{"iri": iri, "prefix": prefix} for iri, prefix in sorted(namespaces.items())]
    return ("\n".join(lines) + "\n").encode(), listed


def fetch(args: argparse.Namespace) -> None:
    manifest = json.loads(args.manifest.read_text())
    output = outside_tree(args.output)
    raw_dir = output / "raw"
    admission = manifest["admission"]
    originals = []
    for pin in admission["files"]:
        raw = acquire(pin["path"], pin["sha256"], raw_dir)
        if is_ontology(pin["path"]):
            if not declares_licence(raw, admission["license"]):
                raise SystemExit(f"{pin['path']}: the header does not declare {admission['license']}")
            originals.append((pin["path"], raw))
    graph, namespaces = normalise(originals)
    if sha256(graph) != manifest["graph"]["sha256"]:
        raise SystemExit(f"normalised graph {sha256(graph)} is not the pinned {manifest['graph']['sha256']}")
    if namespaces != manifest["namespaces"]:
        raise SystemExit("the originals' namespace prefixes differ from the manifest")
    (output / "graph.nt").write_bytes(graph)
    print(json.dumps({"originals": len(admission["files"]), "graph": manifest["graph"]}))


def pin(args: argparse.Namespace) -> None:
    """Recompute the closure of the roots from the network and rewrite the manifest's pins."""
    manifest = json.loads(args.manifest.read_text())
    selection = manifest["selection"]
    licence = manifest["admission"]["license"]
    files, originals, seen, queue = [], [], set(), list(selection["roots"])
    for url in selection.get("evidence", []):
        raw, served = fetch_url(url, follow=True)
        files.append({"path": served, "sha256": sha256(raw)})
    while queue:
        iri = queue.pop(0)
        if iri in seen:
            continue
        seen.add(iri)
        rule = next((r for r in selection["resolve"] if iri.startswith(r["namespace"])), None)
        if rule is None:
            raise SystemExit(f"{iri}: imported, but no resolve rule covers it; the closure is refused")
        url = rule["url"].replace("{path}", iri[len(rule["namespace"]):].rstrip("/"))
        raw, url = fetch_url(url, follow=True)
        declared = header(raw)
        if declared["iri"] != iri:
            raise SystemExit(f"{url}: does not declare ontology {iri}")
        if not declares_licence(raw, licence):
            raise SystemExit(f"{iri}: the header does not declare {licence}; the closure is refused")
        if declared["version"] is not None and rule.get("preferVersionUrl"):
            # Pin the dated release URL when it serves the same bytes as the current one.
            dated = declared["version"].rstrip("/") + ".rdf"
            try:
                dated_raw, dated = fetch_url(dated, attempts=1, follow=True)
                if dated_raw == raw:
                    url = dated
            except (OSError, http.client.HTTPException):
                pass
        files.append({"path": url, "sha256": sha256(raw)})
        originals.append((url, raw))
        queue.extend(declared["imports"])
    graph, namespaces = normalise(sorted(originals))
    manifest["admission"]["files"] = sorted(files, key=lambda item: item["path"])
    manifest["namespaces"] = namespaces
    manifest["graph"] = {"sha256": sha256(graph), "triples": graph.count(b"\n")}
    args.manifest.write_text(json.dumps(manifest, indent=2) + "\n")
    print(json.dumps({"files": len(files), "graph": manifest["graph"]}))


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest="command", required=True)
    for name, help_text in (("fetch", "download, verify and normalise the pinned originals"),
                            ("pin", "recompute the closure and rewrite the manifest's pins")):
        command = commands.add_parser(name, help=help_text)
        command.add_argument("manifest", type=Path)
        if name == "fetch":
            command.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    {"fetch": fetch, "pin": pin}[args.command](args)


if __name__ == "__main__":
    main()
