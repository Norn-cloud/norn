#!/usr/bin/env python3
"""Append measurements to Norn's permanent metrics record, and check that record.

The record is the `metrics` branch of this repository: one JSON object per line in
`metrics/<YYYY-MM>.jsonl`, sharded by the month a record was appended, plus a generated
trend view (`README.md`, `index.html`). Lines are only ever appended: a shard is never
edited, and the branch is never rewritten or force-pushed. `record.schema.json` (next to
this file) defines a record; `verify` checks the whole branch, history included.

Record one measurement (run inside any clone whose `origin` is Norn-cloud/norn):

    python3 scripts/metrics/record.py add --metric sim.cell.serial_s --value 34.7 \\
        --unit s --statistic total --commit <sha> --by L-dst --runner boat \\
        --vm-type default --env-file env.json --workload threads=1 --workload tests=11 \\
        --caveats "serial, one run, warm target dir" --source <PR comment or run URL>

`python3 scripts/metrics/record.py env > env.json` on the measuring machine (the Boat VM)
captures vCPU, RAM, CPU model, OS and the Rust toolchain. `--file records.jsonl` appends
many records at once (each line a record; `schema`, `id` and `recorded_at` are filled in).
`--dry-run` builds the commit without pushing it. A wrong value is never edited: record the
right one with `--supersedes <id of the wrong record>`, and the view and the ratchet skip the
wrong one while the history keeps both. A value more than 2x off its series' latest value
prints a warning. `record.py ids` lists the ids already in use.

Concurrent appends are safe: the commit is built with plumbing on top of the fetched tip,
outside any work tree, and a push that loses the race is rebuilt on the new tip and
retried. Nothing is ever forced. Re-sending a record whose `id` is already stored is a
no-op, so producers with deterministic ids (the CI sweep) can retry freely.

    python3 scripts/metrics/record.py validate FILE...   # schema and semantic checks only
    python3 scripts/metrics/record.py verify             # the fetched origin/metrics, history included

Standard library only; `METRICS_PUSH_TOKEN` (CI) authenticates fetch and push over HTTPS.
"""

from __future__ import annotations

import argparse
import base64
import datetime as dt
import hashlib
import json
import os
import platform
import re
import subprocess
import sys
import tempfile
import time
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


HERE = Path(__file__).resolve().parent
SCHEMA_PATH = HERE / "record.schema.json"
BRANCH = "metrics"
SHARD_DIR = "metrics"
SHARD_RE = re.compile(r"^metrics/(\d{4}-\d{2})\.jsonl$")
DATE_TIME_RE = re.compile(
    r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d+)?(Z|[+-]\d{2}:\d{2})$")
URI_RE = re.compile(r"^[a-z][a-z0-9+.-]*://[^\s]+$")
PUSH_ATTEMPTS = 8


class Invalid(Exception):
    """A record or store that breaks the schema or a store rule."""


# --- schema --------------------------------------------------------------------------

def load_schema() -> dict:
    return json.loads(SCHEMA_PATH.read_text(encoding="utf-8"))


# Annotation keywords carry no constraint.
_ANNOTATIONS = {"$schema", "$id", "title", "description"}
_TYPES = {
    "object": lambda v: isinstance(v, dict),
    "array": lambda v: isinstance(v, list),
    "string": lambda v: isinstance(v, str),
    "integer": lambda v: isinstance(v, int) and not isinstance(v, bool),
    "number": lambda v: isinstance(v, (int, float)) and not isinstance(v, bool),
    "boolean": lambda v: isinstance(v, bool),
    "null": lambda v: v is None,
}


def _check_format(name: str, value: str) -> bool:
    if name == "date-time":
        if not DATE_TIME_RE.match(value):
            return False
        try:
            dt.datetime.fromisoformat(value.replace("Z", "+00:00"))
        except ValueError:
            return False
        return True
    if name == "uri":
        return bool(URI_RE.match(value))
    raise Invalid(f"schema uses unsupported format {name!r}")


