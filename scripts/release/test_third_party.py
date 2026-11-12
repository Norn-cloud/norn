#!/usr/bin/env python3
"""Tests for third_party.py over a synthetic workspace: no cargo, no network."""

from __future__ import annotations

import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import third_party  # noqa: E402

MIT_A = b"MIT License\n\nCopyright (c) A\n"
APACHE = b"Apache License\nVersion 2.0\n"


def package(repo: Path, name: str, where: str, source: str | None, licence: str | None, files: dict[str, bytes]) -> dict:
    directory = repo / where
    directory.mkdir(parents=True, exist_ok=True)
    (directory / "Cargo.toml").write_text(f'[package]\nname = "{name}"\n')
    for file, data in files.items():
        (directory / file).write_bytes(data)
    return {"id": f"{name}-id", "name": name, "version": "1.0.0", "source": source, "license": licence,
            "license_file": None, "repository": f"https://example.invalid/{name}",
            "manifest_path": str(directory / "Cargo.toml")}


def metadata(packages: list[dict], edges: dict[str, list[tuple[str, str | None]]], members: list[str]) -> dict:
    nodes = [{"id": p["id"], "deps": [{"pkg": f"{dep}-id", "dep_kinds": [{"kind": kind}]} for dep, kind in edges.get(p["name"], [])]}
             for p in packages]
    return {"packages": packages, "workspace_members": [f"{m}-id" for m in members], "resolve": {"nodes": nodes}}


class ThirdParty(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.repo = Path(self.tmp.name)
        (self.repo / "LICENSE").write_bytes(b"Norn's own licence: never reproduced as third-party\n")
        registry = "registry+https://github.com/rust-lang/crates.io-index"
        cache = self.repo.parent / f"{self.repo.name}-registry"
        self.addCleanup(lambda: __import__("shutil").rmtree(cache, ignore_errors=True))
        self.packages = [
            package(self.repo, "norn-checker-wasm", "crates/norn-checker-wasm", None, "Apache-2.0", {}),
            package(self.repo, "norn-cell-do", "crates/norn-cell-do", None, "Apache-2.0", {}),
            package(self.repo, "norn-ir", "crates/norn-ir", None, "Apache-2.0", {}),
            package(cache, "serde", "serde", registry, "MIT OR Apache-2.0", {"LICENSE-MIT": MIT_A, "LICENSE-APACHE": APACHE}),
            package(cache, "itoa", "itoa", registry, "MIT OR Apache-2.0", {"LICENSE-APACHE": APACHE}),
            package(cache, "only-dev", "only-dev", registry, "MIT", {"LICENSE": b"dev only\n"}),
            package(self.repo, "zen-expression", "vendor/zen/core/expression", None, "MIT", {}),
        ]
        (self.repo / "vendor/zen/LICENSE").write_bytes(b"MIT License\n\nCopyright (c) GoRules\n")
        self.edges = {
            "norn-checker-wasm": [("norn-ir", None), ("serde", None), ("only-dev", "dev")],
            "norn-cell-do": [("norn-ir", None), ("zen-expression", None), ("itoa", "build")],
            "norn-ir": [("serde", None), ("itoa", None)],
        }
        self.members = ["norn-checker-wasm", "norn-cell-do", "norn-ir", "zen-expression"]

    def tearDown(self) -> None:
        self.tmp.cleanup()

    def render(self) -> str:
        return third_party.render(metadata(self.packages, self.edges, self.members), self.repo).decode()

    def test_lists_the_normal_closure_and_reproduces_each_text_once(self) -> None:
        text = self.render()
        self.assertIn("Packages (3):", text)
        for line in ("itoa 1.0.0  [MIT OR Apache-2.0]", "serde 1.0.0  [MIT OR Apache-2.0]", "zen-expression 1.0.0  [MIT]  vendored at vendor/zen/core/expression"):
            self.assertIn(line, text)
        # Dev-only dependencies do not ship; first-party crates are not third-party.
        self.assertNotIn("only-dev", text)
        self.assertNotIn("norn-ir 1.0.0", text)
        self.assertNotIn("Norn's own licence", text)
        # One copy of the shared Apache text, naming both carriers.
        self.assertEqual(text.count("Apache License\nVersion 2.0"), 1)
        self.assertIn("LICENSE-APACHE — carried by: itoa 1.0.0, serde 1.0.0", text)
        # A vendored package inherits its vendor directory's licence.
        self.assertIn("Copyright (c) GoRules", text)

    def test_is_deterministic(self) -> None:
        first = self.render()
        self.packages.reverse()
        self.assertEqual(first, self.render())

    def test_refuses_a_package_without_any_licence(self) -> None:
        bare = package(self.repo.parent / f"{self.repo.name}-registry", "bare", "bare", "registry+x", None, {})
        self.packages.append(bare)
        self.edges["norn-ir"].append(("bare", None))
        with self.assertRaisesRegex(third_party.Refused, "bare 1.0.0: no licence expression and no licence file"):
            self.render()

    def test_refuses_a_path_package_outside_the_known_roots(self) -> None:
        self.packages.append(package(self.repo, "stray", "tools/stray", None, "MIT", {"LICENSE": b"x\n"}))
        self.edges["norn-ir"].append(("stray", None))
        with self.assertRaisesRegex(third_party.Refused, "stray: path package tools/stray"):
            self.render()

    def test_refuses_an_oversized_licence_file(self) -> None:
        self.packages.append(package(self.repo.parent / f"{self.repo.name}-registry", "huge", "huge", "registry+x", "MIT",
                                     {"LICENSE": b"x" * (third_party.MAX_LICENCE_BYTES + 1)}))
        self.edges["norn-ir"].append(("huge", None))
        with self.assertRaisesRegex(third_party.Refused, "too large"):
            self.render()

    def test_needs_each_shipped_root(self) -> None:
        self.members.remove("norn-cell-do")
        with self.assertRaisesRegex(third_party.Refused, "norn-cell-do: expected exactly one workspace package"):
            self.render()


if __name__ == "__main__":
    unittest.main()
