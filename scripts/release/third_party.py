#!/usr/bin/env python3
"""THIRD-PARTY-LICENSES for Norn's release packages, from `cargo metadata`.

    third_party.py [--repo DIR] --out FILE

What ships compiled from Rust is the checker WebAssembly module
(`norn-checker-wasm` for wasm32-unknown-unknown) and the dev host
(`norn-cell-do`'s `norn-dev-host` for the Linux musl targets). This script
resolves the normal (not dev, not build) dependency closure of each for its
target, as `cargo metadata --filter-platform` reports it, and writes one file
that lists every third-party package in the union with its licence
expression and source, and reproduces each distinct licence, notice or
copyright file the package carries once, naming the packages that carry it.

A package is first-party (Norn's own, under the repository's LICENSE and
NOTICE) only when it has no registry source and its manifest is under
`crates/`. Vendored packages (`vendor/`, `third_party/`) are third-party: their
licence files are looked for in the package directory and then in each parent
directory below the repository root. The script fails closed: a third-party
package with neither a licence expression nor a licence file, or a path
package outside `crates/`, `vendor/` and `third_party/`, is refused.

`cargo metadata` resolves features for the whole workspace, so the closure can
include a few packages a particular binary does not link: the file may list
more than ships, never less.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import subprocess
import sys
from pathlib import Path

# What the release builds from Rust, and for which targets (scripts/release/assemble.py PLATFORMS).
SHIPPED = (
    ("norn-checker-wasm", "wasm32-unknown-unknown"),
    ("norn-cell-do", "x86_64-unknown-linux-musl"),
    ("norn-cell-do", "aarch64-unknown-linux-musl"),
)
FIRST_PARTY = "crates/"
VENDORED = ("vendor/", "third_party/")
LICENCE_FILE = re.compile(r"(?i)^(licen[cs]e|copying|notice|copyright|unlicense|authors)([-._].*)?$")
# A file this large is not a licence text; refusing keeps a stray archive out of the release.
MAX_LICENCE_BYTES = 256 * 1024


class Refused(Exception):
    """A shipped dependency whose licence this script cannot reproduce."""


def closure(metadata: dict, root: str) -> set[str]:
    """Package ids reachable from the workspace package named `root` through normal dependencies."""
    nodes = {node["id"]: node for node in metadata["resolve"]["nodes"]}
    members = set(metadata["workspace_members"])
    by_name = [p["id"] for p in metadata["packages"] if p["name"] == root and p["id"] in members]
    if len(by_name) != 1:
        raise Refused(f"{root}: expected exactly one workspace package, found {len(by_name)}")
    seen, stack = set(), [by_name[0]]
    while stack:
        current = stack.pop()
        if current in seen:
            continue
        seen.add(current)
        for dep in nodes[current]["deps"]:
            if any(kind.get("kind") is None for kind in dep["dep_kinds"]):
                stack.append(dep["pkg"])
    return seen


def relative(path: str, repo: Path) -> str | None:
    try:
        return Path(path).resolve().relative_to(repo.resolve()).as_posix()
    except ValueError:
        return None


def licence_files(package: dict, repo: Path) -> list[Path]:
    """The package's licence files; a vendored package also inherits its vendor directory's."""
    directory = Path(package["manifest_path"]).parent
    found: list[Path] = []
    if package.get("license_file"):
        found.append(directory / package["license_file"])
    stop = repo.resolve()
    current = directory.resolve()
    while True:
        found += sorted(p for p in current.iterdir() if p.is_file() and LICENCE_FILE.match(p.name))
        if found or package["source"] is not None or current.parent == stop or stop not in current.parents:
            break
        current = current.parent
    unique: list[Path] = []
    for path in found:
        if path.resolve() not in {u.resolve() for u in unique}:
            unique.append(path)
    return unique


def render(metadata: dict, repo: Path) -> bytes:
    packages = {p["id"]: p for p in metadata["packages"]}
    shipped: set[str] = set()
    for root, _target in SHIPPED:
        shipped |= closure(metadata, root)
    entries = []
    texts: dict[str, tuple[str, bytes, list[str]]] = {}
    for package_id in shipped:
        package = packages[package_id]
        where = relative(package["manifest_path"], repo) if package["source"] is None else None
        if package["source"] is None:
            if where is None:
                raise Refused(f"{package['name']}: a path package outside the repository")
            if where.startswith(FIRST_PARTY):
                continue
            if not where.startswith(VENDORED):
                raise Refused(f"{package['name']}: path package {where} is neither first-party (crates/) nor vendored")
        label = f"{package['name']} {package['version']}"
        files = licence_files(package, repo)
        if not package.get("license") and not files:
            raise Refused(f"{label}: no licence expression and no licence file")
        source = f"vendored at {Path(where).parent.as_posix()}" if where else (package.get("repository") or package["source"])
        entries.append((package["name"], package["version"], package.get("license") or "see licence file", source))
        for path in files:
            data = path.read_bytes()
            if len(data) > MAX_LICENCE_BYTES:
                raise Refused(f"{label}: {path.name} is {len(data)} bytes, too large for a licence file")
            digest = hashlib.sha256(data).hexdigest()
            texts.setdefault(digest, (path.name, data, []))[2].append(label)
    entries.sort()
    lines = [
        "Third-party software in Norn's release packages",
        "",
        "The checker WebAssembly module (@norn/cli) and the dev host (@norn/dev-host-*) are",
        "compiled from Norn's own crates (Apache-2.0; see LICENSE and NOTICE) and the",
        "packages below. Each licence, notice or copyright file those packages carry is",
        "reproduced once after the list, with the packages that carry it.",
        "",
        f"Packages ({len(entries)}):",
        *(f"  {name} {version}  [{licence}]  {source}" for name, version, licence, source in entries),
        "",
    ]
    for _digest, (name, data, owners) in sorted(texts.items(), key=lambda item: (sorted(item[1][2]), item[0])):
        lines += ["=" * 78, f"{name} — carried by: {', '.join(sorted(owners))}", "-" * 78,
                  data.decode("utf-8", errors="replace").rstrip("\n"), ""]
    return ("\n".join(lines) + "\n").encode()


def metadata_for(repo: Path, target: str) -> dict:
    out = subprocess.run(["cargo", "metadata", "--locked", "--format-version", "1", "--filter-platform", target],
                         cwd=repo, check=True, capture_output=True).stdout
    return json.loads(out)


def merged(repo: Path) -> dict:
    """One metadata view per target, merged: the packages and resolve nodes of every target."""
    result: dict = {}
    for target in sorted({target for _root, target in SHIPPED}):
        data = metadata_for(repo, target)
        if not result:
            result = data
            continue
        known = {p["id"] for p in result["packages"]}
        result["packages"] += [p for p in data["packages"] if p["id"] not in known]
        nodes = {node["id"]: node for node in result["resolve"]["nodes"]}
        for node in data["resolve"]["nodes"]:
            if node["id"] in nodes:
                have = {dep["pkg"] for dep in nodes[node["id"]]["deps"]}
                nodes[node["id"]]["deps"] += [dep for dep in node["deps"] if dep["pkg"] not in have]
            else:
                result["resolve"]["nodes"].append(node)
    return result


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--repo", type=Path, default=Path.cwd())
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    try:
        args.out.write_bytes(render(merged(args.repo), args.repo))
    except Refused as refusal:
        print(f"third_party: refused: {refusal}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
