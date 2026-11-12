#!/usr/bin/env python3
"""Assemble Norn's release packages from one commit, and prove what is in them.

    assemble.py build  --commit REV --channel registry|tarball [--base-url URL] --toolchain X.Y.Z \
                       --platforms P[,P...] --wasm DIR --hosts DIR --third-party FILE --out DIR
    assemble.py verify --commit REV --channel registry|tarball [--base-url URL] --toolchain X.Y.Z \
                       --platforms P[,P...] --out DIR

`build` writes, under `--out`:

    packages/<name>/...   the expanded npm packages (@norn/cli, @norn/authoring,
                          one @norn/dev-host-<platform> per prebuilt host)
    release/              what a release publishes: one .tgz per package, each
                          host binary on its own, manifest.json and SHA256SUMS

and then runs `verify` on it. `verify` fails closed. It holds the provenance
model for release artifacts (the guard in `norn-skeleton-guard` covers the
tagged source tree; this script covers what is built from it):

- The release's facts come from the commit and the caller, never from the
  files being checked: the version is `tools/norn-ts/package.json` at
  `--commit`; channel, base URL, toolchain and the platforms whose hosts ship
  are arguments (the release workflow passes its own). `manifest.json` must
  state exactly these, and the packages must be exactly the CLI, the
  authoring profile and one host package per platform.
- Every file in every package is either a *source* file or a *built* file.
- A source file must be byte-identical to a tracked file at `--commit`, read
  from git, at the repository path this script maps it to. Nothing from the
  working tree is trusted. Symlinks are refused.
- A built file must be on the closed list for its package (`BUILT`). A built
  file that is not listed, a listed one that is missing, and any other extra
  file each fail. The built files that are derived, each `package.json` and
  the CLI's `generated/build.json`, must be byte-identical to a rebuild from
  the commit, the inputs and the host digests.
- Each release tarball must hold exactly its package directory (compared as
  the decompressed tar stream).
- `release/manifest.json` records the sha256 and size of every published file,
  and `SHA256SUMS` repeats them.

The compiled and generated files are inputs whose contents this script does not
check: the checker WebAssembly module and its JavaScript loader
(`norn_checker_wasm.js`, which the CLI executes), the dev hosts, and the
third-party licence text. It proves where they sit and that every copy agrees,
not how they were made. `.github/workflows/release.yml` builds them from the
same commit with `scripts/release/build.sh`; when it publishes, it attests them
to that run and commit. This script never downloads anything.
"""

from __future__ import annotations

import argparse
import gzip
import hashlib
import io
import json
import os
import re
import shutil
import subprocess
import sys
import tarfile
import zlib
from pathlib import Path

TOOL = "tools/norn-ts"
# The alpha requires Bun 1.3.x (D97): a later minor version is not admitted until it is tested.
BUN_RANGE = ">=1.3.0 <1.4.0"
# A release address: https, or http on this machine only (a local dry run). The CLI applies the same rule.
RELEASE_URL = re.compile(r"(https://\S+|http://(127\.0\.0\.1|localhost)(:\d+)?(/\S*)?)(?<!/)")
VERSION = re.compile(r"0\.\d+\.\d+-alpha\.\d+")
# Prebuilt dev hosts: the platform key the CLI computes (`<process.platform>-<process.arch>`),
# with the npm `os`/`cpu` of its package and the Rust target it is built for.
PLATFORMS = {
    "linux-x64": {"os": "linux", "cpu": "x64", "target": "x86_64-unknown-linux-musl"},
    "linux-arm64": {"os": "linux", "cpu": "arm64", "target": "aarch64-unknown-linux-musl"},
}
# What `crates/norn-checker-wasm/scripts/build-pkg.sh` writes that the CLI loads or types against.
CHECKER = ("norn_checker_wasm.js", "norn_checker_wasm.d.ts", "norn_checker_wasm_bg.wasm", "norn_checker_wasm_bg.wasm.d.ts")
# The limits profile the CLI reads: in a package it sits beside the authoring profile.
LIMITS = ("profile/norn.limits.v1.canonical.json", "crates/norn-ir/tests/golden/norn.limits.v1.canonical.json")
SOURCE_DIRS = {
    "@norn/cli": ("src", "profile", "schema", "templates", "generated"),
    "@norn/authoring": ("src", "profile"),
}
SOURCE_FILES = ("tsconfig.authoring.json",)
ROOT_FILES = ("LICENSE", "NOTICE")
# The closed list of built files per package. Anything else that is not a source file fails.
BUILT = {
    "@norn/cli": ("package.json", "THIRD-PARTY-LICENSES", "generated/build.json", *(f"checker/{name}" for name in CHECKER)),
    "@norn/authoring": ("package.json",),
    "@norn/dev-host": ("package.json", "THIRD-PARTY-LICENSES", "bin/norn-dev-host"),
}


