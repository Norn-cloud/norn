"""Host-only, replaceable CodeGraph substrate. Never indexes the source tree.

The pinned MIT upstream binary owns tree-sitter, resolution and SQLite. Norn
consumes its documented node-link export; no database schema is coupled here.
"""
from __future__ import annotations

from pathlib import Path
import argparse
import hashlib
import io
import json
import os
import subprocess
import sys
import tarfile
import tempfile
from typing import Protocol
import urllib.request

VERSION = "0.54.0"
ARCHIVE_SHA256 = "dcb3da1d007bf0d9c9e7f0d54ad71281514d930ddfd6a7841eccdd8bee9f8c69"
# SHA-256 of the one binary inside that archive. Checked before every use.
BINARY_SHA256 = "9774545e1e565f90cbda944854f15a77431b3f3b606063b009db1c69f48d222b"
ARCHIVE = f"codegraph-{VERSION}-x86_64-unknown-linux-musl.tar.gz"
ARCHIVE_URL = f"https://github.com/sunerpy/codegraph-rust/releases/download/v{VERSION}/{ARCHIVE}"


class CodeGraph(Protocol):
    """Small host trait; implementations return the canonical node-link graph."""
    def extract(self, sources: dict[str, bytes]) -> dict: ...


class UnpinnedBinary(ValueError):
    """The binary's bytes are not the pinned release's."""


class CodeGraphRust:
    """Runs the pinned binary. Its bytes are hashed before anything executes.

    The verified bytes are copied into this run's private scratch directory and
    only that copy is executed, so the file cannot change between the check and
    the run. `allow_unpinned` is for a locally built binary only: it prints a
    warning and is refused when `CI` is set.
    """

    def __init__(self, binary: Path, scratch: Path, allow_unpinned: bool = False):
        self.binary = Path(binary).resolve()
        self.scratch = scratch
        self.allow_unpinned = allow_unpinned

    def verified(self, directory: Path) -> str:
        raw = self.binary.read_bytes()
        digest = hashlib.sha256(raw).hexdigest()
        if digest != BINARY_SHA256:
            if not self.allow_unpinned:
                raise UnpinnedBinary(f"{self.binary}: sha256 {digest} is not the pinned {BINARY_SHA256}")
            if os.environ.get("CI"):
                raise UnpinnedBinary("an unpinned codegraph binary is refused when CI is set")
            print(f"warning: running UNPINNED codegraph binary {self.binary} (sha256 {digest}); "
                  "its output is not evidence", file=sys.stderr)
        copy = directory / "codegraph"
        copy.write_bytes(raw)
        copy.chmod(0o700)
        return str(copy)

    def extract(self, sources: dict[str, bytes]) -> dict:
        self.scratch.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(prefix="graph-", dir=self.scratch) as temp:
            tools = Path(temp) / "bin"
            tools.mkdir()
            binary = self.verified(tools)
            version = subprocess.check_output([binary, "--version"], text=True).strip()
            if version.split()[-1] != VERSION:
                raise ValueError(f"codegraph {VERSION} required, got {version}")
            root = Path(temp) / "src"
            root.mkdir()
            for name, raw in sorted(sources.items()):
                relative = Path(name)
                if relative.is_absolute() or ".." in relative.parts:
                    raise ValueError("unsafe graph source path")
                path = root / relative
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(raw)
            # Explicit target=none creates only the scratch index, never agent
            # configuration. init performs a full index of the copied files.
            subprocess.run([binary, "init", str(root), "--target", "none"], check=True,
                           stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=300)
            output = root / "graph.json"
            subprocess.run([binary, "export", "-p", str(root), "--no-centrality", "-o", str(output)],
                           check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=120)
            graph = json.loads(output.read_text())
            if not isinstance(graph.get("nodes"), list) or not isinstance(graph.get("links"), list):
                raise ValueError("unsupported codegraph export")
            return graph


def fetch(directory: Path) -> Path:
    """Download the pinned release archive and unpack its one binary.

    The archive is refused unless its SHA-256 is the pinned one, and only the
    member named `codegraph` is written. Nothing is installed or configured.
    """
    binary = directory / "codegraph"
    if binary.is_file() and hashlib.sha256(binary.read_bytes()).hexdigest() != BINARY_SHA256:
        raise UnpinnedBinary(f"{binary}: an existing file there is not the pinned binary; remove it")
    if not binary.is_file():
        with urllib.request.urlopen(ARCHIVE_URL, timeout=120) as response:
            raw = response.read()
        digest = hashlib.sha256(raw).hexdigest()
        if digest != ARCHIVE_SHA256:
            raise ValueError(f"{ARCHIVE}: sha256 {digest} is not the pinned {ARCHIVE_SHA256}")
        with tarfile.open(fileobj=io.BytesIO(raw), mode="r:gz") as archive:
            member = archive.extractfile("codegraph")
            if member is None:
                raise ValueError(f"{ARCHIVE}: no codegraph binary")
            extracted = member.read()
        if hashlib.sha256(extracted).hexdigest() != BINARY_SHA256:
            raise UnpinnedBinary(f"{ARCHIVE}: its codegraph binary is not the pinned one")
        directory.mkdir(parents=True, exist_ok=True)
        binary.write_bytes(extracted)
        binary.chmod(0o755)
    return binary.resolve()


def main():
    parser = argparse.ArgumentParser(description="Fetch the pinned CodeGraph binary and print its path.")
    parser.add_argument("--fetch", type=Path, required=True, metavar="DIR")
    print(fetch(parser.parse_args().fetch))


if __name__ == "__main__":
    main()
