#!/usr/bin/env python3
"""record_new_files: enrollment and provenance follow the tree, idempotently, and refuse what needs a person."""

import hashlib
import json
import subprocess
import tempfile
import unittest
from pathlib import Path

import coverage_manifest
import record_new_files as tool
from proof_enrollment import discover_sources

COVERAGE = coverage_manifest.COVERAGE.relative_to(coverage_manifest.ROOT)
PROVENANCE = tool.PROVENANCE
AUTHORED = {"origin": "authored", "category": "code", "license": "Apache-2.0", "sources": [], "analyzed_sources": [], "parents": [],
            "rights_evidence": [{"path": "NOTICE", "sha256": "0" * 64}], "notices": [], "connector": None}


def sha(text: str) -> str:
    return hashlib.sha256(text.encode()).hexdigest()


class RecordNewFilesTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        subprocess.run(["git", "init", "-q", str(self.root)], check=True)
        (self.root / COVERAGE / "subsystems").mkdir(parents=True)
        self.write(f"{COVERAGE}/index.json", "{}\n")
        for subsystem in ("app", "lib"):
            self.write(f"{COVERAGE}/subsystems/{subsystem}/index.json", json.dumps({"sourceFiles": {"$entries": "sourceFiles"}}) + "\n")
        for index in [f"{COVERAGE}/index.json", *(f"{COVERAGE}/subsystems/{name}/index.json" for name in ("app", "lib"))]:
            self.recorded(index)
        self.add("app/src/main.ts", "export {}\n", "app")
        self.add("app/src/other.ts", "export {}\n", "app")
        self.add("lib/a.py", "pass\n", "lib")
        self.write(".github/ci/provenance/manifest.json", "{}\n")
        subprocess.run(["git", "-C", str(self.root), "add", "-A"], check=True)  # the tree as a checkout holds it

    def tearDown(self):
        self.temporary.cleanup()

    def write(self, path: str, text: str):
        (self.root / path).parent.mkdir(parents=True, exist_ok=True)
        (self.root / path).write_text(text, encoding="utf-8")

    def add(self, path: str, text: str, subsystem: str, origin: str = "authored"):
        """A file that is already enrolled and recorded."""
        self.write(path, text)
        entry = f"{COVERAGE}/subsystems/{subsystem}/sourceFiles/{coverage_manifest.entry_id(path)}.json"
        self.write(entry, coverage_manifest.entry_bytes({"order": 0, "value": path}).decode())
        self.recorded(path, origin)
        self.recorded(entry)

    def recorded(self, path: str, origin: str = "authored"):
        body = (self.root / path).read_text(encoding="utf-8")
        self.write(f"{PROVENANCE}/{path}.json", json.dumps({"sha256": sha(body), **AUTHORED, "origin": origin}, indent=2) + "\n")

    def run_tool(self, *args) -> int:
        return tool.main(["--root", str(self.root), *args])

    def record(self, path: str) -> dict:
        return json.loads((self.root / PROVENANCE / f"{path}.json").read_text(encoding="utf-8"))

    def enrolled(self) -> dict:
        return {source: subsystem for source, (subsystem, _, _) in tool._enrolled(self.root).items()}

    def test_a_consistent_tree_needs_nothing(self):
        self.assertEqual(self.run_tool("--check"), 0)

    def test_a_new_source_is_enrolled_with_its_siblings_and_recorded_like_them(self):
        self.write("app/src/grid/new.tsx", "export const x = 1\n")
        self.assertEqual(self.run_tool("--check"), 1)
        self.assertFalse((self.root / PROVENANCE / "app/src/grid/new.tsx.json").exists(), "--check wrote")
        self.assertEqual(self.run_tool(), 0)
        self.assertEqual(self.enrolled()["app/src/grid/new.tsx"], "app")
        self.assertEqual(discover_sources(self.root), set(self.enrolled()))
        record = self.record("app/src/grid/new.tsx")
        self.assertEqual(record["sha256"], sha("export const x = 1\n"))
        self.assertEqual({k: v for k, v in record.items() if k != "sha256"}, AUTHORED)
        entry = next(p for p in (self.root / COVERAGE).rglob("*.json") if "new.tsx" in p.read_text())
        self.assertEqual(self.record(entry.relative_to(self.root).as_posix())["sha256"], hashlib.sha256(entry.read_bytes()).hexdigest())
        coverage_manifest.load(self.root / COVERAGE)
        self.assertEqual(self.run_tool("--check"), 0, "a second run changed something")

    def test_an_edit_refreshes_only_the_hash(self):
        self.write("app/src/main.ts", "export const changed = true\n")
        self.assertEqual(self.run_tool(), 0)
        self.assertEqual(self.record("app/src/main.ts"), {"sha256": sha("export const changed = true\n"), **AUTHORED})

    def test_a_deleted_source_loses_its_entry_and_record(self):
        entry = f"{COVERAGE}/subsystems/app/sourceFiles/{coverage_manifest.entry_id('app/src/other.ts')}.json"
        (self.root / "app/src/other.ts").unlink()
        self.assertEqual(self.run_tool(), 0)
        self.assertFalse((self.root / entry).exists())
        self.assertFalse((self.root / PROVENANCE / f"{entry}.json").exists())
        self.assertNotIn("app/src/other.ts", self.enrolled())
        self.assertFalse((self.root / PROVENANCE / "app/src/other.ts.json").exists())
        self.assertEqual(self.run_tool("--check"), 0)

    def test_a_changed_vendored_file_is_refused_and_nothing_is_written(self):
        self.add("vendor/x.rs", "fn a() {}\n", "lib", origin="vendored")
        self.write("vendor/x.rs", "fn b() {}\n")
        self.write("app/src/also-new.ts", "export {}\n")
        self.assertEqual(self.run_tool(), 2)
        self.assertEqual(self.record("vendor/x.rs")["sha256"], sha("fn a() {}\n"))
        self.assertNotIn("app/src/also-new.ts", self.enrolled())

    def test_a_source_with_no_enrolled_sibling_anywhere_is_refused(self):
        for subsystem in ("app", "lib"):
            for entry in (self.root / COVERAGE / "subsystems" / subsystem / "sourceFiles").glob("*.json"):
                entry.unlink()
        self.write("elsewhere/new.py", "pass\n")
        self.assertEqual(self.run_tool(), 2)

    def test_a_source_enrolled_twice_is_refused_before_any_write(self):
        duplicate = f"{COVERAGE}/subsystems/lib/sourceFiles/{coverage_manifest.entry_id('app/src/main.ts')}.json"
        self.write(duplicate, coverage_manifest.entry_bytes({"order": 5, "value": "app/src/main.ts"}).decode())
        self.recorded(duplicate)
        self.write("app/src/new.ts", "export {}\n")
        self.assertEqual(self.run_tool("--check"), 2)
        self.assertEqual(self.run_tool(), 2)
        self.assertFalse((self.root / PROVENANCE / "app/src/new.ts.json").exists())

    def test_a_new_file_in_a_mixed_origin_directory_is_refused(self):
        self.add("lib/vendored.py", "pass\n", "lib", origin="vendored")  # a .py beside the authored lib/a.py
        self.write("lib/new.py", "pass\n")
        self.assertEqual(self.run_tool(), 2)
        self.assertFalse((self.root / PROVENANCE / "lib/new.py.json").exists())

    def test_a_derived_file_of_another_kind_does_not_block_its_neighbours(self):
        self.add("app/src/index.css", "body {}\n", "app", origin="derived")
        self.write("app/src/page.ts", "export {}\n")
        self.assertEqual(self.run_tool(), 0)
        self.assertEqual(self.record("app/src/page.ts")["origin"], "authored")

    def test_a_new_file_takes_the_category_of_siblings_with_its_suffix(self):
        self.write("app/src/notes.md", "# notes\n")
        self.write(f"{PROVENANCE}/app/src/notes.md.json", json.dumps({"sha256": sha("# notes\n"), **AUTHORED, "category": "documentation"}, indent=2) + "\n")
        self.write("app/src/more.md", "# more\n")
        self.write("app/src/new.ts", "export {}\n")
        self.assertEqual(self.run_tool(), 0)
        self.assertEqual(self.record("app/src/more.md")["category"], "documentation")
        self.assertEqual(self.record("app/src/new.ts")["category"], "code")

    def test_the_provenance_directory_itself_gets_no_records(self):
        self.assertFalse((self.root / PROVENANCE / ".github/ci/provenance/manifest.json.json").exists())
        self.assertEqual(self.run_tool("--check"), 0)


if __name__ == "__main__":
    unittest.main()
