#!/usr/bin/env python3
"""Translate an old-layout PR delta onto an already migrated merge worktree."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile


def _sibling(name, directory=None):
    """Load sibling module `name` by explicit path, never through sys.path: gated scripts
    run Python with PYTHONSAFEPATH or -I, so the script directory is not on it."""
    import importlib.util
    import pathlib
    import sys

    if name in sys.modules:
        return sys.modules[name]
    path = (directory or pathlib.Path(__file__).resolve().parent) / f"{name}.py"
    spec = importlib.util.spec_from_file_location(name, path)
    if spec is None or spec.loader is None:
        raise ImportError(f"cannot load {name} from {path}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module

coverage = _sibling("coverage_manifest")
_coverage_manifest = _sibling("coverage_manifest")
json_bytes = _coverage_manifest.entry_bytes

COVERAGE = "verification/coverage-v1/manifest"
DIAGNOSTICS = "crates/norn-checker/conformance/diagnostic-codes"
RUST = "crates/norn-checker/src/codes.rs"
RECORDS = ".github/ci/provenance/artifacts/"
MISSING = object()


def git(root, *args, check=True, **kwargs):
    return subprocess.run(["git", "-C", str(root), *args], capture_output=True, check=check, timeout=300, env={k: v for k, v in os.environ.items() if k not in ("GIT_DIR", "GIT_WORK_TREE", "GIT_COMMON_DIR", "GIT_INDEX_FILE", "GIT_OBJECT_DIRECTORY", "GIT_ALTERNATE_OBJECT_DIRECTORIES", "GIT_PREFIX")}, **kwargs)


def read(root, revision, path):
    result = git(root, "show", f"{revision}:{path}", check=False)
    if result.returncode and not revision.startswith(":") and git(root, "ls-tree", revision, "--", path).stdout:
        raise ValueError(f"cannot read {revision}:{path}")
    return MISSING if result.returncode else result.stdout


def merge(base, current, proposed):
    if current == proposed or base == proposed:
        return current
    if current == base:
        return proposed
    if all(isinstance(x, dict) for x in (base, current, proposed)):
        result = {}
        for key in dict.fromkeys([*current, *proposed]):
            value = merge(base.get(key, MISSING), current.get(key, MISSING), proposed.get(key, MISSING))
            if value is not MISSING:
                result[key] = value
        return result
    raise ValueError("concurrent registry edit or delete/edit conflict")


def object_bytes(raw):
    def unique(pairs):
        result = {}
        for key, value in pairs:
            if key in result:
                raise ValueError(f"duplicate JSON key {key}")
            result[key] = value
        return result
    return json.loads(raw, object_pairs_hook=unique)


def projection(raw, identity, directory):
    if raw is MISSING:
        return {}
    body = object_bytes(raw)
    if "id" in body:
        raise ValueError("legacy subsystem must not declare id")
    coverage.write_entries({"subsystems": [{"id": identity, **body}]}, directory)
    return {p.relative_to(directory).as_posix(): coverage.read_entry(p) for p in coverage.files(directory)
            if p.relative_to(directory).parts[0] == "subsystems"}


def rust_projection(root, revision, raw, manifest, expected_helpers):
    if raw is MISSING:
        raise ValueError("missing old Rust registry")
    text = raw.decode()
    prefix, body = text.split("pub static REGISTRY: &[CodeEntry] = &[\n")
    rows, suffix = body.split("\n];", 1)
    literal = r'"(?:[^"\\]|\\.)*"'
    row = re.compile(r'\s*(?://[^\n]*\n\s*)*(entry|introduced|warning|warning_introduced)\(\s*('
                     + literal + r'|[A-Za-z_][\w:]*)\s*,\s*Tier::(T[0-7])\s*,\s*('
                     + literal + r')\s*(?:,\s*(' + literal + r')\s*)?,?\s*\),')
    default_since = json.loads(re.search(r'const INITIAL_SINCE: &str = ("[^"\\]*");', text)[1])
    entries, position = [], 0
    while rows[position:].strip():
        match = row.match(rows, position)
        if not match:
            raise ValueError("unsupported Rust registry expression")
        kind, code, tier, message, since = match.groups()
        if not code.startswith('"'):
            name = code.split("::")[-1]
            paths = git(root, "grep", "-l", "-w", name, revision, "--", "crates/norn-checker/src", check=False).stdout.splitlines()
            values = set()
            for path in paths:
                source = git(root, "show", path.decode()).stdout.decode()
                values.update(re.findall(r'\bconst\s+' + re.escape(name) + r'\s*:\s*&str\s*=\s*(' + literal + r')\s*;', source))
            if len(values) != 1:
                raise ValueError(f"unresolved Rust diagnostic constant {code}")
            code = values.pop()
        if (since is not None) != (kind in ("introduced", "warning_introduced")):
            raise ValueError("invalid Rust registry constructor")
        entries.append(dict(code=json.loads(code), tier=tier, message=json.loads(message),
                            since=json.loads(since) if since else default_since,
                            severity="warning" if kind.startswith("warning") else "error"))
        if not re.fullmatch(r"[a-z0-9_]+\.[a-z0-9_]+\.[a-z0-9_]+", entries[-1]["code"]) or not entries[-1]["message"].strip() or not entries[-1]["since"].strip():
            raise ValueError("invalid diagnostic metadata")
        position = match.end()
    if entries != manifest["codes"]:
        raise ValueError("Rust registry and diagnostic JSON disagree")
    # Only the known, unchanged constructors may be discarded.
    if prefix[prefix.index("const fn entry("):] != expected_helpers:
        raise ValueError("Rust registry constructors changed")
    return (prefix[:prefix.index("const fn entry(")] + 'include!(concat!(env!("OUT_DIR"), "/diagnostic_registry.rs"));'
            + suffix).replace("conformance/diagnostic-codes.json", "conformance/diagnostic-codes/").encode()


def migrate(root, base, head):
    base, head = (git(root, "rev-parse", "--verify", "--end-of-options", ref + "^{commit}").stdout.decode().strip() for ref in (base, head))
    git(root, "merge-base", "--is-ancestor", base, head)
    changed = git(root, "diff", "--name-only", "--no-renames", base, head, "--", COVERAGE, COVERAGE + ".json", DIAGNOSTICS, DIAGNOSTICS + ".json", RUST).stdout.decode().splitlines()
    for line in git(root, "diff", "--raw", "--no-renames", base, head, "--", *changed).stdout.decode().splitlines() if changed else []:
        if any(mode.lstrip(":") not in ("000000", "100644") for mode in line.split()[:2]):
            raise ValueError("registry file modes cannot be mapped")
    if any(p == COVERAGE + ".json" or p.startswith(DIAGNOSTICS + "/") or (p.startswith(COVERAGE + "/") and not re.fullmatch(COVERAGE + r"/subsystems/[a-z0-9][a-z0-9-]*\.json", p)) for p in changed):
        raise ValueError("only legacy subsystem deltas can be mapped")
    for path in changed:
        records = [read(root, revision, RECORDS + path + ".json") for revision in (base, head)]
        if all(raw is not MISSING for raw in records) and len({json.dumps({k: v for k, v in object_bytes(raw).items() if k != "sha256"}, sort_keys=True) for raw in records}) != 1:
            raise ValueError("non-hash legacy provenance changes need manual mapping")
    plan = {}
    def current(path):
        target = root / path
        if git(root, "ls-files", "-u", "--", path).stdout:
            if not path.startswith(RECORDS) and path != RUST:
                raise ValueError(f"conflicted new-layout entry {path}")
            sides = [read(root, f":{stage}", path) for stage in (2, 3)]
            selected = sides[1]
            if path == RUST:
                migrated = [s for s in sides if s is not MISSING and b'include!(concat!(env!("OUT_DIR"), "/diagnostic_registry.rs"))' in s]
                if len(migrated) != 1:
                    raise ValueError("no unambiguous migrated Rust side")
                selected = migrated[0]
            if path.startswith(RECORDS):
                if any(s is MISSING for s in sides) or any(
                    {k: v for k, v in object_bytes(sides[0]).items() if k != "sha256"}
                    != {k: v for k, v in object_bytes(s).items() if k != "sha256"} for s in sides[1:]):
                    raise ValueError(f"non-hash provenance conflict {path}")
            raw = target.read_bytes() if target.exists() else b""
            return selected if re.search(rb"^<<<<<<< ", raw, re.M) else raw
        if target.is_symlink() or (target.exists() and not target.is_file()):
            raise ValueError(f"not a regular file {path}")
        return (json_bytes(coverage.read_entry(target)) if path.startswith(COVERAGE + "/") else target.read_bytes()) if target.exists() else MISSING
    def put(path, value, template):
        plan[path] = value
        record = RECORDS + path + ".json"
        raw = current(record)
        metadata = (template if raw is MISSING else object_bytes(raw)).copy()
        if value is not MISSING:
            metadata["sha256"] = hashlib.sha256(value).hexdigest()
        plan[record] = json_bytes(metadata) if value is not MISSING else MISSING
    def apply(before, after, prefix, template):
        ranks = {}
        for path in sorted(set(before) | set(after), key=lambda p: (after.get(p, {}).get("order", -1), p)):
            old, new = before.get(path, MISSING), after.get(path, MISSING)
            if old == new:
                continue
            if new is MISSING and isinstance(old, dict) and isinstance(old.get("value"), dict) and "id" not in old["value"]:
                raise ValueError("unkeyed object removal/replacement needs manual mapping")
            destination = prefix + path
            raw = current(destination)
            value = object_bytes(raw) if raw is not MISSING else MISSING
            if isinstance(new, dict) and set(new) == {"order", "value"}:
                folder = str(Path(destination).parent)
                if isinstance(value, dict):
                    new = {**new, "order": value["order"]}
                else:
                    if folder not in ranks:
                        ranks[folder] = max((coverage.read_entry(p)["order"] for p in (root / folder).glob("*.json")), default=-1)
                    ranks[folder] += 1
                    new = {**new, "order": ranks[folder]}
            if isinstance(old, dict) and set(old) == {"order", "value"} and isinstance(value, dict):
                old = {**old, "order": value["order"]}
            result = merge(old, value, new)
            if result != value:
                put(destination, json_bytes(result) if result is not MISSING else MISSING, template)
    with tempfile.TemporaryDirectory() as temporary:
        temporary = Path(temporary)
        for path in changed:
            if not path.startswith(COVERAGE + "/"):
                continue
            identity = Path(path).stem
            before = projection(read(root, base, path), identity, temporary / "base")
            after = projection(read(root, head, path), identity, temporary / "head")
            record = read(root, head, RECORDS + path + ".json")
            template = object_bytes(read(root, base, RECORDS + path + ".json") if record is MISSING else record)
            apply(before, after, COVERAGE + "/", template)
        diagnostic_changed = RUST in changed or DIAGNOSTICS + ".json" in changed
        if diagnostic_changed and not (root / DIAGNOSTICS).is_dir() and git(root, "ls-files", "-u", "--", RUST, DIAGNOSTICS + ".json").stdout:
            raise ValueError("unmigrated diagnostic conflict needs the legacy resolver")
        if diagnostic_changed and (root / DIAGNOSTICS).is_dir():
            manifests = [object_bytes(read(root, rev, DIAGNOSTICS + ".json")) for rev in (base, head)]
            if {k: v for k, v in manifests[0].items() if k != "codes"} != {k: v for k, v in manifests[1].items() if k != "codes"}:
                raise ValueError("diagnostic manifest header changed")
            helpers = "const fn entry(" + read(root, base, RUST).decode().split("pub static REGISTRY: &[CodeEntry] = &[\n")[0].split("const fn entry(", 1)[1]
            projections = [rust_projection(root, rev, read(root, rev, RUST), doc, helpers) for rev, doc in zip((base, head), manifests)]
            template = object_bytes(read(root, head, RECORDS + RUST + ".json"))
            maps = [{e["code"] + ".json": e for e in doc["codes"]} for doc in manifests]
            if any(len(m) != len(d["codes"]) for m, d in zip(maps, manifests)):
                raise ValueError("duplicate diagnostic identity")
            if (root / DIAGNOSTICS).is_symlink() or not (root / DIAGNOSTICS).is_dir():
                raise ValueError("diagnostic target has not migrated")
            apply(*maps, DIAGNOSTICS + "/", template)
            inputs = [current(RUST), *projections]
            if b'include!(concat!(env!("OUT_DIR"), "/diagnostic_registry.rs"))' not in inputs[0]:
                raise ValueError("Rust target has not migrated")
            paths = [temporary / str(i) for i in range(3)]
            for path, content in zip(paths, inputs):
                path.write_bytes(content)
            result = git(root, "merge-file", "-p", *map(str, paths), check=False)
            if result.returncode:
                raise ValueError("non-registry Rust changes conflict")
            if result.stdout != (root / RUST).read_bytes():
                put(RUST, result.stdout, template)
        coverage.apply_entries(root, {p: None if v is MISSING else v for p, v in plan.items()}, changed)
    return sorted(plan)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("base")
    parser.add_argument("head")
    args = parser.parse_args()
    try:
        print("\n".join(migrate(Path(git(Path.cwd(), "rev-parse", "--show-toplevel").stdout.decode().strip()), args.base, args.head)))
    except (ValueError, OSError, KeyError, TypeError, subprocess.CalledProcessError, subprocess.TimeoutExpired) as error:
        parser.exit(1, f"needs-human: {error}\n")
