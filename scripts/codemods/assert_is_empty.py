#!/usr/bin/env python3
"""Codemod for clippy::assert_is_empty (Rust 1.100).

`assert!(x.is_empty(), ..)` becomes `assert_eq!(x, [], ..)` and `assert!(!x.is_empty(), ..)` becomes
`assert_ne!(x, [], ..)`, so a failure prints the value. Where `x` can't be compared with `[]` (maps,
strings, types without ==, or no inferable element type), the compiler's error marks the site and
`lenfix` switches it to `assert_xx!(x.len(), 0, ..)`. Asserts on comment lines (doc examples
included) are left as they are: clippy doesn't lint doctests, so nothing would type-check them.

Usage, from the repository root:
  python3 scripts/codemods/assert_is_empty.py rewrite $(git ls-files 'crates/*.rs')
  cargo check --locked --workspace --all-targets --all-features --keep-going 2> check.log
  cargo check --locked --manifest-path crates/norn-skeleton-guard/Cargo.toml --all-targets \
    --keep-going 2> guard.log
  python3 scripts/codemods/assert_is_empty.py lenfix check.log guard.log   # repeat until 0
  cargo fmt --all

`cargo check` rather than clippy, so a lint error in one crate can't hide its dependents' type
errors, and `--all-features` so feature-gated code (duckdb, fixtures, counters) is checked too.

Re-run it, rather than resolving conflicts by hand, when a branch that predates the Rust 1.100
pin merges with main (demo/robin -> main): take the branch's side of a conflicting crate file,
then run the steps above on the merged tree.
"""
import re, sys

# A char literal: 'x', '\n', '\'', '\x41' or '\u{1F600}'. A lone ' is a lifetime or label.
CHAR = re.compile(r"'(?:\\(?:u\{[0-9a-fA-F]+\}|x[0-9a-fA-F]{2}|.)|[^\\'])'")
# A raw string's opener, with or without the byte prefix: r"..", r#".."#, br##".."##.
RAW = re.compile(r'b?r(#*)"')

def literal_end(text, i):
    """Index just past the string or char literal starting at text[i], or None if none starts there."""
    if not (i and (text[i - 1].isalnum() or text[i - 1] == "_")):
        raw = RAW.match(text, i)
        if raw:
            close = '"' + raw.group(1)
            return text.index(close, raw.end()) + len(close)
    if text[i] == '"':
        j = i + 1
        while text[j] != '"':
            j += 2 if text[j] == "\\" else 1
        return j + 1
    if text[i] == "'":
        char = CHAR.match(text, i)
        if char:
            return char.end()
    return None

def args_end(text, start):
    """Index of the ')' closing the '(' at text[start], skipping string and char literals."""
    depth, i = 0, start
    while i < len(text):
        end = literal_end(text, i)
        if end is not None:
            i = end
            continue
        c = text[i]
        if c in "([{":
            depth += 1
        elif c in ")]}":
            depth -= 1
            if depth == 0:
                return i
        i += 1
    raise ValueError("unbalanced")

def split_top(s):
    """Split macro arguments at top-level commas, skipping string and char literals."""
    parts, depth, cur, i = [], 0, "", 0
    while i < len(s):
        end = literal_end(s, i)
        if end is not None:
            cur += s[i:end]; i = end; continue
        c = s[i]
        if c in "([{": depth += 1
        if c in ")]}": depth -= 1
        if c == "," and depth == 0:
            parts.append(cur); cur = ""
        else:
            cur += c
        i += 1
    parts.append(cur)
    return parts

def rewrite(text, use_len_at=frozenset()):
    out, pos, sites = [], 0, []
    for m in re.finditer(r"\bassert!\(", text):
        if m.start() < pos:
            continue
        # Comments, doc examples included, are left alone: clippy doesn't lint doctests, so a
        # rewritten one could stop compiling with nothing to report it before the test run.
        if "//" in text[text.rfind("\n", 0, m.start()) + 1:m.start()]:
            continue
        open_at = m.end() - 1
        close = args_end(text, open_at)
        inner = text[open_at + 1:close]
        parts = split_top(inner)
        first = parts[0].strip()
        neg = first.startswith("!")
        body = first[1:].strip() if neg else first
        mm = re.fullmatch(r"(?s)(.+)\.is_empty\(\)", body)
        if not mm or "&&" in body or "||" in body:
            continue
        expr = mm.group(1).strip()
        line = text.count("\n", 0, m.start()) + 1
        rest = ",".join(parts[1:])
        if line in use_len_at:
            new = f"{'assert_ne' if neg else 'assert_eq'}!({expr}.len(), 0{',' + rest if rest.strip() else ''})"
        else:
            new = f"{'assert_ne' if neg else 'assert_eq'}!({expr}, []{',' + rest if rest.strip() else ''})"
        out.append(text[pos:m.start()]); out.append(new); pos = close + 1
        sites.append(line)
    out.append(text[pos:])
    return "".join(out), sites


def lenfix(logs):
    """Switch each `assert_xx!(X, [])` that a type error points at to `assert_xx!(X.len(), 0)`."""
    import os
    sites = {}
    for log in logs:
        text = open(log, errors="replace").read()
        for block in re.split(r"\n(?=error)", text):
            if not re.match(r"error\[E0(277|282|283|308|369)\]", block):
                continue
            m = re.search(r"--> ([^:\s]+\.rs):(\d+):", block)
            if m:
                sites.setdefault(m.group(1), set()).add(int(m.group(2)))
    fixed = 0
    for f, lines in sites.items():
        path = next((p for p in (f, "crates/norn-skeleton-guard/" + f) if os.path.exists(p)), None)
        if not path:
            continue
        t = open(path).read()
        for line in sorted(lines, reverse=True):
            m = None
            for candidate in re.finditer(r"\bassert_(eq|ne)!\(", t):
                if t.count("\n", 0, candidate.start()) + 1 <= line:
                    m = candidate
            if not m:
                continue
            close = args_end(t, m.end() - 1)
            parts = split_top(t[m.end():close])
            if len(parts) < 2 or parts[1].strip() != "[]":
                continue
            parts[0] = parts[0].rstrip() + ".len()"
            parts[1] = " 0"
            t = t[:m.end()] + ",".join(parts) + t[close:]
            fixed += 1
        open(path, "w").write(t)
    print("len-fixed", fixed)


if __name__ == "__main__":
    command, *paths = sys.argv[1:] or ["help"]
    if command == "rewrite":
        total = 0
        for f in paths:
            new, found = rewrite(open(f).read())
            if found:
                open(f, "w").write(new)
                total += len(found)
        print("rewrote", total)
    elif command == "lenfix":
        lenfix(paths)
    else:
        sys.exit("usage: assert_is_empty.py rewrite FILE... | lenfix LOG...")