def schema_errors(value, schema: dict, path: str = "$") -> list[str]:
    """Errors of `value` against the subset of JSON Schema 2020-12 that record.schema.json
    uses. An unknown keyword is refused rather than ignored, so the schema cannot grow a
    rule this validator silently skips (test_metrics.py cross-checks with `jsonschema`)."""
    errors: list[str] = []
    unknown = set(schema) - _ANNOTATIONS - {
        "type", "properties", "required", "additionalProperties", "const", "enum", "pattern",
        "minLength", "maxLength", "minimum", "exclusiveMinimum", "format"}
    if unknown:
        raise Invalid(f"schema at {path} uses unsupported keywords {sorted(unknown)}")
    kinds = schema.get("type")
    if kinds is not None:
        kinds = [kinds] if isinstance(kinds, str) else kinds
        if not any(_TYPES[k](value) for k in kinds):
            return [f"{path}: expected {' or '.join(kinds)}, got {type(value).__name__}"]
    if "const" in schema and value != schema["const"]:
        errors.append(f"{path}: must be {schema['const']!r}")
    if "enum" in schema and value not in schema["enum"]:
        errors.append(f"{path}: {value!r} is not one of {schema['enum']}")
    if isinstance(value, str):
        if "pattern" in schema and not re.search(schema["pattern"], value):
            errors.append(f"{path}: {value!r} does not match {schema['pattern']}")
        if len(value) < schema.get("minLength", 0):
            errors.append(f"{path}: shorter than {schema['minLength']}")
        if "maxLength" in schema and len(value) > schema["maxLength"]:
            errors.append(f"{path}: longer than {schema['maxLength']}")
        if "format" in schema and not _check_format(schema["format"], value):
            errors.append(f"{path}: {value!r} is not a {schema['format']}")
    if _TYPES["number"](value):
        if "minimum" in schema and value < schema["minimum"]:
            errors.append(f"{path}: below {schema['minimum']}")
        if "exclusiveMinimum" in schema and value <= schema["exclusiveMinimum"]:
            errors.append(f"{path}: not above {schema['exclusiveMinimum']}")
    if isinstance(value, dict):
        properties = schema.get("properties", {})
        for key in schema.get("required", []):
            if key not in value:
                errors.append(f"{path}: missing {key!r}")
        extra = schema.get("additionalProperties", True)
        for key, item in value.items():
            if key in properties:
                errors += schema_errors(item, properties[key], f"{path}.{key}")
            elif extra is False:
                errors.append(f"{path}: unexpected {key!r}")
            elif isinstance(extra, dict):
                errors += schema_errors(item, extra, f"{path}.{key}")
    return errors


def record_errors(record, schema: dict) -> list[str]:
    """Schema errors plus the rules a schema cannot state."""
    errors = schema_errors(record, schema)
    if errors:
        return errors
    if isinstance(record["value"], float) and record["value"] != record["value"]:
        errors.append("$.value: NaN")
    spread = record.get("range")
    if spread and not spread["min"] <= record["value"] <= spread["max"]:
        errors.append(f"$.value: {record['value']} lies outside range {spread}")
    if record.get("supersedes") == record["id"]:
        errors.append("$.supersedes: a record cannot supersede itself")
    if (record["commit"]["sha"] is None) != (record["commit"]["date"] is None):
        errors.append("$.commit: sha and date are both known or both null")
    measured, recorded = parse_time(record["measured_at"]), parse_time(record["recorded_at"])
    if measured > recorded + dt.timedelta(minutes=5):
        errors.append("$.measured_at: after recorded_at")
    return errors


def parse_time(text: str) -> dt.datetime:
    return dt.datetime.fromisoformat(text.replace("Z", "+00:00"))


