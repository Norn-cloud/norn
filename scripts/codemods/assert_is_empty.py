#!/usr/bin/env python3
"""Codemod for clippy::assert_is_empty (Rust 1.100).

`assert!(x.is_empty(), ..)` becomes `assert_eq!(x, [], ..)` and `assert!(!x.is_empty(), ..)` becomes
`assert_ne!(x, [], ..)`, so a failure prints the value. Where `x` can't be compared with `[]` (maps,
strings, types without ==, or no inferable element type), the compiler's error marks the site and
`lenfix` switches it to `assert_xx!(x.len(), 0, ..)`.

Usage, from the repository root:
  python3 scripts/codemods/assert_is_empty.py rewrite $(git ls-files 'crates/*.rs')
  cargo clippy --locked --workspace --all-targets --keep-going -- -D warnings 2> clippy.log
  python3 scripts/codemods/assert_is_empty.py lenfix clippy.log   # repeat these two until 0
  cargo fmt --all

Re-run it, rather than resolving conflicts by hand, when a branch that predates the Rust 1.100
pin merges with main (demo/robin -> main): take the branch's side of a conflicting crate file,
then run the steps above on the merged tree.
"""
import re, sys

def args_end(text, start):
    """Index of the ')' closing the '(' at text[start], skipping strings and chars."""
    depth, i = 0, start
    while i < len(text):
        c = text[i]
        if c == '"':
            i += 1
            while text[i] != '"':
                i += 2 if text[i] == "\\" else 1
        elif c == "'" and re.match(r"'(\\.|[^\\'])'", text[i:i + 4]):
            i += text[i + 1:].index("'") + 1
        elif c in "([{":
            depth += 1
        elif c in ")]}":
            depth -= 1
            if depth == 0:
                return i
        i += 1
    raise ValueError("unbalanced")

def split_top(s):
    parts, depth, cur, i = [], 0, "", 0
    while i < len(s):
        c = s[i]
        if c == '"':
            j = i + 1
            while s[j] != '"':
                j += 2 if s[j] == "\\" else 1
            cur += s[i:j + 1]; i = j + 1; continue
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
