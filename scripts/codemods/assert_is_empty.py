#!/usr/bin/env python3
"""Codemod for clippy::assert_is_empty (Rust 1.100).

`assert!(x.is_empty(), ..)` becomes `assert_eq!(x, [], ..)` and `assert!(!x.is_empty(), ..)` becomes
`assert_ne!(x, [], ..)`, so a failure prints the value. Where `x` can't be compared with `[]` (maps,
strings, types without ==, or no inferable element type), the compiler's error marks the site and
`lenfix` switches it to `assert_xx!(x.len(), 0, ..)`.

Only real code is rewritten: the scanner skips string, raw-string, byte-string and char literals and
line and (nested) block comments, so asserts in comments, doc examples and string fixtures stay as
they are (clippy doesn't lint doctests, so nothing would type-check a rewritten one). Only a
condition that is exactly one receiver's `.is_empty()`, optionally negated, is rewritten; anything
else at the top level of the condition (`a == b.is_empty()`, `x || y.is_empty()`, a closure) is left
for a person.

Usage, from the repository root:
  python3 scripts/codemods/assert_is_empty.py rewrite $(git ls-files 'crates/*.rs')
  cargo check --locked --workspace --all-targets --all-features --keep-going \
    --message-format short 2> check.log
  cargo check --locked --manifest-path crates/norn-skeleton-guard/Cargo.toml --all-targets \
    --keep-going --message-format short 2> guard.log
  python3 scripts/codemods/assert_is_empty.py lenfix check.log guard.log   # repeat until 0
  cargo fmt --all
  python3 scripts/codemods/assert_is_empty.py test   # the scanner's own cases

`cargo check` rather than clippy, so a lint error in one crate can't hide its dependents' type
errors, and `--all-features` so feature-gated code (duckdb, fixtures, counters) is checked too.
`lenfix` rewrites only an `assert_xx!(.., [])` whose span contains the error's line and column.

When a branch that predates the Rust 1.100 pin merges with main (demo/robin -> main), resolve the
conflicts as usual, keeping both sides' edits. Where a hunk differs only because one side ran this
codemod (an `assert!(..is_empty())` against its rewrite), take the other side's code. Then run the
steps above on the merged tree: already rewritten asserts are left as they are.
"""
import os, re, sys

# A char literal: 'x', '\n', '\'', '\x41' or '\u{1F600}'. A lone ' is a lifetime or label.
CHAR = re.compile(r"b?'(?:\\(?:u\{[0-9a-fA-F]+\}|x[0-9a-fA-F]{2}|.)|[^\\'\n])'")
# A raw string's opener, with or without the byte or C prefix: r"..", r#".."#, br##".."##.
RAW = re.compile(r'(?:b|c)?r(#*)"')
STRING = re.compile(r'(?:b|c)?"')

def skip_end(text, i):
    """Index just past the literal or comment starting at text[i], or None if none starts there."""
    if text.startswith("//", i):
        end = text.find("\n", i)
        return len(text) if end < 0 else end
    if text.startswith("/*", i):
        depth, j = 0, i
        while j < len(text):
            if text.startswith("/*", j):
                depth, j = depth + 1, j + 2
            elif text.startswith("*/", j):
                depth, j = depth - 1, j + 2
                if depth == 0:
                    return j
            else:
                j += 1
        return len(text)
    # A prefix letter (r, b, c) inside an identifier is part of the name (`bar"`, `r#type`), but a
    # quote always opens a literal: `r#type"…"` is the identifier `r#type`, then a string.
    after_ident = i and (text[i - 1].isalnum() or text[i - 1] == "_")
    raw = None if after_ident else RAW.match(text, i)
    if raw:
        close = '"' + raw.group(1)
        return text.index(close, raw.end()) + len(close)
    string = STRING.match(text, i) if not after_ident or text[i] == '"' else None
    if string:
        j = string.end()
        while text[j] != '"':
            j += 2 if text[j] == "\\" else 1
        return j + 1
    char = CHAR.match(text, i) if not after_ident or text[i] == "'" else None
    if char:
        return char.end()
    return None

def code_positions(text):
    """Yield (index, char) for every character of real code, outside literals and comments."""
    i = 0
    while i < len(text):
        end = skip_end(text, i)
        if end is not None:
            i = end
            continue
        yield i, text[i]
        i += 1

def args_end(text, start):
    """Index of the bracket closing the one at text[start], skipping literals and comments."""
    depth = 0
    for i, c in code_positions_from(text, start):
        if c in "([{":
            depth += 1
        elif c in ")]}":
            depth -= 1
            if depth == 0:
                return i
    raise ValueError("unbalanced")

def code_positions_from(text, start):
    for i, c in code_positions(text[start:]):
        yield start + i, c

def split_top(s):
    """Split macro arguments at top-level commas, skipping literals and comments."""
    cuts, depth = [], 0
    for i, c in code_positions(s):
        if c in "([{":
            depth += 1
        elif c in ")]}":
            depth -= 1
        elif c == "," and depth == 0:
            cuts.append(i)
    bounds = [-1] + cuts + [len(s)]
    return [s[a + 1:b] for a, b in zip(bounds, bounds[1:])]

