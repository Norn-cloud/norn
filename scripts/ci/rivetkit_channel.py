#!/usr/bin/env python3
"""rivetkit_channel: a RivetKit change ships only with a Norn storage version bump (#1513, D131).

`RivetKit` may commit its own SQLite migrations before the Rivet host's `on_start` admits a
cell, so `docs/platform/engine-upgrades.md` ("RivetKit upgrades") ties every RivetKit release
to the storage versions written under it: `RIVETKIT_CHANNEL` in
`crates/norn-cell-rivet/src/config.rs`. The unit test
`config::tests::rivetkit_pin_matches_the_lock_and_the_storage_version` checks the tree against
itself; this check compares it with the merge-base of BASE and the working tree, which a tree
alone cannot do:

- `rivetkit` and `rivetkit-core` each have exactly one `Cargo.lock` entry, from the crates.io
  registry, with a checksum (no git or path source, no second copy);
- the channel is append-only: the head's channel is the base's with some oldest entries
  dropped (which raises the Rivet admission floor) and new ones appended;
- when either crate's locked identity (version, source, checksum) differs from the base's, a
  new entry is appended for it, at the head's `STORAGE_VERSION`, which is above the base's;
  when neither changed, nothing is appended;
- neither crate's locked SemVer falls below the base's, and each channel entry's version is
  strictly above the one before it, counting the base's last entry before the appended ones
  even when it is dropped (no downgrade or re-entry of a release as a new one).

A base that cannot be resolved, or whose files cannot be read, fails the check (closed). A
base whose tree lists no Rivet crate lockfile is its first introduction: only the head's own
rules apply. Without `--require-lock`, a head without the lockfile has nothing to check.

Usage:
  rivetkit_channel.py [--base REV] [--require-lock]   # REV defaults to $NORN_CI_BASE, else origin/main
"""

from __future__ import annotations

import argparse
import os
import re
import subprocess
import sys
import tomllib
from pathlib import Path

LOCK = "crates/norn-cell-rivet/Cargo.lock"
CONFIG = "crates/norn-cell-rivet/src/config.rs"
RELEASE = "crates/norn-cell/src/engine_release.rs"
CRATES = ("rivetkit", "rivetkit-core")
REGISTRY = "registry+https://github.com/rust-lang/crates.io-index"
CHECKSUM = re.compile(r"[0-9a-f]{64}")
VERSION = re.compile(r'^pub const RIVETKIT_VERSION: &str = "([^"]+)";$', re.M)
STORAGE = re.compile(r"^pub const STORAGE_VERSION: u32 = (\d+);$", re.M)
CHANNEL = re.compile(r"^pub const RIVETKIT_CHANNEL: &\[RivetkitRelease\] = &\[(.*?)\];$", re.M | re.S)
SEMVER = re.compile(r"(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-([0-9A-Za-z.-]+))?(?:\+[0-9A-Za-z.-]+)?")
ENTRY = re.compile(r'RivetkitRelease\s*\{\s*rivetkit:\s*"([^"]+)",\s*from_storage:\s*(\d+),?\s*\}')


class Refused(Exception):
    """A rule the tree breaks."""


def semver(version: str, where: str) -> tuple:
    """A SemVer 2.0 precedence key for `version` (build metadata ignored), refusing anything else."""
    match = SEMVER.fullmatch(version)
    if not match:
        raise Refused(f"{where}: {version!r} is not a SemVer version")
    major, minor, patch, pre = match.groups()
    # A release ranks above its pre-releases; numeric identifiers rank below alphanumeric ones.
    pre_key = (1,) if pre is None else (0, *((0, int(part), "") if part.isdigit() else (1, 0, part)
                                             for part in pre.split(".")))
    return int(major), int(minor), int(patch), pre_key


