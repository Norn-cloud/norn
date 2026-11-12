"""Replay frozen registry deltas from approved PRs #372 and #409 without network."""
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch


def _sibling(name, directory=None):
    """Load sibling module `name` from its source by explicit path: never through sys.path,
    never from a bytecode cache, never a same-named module loaded some other way (#513)."""
    import pathlib
    import sys
    import types

    path = (directory or pathlib.Path(__file__).resolve().parent) / f"{name}.py"
    loaded = sys.modules.get(name)
    if getattr(loaded, "__norn_sibling__", None) == str(path):
        return loaded
    module = types.ModuleType(name)
    module.__file__ = str(path)
    module.__norn_sibling__ = str(path)
    sys.modules[name] = module
    try:
        exec(compile(path.read_bytes(), str(path), "exec", dont_inherit=True), module.__dict__)
    except BaseException:
        del sys.modules[name]
        raise
    return module

coverage = _sibling("coverage_manifest")
migration = _sibling("migrate_registry_pr")


class MigrationTests(unittest.TestCase):
    def test_three_way_refusals(self):
        for base, current, proposed in (({"x": 1}, {}, {"x": 2}), ({}, {"x": None}, {"x": 1})):
            with self.assertRaises(ValueError):
                migration.merge(base, current, proposed)

    @patch.dict("os.environ", {"GIT_DIR": "/dev/null/repository", "GIT_WORK_TREE": "/dev/null", "GIT_INDEX_FILE": "/dev/null/index", "GIT_CONFIG_COUNT": "1", "GIT_CONFIG_KEY_0": "norn.migrationFixture", "GIT_CONFIG_VALUE_0": "retained"})
    def test_real_approved_prs(self):
        fixture = json.loads((Path(__file__).parent / "fixtures/registry-prs.json").read_text(encoding="utf-8"))
        self.assertEqual(fixture["base_sha"], "5e33eab0294012f8fb683fc2bf95c571c1bf4bcd")
        for case, reverse, native in [(case, r, n) for case in fixture["cases"] for r, n in ((False, False), (True, False), (False, True))]:
            with self.subTest(pr=case["pr"], reverse=reverse, native=native), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                def git(*args):
                    return migration.git(root, *args).stdout.decode().strip()
                def commit():
                    git("add", "-A")
                    git("-c", "user.name=fixture", "-c", "user.email=fixture@example.invalid", "commit", "-qm", "fixture")
                    return git("rev-parse", "HEAD")
                git("init", "-q")
                self.assertEqual(git("config", "norn.migrationFixture"), "retained")
                for name, content in fixture["base"].items():
                    path = root / name
                    path.parent.mkdir(parents=True, exist_ok=True)
                    path.write_bytes(content.encode("utf-8"))
                (root / migration.COVERAGE / "index.json").write_bytes(b"{}")
                base = commit()
                migration.git(root, "apply", "-", input=case["patch"].encode())
                head = commit()
                git("checkout", "-q", base)
                document = {"subsystems": [{"id": p.stem, **coverage.read_entry(p)} for p in sorted((root / migration.COVERAGE / "subsystems").glob("*.json"))]}
                document["subsystems"][0]["sourceFiles"].append("synthetic/main.rs")
                coverage.write_entries(document, root / migration.COVERAGE)
                old = json.loads((root / (migration.DIAGNOSTICS + ".json")).read_text(encoding="utf-8"))
                target = root / migration.DIAGNOSTICS
                extra = {**old["codes"][0], "code": "toy.validation.concurrent"}
                if not native:
                    target.mkdir()
                    for entry in old["codes"]:
                        (target / (entry["code"] + ".json")).write_bytes(migration.json_bytes(entry))
                    (target / (extra["code"] + ".json")).write_bytes(migration.json_bytes(extra))
                    (root / (migration.DIAGNOSTICS + ".json")).unlink()
                raw = (root / migration.RUST).read_bytes()
                helpers = "const fn entry(" + raw.decode().split("pub static REGISTRY: &[CodeEntry] = &[\n")[0].split("const fn entry(", 1)[1]
                if not native:
                    (root / migration.RUST).write_bytes(migration.rust_projection(root, base, raw, old, helpers))
                main = commit()
                git("checkout", "-q", main if reverse else head)
                migration.git(root, "-c", "user.name=fixture", "-c", "user.email=fixture@example.invalid", "merge", "--no-edit", head if reverse else main, check=False)
                subprocess.run([sys.executable, migration.__file__, base, head], cwd=root, check=True, capture_output=True)
                self.assertNotIn(b"<<<<<<< ", (root / migration.RUST).read_bytes())
                for path in migration.git(root, "diff", "--name-only", base, head, "--", migration.COVERAGE).stdout.decode().splitlines():
                    (root / path).unlink(missing_ok=True)
                result = coverage.load(root / migration.COVERAGE)
                proposed = [json.loads(git("show", head + ":" + migration.COVERAGE + "/subsystems/" + s["id"] + ".json")) for s in result["subsystems"]]
                for actual, expected in zip(result["subsystems"], proposed):
                    self.assertEqual(set(actual["sourceFiles"]), set(expected["sourceFiles"]) | ({"synthetic/main.rs"} if actual["id"] == document["subsystems"][0]["id"] else set()))
                    main_sources = next(s["sourceFiles"] for s in document["subsystems"] if s["id"] == actual["id"])
                    self.assertEqual(actual["sourceFiles"][:len(main_sources)], main_sources)
                expected_codes = json.loads(git("show", head + ":" + migration.DIAGNOSTICS + ".json"))["codes"]
                actual_codes = json.loads((root / (migration.DIAGNOSTICS + ".json")).read_text(encoding="utf-8"))["codes"] if native else [json.loads(p.read_text(encoding="utf-8")) for p in target.glob("*.json")]
                self.assertEqual(sorted(actual_codes, key=lambda x: x["code"]), sorted(expected_codes + ([] if native else [extra]), key=lambda x: x["code"]))
                before = {p.relative_to(root): p.read_bytes() for p in root.rglob("*") if p.is_file() and ".git" not in p.parts}
                self.assertEqual(migration.migrate(root, base, head), [])
                self.assertEqual(before, {p.relative_to(root): p.read_bytes() for p in root.rglob("*") if p.is_file() and ".git" not in p.parts})
                old_head = json.loads(git("show", head + ":" + migration.DIAGNOSTICS + ".json"))
                old_head["codes"][0]["message"] += " corrupted"
                with self.assertRaisesRegex(ValueError, "disagree"):
                    migration.rust_projection(root, head, migration.read(root, head, migration.RUST), old_head, helpers)
                if native:
                    continue
                path = target / (next(e["code"] for e in expected_codes if e not in old["codes"] and e != extra) + ".json")
                path.write_bytes(migration.json_bytes({**json.loads(path.read_text(encoding="utf-8")), "message": "concurrent edit"}))
                before = {p.relative_to(root): p.read_bytes() for p in root.rglob("*") if p.is_file() and ".git" not in p.parts}
                with self.assertRaises(ValueError):
                    migration.migrate(root, base, head)
                self.assertEqual(before, {p.relative_to(root): p.read_bytes() for p in root.rglob("*") if p.is_file() and ".git" not in p.parts})


if __name__ == "__main__":
    unittest.main()