class Refused(Exception):
    """The release is not what this script can vouch for."""


def git(*args: str, cwd: Path | None = None) -> bytes:
    return subprocess.run(["git", *args], check=True, capture_output=True, cwd=cwd).stdout


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


class Commit:
    """The tracked tree at one commit, read from git objects only."""

    def __init__(self, repo: Path, rev: str):
        self.repo = repo
        self.sha = git("rev-parse", "--verify", f"{rev}^{{commit}}", cwd=repo).decode().strip()
        self.modes: dict[str, str] = {}
        for line in git("ls-tree", "-r", "-z", self.sha, cwd=repo).decode().split("\0"):
            if line:
                meta, path = line.split("\t", 1)
                mode, kind, _ = meta.split(" ")
                if kind == "blob" and mode in ("100644", "100755"):
                    self.modes[path] = mode

    def read(self, path: str) -> bytes:
        if path not in self.modes:
            raise Refused(f"{path} is not a tracked file at {self.sha[:12]}")
        return git("cat-file", "blob", f"{self.sha}:{path}", cwd=self.repo)


def package_dir(name: str) -> str:
    return name.replace("@norn/", "norn-")


def host_package(platform: str) -> str:
    return f"@norn/dev-host-{platform}"


def kind_of(name: str) -> str:
    return "@norn/dev-host" if name.startswith("@norn/dev-host-") else name


def sources(commit: Commit, name: str) -> dict[str, str]:
    """Package path -> repository path of every source file of a package."""
    mapping = {path: path for path in ROOT_FILES if path in commit.modes}
    if kind_of(name) == "@norn/dev-host":
        return mapping
    for directory in SOURCE_DIRS[name]:
        prefix = f"{TOOL}/{directory}/"
        for path in commit.modes:
            if path.startswith(prefix):
                mapping[path[len(TOOL) + 1:]] = path
    for path in SOURCE_FILES:
        if name == "@norn/cli" or path == "tsconfig.authoring.json":
            mapping[path] = f"{TOOL}/{path}"
    if name == "@norn/cli":
        mapping[LIMITS[0]] = LIMITS[1]
    return mapping


def manifest_of(commit: Commit, name: str, version: str, channel: str, platforms: list[str]) -> dict:
    tracked = json.loads(commit.read(f"{TOOL}/package.json"))
    # The full URL form: npm provenance checks it against the publishing repository.
    common = {"version": version, "license": tracked["license"],
              "repository": {"type": "git", "url": "git+https://github.com/Norn-cloud/norn.git"}}
    if kind_of(name) == "@norn/dev-host":
        platform = name.removeprefix("@norn/dev-host-")
        return {"name": name, **common, "description": f"Norn dev host (norn-dev-host) for {platform}",
                "os": [PLATFORMS[platform]["os"]], "cpu": [PLATFORMS[platform]["cpu"]],
                "files": ["bin/norn-dev-host", "THIRD-PARTY-LICENSES", "LICENSE", "NOTICE"]}
    package = {"name": name, **common, "type": "module", "types": tracked["types"], "exports": tracked["exports"],
               "dependencies": tracked["dependencies"], "engines": {"bun": BUN_RANGE}}
    if name == "@norn/authoring":
        # Export only what this package ships.
        shipped = set(sources(commit, name))
        exports = {key: value for key, value in tracked["exports"].items()
                   if key == "." or value.removeprefix("./") in shipped}
        return {**package, "exports": exports, "description": "Norn authoring profile: the types `@norn/authoring` resolves to"}
    package |= {"description": "Norn CLI: init, dev, check and explain for a Norn project", "bin": tracked["bin"]}
    if channel == "registry" and platforms:
        package["optionalDependencies"] = {host_package(platform): version for platform in platforms}
    return package


def write(path: Path, data: bytes, mode: str = "100644") -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(data)
    path.chmod(0o755 if mode == "100755" else 0o644)


