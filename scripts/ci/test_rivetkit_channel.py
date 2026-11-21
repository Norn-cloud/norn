#!/usr/bin/env python3
"""rivetkit_channel end to end over a scratch repository: each way round a RivetKit bump
without a storage version bump that Sol named on #1513 (round 4, S2; round 5, S5-S8), and
the legitimate changes beside them."""

import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


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


rc = _sibling("rivetkit_channel")

REGISTRY = rc.REGISTRY


def package(name, version, source=REGISTRY, checksum=None):
    lines = [f'name = "{name}"', f'version = "{version}"']
    if source is not None:
        lines.append(f'source = "{source}"')
    if checksum is not False:
        lines.append(f'checksum = "{checksum or (str(len(name)) + version.replace(".", "")).ljust(64, "a")[:64]}"')
    return "[[package]]\n" + "\n".join(lines) + "\n"


def lock(version, **kwargs):
    extra = kwargs.pop("extra", "")
    return "version = 4\n\n" + "\n".join(package(name, version, **kwargs) for name in rc.CRATES) + extra


def config(version, entries):
    body = "".join(f'    RivetkitRelease {{\n        rivetkit: "{name}",\n        from_storage: {storage},\n    }},\n'
                   for name, storage in entries)
    return (f'pub const RIVETKIT_VERSION: &str = "{version}";\n\n'
            f"pub const RIVETKIT_CHANNEL: &[RivetkitRelease] = &[\n{body}];\n")


def release(storage):
    return f"pub const STORAGE_VERSION: u32 = {storage};\n"