def has_comment(s):
    """True when s holds a line or block comment outside its literals."""
    i = 0
    while i < len(s):
        end = skip_end(s, i)
        if end is not None:
            if s.startswith(("//", "/*"), i):
                return True
            i = end
        else:
            i += 1
    return False

def top_level(s):
    """s's code at bracket depth 0, with literals, comments and bracketed contents removed."""
    out, depth = [], 0
    for _, c in code_positions(s):
        if c in "([{":
            depth += 1
        elif c in ")]}":
            depth -= 1
        elif depth == 0:
            out.append(c)
    return "".join(out)

# A receiver: a path, method chain or indexing, optionally behind & or *. No operator, closure,
# cast or block at its top level, so `X.is_empty()` is the whole condition.
RECEIVER = re.compile(r"[&*]*\s*[A-Za-z_][A-Za-z0-9_]*(?:\s*(?:\.|::|\?)\s*[A-Za-z0-9_]*|!)*\s*")

def condition(first):
    """(negated, receiver) when `first` is exactly `[!]X.is_empty()`, else None."""
    if has_comment(first):
        return None  # a comment inside the condition: rewriting could move code into it
    body = first.strip()
    neg = body.startswith("!")
    if neg:
        body = body[1:].strip()
    if not body.endswith(".is_empty()"):
        return None
    receiver = body[: -len(".is_empty()")].rstrip()
    shape = top_level(receiver)
    if not receiver or not RECEIVER.fullmatch(shape) or re.search(r"\b(as|if|match|let|move|return|break|continue|loop|while|for|unsafe|async|yield)\b", shape):
        return None
    return neg, receiver

def rewrite(text):
    out, pos, sites = [], 0, 0
    starts = {i for i, c in code_positions(text) if c == "a"}
    for m in re.finditer(r"\bassert!\(", text):
        if m.start() < pos or m.start() not in starts:
            continue
        open_at = m.end() - 1
        close = args_end(text, open_at)
        parts = split_top(text[open_at + 1:close])
        found = condition(parts[0])
        if not found:
            continue
        neg, expr = found
        rest = ",".join(parts[1:])
        macro = "assert_ne" if neg else "assert_eq"
        new = f"{macro}!({expr}, []{',' + rest if rest.strip() else ''})"
        out.append(text[pos:m.start()]); out.append(new); pos = close + 1
        sites += 1
    out.append(text[pos:])
    return "".join(out), sites

def offset(text, line, column):
    """Index of 1-based (line, column) in text, columns counted in characters as rustc does."""
    start = 0
    for _ in range(line - 1):
        start = text.index("\n", start) + 1
    return start + column - 1

def lenfix_text(text, at):
    """Switch the innermost `assert_xx!(X, [])` whose compared pair holds each index in `at` to
    `assert_xx!((X).len(), 0)`. An error in the message arguments, or in a receiver holding a
    comment, is left alone."""
    fixed = 0
    for point in sorted(at, reverse=True):
        starts = {i for i, c in code_positions(text) if c == "a"}
        best = None
        for m in re.finditer(r"\bassert_(eq|ne)!\(", text):
            if m.start() not in starts or m.start() > point:
                continue
            close = args_end(text, m.end() - 1)
            if point > close:
                continue
            best = m  # later starts are nested deeper: the last one holding the point wins
        if best is None:
            continue
        close = args_end(text, best.end() - 1)
        parts = split_top(text[best.end():close])
        if len(parts) < 2 or parts[1].strip() != "[]" or has_comment(parts[0]):
            continue
        if point > best.end() + len(parts[0]) + 1 + len(parts[1]):
            continue  # the error is in the message arguments, not the comparison
        receiver = parts[0].strip()
        simple = RECEIVER.fullmatch(top_level(receiver)) and not receiver.startswith(("&", "*"))
        lead = parts[0][: len(parts[0]) - len(parts[0].lstrip())]
        parts[0] = lead + (receiver if simple else f"({receiver})") + ".len()"
        parts[1] = " 0"
        text = text[:best.end()] + ",".join(parts) + text[close:]
        fixed += 1
    return text, fixed

def lenfix(logs):
    """Apply `lenfix_text` at every type error (E0277/E0282/E0283/E0308/E0369) in the logs."""
    sites = {}
    for log in logs:
        for m in re.finditer(r"^(?:(?P<sf>[^:\s]+\.rs):(?P<sl>\d+):(?P<sc>\d+): error\[E0(?:277|282|283|308|369)\]"
                             r"|error\[E0(?:277|282|283|308|369)\][^\n]*\n\s*--> (?P<lf>[^:\s]+\.rs):(?P<ll>\d+):(?P<lc>\d+))",
                             open(log, errors="replace").read(), re.M):
            f = m.group("sf") or m.group("lf")
            sites.setdefault(f, set()).add((int(m.group("sl") or m.group("ll")), int(m.group("sc") or m.group("lc"))))
    fixed = 0
    for f, points in sites.items():
        path = next((p for p in (f, "crates/norn-skeleton-guard/" + f) if os.path.exists(p)), None)
        if not path:
            continue
        text = open(path).read()
        text, count = lenfix_text(text, {offset(text, line, column) for line, column in points})
        open(path, "w").write(text)
        fixed += count
    print("len-fixed", fixed)