def series_key(record: dict) -> tuple:
    """A series' order: the measured commit's date (the code's history), then when it was measured.
    A record with no commit sorts by its measurement time."""
    measured = parse_time(record["measured_at"])
    committed = parse_time(record["commit"]["date"]) if record["commit"]["date"] else measured
    return (committed, measured, record["recorded_at"], record["id"])


def load_aliases() -> dict[str, str]:
    path = HERE / "aliases.json"
    return json.loads(path.read_text(encoding="utf-8"))["aliases"] if path.exists() else {}


def fold(records: list[dict], aliases: dict[str, str] | None = None) -> list[dict]:
    """Records as series: a record another one supersedes is dropped, each alias id is folded into
    its canonical id, and a value re-reported for the same commit (same metric, commit sha and
    value) is kept once, as first recorded."""
    aliases = load_aliases() if aliases is None else aliases
    superseded = {r["supersedes"] for r in records if r.get("supersedes")}
    seen, out = set(), []
    for record in sorted(records, key=lambda r: (r["recorded_at"], r["id"])):
        if record["id"] in superseded:
            continue
        metric = aliases.get(record["metric"], record["metric"])
        if metric != record["metric"]:
            record = {**record, "metric": metric}
        sha = record["commit"]["sha"]
        if sha is not None:
            key = (metric, sha, record["value"])
            if key in seen:
                continue
            seen.add(key)
        out.append(record)
    return out


def now_utc() -> str:
    return dt.datetime.now(dt.timezone.utc).replace(microsecond=0).isoformat().replace("+00:00", "Z")


def canonical(record: dict) -> str:
    return json.dumps(record, sort_keys=True, separators=(",", ":"), ensure_ascii=False)


def complete(record: dict, recorded_at: str | None = None) -> dict:
    """Fill the fields record.py owns: schema version, recorded_at and a content id."""
    record = dict(record)
    record.setdefault("schema", 1)
    record.setdefault("estimate", False)
    record.setdefault("workload", {})
    record.setdefault("recorded_at", recorded_at or now_utc())
    if "id" not in record:
        body = {k: v for k, v in record.items() if k != "recorded_at"}
        digest = hashlib.sha256(canonical(body).encode()).hexdigest()[:20]
        record["id"] = f"{record['metric']}/{digest}" if "metric" in record else digest
    return record


def shard_of(record: dict) -> str:
    return f"{SHARD_DIR}/{record['recorded_at'][:7]}.jsonl"


# --- git -----------------------------------------------------------------------------

def _auth_config() -> list[str]:
    token = os.environ.get("METRICS_PUSH_TOKEN")
    if not token:
        return []
    basic = base64.b64encode(f"x-access-token:{token}".encode()).decode()
    return ["-c", f"http.https://github.com/.extraheader=AUTHORIZATION: basic {basic}"]


def git(repo: Path, *args: str, data: bytes | None = None, env: dict | None = None,
        check: bool = True) -> subprocess.CompletedProcess:
    result = subprocess.run(["git", "-C", str(repo), *args], input=data, capture_output=True,
                            env={**os.environ, **(env or {})})
    if check and result.returncode != 0:
        raise RuntimeError(f"git {' '.join(args[:3])} failed: {result.stderr.decode(errors='replace').strip()}")
    return result


def fetch_tip(repo: Path, remote: str, branch: str) -> str | None:
    """Fetch the branch into refs/remotes/<remote>/<branch>; None when it does not exist yet."""
    probe = git(repo, *_auth_config(), "ls-remote", "--exit-code", remote, f"refs/heads/{branch}", check=False)
    if probe.returncode == 2:
        return None
    if probe.returncode != 0:
        raise RuntimeError(f"git ls-remote {remote} failed: {probe.stderr.decode(errors='replace').strip()}")
    git(repo, *_auth_config(), "fetch", "--quiet", "--no-tags", remote,
        f"+refs/heads/{branch}:refs/remotes/{remote}/{branch}")
    return git(repo, "rev-parse", f"refs/remotes/{remote}/{branch}^{{commit}}").stdout.decode().strip()