def pack(directory: Path) -> bytes:
    """An npm tarball of a package directory: `package/` prefix, fixed order, owner and times."""
    raw = io.BytesIO()
    with tarfile.open(fileobj=raw, mode="w", format=tarfile.USTAR_FORMAT) as archive:
        for path in sorted(p for p in directory.rglob("*") if p.is_file()):
            info = tarfile.TarInfo(f"package/{path.relative_to(directory).as_posix()}")
            info.size = path.stat().st_size
            info.mode = 0o755 if path.stat().st_mode & 0o100 else 0o644
            info.mtime = 0
            with path.open("rb") as handle:
                archive.addfile(info, handle)
    packed = io.BytesIO()
    with gzip.GzipFile(fileobj=packed, mode="wb", mtime=0, compresslevel=9) as stream:
        stream.write(raw.getvalue())
    return packed.getvalue()


def tarball_name(name: str, version: str) -> str:
    return f"{package_dir(name)}-{version}.tgz"


def package_json(commit: Commit, name: str, version: str, channel: str, platforms: list[str]) -> bytes:
    return (json.dumps(manifest_of(commit, name, version, channel, platforms), indent=2) + "\n").encode()


def build_info_of(commit: Commit, version: str, channel: str, base_url: str, toolchain: str,
                  digests: dict[str, tuple[str, int]]) -> bytes:
    """`generated/build.json`, wholly derived from the release's facts and its host digests.
    Hosts carry their digest on both channels: the CLI checks an installed host
    against it. Only the tarball channel has an address to fetch one from."""
    spec = (lambda name: version) if channel == "registry" else (lambda name: f"{base_url}/{tarball_name(name, version)}")
    hosts = {platform: ({"url": f"{base_url}/norn-dev-host-{platform}"} if channel == "tarball" else {})
             | {"sha256": digest, "size": size} for platform, (digest, size) in sorted(digests.items())}
    info = {"version": version, "commit": commit.sha, "toolchain": toolchain,
            "packages": {name: spec(name) for name in ("@norn/cli", "@norn/authoring")}, "hosts": hosts}
    return (json.dumps(info, indent=2) + "\n").encode()


def unpacked(tarball: bytes, name: str = "tarball") -> bytes:
    """The tar stream inside an npm tarball: compared instead of gzip bytes,
    which differ between zlib builds for the same content."""
    try:
        return gzip.decompress(tarball)
    except (OSError, EOFError, zlib.error) as error:
        raise Refused(f"{name} is not a gzip stream: {error}") from error


def release_inputs(commit: Commit, channel: str, base_url: str, toolchain: str, platforms: list[str]) -> str:
    """The release's version, from the commit, once the caller's inputs are ones this script builds.
    `verify` takes the same inputs from its caller: nothing in the files it checks decides them."""
    if not platforms or len(set(platforms)) != len(platforms) or not set(platforms) <= set(PLATFORMS):
        raise Refused(f"--platforms {','.join(platforms) or '(none)'} must name distinct prebuilt platforms from {', '.join(sorted(PLATFORMS))}")
    version = json.loads(commit.read(f"{TOOL}/package.json")).get("version", "")
    if not VERSION.fullmatch(version):
        raise Refused(f"{TOOL}/package.json version {version!r} is not 0.x.y-alpha.N")
    if channel not in ("registry", "tarball"):
        raise Refused(f"--channel {channel!r} is not registry or tarball")
    if channel == "tarball" and not RELEASE_URL.fullmatch(base_url):
        raise Refused("--channel tarball needs an https --base-url (or http on 127.0.0.1 or localhost) without a trailing slash")
    if channel == "registry" and base_url:
        raise Refused("--channel registry takes no --base-url: npm installs the packages")
    if not re.fullmatch(r"\d+\.\d+\.\d+", toolchain):
        raise Refused(f"--toolchain {toolchain!r} is not an exact Rust version")
    return version