def identities(lock_text: str, where: str) -> dict[str, tuple[str, str, str]]:
    """Each RivetKit crate's (version, source, checksum), refusing anything but one registry entry."""
    try:
        packages = tomllib.loads(lock_text).get("package", [])
    except tomllib.TOMLDecodeError as error:
        raise Refused(f"{where}: {LOCK} does not parse: {error}") from error
    found = {}
    for name in CRATES:
        entries = [p for p in packages if p.get("name") == name]
        if len(entries) != 1:
            raise Refused(f"{where}: {LOCK} has {len(entries)} `{name}` entries; exactly one is allowed")
        entry = entries[0]
        source, checksum = entry.get("source"), entry.get("checksum")
        if source != REGISTRY:
            raise Refused(f"{where}: `{name}` is locked from {source or 'a path'}, not the crates.io registry")
        if not isinstance(checksum, str) or not CHECKSUM.fullmatch(checksum):
            raise Refused(f"{where}: `{name}` has no checksum in {LOCK}")
        found[name] = (str(entry.get("version")), source, checksum)
    return found


def channel(config_text: str, where: str) -> tuple[str, list[tuple[str, int]]]:
    """`RIVETKIT_VERSION` and the `RIVETKIT_CHANNEL` entries, refusing a form this does not read."""
    version = VERSION.findall(config_text)
    block = CHANNEL.findall(config_text)
    if len(version) != 1 or len(block) != 1:
        raise Refused(f"{where}: {CONFIG} must declare RIVETKIT_VERSION and RIVETKIT_CHANNEL once each, as literals")
    entries = [(name, int(storage)) for name, storage in ENTRY.findall(block[0])]
    if ENTRY.sub("", block[0]).replace(",", "").strip():
        raise Refused(f"{where}: RIVETKIT_CHANNEL holds something other than RivetkitRelease literals")
    if not entries:
        raise Refused(f"{where}: RIVETKIT_CHANNEL is empty")
    return version[0], entries


def storage_version(release_text: str, where: str) -> int:
    found = STORAGE.findall(release_text)
    if len(found) != 1:
        raise Refused(f"{where}: {RELEASE} must declare STORAGE_VERSION once, as a literal")
    return int(found[0])


def check_tree(lock: dict, version: str, entries: list, storage: int, where: str) -> None:
    """The rules a tree meets alone (the unit test checks them too)."""
    for name, (locked, _, _) in lock.items():
        if locked != version:
            raise Refused(f"{where}: `{name}` is locked at {locked}, but RIVETKIT_VERSION is {version}")
    if entries[-1][0] != version:
        raise Refused(f"{where}: the last RIVETKIT_CHANNEL entry is {entries[-1][0]}, not RIVETKIT_VERSION {version}")
    if entries[-1][1] > storage:
        raise Refused(f"{where}: RIVETKIT_CHANNEL names storage version {entries[-1][1]}, above STORAGE_VERSION {storage}")
    for (old, old_from), (new, new_from) in zip(entries, entries[1:]):
        if new_from <= old_from or semver(new, where) <= semver(old, where):
            raise Refused(f"{where}: RivetKit {new} at storage version {new_from} does not start above {old} at {old_from}")