def read_store(repo: Path, rev: str | None) -> dict[str, bytes]:
    """Shard path -> bytes at `rev`."""
    if rev is None:
        return {}
    listing = git(repo, "ls-tree", "-z", "--name-only", rev, f"{SHARD_DIR}/").stdout.decode()
    return {path: git(repo, "cat-file", "blob", f"{rev}:{path}").stdout
            for path in listing.split("\0") if path and SHARD_RE.match(path)}


def parse_shard(path: str, data: bytes) -> list[dict]:
    records = []
    for number, line in enumerate(data.decode("utf-8").splitlines(), 1):
        if not line.strip():
            raise Invalid(f"{path}:{number}: blank line")
        try:
            records.append(json.loads(line))
        except json.JSONDecodeError as error:
            raise Invalid(f"{path}:{number}: {error}") from None
    return records


def all_records(store: dict[str, bytes]) -> list[dict]:
    return [r for path in sorted(store) for r in parse_shard(path, store[path])]


def build_commit(repo: Path, tip: str | None, store: dict[str, bytes], records: list[dict],
                 message: str, views=None) -> tuple[str | None, list[dict]]:
    """A commit on top of `tip` that appends the records whose ids are new. Returns
    (commit or None when nothing is new, the appended records)."""
    known = {r["id"] for r in all_records(store)}
    fresh = []
    for record in records:
        if record["id"] not in known:
            known.add(record["id"])
            fresh.append(record)
    if not fresh:
        return None, []
    files = dict(store)
    for record in fresh:
        path = shard_of(record)
        old = files.get(path, b"")
        if old and not old.endswith(b"\n"):
            raise Invalid(f"{path} does not end with a newline")
        files[path] = old + (canonical(record) + "\n").encode()
    changed = {path: files[path] for path in files if files[path] != store.get(path)}
    if views is not None:
        changed.update(views(all_records(files)))
    with tempfile.TemporaryDirectory() as scratch:
        index = {"GIT_INDEX_FILE": str(Path(scratch) / "index")}
        if tip:
            git(repo, "read-tree", tip, env=index)
        else:
            git(repo, "read-tree", "--empty", env=index)
        for path, data in changed.items():
            blob = git(repo, "hash-object", "-w", "--stdin", data=data).stdout.decode().strip()
            git(repo, "update-index", "--add", "--cacheinfo", f"100644,{blob},{path}", env=index)
        tree = git(repo, "write-tree", env=index).stdout.decode().strip()
    parents = ["-p", tip] if tip else []
    commit = git(repo, "commit-tree", tree, *parents, "-m", message).stdout.decode().strip()
    return commit, fresh


def append(repo: Path, records: list[dict], *, remote: str = "origin", branch: str = BRANCH,
           push: bool = True, message: str | None = None, views=None) -> tuple[str | None, list[dict]]:
    """Validate, then append `records` to the branch tip and push, retrying lost races."""
    schema = load_schema()
    for record in records:
        errors = record_errors(record, schema)
        if errors:
            raise Invalid(f"record {record.get('id', '?')}: " + "; ".join(errors))
    metrics = sorted({r["metric"] for r in records})
    message = message or f"metrics: record {len(records)} measurement(s) of {', '.join(metrics[:4])}" + (
        f" and {len(metrics) - 4} more" if len(metrics) > 4 else "")
    last_tip = object()
    for attempt in range(PUSH_ATTEMPTS):
        tip = fetch_tip(repo, remote, branch)
        store = read_store(repo, tip)
        stored = all_records(store)
        check_series(stored + records)
        known = {x["id"] for x in stored}
        check_supersedes(stored + [r for r in records if r["id"] not in known])
        commit, fresh = build_commit(repo, tip, store, records, message, views)
        if attempt == 0:
            for warning in jump_warnings(stored, fresh):
                print(warning, file=sys.stderr)
        if commit is None or not push:
            return commit, fresh
        result = git(repo, "-c", "http.version=HTTP/1.1", *_auth_config(), "push", "--quiet", remote,
                     f"{commit}:refs/heads/{branch}", check=False)
        if result.returncode == 0:
            git(repo, "update-ref", f"refs/remotes/{remote}/{branch}", commit, check=False)
            return commit, fresh
        if tip == last_tip:
            raise RuntimeError(f"push to {remote}/{branch} failed twice on the same tip: "
                               f"{result.stderr.decode(errors='replace').strip()}")
        last_tip = tip
        time.sleep(min(30, 2 ** attempt) / 4)
    raise RuntimeError(f"push to {remote}/{branch} kept losing races after {PUSH_ATTEMPTS} attempts")


