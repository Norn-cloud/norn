"""Replay frozen registry deltas from approved PRs #372 and #409 without network."""
import json
from pathlib import Path
import subprocess
import tempfile
import unittest

import coverage_manifest as coverage
import migrate_registry_pr as migration


class MigrationTests(unittest.TestCase):
    def test_three_way_refusals(self):
        for base, current, proposed in (({"x": 1}, {}, {"x": 2}), ({}, {"x": None}, {"x": 1})):
            with self.assertRaises(ValueError):
                migration.merge(base, current, proposed)

    def test_real_approved_prs(self):
        fixture = json.loads((Path(__file__).parent / "fixtures/registry-prs.json").read_text())
        for case in fixture["cases"]:
            with self.subTest(pr=case["pr"]), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                def git(*args):
                    return migration.git(root, *args).stdout.decode().strip()
                def commit():
                    git("add", "-A")
                    git("-c", "user.name=fixture", "-c", "user.email=fixture@example.invalid", "commit", "-qm", "fixture")
                    return git("rev-parse", "HEAD")
                git("init", "-q")
                for name, content in fixture["base"].items():
                    path = root / name
                    path.parent.mkdir(parents=True, exist_ok=True)
                    path.write_text(content)
                (root / migration.COVERAGE / "index.json").write_text("{}")
                base = commit()
                subprocess.run(["git", "-C", str(root), "apply", "-"], input=case["patch"].encode(), check=True)
                head = commit()
                git("checkout", "-q", base)
                document = coverage.load(root / migration.COVERAGE)
                document["subsystems"][0]["sourceFiles"].append("synthetic/main.rs")
                coverage.write_entries(document, root / migration.COVERAGE)
                old = json.loads((root / (migration.DIAGNOSTICS + ".json")).read_text())
                target = root / migration.DIAGNOSTICS
                target.mkdir()
                for entry in old["codes"]:
                    (target / (entry["code"] + ".json")).write_bytes(migration.json_bytes(entry))
                extra = {**old["codes"][0], "code": "toy.validation.concurrent"}
                (target / (extra["code"] + ".json")).write_bytes(migration.json_bytes(extra))
                (root / (migration.DIAGNOSTICS + ".json")).unlink()
                raw = (root / migration.RUST).read_bytes()
                helpers = "const fn entry(" + raw.decode().split("pub static REGISTRY: &[CodeEntry] = &[\n")[0].split("const fn entry(", 1)[1]
                (root / migration.RUST).write_bytes(migration.rust_projection(root, base, raw, old, helpers))
                main = commit()
                git("checkout", "-q", head)
                migration.git(root, "-c", "user.name=fixture", "-c", "user.email=fixture@example.invalid", "merge", "--no-edit", main, check=False)
                migration.migrate(root, base, head)
                self.assertNotIn(b"<<<<<<< ", (root / migration.RUST).read_bytes())
                for path in migration.git(root, "diff", "--name-only", base, head, "--", migration.COVERAGE).stdout.decode().splitlines():
                    (root / path).unlink(missing_ok=True)
                result = coverage.load(root / migration.COVERAGE)
                proposed = [json.loads(git("show", head + ":" + migration.COVERAGE + "/subsystems/" + s["id"] + ".json")) for s in result["subsystems"]]
                for actual, expected in zip(result["subsystems"], proposed):
                    self.assertEqual(set(actual["sourceFiles"]), set(expected["sourceFiles"]) | ({"synthetic/main.rs"} if actual["id"] == document["subsystems"][0]["id"] else set()))
                expected_codes = json.loads(git("show", head + ":" + migration.DIAGNOSTICS + ".json"))["codes"] + [extra]
                self.assertEqual(sorted([json.loads(p.read_text()) for p in target.glob("*.json")], key=lambda x: x["code"]), sorted(expected_codes, key=lambda x: x["code"]))
                before = {p.relative_to(root): p.read_bytes() for p in root.rglob("*") if p.is_file() and ".git" not in p.parts}
                self.assertEqual(migration.migrate(root, base, head), [])
                self.assertEqual(before, {p.relative_to(root): p.read_bytes() for p in root.rglob("*") if p.is_file() and ".git" not in p.parts})
                path = target / (next(e["code"] for e in expected_codes if e not in old["codes"] and e != extra) + ".json")
                path.write_bytes(migration.json_bytes({**json.loads(path.read_text()), "message": "concurrent edit"}))
                before = {p.relative_to(root): p.read_bytes() for p in root.rglob("*") if p.is_file() and ".git" not in p.parts}
                with self.assertRaises(ValueError):
                    migration.migrate(root, base, head)
                self.assertEqual(before, {p.relative_to(root): p.read_bytes() for p in root.rglob("*") if p.is_file() and ".git" not in p.parts})


if __name__ == "__main__":
    unittest.main()