def compare(base: dict | None, head: dict) -> None:
    """Head against base; each a dict of lock, version, entries and storage (base None: first introduction)."""
    check_tree(head["lock"], head["version"], head["entries"], head["storage"], "head")
    if base is None:
        return
    old, new = base["entries"], head["entries"]
    # The fewest oldest base entries to drop so that the rest of the base starts the head; at
    # worst all of them. Whatever follows is appended, so a rewritten or reordered entry shows
    # up as an append, which the rules below refuse unless it is exactly the one new release.
    dropped = next(drop for drop in range(len(old) + 1) if new[:len(old) - drop] == old[drop:])
    appended = new[len(old) - dropped:]
    for name in CRATES:
        if semver(head["lock"][name][0], "head") < semver(base["lock"][name][0], "base"):
            raise Refused(f"`{name}` is downgraded from {base['lock'][name][0]} to {head['lock'][name][0]}; "
                          "a RivetKit pin only moves up")
    if appended and semver(appended[0][0], "head") <= semver(old[-1][0], "base"):
        raise Refused(f"RIVETKIT_CHANNEL appends RivetKit {appended[0][0]}, not above the base's last entry {old[-1][0]}")
    changed = [name for name in CRATES if base["lock"][name] != head["lock"][name]]
    if not changed:
        if appended:
            raise Refused(f"RIVETKIT_CHANNEL appends {appended}, but neither {' nor '.join(CRATES)} changed in {LOCK}")
        return
    detail = ", ".join(f"`{name}` {base['lock'][name]} -> {head['lock'][name]}" for name in changed)
    if head["storage"] <= base["storage"]:
        raise Refused(f"RivetKit changed ({detail}) without a storage version bump: STORAGE_VERSION is {head['storage']}, "
                      f"the base's is {base['storage']} (docs/platform/engine-upgrades.md, \"RivetKit upgrades\")")
    if not appended:
        raise Refused(f"RivetKit changed ({detail}) without a new RIVETKIT_CHANNEL entry")
    if appended != [(head["version"], head["storage"])]:
        raise Refused(f"RivetKit changed ({detail}): append exactly one entry, ({head['version']!r}, {head['storage']}), "
                      f"not {appended}")


def git(*args: str) -> str:
    return subprocess.run(["git", *args], check=True, capture_output=True, text=True).stdout


def at(rev: str, path: str) -> str | None:
    """`path` at `rev`; None only when the revision's tree lists no such path. A failed listing
    or blob read (a blobless clone, a missing object) is refused, never taken for absence."""
    listed = subprocess.run(["git", "ls-tree", "--full-tree", rev, "--", path], capture_output=True, text=True)
    if listed.returncode != 0:
        raise Refused(f"cannot list {path} at {rev[:12]}: {listed.stderr.strip()}")
    if not listed.stdout.strip():
        return None
    result = subprocess.run(["git", "cat-file", "blob", f"{rev}:{path}"], capture_output=True, text=True)
    if result.returncode != 0:
        raise Refused(f"cannot read {path} at {rev[:12]}: {result.stderr.strip()}")
    return result.stdout


def tree(read, where: str) -> dict | None:
    lock = read(LOCK)
    if lock is None:
        return None
    config, release = read(CONFIG), read(RELEASE)
    if config is None or release is None:
        raise Refused(f"{where}: {LOCK} exists without {CONFIG} or {RELEASE}")
    version, entries = channel(config, where)
    return {"lock": identities(lock, where), "version": version, "entries": entries,
            "storage": storage_version(release, where)}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--base", default=os.environ.get("NORN_CI_BASE") or "origin/main")
    parser.add_argument("--require-lock", action="store_true", help=f"fail when the head has no {LOCK}")
    args = parser.parse_args()
    try:
        merge_base = git("merge-base", args.base, "HEAD").strip()
    except (subprocess.CalledProcessError, OSError) as error:
        stderr = getattr(error, "stderr", "") or ""
        print(f"rivetkit channel: cannot resolve the merge-base of {args.base} and HEAD; failing closed. {stderr.strip()}")
        return 1

    def disk(path: str) -> str | None:
        file = Path(path)
        return file.read_text(encoding="utf-8") if file.is_file() else None

    try:
        head = tree(disk, "head")
        if head is None:
            if args.require_lock:
                print(f"rivetkit channel: {LOCK} is absent, and --require-lock needs it")
                return 1
            print(f"rivetkit channel: {LOCK} is absent; nothing to check")
            return 0
        base = tree(lambda path: at(merge_base, path), f"base {merge_base[:12]}")
        compare(base, head)
    except Refused as refusal:
        print(f"rivetkit channel: {refusal}")
        return 1
    print(f"rivetkit channel: RivetKit {head['version']} at storage version {head['storage']}, channel {head['entries']}; "
          + ("first introduction" if base is None else f"checked against {merge_base[:12]}"))
    return 0


if __name__ == "__main__":
    sys.exit(main())