# --- store checks --------------------------------------------------------------------

def check_supersedes(records: list[dict]) -> None:
    """A correction names an earlier record of the same metric (aliases folded)."""
    aliases = load_aliases()
    by_id, order = {}, {}
    for index, record in enumerate(records):
        by_id[record["id"]] = record
        order[record["id"]] = index
    for record in records:
        target = record.get("supersedes")
        if not target:
            continue
        old = by_id.get(target)
        if old is None or order[target] > order[record["id"]]:
            raise Invalid(f"{record['id']}: supersedes {target}, which is not an earlier record")
        if aliases.get(old["metric"], old["metric"]) != aliases.get(record["metric"], record["metric"]):
            raise Invalid(f"{record['id']}: supersedes {target}, a record of another metric ({old['metric']})")


def jump_warnings(stored: list[dict], fresh: list[dict], factor: float = 2.0) -> list[str]:
    """New values more than `factor` times off their series' latest value, unless they correct one."""
    latest: dict[str, dict] = {}
    for record in sorted(fold(stored), key=series_key):
        latest[record["metric"]] = record
    aliases = load_aliases()
    warnings = []
    for record in fresh:
        last = latest.get(aliases.get(record["metric"], record["metric"]))
        if record.get("supersedes") or last is None or last["value"] == 0 or record["value"] == 0 \
                or (last["value"] > 0) != (record["value"] > 0):
            continue
        ratio = record["value"] / last["value"]
        if ratio > factor or ratio < 1 / factor:
            warnings.append(
                f"warning: {record['metric']} = {record['value']} {record['unit']} is {ratio:.2g}x its latest value "
                f"{last['value']} (record {last['id']}). If that one was wrong, record this with --supersedes "
                f"{last['id']}; otherwise make sure --caveats explains the jump.")
    return warnings


def check_series(records: list[dict]) -> None:
    """Each metric id keeps one unit and one statistic forever."""
    seen: dict[str, tuple[str, str]] = {}
    for record in records:
        shape = (record["unit"], record["statistic"])
        first = seen.setdefault(record["metric"], shape)
        if first != shape:
            raise Invalid(f"{record['metric']}: recorded as {first[1]} in {first[0]}, now {shape[1]} in "
                          f"{shape[0]}; a new definition needs a new metric id")