def test():
    cases = [
        ("assert!(v.is_empty());", "assert_eq!(v, []);"),
        ("assert!(!v.is_empty(), \"{v:?}\");", "assert_ne!(v, [], \"{v:?}\");"),
        ("assert!(self.items.lock().unwrap().is_empty());", "assert_eq!(self.items.lock().unwrap(), []);"),
        ("assert!(&a[1..].is_empty());", "assert_eq!(&a[1..], []);"),
        # Not a single receiver: left for a person.
        ("assert!(before == items.is_empty());", None),
        ("assert!(a.is_empty() && b.is_empty());", None),
        ("assert!(x || y.is_empty());", None),
        ("assert!(v.iter().any(|x| x > 1) || w.is_empty());", None),
        # Inside literals and comments: never rewritten.
        ('let s = r#"assert!(v.is_empty());"#;', None),
        ("/* assert!(v.is_empty()); */ let a = 1;", None),
        ("/* outer /* assert!(v.is_empty()); */ still */ let a = 1;", None),
        ("// assert!(v.is_empty());", None),
        ("/// assert!(v.is_empty());", None),
        ('let s = "assert!(v.is_empty())";', None),
        # Literals and comments inside a rewritten assert don't confuse the bracket scanner.
        ('assert!(v.is_empty(), "/* ( */ {}", \')\');', 'assert_eq!(v, [], "/* ( */ {}", \')\');'),
        ("assert!(v.is_empty(), /* ( */ \"x\");", "assert_eq!(v, [], /* ( */ \"x\");"),
        ('assert!(v.is_empty(), r#"a "( b"#);', 'assert_eq!(v, [], r#"a "( b"#);'),
        # A raw identifier followed by a string (Rust 2024 macro input) doesn't derail the scan.
        ('m!(r#type"assert!(xs.is_empty())"); assert!(v.is_empty());', 'm!(r#type"assert!(xs.is_empty())"); assert_eq!(v, []);'),
        # Comments or control flow inside the condition: left for a person.
        ("assert!(ok // .is_empty()\n);", None),
        ("assert!(xs // why\n    .is_empty());", None),
        ("assert!(return(xs).is_empty());", None),
    ]
    bad = 0
    for source, want in cases:
        got, n = rewrite(source)
        want = source if want is None else want
        if got != want:
            bad += 1
            print(f"rewrite {source!r}: got {got!r}, want {want!r}")
    # lenfix touches only the assert whose span holds the error, not the nearest one above it.
    text = "fn t() {\n    assert_eq!(items, []);\n    let x: u8 = \"no\";\n    assert_eq!(map, []);\n}\n"
    got, n = lenfix_text(text, {offset(text, 3, 17)})
    if (got, n) != (text, 0):
        bad += 1; print("lenfix rewrote an assert the error is not in:", got)
    got, n = lenfix_text(text, {offset(text, 4, 16)})
    if n != 1 or "assert_eq!(map.len(), 0)" not in got or "assert_eq!(items, [])" not in got:
        bad += 1; print("lenfix missed the assert the error is in:", got)
    lens = [
        # The innermost assert holding the error is the one rewritten.
        ("assert_eq!(f(|| assert_eq!(map, [])), [])", 27, "assert_eq!(f(|| assert_eq!(map.len(), 0)), [])"),
        # An error in the message arguments leaves the comparison alone.
        ('assert_eq!(xs, [], "{}", bad)', 26, None),
        # A borrowed or indexed receiver keeps its grouping.
        ("assert_eq!(&xs[1..], [])", 12, "assert_eq!((&xs[1..]).len(), 0)"),
        # A receiver holding a comment is left for a person.
        ("assert_eq!(xs // why\n, [])", 12, None),
    ]
    for source, column, want in lens:
        got, n = lenfix_text(source, {offset(source, 1, column)})
        if got != (source if want is None else want):
            bad += 1; print(f"lenfix {source!r}: got {got!r}, want {want!r}")
    total = len(cases) + 2 + len(lens)
    print(f"{total - bad}/{total} cases passed")
    sys.exit(1 if bad else 0)

if __name__ == "__main__":
    command, *paths = sys.argv[1:] or ["help"]
    if command == "rewrite":
        total = 0
        for f in paths:
            new, found = rewrite(open(f).read())
            if found:
                open(f, "w").write(new)
                total += found
        print("rewrote", total)
    elif command == "lenfix":
        lenfix(paths)
    elif command == "test":
        test()
    else:
        sys.exit("usage: assert_is_empty.py rewrite FILE... | lenfix LOG... | test")