def build(commit: Commit, out: Path, wasm: Path, hosts: Path, third_party: Path, channel: str, base_url: str,
          toolchain: str, platforms: list[str]) -> None:
    version = release_inputs(commit, channel, base_url, toolchain, platforms)
    platforms = sorted(platforms)
    wanted = {PLATFORMS[p]["target"] for p in platforms}
    present = {p.name for p in hosts.iterdir() if p.is_dir()}
    missing = sorted(t for t in wanted if not (hosts / t / "norn-dev-host").is_file())
    if present != wanted or missing:
        raise Refused(f"--hosts must hold <target>/norn-dev-host for exactly {', '.join(sorted(wanted))}; "
                      f"found {', '.join(sorted(present)) or 'none'}{f' (missing the binary for {missing})' if missing else ''}")
    if out.exists():
        shutil.rmtree(out)
    release = out / "release"
    release.mkdir(parents=True)
    licenses = third_party.read_bytes()
    host_bytes = {platform: (hosts / PLATFORMS[platform]["target"] / "norn-dev-host").read_bytes() for platform in platforms}
    names = ["@norn/cli", "@norn/authoring", *(host_package(platform) for platform in platforms)]
    digests = {platform: (sha256(data), len(data)) for platform, data in host_bytes.items()}
    build_info = build_info_of(commit, version, channel, base_url, toolchain, digests)
    for name in names:
        directory = out / "packages" / package_dir(name)
        for target, source in sources(commit, name).items():
            write(directory / target, commit.read(source), commit.modes[source])
        write(directory / "package.json", package_json(commit, name, version, channel, platforms))
        if name == "@norn/cli":
            write(directory / "THIRD-PARTY-LICENSES", licenses)
            write(directory / "generated/build.json", build_info)
            for member in CHECKER:
                write(directory / "checker" / member, (wasm / member).read_bytes())
        elif kind_of(name) == "@norn/dev-host":
            platform = name.removeprefix("@norn/dev-host-")
            write(directory / "THIRD-PARTY-LICENSES", licenses)
            write(directory / "bin/norn-dev-host", host_bytes[platform], "100755")
        write(release / tarball_name(name, version), pack(directory))
    for platform, data in host_bytes.items():
        write(release / f"norn-dev-host-{platform}", data, "100755")
    files = {path.name: {"sha256": sha256(path.read_bytes()), "size": path.stat().st_size} for path in sorted(release.iterdir())}
    write(release / "manifest.json", (json.dumps({"format": "norn-release/1", "version": version, "commit": commit.sha,
                                                    "channel": channel, "baseUrl": base_url, "toolchain": toolchain,
                                                    "files": files}, indent=2) + "\n").encode())
    write(release / "SHA256SUMS", "".join(f"{entry['sha256']}  {name}\n" for name, entry in files.items()).encode())