def verify_store(repo: Path, rev: str) -> int:
    """Check every record at `rev`, then that no commit in its history removed a line."""
    schema = load_schema()
    store = read_store(repo, rev)
    ids: set[str] = set()
    records = []
    for path in sorted(store):
        for number, record in enumerate(parse_shard(path, store[path]), 1):
            errors = record_errors(record, schema)
            if errors:
                raise Invalid(f"{path}:{number}: " + "; ".join(errors))
            if shard_of(record) != path:
                raise Invalid(f"{path}:{number}: recorded {record['recorded_at']} belongs in {shard_of(record)}")
            if record["id"] in ids:
                raise Invalid(f"{path}:{number}: duplicate id {record['id']}")
            ids.add(record["id"])
            records.append(record)
    check_series(records)
    check_supersedes(records)
    check_series(fold(records))
    log = git(repo, "log", "--format=commit %H", "--numstat", "--no-renames", rev, "--", f"{SHARD_DIR}/").stdout
    commit = None
    for line in log.decode().splitlines():
        if line.startswith("commit "):
            commit = line.split()[1]
        elif line.strip():
            added, removed, path = line.split("\t", 2)
            if removed != "0":
                raise Invalid(f"commit {commit} removed lines from {path}; the record is append-only")
    merges = git(repo, "rev-list", "--merges", "--count", rev).stdout.decode().strip()
    if merges != "0":
        raise Invalid(f"{rev} has {merges} merge commit(s); appends are linear")
    print(f"{rev}: {len(records)} records in {len(store)} shards, {len({r['metric'] for r in records})} metrics; "
          "history append-only")
    return 0


# --- environment -----------------------------------------------------------------------

def detect_env() -> dict:
    """The measuring machine: vCPU, RAM, CPU model, OS and the Rust toolchain."""
    env: dict = {"vcpu": os.cpu_count() or 1}
    try:
        meminfo = Path("/proc/meminfo").read_text()
        kib = int(re.search(r"^MemTotal:\s+(\d+) kB", meminfo, re.M).group(1))
        env["ram_gib"] = round(kib / 1024 / 1024, 1)
    except (OSError, AttributeError):
        pass
    try:
        model = re.search(r"^model name\s*:\s*(.+)$", Path("/proc/cpuinfo").read_text(), re.M)
        if model:
            env["cpu_model"] = model.group(1).strip()
    except OSError:
        pass
    try:
        release = Path("/etc/os-release").read_text()
        name = re.search(r'^PRETTY_NAME="?([^"\n]+)"?', release, re.M)
        env["os"] = name.group(1) if name else platform.platform()
    except OSError:
        env["os"] = platform.platform()
    toolchain = {}
    for tool, args, pattern in (("rust", ["rustc", "-V"], r"rustc (\S+)"),
                                ("bun", ["bun", "--version"], r"(\S+)"),
                                ("python", [sys.executable, "--version"], r"Python (\S+)")):
        try:
            out = subprocess.run(args, capture_output=True, text=True, timeout=20).stdout
        except (OSError, subprocess.TimeoutExpired):
            continue
        match = re.search(pattern, out)
        if match:
            toolchain[tool] = match.group(1)
    if toolchain:
        env["toolchain"] = toolchain
    return env


# --- CLI -------------------------------------------------------------------------------

def _scalar(text: str):
    if text in ("true", "false"):
        return text == "true"
    if re.fullmatch(r"-?\d+", text):
        return int(text)
    if re.fullmatch(r"-?\d+\.\d+([eE][-+]?\d+)?", text):
        return float(text)
    return text


def _pairs(items: list[str], what: str, scalar=_scalar) -> dict:
    out = {}
    for item in items or []:
        key, sep, value = item.partition("=")
        if not sep or not key:
            raise SystemExit(f"--{what} takes KEY=VALUE, got {item!r}")
        out[key] = scalar(value)
    return out


def commit_of(repo: Path, sha: str, date: str | None, ref: str | None) -> dict:
    if sha in ("", "none", "null"):
        commit = {"sha": None, "date": None}
    else:
        shown = git(repo, "show", "-s", "--format=%H%n%cI", sha, check=False)
        if shown.returncode == 0:
            full, committed = shown.stdout.decode().split()
            commit = {"sha": full, "date": date or committed}
        elif re.fullmatch(r"[0-9a-f]{40}", sha) and date:
            commit = {"sha": sha, "date": date}
        else:
            raise SystemExit(f"--commit {sha} is not in {repo}; pass the full sha and --commit-date")
    if ref:
        commit["ref"] = ref
    return commit


