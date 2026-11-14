#!/usr/bin/env python3
"""Static test counts of a commit, as metric records (JSONL).

Counts occurrences of the text `#[test]` in the tracked Rust sources of the Norn-owned
crates (`crates/`), read with `git grep` at one commit, so the number does not depend on the
machine. This is the method of the first baseline (3,279 at 5327e2d084), kept so the series
stays comparable. It counts declared tests, not executed ones: proptest cases, doctests,
`#[ignore]` and cfg-gated tests are not told apart, and the text inside a comment or string
counts too.

    tests.rust.count                 `#[test]` occurrences under crates/
    tests.rust.crate.<crate>.count   the same, per crate directory
    tests.rust.tokio.count           lines that are a #[tokio::test] attribute
    tests.rust.proptest_blocks.count lines that open a `proptest! {` block

    python3 scripts/metrics/count_tests.py [--rev HEAD] > tests.jsonl
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import re
import subprocess
import sys
from collections import Counter

PATTERNS = {
    "test": "#[test]",
    "tokio": r"^\s*#\[tokio::test(\(.*\))?\]\s*$",
    "proptest": r"^\s*(proptest::)?proptest!\s*\{",
}
CAVEATS = ("Static count at one commit with git grep over tracked .rs files under crates/ (vendor/ and "
           "third_party/ excluded): #[test] counts text occurrences, the baseline's method; tokio and proptest "
           "count attribute/block lines. Declared tests, not executed ones: proptest cases, doctests, #[ignore] "
           "and cfg-gated tests are not told apart. Machine-independent.")


def grep(repo: str, rev: str, pattern: str, fixed: bool = False) -> Counter:
    """Matches per crate directory: every occurrence of a fixed string, or every matching line."""
    mode = ["-o", "-F"] if fixed else ["-E"]
    out = subprocess.run(["git", "-C", repo, "grep", "--no-color", *mode, pattern, rev, "--",
                          "crates/**/*.rs", "crates/*.rs"], capture_output=True, text=True)
    if out.returncode not in (0, 1):
        raise SystemExit(out.stderr)
    counts: Counter = Counter()
    for line in out.stdout.splitlines():
        path = line.split(":", 2)[1]
        counts[path.split("/")[1]] += 1
    return counts


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--repo", default=".")
    parser.add_argument("--rev", default="HEAD")
    parser.add_argument("--by", default="metrics-nightly")
    parser.add_argument("--by-url", help="the run that counted")
    parser.add_argument("--ref", default="main")
    args = parser.parse_args(argv)
    sha, date = subprocess.run(["git", "-C", args.repo, "show", "-s", "--format=%H%n%cI", args.rev],
                               capture_output=True, text=True, check=True).stdout.split()
    now = dt.datetime.now(dt.timezone.utc).replace(microsecond=0).isoformat().replace("+00:00", "Z")
    tests = grep(args.repo, sha, PATTERNS["test"], fixed=True)
    tokio, blocks = (grep(args.repo, sha, PATTERNS[k]) for k in ("tokio", "proptest"))
    values = {"tests.rust.count": sum(tests.values()), "tests.rust.tokio.count": sum(tokio.values()),
              "tests.rust.proptest_blocks.count": sum(blocks.values())}
    for crate, count in sorted(tests.items()):
        if re.fullmatch(r"[a-z0-9][a-z0-9_-]*", crate):
            values[f"tests.rust.crate.{crate}.count"] = count
    measured_by = {"name": args.by, **({"url": args.by_url} if args.by_url else {})}
    for metric, value in values.items():
        print(json.dumps({
            "id": f"{metric}/{sha}", "metric": metric, "value": value, "unit": "count", "statistic": "single",
            "commit": {"sha": sha, "date": date, "ref": args.ref}, "measured_at": now, "measured_by": measured_by,
            "env": {"runner": "local" if not args.by_url else "github-hosted"},
            "workload": {"method": "git grep", "scope": "crates/"}, "caveats": CAVEATS, "estimate": False,
            "source": {"url": args.by_url or f"https://github.com/Norn-cloud/norn/commit/{sha}"},
        }, sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main())