def verify(commit: Commit, out: Path, channel: str, base_url: str, toolchain: str, platforms: list[str]) -> None:
    """Check `out` against what this commit and the caller's release inputs produce.
    The version comes from the commit; channel, base URL and toolchain come from the
    caller (the release workflow passes its own), and so do the platforms whose hosts
    must ship. The manifest must agree with them: it is checked, never trusted, since
    nothing signs it."""
    version = release_inputs(commit, channel, base_url, toolchain, platforms)
    release = out / "release"
    try:
        manifest = json.loads((release / "manifest.json").read_text())
    except (OSError, ValueError) as error:
        raise Refused(f"release/manifest.json is unreadable: {error}") from error
    if not isinstance(manifest, dict):
        raise Refused(f"release/manifest.json is a JSON {type(manifest).__name__}, not an object")
    stated = {key: manifest.get(key) for key in ("format", "version", "commit", "channel", "baseUrl", "toolchain")}
    wanted = {"format": "norn-release/1", "version": version, "commit": commit.sha, "channel": channel,
              "baseUrl": base_url, "toolchain": toolchain}
    if stated != wanted:
        differ = sorted(key for key in wanted if stated[key] != wanted[key])
        raise Refused(f"release/manifest.json is not for this release: {', '.join(differ)} differ from the commit and the given inputs")
    for path in sorted((out / "packages").rglob("*")):
        if path.is_symlink():
            raise Refused(f"{path.relative_to(out)} is a symlink")
    packages = sorted(path.name for path in (out / "packages").iterdir())
    names = ["@norn/cli", "@norn/authoring", *(host_package(p) for p in sorted(platforms))]
    if sorted(package_dir(name) for name in names) != packages:
        raise Refused(f"packages/ holds {packages}, not the CLI, the authoring profile and the hosts for {', '.join(sorted(platforms))}")
    for name in names:
        directory = out / "packages" / package_dir(name)
        expected = sources(commit, name)
        built = set(BUILT[kind_of(name)])
        found = {path.relative_to(directory).as_posix() for path in directory.rglob("*") if not path.is_dir()}
        for path in sorted(found - set(expected) - built):
            raise Refused(f"{name}: {path} is neither a tracked source file nor a listed built file")
        for path in sorted((set(expected) | built) - found):
            raise Refused(f"{name}: {path} is missing")
        for path, source in sorted(expected.items()):
            file = directory / path
            if file.is_symlink() or file.read_bytes() != commit.read(source):
                raise Refused(f"{name}: {path} is not byte-identical to {source} at {commit.sha[:12]}")
        platforms = sorted(n.removeprefix("@norn/dev-host-") for n in names if kind_of(n) == "@norn/dev-host")
        if (directory / "package.json").read_bytes() != package_json(commit, name, version, channel, platforms):
            raise Refused(f"{name}: package.json is not the one this commit and release produce")
        tarball = release / tarball_name(name, version)
        if not tarball.is_file() or unpacked(tarball.read_bytes(), f"release/{tarball.name}") != unpacked(pack(directory)):
            raise Refused(f"{name}: release/{tarball.name} is not the tarball of packages/{package_dir(name)}")
    hosts = {name.removeprefix("@norn/dev-host-") for name in names if kind_of(name) == "@norn/dev-host"}
    expected_files = {tarball_name(name, version) for name in names} | {f"norn-dev-host-{platform}" for platform in hosts}
    files = manifest.get("files", {})
    on_disk = {path.name for path in release.iterdir()} - {"manifest.json", "SHA256SUMS"}
    if set(files) != expected_files or on_disk != expected_files:
        raise Refused(f"release/ and its manifest must list exactly {sorted(expected_files)}; "
                      f"manifest has {sorted(files)}, release/ has {sorted(on_disk)}")
    sums = {}
    for line in (release / "SHA256SUMS").read_text().splitlines():
        digest, _, file = line.partition("  ")
        sums[file] = digest
    for file, entry in files.items():
        data = (release / file).read_bytes()
        if entry != {"sha256": sha256(data), "size": len(data)} or sums.get(file) != entry["sha256"]:
            raise Refused(f"release/{file} does not match its manifest or SHA256SUMS entry")
    if set(sums) != expected_files:
        raise Refused("SHA256SUMS does not list exactly the release files")
    for platform in hosts:
        binary = (out / "packages" / package_dir(host_package(platform)) / "bin/norn-dev-host").read_bytes()
        if binary != (release / f"norn-dev-host-{platform}").read_bytes():
            raise Refused(f"the {platform} host in its package differs from release/norn-dev-host-{platform}")
    # The build manifest is derived entirely from the release's facts and its host digests: rebuild it and compare bytes.
    digests = {platform: (files[f"norn-dev-host-{platform}"]["sha256"], files[f"norn-dev-host-{platform}"]["size"])
               for platform in hosts}
    built = (out / "packages" / package_dir("@norn/cli") / "generated/build.json").read_bytes()
    if built != build_info_of(commit, version, channel, base_url, toolchain, digests):
        raise Refused("generated/build.json is not the one this commit and release produce")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    commands = parser.add_subparsers(dest="command", required=True)
    for name in ("build", "verify"):
        command = commands.add_parser(name)
        command.add_argument("--commit", required=True)
        command.add_argument("--out", required=True, type=Path)
        command.add_argument("--repo", type=Path, default=Path.cwd())
        command.add_argument("--channel", required=True, choices=("registry", "tarball"))
        command.add_argument("--base-url", default="", help="the release's download address (tarball channel only)")
        command.add_argument("--toolchain", required=True, help="the exact Rust version the hosts were built with")
        command.add_argument("--platforms", required=True, type=lambda text: text.split(","),
                             help="comma-separated platforms whose prebuilt hosts the release ships, e.g. linux-x64,linux-arm64")
        if name == "build":
            command.add_argument("--wasm", required=True, type=Path)
            command.add_argument("--hosts", required=True, type=Path)
            command.add_argument("--third-party", required=True, type=Path)
    args = parser.parse_args()
    try:
        commit = Commit(args.repo, args.commit)
        if args.command == "build":
            build(commit, args.out, args.wasm, args.hosts, args.third_party, args.channel, args.base_url, args.toolchain,
                  args.platforms)
        verify(commit, args.out, args.channel, args.base_url, args.toolchain, args.platforms)
    except Refused as refusal:
        print(f"release refused: {refusal}", file=sys.stderr)
        return 1
    print(f"release verified for {commit.sha[:12]}: {args.out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