def record_from_args(args, repo: Path) -> dict:
    env = json.loads(Path(args.env_file).read_text()) if args.env_file else {}
    if args.detect_env:
        env = {**detect_env(), **env}
    for key in ("vm_type", "vcpu", "ram_gib", "cpu_model", "os"):
        if getattr(args, key) is not None:
            env[key] = getattr(args, key)
    toolchain = {**env.get("toolchain", {}), **_pairs(args.toolchain, "toolchain", str)}
    if toolchain:
        env["toolchain"] = toolchain
    env["runner"] = args.runner
    record = {
        "metric": args.metric, "value": _scalar(args.value) if isinstance(_scalar(args.value), (int, float)) else args.value,
        "unit": args.unit, "statistic": args.statistic,
        "commit": commit_of(repo, args.commit, args.commit_date, args.commit_ref),
        "measured_at": args.measured_at or now_utc(),
        "measured_by": {"name": args.by, **({"url": args.by_url} if args.by_url else {})},
        "env": env, "workload": _pairs(args.workload, "workload"), "caveats": args.caveats,
        "estimate": args.estimate, "source": {"url": args.source, **({"quote": args.quote} if args.quote else {})},
    }
    if args.samples is not None:
        record["samples"] = args.samples
    if args.range:
        record["range"] = {"min": args.range[0], "max": args.range[1]}
    if args.noise_pct is not None:
        record["noise_pct"] = args.noise_pct
    if args.id:
        record["id"] = args.id
    if args.supersedes:
        record["supersedes"] = args.supersedes
    return record


def read_jsonl(path: str) -> list[dict]:
    text = sys.stdin.read() if path == "-" else Path(path).read_text(encoding="utf-8")
    return [json.loads(line) for line in text.splitlines() if line.strip()]


