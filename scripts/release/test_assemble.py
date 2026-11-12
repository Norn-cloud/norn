#!/usr/bin/env python3
"""assemble.py: a release is built from one commit, and `verify` refuses
anything it cannot vouch for. Each canary alters one thing in a good release."""

from __future__ import annotations

import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import assemble  # noqa: E402

SCRIPT = Path(assemble.__file__)
TOOL = assemble.TOOL


class Release(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.repo = self.root / "repo"
        self.repo.mkdir()
        self.git("init", "-q", "-b", "main")
        self.git("config", "user.email", "test@example.invalid")
        self.git("config", "user.name", "test")
        self.source("LICENSE", "Apache-2.0\n")
        self.source("NOTICE", "Norn\n")
        self.source(f"{TOOL}/package.json", json.dumps({
            "name": "@norn/authoring", "version": "0.1.0-alpha.3", "private": True, "license": "Apache-2.0",
            "types": "./profile/norn-authoring.d.ts", "exports": {".": {"types": "./profile/norn-authoring.d.ts"}},
            "bin": {"norn": "./src/cli.ts"}, "dependencies": {"effect": "4.0.0-rc.117", "typescript": "6.0.3"}}))
        self.source(f"{TOOL}/src/cli.ts", "#!/usr/bin/env bun\n")
        self.source(f"{TOOL}/src/builder/index.ts", "export {}\n")
        self.source(f"{TOOL}/profile/norn-authoring.d.ts", 'declare module "@norn/authoring" {}\n')
        self.source(f"{TOOL}/schema/norn-project.schema.json", "{}\n")
        self.source(f"{TOOL}/templates/starter/src/product.ts", "export {}\n")
        self.source(f"{TOOL}/tsconfig.authoring.json", "{}\n")
        self.source(f"{TOOL}/test/x.test.ts", "never shipped\n")
        self.source(assemble.LIMITS[1], "{}\n")
        self.git("update-index", "--add", "--chmod=+x", f"{TOOL}/src/cli.ts")
        self.git("commit", "-q", "-m", "release")
        self.commit = self.git("rev-parse", "HEAD")
        self.wasm = self.root / "wasm"
        self.wasm.mkdir()
        for name in assemble.CHECKER:
            (self.wasm / name).write_bytes(b"\0asm" + name.encode())
        self.hosts = self.root / "hosts"
        for platform, facts in assemble.PLATFORMS.items():
            (self.hosts / facts["target"]).mkdir(parents=True)
            (self.hosts / facts["target"] / "norn-dev-host").write_bytes(b"\x7fELF" + platform.encode())
        self.licenses = self.root / "THIRD-PARTY-LICENSES"
        self.licenses.write_text("licenses\n")
        self.out = self.root / "out"

    def tearDown(self):
        self.temporary.cleanup()

    def git(self, *args):
        return subprocess.run(["git", *args], cwd=self.repo, check=True, capture_output=True, text=True).stdout.strip()

    def source(self, path, text):
        target = self.repo / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(text)
        self.git("add", path)

    def run_script(self, command, *extra):
        return subprocess.run([sys.executable, str(SCRIPT), command, "--commit", self.commit, "--repo", str(self.repo),
                               "--out", str(self.out), *extra], capture_output=True, text=True)

    def build(self, channel="tarball"):
        extra = ["--wasm", str(self.wasm), "--hosts", str(self.hosts), "--third-party", str(self.licenses),
                 "--channel", channel, "--toolchain", "1.98.0"]
        if channel == "tarball":
            extra += ["--base-url", "https://example.invalid/releases/v0.1.0-alpha.3"]
        result = self.run_script("build", *extra)
        self.assertEqual(result.returncode, 0, result.stderr)

    def refused(self, why):
        result = self.run_script("verify")
        self.assertEqual(result.returncode, 1, result.stdout)
        self.assertIn(why, result.stderr)

    def package(self, name):
        return self.out / "packages" / assemble.package_dir(name)

    def test_a_release_is_the_commit_and_the_listed_built_files(self):
        self.build()
        cli = self.package("@norn/cli")
        self.assertEqual((cli / "src/cli.ts").read_text(), "#!/usr/bin/env bun\n")
        self.assertTrue((cli / "src/cli.ts").stat().st_mode & 0o100)
        self.assertFalse((cli / "test").exists())
        self.assertEqual((cli / assemble.LIMITS[0]).read_text(), "{}\n")
        manifest = json.loads((cli / "package.json").read_text())
        self.assertEqual((manifest["name"], manifest["version"]), ("@norn/cli", "0.1.0-alpha.3"))
        self.assertNotIn("private", manifest)
        self.assertNotIn("optionalDependencies", manifest)
        info = json.loads((cli / "generated/build.json").read_text())
        self.assertEqual((info["version"], info["commit"], info["toolchain"]), ("0.1.0-alpha.3", self.commit, "1.98.0"))
        self.assertEqual(info["packages"]["@norn/cli"], "https://example.invalid/releases/v0.1.0-alpha.3/norn-cli-0.1.0-alpha.3.tgz")
        self.assertEqual(sorted(info["hosts"]), sorted(assemble.PLATFORMS))
        host = self.package("@norn/dev-host-linux-x64")
        self.assertEqual(json.loads((host / "package.json").read_text())["cpu"], ["x64"])
        self.assertTrue((host / "bin/norn-dev-host").stat().st_mode & 0o100)
        released = json.loads((self.out / "release/manifest.json").read_text())
        self.assertEqual(sorted(released["files"]), sorted([
            "norn-authoring-0.1.0-alpha.3.tgz", "norn-cli-0.1.0-alpha.3.tgz", "norn-dev-host-linux-arm64",
            "norn-dev-host-linux-arm64-0.1.0-alpha.3.tgz", "norn-dev-host-linux-x64", "norn-dev-host-linux-x64-0.1.0-alpha.3.tgz"]))
        self.assertEqual(info["hosts"]["linux-x64"]["sha256"], released["files"]["norn-dev-host-linux-x64"]["sha256"])
        # Verifying again, alone, passes; and building twice gives the same bytes.
        self.assertEqual(self.run_script("verify").returncode, 0)
        first = (self.out / "release/SHA256SUMS").read_text()
        self.build()
        self.assertEqual((self.out / "release/SHA256SUMS").read_text(), first)

    def test_the_registry_channel_names_versions_and_platform_packages(self):
        self.build("registry")
        manifest = json.loads((self.package("@norn/cli") / "package.json").read_text())
        self.assertEqual(manifest["optionalDependencies"], {"@norn/dev-host-linux-arm64": "0.1.0-alpha.3", "@norn/dev-host-linux-x64": "0.1.0-alpha.3"})
        info = json.loads((self.package("@norn/cli") / "generated/build.json").read_text())
        self.assertEqual(info["packages"], {"@norn/cli": "0.1.0-alpha.3", "@norn/authoring": "0.1.0-alpha.3"})
        self.assertEqual(info["hosts"], {})

    def repack(self, name):
        directory = self.package(name)
        (self.out / "release" / assemble.tarball_name(name, "0.1.0-alpha.3")).write_bytes(assemble.pack(directory))

    def test_canary_an_extra_untracked_file_in_a_package(self):
        self.build()
        (self.package("@norn/cli") / "src/extra.ts").write_text("not in the commit\n")
        self.repack("@norn/cli")
        self.refused("src/extra.ts is neither a tracked source file nor a listed built file")

    def test_canary_a_tracked_file_altered_by_one_byte(self):
        self.build()
        target = self.package("@norn/authoring") / "src/builder/index.ts"
        target.write_bytes(target.read_bytes()[:-1] + b"!")
        self.repack("@norn/authoring")
        self.refused(f"src/builder/index.ts is not byte-identical to {TOOL}/src/builder/index.ts")

    def test_canary_an_unlisted_built_file(self):
        self.build()
        (self.package("@norn/cli") / "checker/extra.wasm").write_bytes(b"\0asm")
        self.repack("@norn/cli")
        self.refused("checker/extra.wasm is neither a tracked source file nor a listed built file")
        (self.package("@norn/cli") / "checker/extra.wasm").unlink()
        (self.package("@norn/cli") / "checker/norn_checker_wasm_bg.wasm").unlink()
        self.repack("@norn/cli")
        self.refused("checker/norn_checker_wasm_bg.wasm is missing")

    def test_canary_a_missing_manifest_entry(self):
        self.build()
        path = self.out / "release/manifest.json"
        manifest = json.loads(path.read_text())
        del manifest["files"]["norn-dev-host-linux-arm64"]
        path.write_text(json.dumps(manifest))
        self.refused("must list exactly")

    def test_canary_a_release_file_that_differs_from_its_recorded_hash(self):
        self.build()
        (self.out / "release/norn-dev-host-linux-x64").write_bytes(b"\x7fELF another host")
        self.refused("release/norn-dev-host-linux-x64 does not match its manifest or SHA256SUMS entry")

    def test_canary_a_file_in_the_release_that_no_manifest_lists(self):
        self.build()
        (self.out / "release/unlisted.tgz").write_bytes(b"")
        self.refused("must list exactly")

    def test_a_working_tree_edit_never_reaches_a_package(self):
        (self.repo / TOOL / "src/cli.ts").write_text("edited after the commit\n")
        self.build()
        self.assertEqual((self.package("@norn/cli") / "src/cli.ts").read_text(), "#!/usr/bin/env bun\n")

    def test_builds_refuse_what_they_cannot_name(self):
        (self.hosts / "x86_64-pc-windows-msvc").mkdir()
        result = self.run_script("build", "--wasm", str(self.wasm), "--hosts", str(self.hosts), "--third-party", str(self.licenses),
                                 "--channel", "registry", "--toolchain", "1.98.0")
        self.assertEqual(result.returncode, 1)
        self.assertIn("known targets only", result.stderr)
        (self.hosts / "x86_64-pc-windows-msvc").rmdir()
        result = self.run_script("build", "--wasm", str(self.wasm), "--hosts", str(self.hosts), "--third-party", str(self.licenses),
                                 "--channel", "tarball", "--toolchain", "1.98.0")
        self.assertEqual(result.returncode, 1)
        self.assertIn("--base-url", result.stderr)


if __name__ == "__main__":
    unittest.main()