class Scenarios(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.git("init", "-q", "-b", "main")
        self.git("config", "user.email", "test@example.invalid")
        self.git("config", "user.name", "test")
        self.write("README", "scratch\n")
        self.commit("empty")

    def tearDown(self):
        self.temporary.cleanup()

    def git(self, *args):
        return subprocess.run(["git", *args], cwd=self.root, check=True, capture_output=True, text=True).stdout.strip()

    def write(self, path, text):
        target = self.root / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(text)

    def commit(self, message):
        self.git("add", "-A")
        self.git("commit", "-q", "--allow-empty", "-m", message)

    def tree(self, lock_text, version, entries, storage):
        self.write(rc.LOCK, lock_text)
        self.write(rc.CONFIG, config(version, entries))
        self.write(rc.RELEASE, release(storage))

    def base(self, lock_text, version, entries, storage):
        """Main at this tree; the head branch starts from it."""
        self.tree(lock_text, version, entries, storage)
        self.commit("base")
        self.git("switch", "-q", "-c", "head")

    def check(self, *args):
        result = subprocess.run([sys.executable, str(Path(rc.__file__)), *(args or ("--base", "main"))],
                                cwd=self.root, capture_output=True, text=True)
        return result.returncode, result.stdout + result.stderr

    def assert_tree_alone_passes(self):
        """The head meets every rule a tree can check against itself (the in-tree unit test's,
        all 57c8299 had), so only the base comparison refuses it."""
        head = rc.tree(lambda path: (self.root / path).read_text(), "head")
        rc.check_tree(head["lock"], head["version"], head["entries"], head["storage"], "head")

    def assert_passes(self):
        code, out = self.check()
        self.assertEqual(code, 0, out)

    def assert_refused(self, *expected):
        code, out = self.check()
        self.assertEqual(code, 1, out)
        for text in expected:
            self.assertIn(text, out)

    def test_first_introduction_checks_the_head_alone(self):
        self.git("switch", "-q", "-c", "head")
        self.tree(lock("3.1.1"), "3.1.1", [("3.1.1", 4)], 4)
        self.assert_passes()

    def test_an_unchanged_lock_passes(self):
        self.base(lock("3.1.1"), "3.1.1", [("3.1.1", 4)], 4)
        self.write("README", "other change\n")
        self.assert_passes()

    def test_a_bump_with_a_storage_bump_and_its_entry_passes(self):
        self.base(lock("3.1.1"), "3.1.1", [("3.1.1", 4)], 4)
        self.tree(lock("3.2.0"), "3.2.0", [("3.1.1", 4), ("3.2.0", 5)], 5)
        self.assert_passes()

    def test_a_norn_only_storage_bump_passes(self):
        self.base(lock("3.1.1"), "3.1.1", [("3.1.1", 4)], 4)
        self.tree(lock("3.1.1"), "3.1.1", [("3.1.1", 4)], 5)
        self.assert_passes()

    def test_retiring_the_oldest_release_passes(self):
        self.base(lock("3.2.0"), "3.2.0", [("3.1.1", 4), ("3.2.0", 5)], 5)
        self.tree(lock("3.2.0"), "3.2.0", [("3.2.0", 5)], 5)
        self.assert_passes()

    def test_a_bump_that_replaces_the_only_entry_at_the_same_storage_version_is_refused(self):
        # Bypass (a): both pins and the only channel entry moved to the new release, still at v4.
        self.base(lock("3.1.1"), "3.1.1", [("3.1.1", 4)], 4)
        self.tree(lock("3.2.0"), "3.2.0", [("3.2.0", 4)], 4)
        self.assert_tree_alone_passes()
        self.assert_refused("without a storage version bump", "STORAGE_VERSION is 4, the base's is 4")

    def test_a_bump_appended_at_a_storage_version_the_base_already_writes_is_refused(self):
        # Bypass (b): an earlier Norn-only bump took the base to v5; the release is appended at v5.
        self.base(lock("3.1.1"), "3.1.1", [("3.1.1", 4)], 5)
        self.tree(lock("3.2.0"), "3.2.0", [("3.1.1", 4), ("3.2.0", 5)], 5)
        self.assert_tree_alone_passes()
        self.assert_refused("without a storage version bump", "STORAGE_VERSION is 5, the base's is 5")

    def test_a_git_or_path_source_at_the_same_version_is_refused(self):
        # Bypass (c): the version string stays 3.1.1, the code comes from elsewhere.
        self.base(lock("3.1.1"), "3.1.1", [("3.1.1", 4)], 4)
        self.write(rc.LOCK, lock("3.1.1", source="git+https://github.com/example/rivet?rev=abc#abc", checksum=False))
        self.assert_refused("not the crates.io registry")
        self.write(rc.LOCK, lock("3.1.1", source=None, checksum=False))
        self.assert_refused("is locked from a path")

    def test_a_registry_entry_without_a_checksum_or_a_second_copy_is_refused(self):
        self.base(lock("3.1.1"), "3.1.1", [("3.1.1", 4)], 4)
        self.write(rc.LOCK, lock("3.1.1", checksum=False))
        self.assert_refused("has no checksum")
        self.write(rc.LOCK, lock("3.1.1", extra=package("rivetkit", "3.0.0")))
        self.assert_refused("2 `rivetkit` entries")

    def test_a_changed_checksum_at_the_same_version_needs_a_bump(self):
        self.base(lock("3.1.1"), "3.1.1", [("3.1.1", 4)], 4)
        self.write(rc.LOCK, lock("3.1.1", checksum="b" * 64))
        self.assert_refused("without a storage version bump")

    def test_a_bump_without_its_entry_or_with_a_wrong_one_is_refused(self):
        self.base(lock("3.1.1"), "3.1.1", [("3.1.1", 4)], 4)
        self.tree(lock("3.2.0"), "3.1.1", [("3.1.1", 4)], 5)
        self.assert_refused("is locked at 3.2.0, but RIVETKIT_VERSION is 3.1.1")
        self.tree(lock("3.2.0"), "3.2.0", [("3.1.1", 4), ("3.2.0", 4)], 5)
        self.assert_refused("does not start above")
        self.tree(lock("3.2.0"), "3.2.0", [("3.1.1", 4), ("3.1.9", 5), ("3.2.0", 6)], 6)
        self.assert_refused("append exactly one entry")

    def test_the_channel_is_append_only(self):
        self.base(lock("3.2.0"), "3.2.0", [("3.1.1", 4), ("3.2.0", 5)], 5)
        # A rewritten entry that is not the oldest.
        self.tree(lock("3.2.0"), "3.2.0", [("3.1.1", 3), ("3.2.0", 5)], 5)
        self.assert_refused("appends")
        # An entry appended without a RivetKit change.
        self.tree(lock("3.2.0"), "3.2.0", [("3.1.1", 4), ("3.1.5", 5), ("3.2.0", 6)], 6)
        self.assert_refused("appends")

    def test_a_downgrade_with_a_storage_bump_is_refused(self):
        # Sol round 5, S5: 3.1.1 re-pinned to 3.1.0 with storage 4 -> 5 read as a new release.
        self.base(lock("3.1.1"), "3.1.1", [("3.1.1", 4)], 4)
        self.tree(lock("3.1.0"), "3.1.0", [("3.1.1", 4), ("3.1.0", 5)], 5)
        self.assert_refused("RivetKit 3.1.0 at storage version 5 does not start above 3.1.1 at 4")
        # The same downgrade with the older release dropped: the head alone is consistent.
        self.tree(lock("3.1.0"), "3.1.0", [("3.1.0", 5)], 5)
        self.assert_tree_alone_passes()
        self.assert_refused("is downgraded from 3.1.1 to 3.1.0")
        # A pre-release of the pinned version ranks below it.
        self.tree(lock("3.1.1-rc.1", checksum="c" * 64), "3.1.1-rc.1", [("3.1.1-rc.1", 5)], 5)
        self.assert_refused("is downgraded from 3.1.1 to 3.1.1-rc.1")

    def test_an_equal_version_re_entry_is_refused(self):
        # The same version under a new checksum, entered again at a new storage version.
        self.base(lock("3.1.1"), "3.1.1", [("3.1.1", 4)], 4)
        self.tree(lock("3.1.1", checksum="b" * 64), "3.1.1", [("3.1.1", 4), ("3.1.1", 5)], 5)
        self.assert_refused("does not start above")
        # And with the base's entry dropped, so the head alone is consistent.
        self.tree(lock("3.1.1", checksum="b" * 64), "3.1.1", [("3.1.1", 5)], 5)
        self.assert_tree_alone_passes()
        self.assert_refused("not above the base's last entry 3.1.1")

    def test_an_unreadable_base_lockfile_is_refused_not_a_first_introduction(self):
        # Sol round 5, S7: the base lists the lockfile but its blob is missing (a blobless clone).
        self.base(lock("3.1.1"), "3.1.1", [("3.1.1", 4)], 4)
        self.tree(lock("3.2.0"), "3.2.0", [("3.2.0", 4)], 4)
        blob = self.git("rev-parse", f"main:{rc.LOCK}")
        (self.root / ".git" / "objects" / blob[:2] / blob[2:]).unlink()
        self.assert_refused(f"cannot read {rc.LOCK}")

    def test_a_missing_head_lockfile_fails_under_require_lock(self):
        # Sol round 5, S8.
        self.git("switch", "-q", "-c", "head")
        code, out = self.check("--base", "main")
        self.assertEqual(code, 0, out)
        code, out = self.check("--base", "main", "--require-lock")
        self.assertEqual(code, 1, out)
        self.assertIn("--require-lock needs it", out)

    def test_an_unreadable_channel_is_refused(self):
        self.base(lock("3.1.1"), "3.1.1", [("3.1.1", 4)], 4)
        self.write(rc.CONFIG, config("3.1.1", [("3.1.1", 4)]).replace("&[\n", "&[\n    OTHER,\n"))
        self.assert_refused("something other than RivetkitRelease literals")

    def test_an_unresolvable_base_fails_closed(self):
        self.git("switch", "-q", "-c", "head")
        self.tree(lock("3.1.1"), "3.1.1", [("3.1.1", 4)], 4)
        code, out = self.check("--base", "no-such-ref")
        self.assertEqual(code, 1, out)
        self.assertIn("failing closed", out)


if __name__ == "__main__":
    unittest.main()