def views_or_none(enabled: bool):
    if not enabled:
        return None
    report = _sibling("report")
    return report.render_files


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0],
                                     formatter_class=argparse.RawDescriptionHelpFormatter, epilog=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    add = sub.add_parser("add", help="append records and push")
    add.add_argument("--repo", default=".", help="a clone whose remote holds the metrics branch")
    add.add_argument("--remote", default="origin")
    add.add_argument("--branch", default=BRANCH)
    add.add_argument("--dry-run", action="store_true", help="build the commit, print it, do not push")
    add.add_argument("--no-report", action="store_true", help="do not refresh README.md and index.html")
    add.add_argument("--file", help="JSONL of records ('-' for stdin) instead of the flags below")
    add.add_argument("--metric")
    add.add_argument("--value")
    add.add_argument("--unit")
    add.add_argument("--statistic", "--stat")
    add.add_argument("--samples", type=int)
    add.add_argument("--range", nargs=2, type=float, metavar=("MIN", "MAX"))
    add.add_argument("--noise-pct", type=float)
    add.add_argument("--commit", help="sha (or rev in --repo) measured; 'none' when the source names none")
    add.add_argument("--commit-date", help="needed when the sha is not in --repo")
    add.add_argument("--commit-ref", help="what the sha is, e.g. 'main' or 'PR #1223 head'")
    add.add_argument("--measured-at", help="ISO 8601 with zone; default now")
    add.add_argument("--by", help="lane, job or person that measured")
    add.add_argument("--by-url")
    add.add_argument("--runner", choices=["boat", "github-hosted", "local", "unknown"])
    add.add_argument("--env-file", help="JSON from `record.py env` on the measuring machine")
    add.add_argument("--detect-env", action="store_true", help="describe this machine (only if it measured)")
    add.add_argument("--vm-type")
    add.add_argument("--vcpu", type=int)
    add.add_argument("--ram-gib", type=float)
    add.add_argument("--cpu-model")
    add.add_argument("--os")
    add.add_argument("--toolchain", action="append", metavar="TOOL=VERSION")
    add.add_argument("--workload", action="append", metavar="KEY=VALUE")
    add.add_argument("--caveats", help="what limits the number")
    add.add_argument("--estimate", action="store_true")
    add.add_argument("--source", help="PR, comment, run or thread URL")
    add.add_argument("--quote")
    add.add_argument("--id", help="deterministic id, so a retry is a no-op")
    add.add_argument("--supersedes", metavar="ID", help="an earlier record of this metric that this one corrects")
    validate = sub.add_parser("validate", help="check JSONL files of complete records")
    validate.add_argument("files", nargs="+")
    verify = sub.add_parser("verify", help="check the whole store and its history")
    verify.add_argument("--repo", default=".")
    verify.add_argument("--remote", default="origin")
    verify.add_argument("--branch", default=BRANCH)
    verify.add_argument("--rev", help="check this rev instead of fetching <remote>/<branch>")
    ids = sub.add_parser("ids", help="list the metric ids already recorded, to reuse rather than reinvent")
    ids.add_argument("--repo", default=".")
    ids.add_argument("--remote", default="origin")
    ids.add_argument("--branch", default=BRANCH)
    sub.add_parser("env", help="print this machine's env as JSON")
    sub.add_parser("schema", help="print the record schema path")
    args = parser.parse_args(argv)

    if args.command == "env":
        print(json.dumps(detect_env(), indent=2))
        return 0
    if args.command == "ids":
        tip = fetch_tip(Path(args.repo), args.remote, args.branch)
        rows = fold(all_records(read_store(Path(args.repo), tip)))
        shapes: dict[str, list] = {}
        for row in rows:
            shapes.setdefault(row["metric"], [row["unit"], row["statistic"], 0])[2] += 1
        aliases = load_aliases()
        for metric, (unit, statistic, count) in sorted(shapes.items()):
            also = [a for a, c in aliases.items() if c == metric]
            print(f"{metric}\t{unit}\t{statistic}\t{count}" + (f"\talias: {', '.join(also)}" if also else ""))
        return 0
    if args.command == "schema":
        print(SCHEMA_PATH)
        return 0
    if args.command == "validate":
        schema = load_schema()
        failed = 0
        for path in args.files:
            for number, record in enumerate(read_jsonl(path), 1):
                for error in record_errors(record, schema):
                    print(f"{path}:{number}: {error}", file=sys.stderr)
                    failed += 1
        return 1 if failed else 0
    if args.command == "verify":
        repo = Path(args.repo)
        rev = args.rev or fetch_tip(repo, args.remote, args.branch)
        if rev is None:
            print(f"{args.remote}/{args.branch} does not exist yet")
            return 0
        try:
            return verify_store(repo, rev)
        except Invalid as error:
            print(f"metrics store invalid: {error}", file=sys.stderr)
            return 1

    repo = Path(args.repo)
    if args.file:
        records = read_jsonl(args.file)
    else:
        needed = ["metric", "value", "unit", "statistic", "commit", "by", "runner", "caveats", "source"]
        missing = [f"--{name.replace('_', '-')}" for name in needed if getattr(args, name) is None]
        if missing:
            parser.error(f"add needs {' '.join(missing)} (or --file)")
        records = [record_from_args(args, repo)]
    recorded_at = now_utc()
    records = [complete(record, recorded_at) for record in records]
    try:
        commit, fresh = append(repo, records, remote=args.remote, branch=args.branch,
                               push=not args.dry_run, views=views_or_none(not args.no_report))
    except Invalid as error:
        print(f"refused: {error}", file=sys.stderr)
        return 1
    if commit is None:
        print(f"nothing new: all {len(records)} record id(s) are already stored")
        return 0
    verb = "built (dry run, not pushed)" if args.dry_run else f"pushed to {args.remote}/{args.branch}"
    print(f"{verb}: {commit} with {len(fresh)} new record(s)")
    for record in fresh:
        print(f"  {record['id']}  {record['metric']} = {record['value']} {record['unit']} ({record['statistic']})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
