#!/usr/bin/env python3
"""Fold each listed crate's top-level integration tests into one test binary.

Every `tests/<name>.rs` that Cargo builds as its own test binary becomes the
module `<name>` of `tests/suites.rs`, one binary per crate (`<crate>::suites`).
Linking one binary instead of one per file is most of the saving: a test
binary links the crate, its dependencies and the Rust standard library once.

The module is declared with `#[path = "<name>.rs"]`. A `#[path]` file owns its
directory as a crate root does, so its own `mod common;` and `#[path]`
declarations resolve to the same files as before and no test file moves.
`required-features` become `#[cfg(feature = ...)]` on the module. A test file
that names its own root (`crate::x`) is rewritten to `crate::<name>::x`.

Every reference to a folded binary is rewritten with it:

* `cargo test ... -p <crate> --test <name> [FILTER] [-- ARGS]` commands, in
  scripts, docs, test messages and the coverage manifest (through
  `scripts/coverage_manifest.py`, which renames an entry whose identity moves);
* nextest filters `binary_id(<crate>::<name>)` in `.config/*.toml` and
  `scripts/ci/core.sh`, whose `test(=x)` / `test(/^x/)` gain the `<name>::`
  prefix;
* `.config/drop-scenarios.json`, keyed by binary id.

It refuses, and writes nothing, on anything it cannot rewrite exactly: an
unknown test-target key, `harness = false`, a module name that shadows a
dependency, a crate-level-only inner attribute, `crate::` in a shared helper,
a reference form it does not know. Run it twice and the second run changes
nothing. Afterwards run `python3 scripts/record_new_files.py` for provenance.

    python3 scripts/codemods/fold_integration_tests.py          # rewrite
    python3 scripts/codemods/fold_integration_tests.py --check  # exit 1 if anything is unfolded
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SUITE = "suites"
# Every multi-binary workspace crate whose integration tests can fold cleanly.
# Remaining non-folded targets:
# - `norn-checker-wasm::parity`: pinned by name (`--test parity -- --test-threads=1`)
#   in `GATE_FILES` (`scripts/ci/core.sh` and `verification/coverage-v1/validate.py`);
#   its other integration tests fold into `tests/suites.rs`.
# - `norn-skeleton-guard`: standalone `GATE_DIRECTORIES` workspace; `gate_source_errors` refuses
#   `#[path = ...]` in gated sources and `baseline-shrink-only.sh` blocks moving `tests/*.rs`.
CRATES = (
    "norn-api",
    "norn-authoring",
    "norn-builtins",
    "norn-canonical",
    "norn-cell",
    "norn-cell-do",
    "norn-checker",
    "norn-checker-wasm",
    "norn-datalog",
    "norn-decisions",
    "norn-expr",
    "norn-formats",
    "norn-ir",
    "norn-kernel",
    "norn-projections",
    "norn-testkit",
)
# Explicit `[[test]]` binaries that stay standalone while the rest of their
# crate folds into `tests/suites.rs`.
PINNED_TARGETS: frozenset[tuple[str, str]] = frozenset({("norn-checker-wasm", "parity")})
TARGET_KEYS = {"name", "path", "required-features", "cfg", "comments"}
MODULE_ATTRIBUTES = {"allow", "deny", "warn", "forbid", "expect", "cfg", "cfg_attr", "doc"}
RESERVED = {"std", "core", "alloc", "test", "proc_macro", SUITE}
# Direct runs of a test binary, which no command or filter pattern covers:
# (file, crate, binary, old text, new text with `{suite}`).
DIRECT = (
    ("scripts/bench_mapping_trials.py", "norn-cell", "bench_mapping",
     "target/debug/deps/bench_mapping-HASH", "target/debug/deps/{suite}-HASH"),
    ("scripts/bench_mapping_trials.py", "norn-cell", "bench_mapping",
     '[binary, "candidate_mapping_admission_and_rows",', '[binary, "bench_mapping::candidate_mapping_admission_and_rows",'),
)
# Historical snapshots, not live references.
FROZEN = (".github/ci/provenance/", "verification/coverage-v1/manifest/")


class Refused(Exception):
    pass


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

# ---------------------------------------------------------------- crates


class Crate:
    def __init__(self, directory: str):
        self.directory = ROOT / "crates" / directory
        self.manifest_path = self.directory / "Cargo.toml"
        self.text = self.manifest_path.read_text()
        self.manifest = tomllib.loads(self.text)
        self.name = self.manifest["package"]["name"]
        self.tests = self.directory / "tests"
        self.targets = self._targets()

    def _targets(self) -> dict[str, dict]:
        """Every integration-test target Cargo builds, plus any unlisted top-level file.

        Once a crate is folded (`autotests = false`), a new `tests/<name>.rs`
        would build nowhere; it is a target to fold, never silently dropped.
        """
        for path in sorted(self.tests.glob("*/main.rs")):
            raise Refused(f"{path.relative_to(ROOT)}: a directory test binary is not folded")
        explicit = {table["name"]: dict(table) for table in self.manifest.get("test", [])}
        claimed = {table.get("path", f"tests/{name}.rs") for name, table in explicit.items()}
        suites = self.tests / f"{SUITE}.rs"
        existing = parse_suites(suites.read_text()) if suites.exists() else {}
        targets: dict[str, dict] = {}
        if self.manifest["package"].get("autotests", True) or suites.exists():
            in_lib = self.included_by_lib()
            for path in sorted(self.tests.glob("*.rs")):
                if path.stem == SUITE or f"tests/{path.name}" in claimed or path.stem in in_lib:
                    continue
                targets[path.stem] = {"name": path.stem, "auto": True, **existing.get(path.stem, {})}
        targets.update(explicit)
        return targets

    def included_by_lib(self) -> set[str]:
        """Suites a crate compiles into its own lib tests (`norn-cell`'s `src/suites.rs`)."""
        names = set()
        for path in self.directory.glob("src/**/*.rs"):
            names |= set(re.findall(r'#\[path = "(?:\.\./)+tests/([a-z0-9_]+)\.rs"\]', path.read_text()))
        return names

    def foldable(self) -> list[dict]:
        for crate_name, pinned_name in sorted(PINNED_TARGETS):
            if crate_name != self.name:
                continue
            pinned = self.targets.get(pinned_name)
            if pinned is None or pinned.get("auto"):
                raise Refused(
                    f"{self.name}: pinned test target {pinned_name!r} must have an explicit [[test]] table in Cargo.toml"
                )
        folded = []
        for name, target in sorted(self.targets.items()):
            if name == SUITE or (self.name, name) in PINNED_TARGETS:
                continue
            path = target.get("path", f"tests/{name}.rs")
            if path != f"tests/{name}.rs":
                continue  # outside tests/ (Kani harnesses and the like) stays its own target
            unknown = set(target) - TARGET_KEYS - {"auto"}
            if unknown:
                raise Refused(f"{self.name}: test {name} has unsupported keys {sorted(unknown)}")
            if not (self.directory / path).is_file():
                raise Refused(f"{self.name}: test {name} has no file {path}")
            folded.append(target)
        return folded

    def extern_names(self) -> set[str]:
        names = {self.name.replace("-", "_")}
        tables = [self.manifest] + [t for t in self.manifest.get("target", {}).values() if isinstance(t, dict)]
        for table in tables:
            for kind in ("dependencies", "dev-dependencies", "build-dependencies"):
                names |= {key.replace("-", "_") for key in table.get(kind, {})}
        return names


SHARED_SUPPORT_HEADER = "// Shared test support, compiled once for every suite in this binary."


def _consume_header(lines: list[str]) -> int:
    """Consume crate doc comments (`//!`), blank lines, and optional `#![allow(...)]` block."""
    idx = 0
    n = len(lines)
    while idx < n:
        stripped = lines[idx].strip()
        if not stripped or stripped.startswith("//!"):
            idx += 1
            continue
        if re.fullmatch(r"#!\[allow\([^\]\n]*\)\]", stripped):
            idx += 1
            continue
        if stripped == "#![allow(":
            idx += 1
            while idx < n:
                inner = lines[idx].strip()
                if inner == ")]":
                    idx += 1
                    break
                if not (
                    re.fullmatch(r"[A-Za-z_][A-Za-z0-9_:]*,", inner)
                    or re.fullmatch(r'reason = "[^"\\\n]*"(?:,)?', inner)
                ):
                    raise Refused(
                        f"tests/{SUITE}.rs: unexpected line inside `#![allow(...)]` ({inner!r})"
                    )
                idx += 1
            else:
                raise Refused(f"tests/{SUITE}.rs: unclosed `#![allow(...)]` block")
            continue
        break
    return idx


def _validate_suite_entries(rest: list[str]) -> None:
    """Require every non-blank line after the shared-support block to be an exact suite entry.

    Grammar of each suite entry (matching `suites_source` exactly):
        ( `// ...` )*
        ( `#[cfg(...)]` )?
        `#[path = "<name>.rs"]`
        `mod <name>;`
    """
    idx = 0
    n = len(rest)
    seen_suites: set[str] = set()
    while idx < n:
        stripped = rest[idx].strip()
        if not stripped:
            idx += 1
            continue
        while idx < n and rest[idx].startswith("//") and not rest[idx].startswith("//!"):
            idx += 1
        if idx < n and re.fullmatch(r"#\[cfg\([^\]\n]+\)\]", rest[idx]):
            idx += 1
        if idx >= n:
            raise Refused(f"tests/{SUITE}.rs: trailing comments/attributes without a suite module")
        path_match = re.fullmatch(r'#\[path = "([a-z0-9_]+)\.rs"\]', rest[idx])
        if not path_match:
            raise Refused(
                f"tests/{SUITE}.rs: unexpected line outside shared support block ({rest[idx]!r})"
            )
        suite_name = path_match[1]
        if suite_name in seen_suites:
            raise Refused(
                f"tests/{SUITE}.rs: duplicate suite entry `mod {suite_name};` would collapse during regeneration"
            )
        seen_suites.add(suite_name)
        idx += 1
        if idx >= n or rest[idx] != f"mod {suite_name};":
            found = rest[idx] if idx < n else "<eof>"
            raise Refused(
                f"tests/{SUITE}.rs: expected `mod {suite_name};` after `#[path = \"{suite_name}.rs\"]`, got {found!r}"
            )
        idx += 1


def split_suites_sections(text: str) -> tuple[list[str], list[str]]:
    """Split an existing `tests/suites.rs` into `(shared_support_lines, suite_mod_lines)`."""
    # Reject lexical forms that can span lines and desynchronize line-oriented parsing.
    for kind, start, end in _iter_rust_spans(text):
        span = text[start:end]
        if kind == "comment" and span.startswith("/*"):
            raise Refused(f"tests/{SUITE}.rs: block comments are not allowed")
        if kind == "literal" and ("\n" in span or "\\" in span):
            raise Refused(f"tests/{SUITE}.rs: multiline or escaped string literals are not allowed")
    lines = text.splitlines()
    body_start = _consume_header(lines)
    if body_start < len(lines) and lines[body_start] == SHARED_SUPPORT_HEADER:
        end = body_start
        while end < len(lines) and lines[end].strip():
            if lines[end].lstrip().startswith("#!["):
                raise Refused(f"tests/{SUITE}.rs: inner attribute not allowed in shared support block")
            end += 1
        shared = lines[body_start:end]
        rest = lines[end:]
    else:
        shared = []
        rest = lines[body_start:]
    _validate_suite_entries(rest)
    return shared, rest


def parse_shared_support(text: str) -> list[str]:
    """Return the hoisted shared test-support block from an existing `tests/suites.rs`."""
    return split_suites_sections(text)[0]


def shared_support_names(lines: list[str]) -> set[str]:
    """Return module and re-export names bound in the shared support block."""
    names: set[str] = set()
    attr_depth = 0
    for line in lines:
        stripped = line.strip()
        if not stripped or stripped.startswith("//"):
            continue
        cursor = stripped
        if attr_depth > 0:
            # Consume remainder of an open multi-line outer attribute `#[...]`.
            idx = 0
            while idx < len(cursor) and attr_depth > 0:
                if cursor[idx] == "[":
                    attr_depth += 1
                elif cursor[idx] == "]":
                    attr_depth -= 1
                idx += 1
            cursor = cursor[idx:].lstrip()
            if not cursor:
                continue
        while cursor.startswith("#["):
            depth = 0
            idx = 1
            while idx < len(cursor):
                if cursor[idx] == "[":
                    depth += 1
                elif cursor[idx] == "]":
                    depth -= 1
                    if depth == 0:
                        cursor = cursor[idx + 1 :].lstrip()
                        break
                idx += 1
            else:
                attr_depth = depth
                cursor = ""
                break
        if not cursor or cursor.startswith("//"):
            continue
        mod_match = re.match(r"^(?:pub(?:\([^)]*\))?\s+)?mod ([A-Za-z0-9_]+);$", cursor)
        if mod_match:
            names.add(mod_match[1])
            continue
        if re.match(r"^(?:pub(?:\([^)]*\))?\s+)?use\b", cursor):
            if (
                "{" in cursor
                or "}" in cursor
                or "*" in cursor
                or cursor.count(";") != 1
                or not cursor.endswith(";")
            ):
                raise Refused(
                    f"tests/{SUITE}.rs: shared support block only supports single-item `use` statements ({stripped!r})"
                )
            use_as = re.match(r"^(?:pub(?:\([^)]*\))?\s+)?use .+\s+as\s+([A-Za-z0-9_]+);$", cursor)
            if use_as:
                names.add(use_as[1])
                continue
            use_tail = re.match(r"^(?:pub(?:\([^)]*\))?\s+)?use (?:.+::)?([A-Za-z0-9_]+);$", cursor)
            if use_tail:
                names.add(use_tail[1])
                continue
            raise Refused(f"tests/{SUITE}.rs: unparseable `use` in shared support block ({stripped!r})")
        raise Refused(f"tests/{SUITE}.rs: unsupported item in shared support block ({stripped!r})")
    if attr_depth != 0:
        raise Refused(f"tests/{SUITE}.rs: unclosed outer attribute in shared support block")
    return names


def parse_suites(text: str) -> dict[str, dict]:
    """The cfg and comments above each module of an existing `tests/suites.rs`."""
    found: dict[str, dict] = {}
    _, lines = split_suites_sections(text)
    for index, line in enumerate(lines):
        match = re.match(r"^mod ([a-z0-9_]+);$", line)
        if not match:
            continue
        comments, cfg, cursor = [], None, index - 1
        while cursor >= 0 and lines[cursor].startswith(("#[", "//")) and not lines[cursor].startswith("//!"):
            current = lines[cursor]
            if current.startswith("//"):
                comments.insert(0, current)
            elif current.startswith("#[cfg("):
                cfg = current.removeprefix("#[cfg(").removesuffix(")]")
            elif not current.startswith("#[path"):
                raise Refused(f"tests/{SUITE}.rs: unknown attribute {current!r} on mod {match[1]}")
            cursor -= 1
        found[match[1]] = {"cfg": cfg, "comments": comments}
    return found


def module_cfg(target: dict) -> str | None:
    if target.get("cfg"):
        return target["cfg"]
    features = target.get("required-features", [])
    for feature in features:
        if not re.fullmatch(r"[A-Za-z0-9_-]+", feature):
            raise Refused(f"required feature {feature!r} cannot become a cfg")
    if not features:
        return None
    clauses = [f'feature = "{feature}"' for feature in features]
    return clauses[0] if len(clauses) == 1 else f"all({', '.join(clauses)})"


_RUST_RAW_STRING_OPEN = re.compile(r'(?:[bc]r|r)(#*)"')
_RUST_CHAR_LITERAL = re.compile(
    r"b?'(?:\\(?:x[0-9A-Fa-f]{2}|u\{[0-9A-Fa-f_]+\}|.)|[^'\\\n])'"
)
# Fixed baseline of pre-existing folded crates whose `tests/suites.rs` carry the legacy
# `#![allow(dead_code, unreachable_pub, ...)]` block. Newly folded crates
# must stay allow-free; adding `#![allow(...)]` to a non-baseline suite fails `--check`.
LEGACY_SUITE_ALLOW_CRATES: frozenset[str] = frozenset(
    {
        "norn-api",
        "norn-cell",
        "norn-cell-do",
        "norn-checker",
        "norn-decisions",
        "norn-expr",
        "norn-formats",
        "norn-ir",
        "norn-kernel",
        "norn-projections",
    }
)
# Subset of `LEGACY_SUITE_ALLOW_CRATES` whose `tests/suites.rs` still compile a
# module more than once (e.g. DuckDB + DataFusion parity suites). Crates outside
# this set must not allow `clippy::duplicate_mod`.
LEGACY_DUPLICATE_MOD_CRATES: frozenset[str] = frozenset(
    {
        "norn-cell",
        "norn-cell-do",
        "norn-checker",
        "norn-kernel",
    }
)
CODEMOD_SELF_FILES: frozenset[str] = frozenset(
    {
        "scripts/codemods/fold_integration_tests.py",
        "scripts/codemods/test_fold_integration_tests.py",
    }
)


def _is_rust_ident_char(ch: str) -> bool:
    return len(ch) == 1 and (ch == "$" or ("a" + ch).isidentifier())


def _iter_rust_spans(text: str):
    """Yield `(kind, start, end)` slices splitting Rust `"code"`, `"comment"`, and `"literal"` spans."""
    n = len(text)
    index = 0
    code_start = 0

    def flush(end: int):
        nonlocal code_start
        if end > code_start:
            yield "code", code_start, end

    while index < n:
        ch = text[index]
        # Line comment: // ... \n
        if ch == "/" and index + 1 < n and text[index + 1] == "/":
            yield from flush(index)
            end = text.find("\n", index + 2)
            end = n if end < 0 else end
            yield "comment", index, end
            index = end
            code_start = index
            continue
        # Nested block comment: /* ... */
        if ch == "/" and index + 1 < n and text[index + 1] == "*":
            yield from flush(index)
            depth = 1
            cursor = index + 2
            while cursor < n and depth > 0:
                if text[cursor] == "/" and cursor + 1 < n and text[cursor + 1] == "*":
                    depth += 1
                    cursor += 2
                elif text[cursor] == "*" and cursor + 1 < n and text[cursor + 1] == "/":
                    depth -= 1
                    cursor += 2
                else:
                    cursor += 1
            yield "comment", index, cursor
            index = cursor
            code_start = index
            continue
        # Raw string literal: r#"..."#, br#"..."#, cr#"..."#
        prev_is_ident = index > 0 and _is_rust_ident_char(text[index - 1])
        if not prev_is_ident and ch in ("r", "b", "c"):
            raw_match = _RUST_RAW_STRING_OPEN.match(text, index)
            if raw_match is not None:
                yield from flush(index)
                closing = '"' + raw_match.group(1)
                close_pos = text.find(closing, raw_match.end())
                end = n if close_pos < 0 else close_pos + len(closing)
                yield "literal", index, end
                index = end
                code_start = index
                continue
        # Byte/C string literal: b"..." / c"..."
        if not prev_is_ident and ch in ("b", "c") and index + 1 < n and text[index + 1] == '"':
            yield from flush(index)
            cursor = index + 2
            while cursor < n:
                if text[cursor] == "\\":
                    cursor += 2
                elif text[cursor] == '"':
                    cursor += 1
                    break
                else:
                    cursor += 1
            yield "literal", index, cursor
            index = cursor
            code_start = index
            continue
        # Normal string literal: "..."
        if ch == '"':
            yield from flush(index)
            cursor = index + 1
            while cursor < n:
                if text[cursor] == "\\":
                    cursor += 2
                elif text[cursor] == '"':
                    cursor += 1
                    break
                else:
                    cursor += 1
            yield "literal", index, cursor
            index = cursor
            code_start = index
            continue
        # Char or byte char literal (not lifetimes like `'static`): `'a'`, `'\''`, `b'x'`
        if (ch == "'" or (not prev_is_ident and ch == "b" and index + 1 < n and text[index + 1] == "'")):
            char_match = _RUST_CHAR_LITERAL.match(text, index)
            if char_match is not None:
                yield from flush(index)
                yield "literal", index, char_match.end()
                index = char_match.end()
                code_start = index
                continue
        index += 1
    yield from flush(n)


def _tokenize_rust_code(text: str) -> list[tuple[str, int, int]]:
    """Tokenize non-comment Rust spans into `(kind, start, end)` with Unicode ident boundaries."""
    tokens: list[tuple[str, int, int]] = []
    for kind, start, end in _iter_rust_spans(text):
        if kind == "comment":
            continue
        if kind == "literal":
            tokens.append(("literal", start, end))
            continue
        cursor = start
        while cursor < end:
            ch = text[cursor]
            if ch.isspace():
                cursor += 1
            elif ch == ":" and cursor + 1 < end and text[cursor + 1] == ":":
                tokens.append(("::", cursor, cursor + 2))
                cursor += 2
            elif (
                ch == "r"
                and cursor + 2 < end
                and text[cursor + 1] == "#"
                and _is_rust_ident_char(text[cursor + 2])
            ):
                id_start = cursor
                cursor += 3
                while cursor < end and _is_rust_ident_char(text[cursor]):
                    cursor += 1
                tokens.append(("ident", id_start, cursor))
            elif _is_rust_ident_char(ch):
                id_start = cursor
                cursor += 1
                while cursor < end and _is_rust_ident_char(text[cursor]):
                    cursor += 1
                tokens.append(("ident", id_start, cursor))
            else:
                tokens.append(("punct", cursor, cursor + 1))
                cursor += 1
    return tokens


def rewrite_crate_paths(text: str, name: str) -> str:
    """Rewrite Rust code `crate ::` paths to `crate ::<name>::`, leaving comments/literals untouched."""
    tokens = _tokenize_rust_code(text)
    insertions: list[int] = []
    for idx, (kind, start, end) in enumerate(tokens):
        if kind != "ident" or text[start:end] != "crate":
            continue
        if idx + 1 >= len(tokens) or tokens[idx + 1][0] != "::":
            continue
        if (
            idx + 3 < len(tokens)
            and tokens[idx + 2][0] == "ident"
            and text[tokens[idx + 2][1] : tokens[idx + 2][2]] == name
            and tokens[idx + 3][0] == "::"
        ):
            continue
        insertions.append(tokens[idx + 1][2])
    if not insertions:
        return text
    out: list[str] = []
    cursor = 0
    for pos in insertions:
        out.append(text[cursor:pos])
        out.append(f"{name}::")
        cursor = pos
    out.append(text[cursor:])
    return "".join(out)


def has_code_crate_path(text: str) -> bool:
    """Return True when `crate ::` appears in executable Rust code (outside comments/literals)."""
    tokens = _tokenize_rust_code(text)
    for idx, (kind, start, end) in enumerate(tokens):
        if kind == "ident" and text[start:end] == "crate":
            if idx + 1 < len(tokens) and tokens[idx + 1][0] == "::":
                return True
    return False


def _ident_name(word: str) -> str:
    """Strip Rust's `r#` raw-identifier prefix when comparing identifier names."""
    return word[2:] if word.startswith("r#") else word


def _scan_use_statement(
    text: str,
    tokens: list[tuple[str, int, int]],
    use_idx: int,
    name: str,
    *,
    allow_local_binding: bool,
) -> tuple[bool, bool, int]:
    """Scan a `use ...;` statement starting at `use_idx`.

    Returns `(references_extern, binds_name_locally, end_idx)` where `end_idx`
    is the index of the terminating `;` (or last token if malformed).
    """
    cur_at_root = True
    expect_segment = True
    root_stack: list[bool] = []
    # Track path prefix into brace groups so `...::<name>::{self, ...}` binds locally.
    last_ident_was_name = False
    last_ident_at_root = False
    prefix_is_name_stack: list[bool] = []
    binds_locally = False

    cursor = use_idx + 1
    while cursor < len(tokens):
        kind, start, end = tokens[cursor]
        if kind == "punct":
            ch = text[start:end]
            if ch in (")", "]"):
                return False, False, cursor - 1
            if ch == ";":
                if (
                    allow_local_binding
                    and not root_stack
                    and last_ident_was_name
                    and not last_ident_at_root
                ):
                    binds_locally = True
                return False, binds_locally, cursor
            if ch == "{":
                root_stack.append(cur_at_root)
                prefix_is_name_stack.append(
                    last_ident_was_name and not last_ident_at_root and not cur_at_root
                )
                expect_segment = True
                last_ident_was_name = False
                last_ident_at_root = False
            elif ch == "}":
                if root_stack:
                    cur_at_root = root_stack.pop()
                if prefix_is_name_stack:
                    prefix_is_name_stack.pop()
                expect_segment = False
                last_ident_was_name = False
                last_ident_at_root = False
            elif ch == ",":
                if root_stack:
                    cur_at_root = root_stack[-1]
                expect_segment = True
                last_ident_was_name = False
                last_ident_at_root = False
            cursor += 1
            continue
        if kind == "::":
            expect_segment = True
            cursor += 1
            continue
        if kind == "ident":
            word = _ident_name(text[start:end])
            if word == "as":
                # `...::geo as other` or `self as other` does not bind `geo`;
                # `...::other as geo` binds `geo` locally if unattributed at top level.
                last_ident_was_name = False
                last_ident_at_root = False
                expect_segment = False
                if (
                    allow_local_binding
                    and cursor + 1 < len(tokens)
                    and tokens[cursor + 1][0] == "ident"
                    and _ident_name(text[tokens[cursor + 1][1] : tokens[cursor + 1][2]]) == name
                ):
                    binds_locally = True
                cursor += 2
                continue
            if expect_segment:
                if cur_at_root:
                    if word == name:
                        return True, False, cursor
                    last_ident_was_name = False
                    last_ident_at_root = True
                    cur_at_root = False
                else:
                    if (
                        allow_local_binding
                        and word == "self"
                        and prefix_is_name_stack
                        and prefix_is_name_stack[-1]
                    ):
                        nxt_is_as = (
                            cursor + 1 < len(tokens)
                            and tokens[cursor + 1][0] == "ident"
                            and text[tokens[cursor + 1][1] : tokens[cursor + 1][2]] == "as"
                        )
                        if not nxt_is_as:
                            binds_locally = True
                    last_ident_was_name = word == name
                    last_ident_at_root = False
                expect_segment = False
        cursor += 1
    return False, binds_locally, len(tokens) - 1


def uses_extern_crate(text: str, name: str) -> bool:
    """Return True if `text` may resolve `name` to an external crate from the extern prelude.

    Declaring `mod <name>;` in `tests/suites.rs` shadows an identically named dependency in
    `use <name>...` paths across the crate. It is safe only when no test file imports `<name>`
    from the root/extern prelude and every bare `<name>::` use is locally shadowed by an
    unconditional (unattributed) `use <other>::...::<name>` import at file scope (with no
    inline `mod` blocks).
    """
    tokens = _tokenize_rust_code(text)
    locally_imported = False
    has_inline_mod = False
    has_bare_path = False
    brace_depth = 0
    paren_depth = 0
    bracket_depth = 0
    stmt_has_attr = False
    idx = 0
    while idx < len(tokens):
        kind, start, end = tokens[idx]
        if kind == "punct":
            ch = text[start:end]
            at_top = brace_depth == 0 and paren_depth == 0 and bracket_depth == 0
            if at_top and ch == "#":
                # Outer attribute `#[...]` on the upcoming top-level item/use statement.
                # Keep scanning tokens inside `[...]` so attribute paths like `#[geo::some]`
                # are still inspected for external-crate references.
                if (
                    idx + 1 < len(tokens)
                    and tokens[idx + 1][0] == "punct"
                    and text[tokens[idx + 1][1] : tokens[idx + 1][2]] == "["
                ):
                    stmt_has_attr = True
            elif ch == "{":
                brace_depth += 1
            elif ch == "}":
                brace_depth = max(0, brace_depth - 1)
                if brace_depth == 0 and paren_depth == 0 and bracket_depth == 0:
                    stmt_has_attr = False
            elif ch == "(":
                paren_depth += 1
            elif ch == ")":
                paren_depth = max(0, paren_depth - 1)
            elif ch == "[":
                bracket_depth += 1
            elif ch == "]":
                bracket_depth = max(0, bracket_depth - 1)
            elif ch == ";" and at_top:
                stmt_has_attr = False
            idx += 1
            continue
        if kind != "ident":
            idx += 1
            continue
        raw_word = text[start:end]
        word = _ident_name(raw_word)
        if raw_word == "extern":
            if (
                idx + 2 < len(tokens)
                and tokens[idx + 1][0] == "ident"
                and text[tokens[idx + 1][1] : tokens[idx + 1][2]] == "crate"
                and tokens[idx + 2][0] == "ident"
                and _ident_name(text[tokens[idx + 2][1] : tokens[idx + 2][2]]) == name
            ):
                return True
            idx += 1
            continue
        if raw_word == "mod":
            # Inline `mod foo { ... }` starts a new scope where bare `<name>::` reaches extern prelude.
            cursor = idx + 1
            if cursor < len(tokens) and tokens[cursor][0] == "ident":
                if (
                    cursor + 1 < len(tokens)
                    and tokens[cursor + 1][0] == "punct"
                    and text[tokens[cursor + 1][1] : tokens[cursor + 1][2]] == "{"
                ):
                    has_inline_mod = True
            idx += 1
            continue
        if raw_word == "use":
            at_top = brace_depth == 0 and paren_depth == 0 and bracket_depth == 0
            refs_extern, binds_local, end_idx = _scan_use_statement(
                text,
                tokens,
                idx,
                name,
                allow_local_binding=(at_top and not stmt_has_attr),
            )
            if refs_extern:
                return True
            if binds_local:
                locally_imported = True
            if at_top:
                stmt_has_attr = False
            idx = end_idx + 1
            continue
        if word != name:
            idx += 1
            continue
        prev = tokens[idx - 1] if idx > 0 else None
        nxt = tokens[idx + 1] if idx + 1 < len(tokens) else None
        if prev is not None and prev[0] == "::":
            if not (idx >= 2 and tokens[idx - 2][0] == "ident"):
                # Leading `::<name>` explicitly targets the extern prelude.
                return True
        elif nxt is not None and nxt[0] == "::":
            has_bare_path = True
        idx += 1
    if has_bare_path and (not locally_imported or has_inline_mod):
        return True
    return False


def check_file(crate: Crate, name: str) -> str:
    """Validate a folded file and return its text with `crate::` rooted at its module."""
    path = crate.tests / f"{name}.rs"
    text = path.read_text()
    for attribute in re.findall(r"^#!\[\s*([A-Za-z_][A-Za-z0-9_:]*)", text, re.MULTILINE):
        if attribute not in MODULE_ATTRIBUTES:
            raise Refused(f"{path.relative_to(ROOT)}: #![{attribute}] is crate-level only")
    if re.search(r"^\s*(?:#\[macro_export\]|extern\s+crate\b)", text, re.MULTILINE):
        raise Refused(f"{path.relative_to(ROOT)}: macro_export or extern crate needs a person")
    return rewrite_crate_paths(text, name)


def check_helpers(crate: Crate) -> None:
    """A shared helper is compiled once per including suite; `crate::` in it would now differ."""
    for path in sorted(crate.tests.rglob("*.rs")):
        if path.parent == crate.tests:
            continue
        if has_code_crate_path(path.read_text()):
            raise Refused(f"{path.relative_to(ROOT)}: a shared helper names `crate::`")


def _needs_suite_allows(crate: Crate) -> bool:
    """Keep legacy `#![allow(...)]` strictly on the fixed legacy baseline of pre-folded crates."""
    return crate.name in LEGACY_SUITE_ALLOW_CRATES


def _needs_duplicate_mod_allow(crate: Crate) -> bool:
    """Keep `clippy::duplicate_mod` strictly on crates that still have duplicate modules."""
    return crate.name in LEGACY_DUPLICATE_MOD_CRATES


def suites_source(
    crate: Crate,
    folded: list[dict],
    comments: dict[str, list[str]],
    shared_support: list[str] | None = None,
) -> str:
    header = f"""//! `{crate.name}`'s integration tests, built as one test binary.
//!
//! Each `tests/<name>.rs` is the module `<name>` here, so nextest names a test
//! `binary_id({crate.name}::{SUITE})` and `test(=<name>::<test>)`, and
//! `cargo test -p {crate.name} --test {SUITE} <name>::` runs one former binary.
//! A `#[path]` module owns its directory as a crate root does, so each suite's
//! own `mod` and `#[path]` helpers resolve as they did when it was a binary.
//! A new suite is a new module here: `scripts/codemods/fold_integration_tests.py`
//! keeps this list and every reference to it in step.
"""
    if _needs_suite_allows(crate):
        if _needs_duplicate_mod_allow(crate):
            header += """#![allow(
    dead_code,
    unreachable_pub,
    clippy::duplicate_mod,
    reason = "each suite keeps the visibility and the shared helpers it had as its own test binary"
)]
"""
        else:
            header += """#![allow(
    dead_code,
    unreachable_pub,
    reason = "each suite keeps the visibility it had as its own test binary"
)]
"""
    if shared_support:
        header += "\n" + "\n".join(shared_support) + "\n"
    body = []
    for target in folded:
        name = target["name"]
        body.extend(target.get("comments") or comments.get(name, []))
        cfg = module_cfg(target)
        if cfg:
            body.append(f"#[cfg({cfg})]")
        body.append(f'#[path = "{name}.rs"]')
        body.append(f"mod {name};")
    return header + "\n" + "\n".join(body) + "\n"


def edit_manifest(crate: Crate, folded: list[dict]) -> tuple[str, dict[str, list[str]]]:
    """Drop the folded `[[test]]` tables, add `suites`, turn autotests off."""
    names = {target["name"] for target in folded if not target.get("auto")}
    lines = crate.text.splitlines()
    out: list[str] = []
    comments: dict[str, list[str]] = {}
    insert_at = None
    index = 0
    while index < len(lines):
        line = lines[index]
        if line.strip() == "[[test]]":
            end = index + 1
            while end < len(lines) and not lines[end].lstrip().startswith("["):
                end += 1
            if end < len(lines):
                while end > index + 1 and lines[end - 1].lstrip().startswith("#"):
                    end -= 1
            block = tomllib.loads("\n".join(lines[index + 1:end]))
            if block.get("name") in names or block.get("name") == SUITE:
                above = []
                while out and out[-1].startswith("#"):
                    above.insert(0, out.pop())
                comments[block["name"]] = ["// " + c.lstrip("#").strip() for c in above]
                if insert_at is None:
                    insert_at = len(out)
                index = end
                continue
        out.append(line)
        index += 1
    if insert_at is None:
        headers = ("[dependencies]", "[dev-dependencies]", "[build-dependencies]", "[features]", "[lints]", "[target.")
        insert_at = next((i for i, line in enumerate(out) if line.startswith(headers)), len(out))
        while insert_at > 0 and out[insert_at - 1].startswith("#"):
            insert_at -= 1
    out[insert_at:insert_at] = ["[[test]]", f'name = "{SUITE}"', f'path = "tests/{SUITE}.rs"', ""]
    if "autotests" not in crate.manifest["package"]:
        end = next((i for i, line in enumerate(out) if i > 0 and line.startswith("[")), len(out))
        while end > 0 and not out[end - 1].strip():
            end -= 1
        out[end:end] = [
            "# Every integration test is a module of tests/suites.rs, one binary",
            "# (scripts/codemods/fold_integration_tests.py).",
            "autotests = false",
        ]
    return "\n".join(out) + "\n", comments


# ------------------------------------------------------------- references


OPTION_VALUES = {
    "-p", "--package", "--features", "-F", "--target", "--profile", "-j", "--jobs",
    "--manifest-path", "--bin", "--example", "--bench", "--color", "--message-format",
    "--target-dir", "--config", "-Z", "--exclude",
}
TOKEN = r"[A-Za-z0-9_:=./{}$+-]+"
# Between two tokens of one command: spaces, a shell or Rust-string line
# continuation, or Python's implicit concatenation of two string literals.
SEP = r'(?:[ \t]*\\\n[ \t]*|[ \t]*"[ \t]*\n[ \t]*"[ \t]*|[ \t]+)'
COMMAND = re.compile(rf"cargo{SEP}test(?:{SEP}{TOKEN})+")
MD_SEP = r"(?:[ \t]*\n[ \t]*|[ \t]+)"
MD_COMMAND = re.compile(rf"cargo{MD_SEP}test(?:{MD_SEP}{TOKEN})+")


def positional_or_exact(tokens: list[str]) -> bool:
    return "--exact" in tokens


def rewrite_command(command: str, folded: dict[tuple[str, str], str], separator: str) -> str:
    parts = re.split(f"({separator})", command)
    tokens, seps = parts[0::2], [""] + parts[1::2]
    if not all(tokens):
        raise Refused(f"cannot tokenize {command!r}")
    package = None
    tests: list[int] = []
    for index, token in enumerate(tokens):
        if index and tokens[index - 1] in {"-p", "--package"}:
            package = token
        if index and tokens[index - 1] == "--test":
            tests.append(index)
    if package is None:
        if any((crate, tokens[i]) in folded for i in tests for crate, _ in folded):
            raise Refused(f"cannot tell the crate of {command!r}")
        return command
    hits = [i for i in tests if (package, tokens[i]) in folded]
    if not hits:
        return command
    if len(hits) != len(tests):
        raise Refused(f"{command!r} mixes folded and unfolded --test targets")
    modules = [tokens[i] for i in hits]
    first = hits[0]
    # A libtest filter is a substring: `pack::` also selects `ontology_pack::`.
    for module in modules:
        wider = {other for crate, other in folded if crate == package and other != module and other.endswith(module)}
        if not wider <= set(modules) and not positional_or_exact(tokens):
            raise Refused(f"{command!r}: `{module}::` would also select {sorted(wider - set(modules))}")
    keep = [i for i in range(len(tokens)) if not any(i in (h - 1, h) for h in hits[1:])]
    dashdash = next((i for i in keep if tokens[i] == "--"), None)
    positional = None
    for i in keep:
        if i <= first or (dashdash is not None and i > dashdash):
            continue
        if tokens[i].startswith("-") or tokens[i - 1] in OPTION_VALUES or tokens[i - 1] == "--test":
            continue
        positional = i
        break
    after = [i for i in keep if i > dashdash] if dashdash is not None else []
    if dashdash is not None and (
        not after or any(tokens[i] not in {"--exact", "--ignored", "--include-ignored", "--nocapture", "--show-output"} for i in after)
    ):
        raise Refused(f"{command!r}: unsupported libtest arguments after --")
    exact = any(tokens[i] == "--exact" for i in after)
    new = {i: tokens[i] for i in keep}
    new[first] = SUITE
    appended: list[str] = []
    inserted_after: dict[int, list[str]] = {}
    if positional is not None:
        if len(modules) != 1:
            raise Refused(f"{command!r}: a filter over several folded binaries")
        new[positional] = f"{modules[0]}::{tokens[positional]}"
    elif exact:
        raise Refused(f"{command!r}: --exact without a test name")
    elif len(modules) == 1:
        inserted_after[first] = [f"{modules[0]}::"]
    elif dashdash is not None:
        inserted_after[dashdash] = [f"{module}::" for module in modules]
    else:
        appended = ["--", *[f"{module}::" for module in modules]]
    out = []
    for i in keep:
        out.append(seps[i] if out else "")
        out.append(new[i])
        for extra in inserted_after.get(i, []):
            out.extend([" ", extra])
    for extra in appended:
        out.extend([" ", extra])
    return "".join(out)


# A rewrite is allowed only where the command provably ends at the match: anything the token
# pattern could not read (a quoted word, a glob, a substitution, a line continuation) may be
# another libtest filter, which libtest would OR with the inserted module filter. So a command
# that would change must be followed by the end of the text or line, a shell separator, a
# comment, a closing quote, bracket or comma, or a string escape such as `\\n`; anything
# else is refused. In a Markdown code
# span a newline does not end the command.
ENDS = re.compile(r"""[ \t]*(?:\Z|\n|[;&|#])|["'`),\]]|\\[nrt]""")
SPAN_ENDS = re.compile(r"""[ \t]*(?:\Z|`)""")


def rewrite_match(match: re.Match, folded, separator: str, span: bool = False) -> str:
    rewritten = rewrite_command(match[0], folded, separator)
    if rewritten != match[0] and not (SPAN_ENDS if span else ENDS).match(match.string, match.end()):
        raise Refused(f"{match[0]!r} continues with text the codemod cannot read")
    return rewritten


def rewrite_commands(text: str, folded, markdown: bool = False) -> str:
    if not markdown:
        return COMMAND.sub(lambda m: rewrite_match(m, folded, SEP), text)
    # Markdown: fenced blocks are shell; prose is rewritten only inside a code
    # span, where a command may wrap across lines.
    out = []
    for index, part in enumerate(re.split(r"(^```.*?^```)", text, flags=re.MULTILINE | re.DOTALL)):
        if index % 2:
            out.append(COMMAND.sub(lambda m: rewrite_match(m, folded, SEP), part))
        else:
            out.append(re.sub(r"`[^`]+`", lambda span: MD_COMMAND.sub(
                lambda m: rewrite_match(m, folded, MD_SEP, span=True), span[0]), part))
    return "".join(out)


PY_LIST = re.compile(r'\[[^\[\]]*"--test",\s*"[a-z0-9_]+"[^\[\]]*\]')


def rewrite_python_lists(text: str, folded) -> str:
    def one(match):
        body = match[0]
        package = re.search(r'"-p",\s*"([a-z0-9-]+)"', body)
        test = re.search(r'"--test",\s*"([a-z0-9_]+)"', body)
        if not package or (package[1], test[1]) not in folded:
            return body
        module = test[1]
        exact = re.search(r'"--exact",\s*"([A-Za-z0-9_]+)"', body)
        if not exact:
            raise Refused(f"python command list without --exact: {body!r}")
        body = body.replace(test[0], test[0].replace(f'"{module}"', f'"{SUITE}"'))
        return body.replace(exact[0], exact[0].replace(f'"{exact[1]}"', f'"{module}::{exact[1]}"'))
    return PY_LIST.sub(one, text)


def rewrite_filter(text: str, folded) -> str:
    """`binary_id(c::m) & X` -> `binary_id(c::suites) & X'`, X' naming `m::` tests."""
    out, cursor = [], 0
    for match in re.finditer(r"binary_id\(([a-z0-9-]+)::([a-z0-9_]+)\)", text):
        crate, module = match[1], match[2]
        if (crate, module) not in folded:
            continue
        out.append(text[cursor:match.start()])
        rest = text[match.end():]
        conjunction = re.match(r"\s*&\s*", rest)
        if not conjunction:
            out.append(f"(binary_id({crate}::{SUITE}) & test(/^{module}::/))")
            cursor = match.end()
            continue
        start = match.end() + conjunction.end()
        if start >= len(text):
            raise Refused(f"truncated filter after {match[0]}")
        if text[start] == "(":
            depth, end = 0, start
            while end < len(text):
                depth += {"(": 1, ")": -1}.get(text[end], 0)
                end += 1
                if depth == 0:
                    break
            else:
                raise Refused(f"unclosed parenthesised filter after {match[0]}")
        else:
            test = re.match(r"test\([^()]*\)", text[start:])
            if not test:
                raise Refused(f"unknown filter after {match[0]}")
            end = start + test.end()
        operand = text[start:end]

        def prefix(test, module=module):
            body = test[1]
            if body.startswith("="):
                return f"test(={module}::{body[1:]})"
            if body.startswith("/^"):
                return f"test(/^{module}::{body[2:]})"
            raise Refused(f"filter {test[0]} cannot be scoped to {module}::")

        scoped = re.sub(r"test\(([^()]*)\)", prefix, operand)
        if re.search(r"binary_id|package\(|kind\(|platform\(", operand):
            raise Refused(f"nested binary filter under {match[0]}")
        out.append(f"binary_id({crate}::{SUITE}){text[match.end():start]}{scoped}")
        cursor = end
    out.append(text[cursor:])
    return "".join(out)


def rewrite_drops(text: str, folded) -> str:
    data = json.loads(text)
    changed = False
    for drop, binaries in data.items():
        merged: dict[str, list[str]] = {}
        for binary, names in binaries.items():
            crate, _, module = binary.partition("::")
            if (crate, module) in folded:
                merged.setdefault(f"{crate}::{SUITE}", []).extend(f"{module}::{name}" for name in names)
                changed = True
            else:
                merged.setdefault(binary, []).extend(names)
        data[drop] = merged
    return json.dumps(data, indent=2, ensure_ascii=False) + "\n" if changed else text


def rewrite_manifest(folded, write: bool) -> list[str]:
    """Rewrite the coverage entries that hold a folded command, one file at a time.

    A string entry's file name is its identity (`coverage_manifest.entry_id`),
    so it moves with its new value; an object entry keeps its name. Nothing
    else in the manifest is rewritten (a full `write` would renumber orders).
    """
    coverage_manifest = _sibling("coverage_manifest", ROOT / "scripts")
    changed = []
    for path in coverage_manifest.files():
        entry = json.loads(path.read_text())
        if path.parent == coverage_manifest.COVERAGE or set(entry) != {"order", "value"}:
            continue
        value = entry["value"]

        def walk(item):
            if isinstance(item, str):
                return rewrite_commands(item, folded) if "--test" in item else item
            if isinstance(item, list):
                return [walk(inner) for inner in item]
            if isinstance(item, dict):
                return {key: walk(inner) for key, inner in item.items()}
            return item

        new = walk(value)
        if new == value:
            continue
        target = path
        if not isinstance(value, dict) or "id" in value:
            identity = re.fullmatch(r"([0-9a-f]{64})((?:dup[1-9][0-9]*)?)\.json", path.name)
            if identity is None or identity[1] != coverage_manifest.entry_id(value) or identity[2]:
                raise Refused(f"{path.relative_to(ROOT)}: unexpected entry identity")
            target = path.with_name(coverage_manifest.entry_id(new) + ".json")
            if target.exists():
                raise Refused(f"{target.relative_to(ROOT)}: rewritten entry collides")
        changed.append(str(path.relative_to(ROOT)))
        if write:
            if target != path:
                path.unlink()
                changed.append(str(target.relative_to(ROOT)))
            target.write_bytes(coverage_manifest.entry_bytes({"order": entry["order"], "value": new}))
    if write and changed:
        coverage_manifest.load()
    return changed


# ------------------------------------------------------------------ main


def tracked() -> list[str]:
    listing = subprocess.check_output(["git", "ls-files", "-z"], cwd=ROOT)
    return [path for path in listing.decode().split("\0") if path]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n", 1)[0])
    parser.add_argument("--check", action="store_true", help="write nothing; exit 1 if anything would change")
    arguments = parser.parse_args()

    writes: dict[Path, str] = {}
    folded: dict[tuple[str, str], str] = {}
    for directory in CRATES:
        crate = Crate(directory)
        targets = crate.foldable()
        if not targets:
            continue
        check_helpers(crate)
        crate_ident = crate.name.replace("-", "_")
        hard_reserved = RESERVED | {crate_ident}
        externs = crate.extern_names()
        test_sources = {
            path: path.read_text()
            for path in sorted(crate.tests.rglob("*.rs"))
            if path.name != f"{SUITE}.rs"
        }
        suites = crate.tests / f"{SUITE}.rs"
        shared_support = parse_shared_support(suites.read_text()) if suites.exists() else []
        support_names = shared_support_names(shared_support)
        for target in targets:
            name = target["name"]
            if name in hard_reserved or name in support_names:
                raise Refused(f"{crate.name}: module {name} would shadow a reserved, crate, or shared-support name")
            if name in externs and any(uses_extern_crate(src, name) for src in test_sources.values()):
                raise Refused(f"{crate.name}: module {name} would shadow a used extern crate")
            folded[(crate.name, name)] = crate.name
            source = check_file(crate, name)
            path = crate.tests / f"{name}.rs"
            if source != path.read_text():
                writes[path] = source
        manifest, comments = edit_manifest(crate, targets)
        writes[crate.manifest_path] = manifest
        writes[suites] = suites_source(crate, targets, comments, shared_support=shared_support)

    for relative in tracked():
        if relative.startswith(FROZEN) or relative in CODEMOD_SELF_FILES:
            continue
        path = ROOT / relative
        if path in writes or not path.is_file() or path.suffix not in {".rs", ".md", ".sh", ".py", ".json", ".toml", ".ts", ".yml", ".yaml", ".txt"}:
            continue
        try:
            text = path.read_text()
        except UnicodeDecodeError:
            continue
        new = text
        if "--test" in new:
            new = rewrite_commands(new, folded, markdown=path.suffix == ".md")
            if path.suffix == ".py":
                new = rewrite_python_lists(new, folded)
        if "binary_id(" in new:
            new = rewrite_filter(new, folded)
        if relative == ".config/drop-scenarios.json":
            new = rewrite_drops(new, folded)
        for file, crate, binary, old, replacement in DIRECT:
            if file == relative and (crate, binary) in folded:
                replacement = replacement.format(suite=SUITE)
                if old not in new and replacement not in new:
                    raise Refused(f"{relative}: expected {old!r}")
                new = new.replace(old, replacement)
        if new != text:
            writes[path] = new

    stale = [path for path, text in writes.items() if not path.exists() or path.read_text() != text]
    if arguments.check:
        pending = [str(path.relative_to(ROOT)) for path in stale] + rewrite_manifest(folded, write=False)
        for path in pending:
            print(f"would rewrite {path}")
        if pending:
            print("run python3 scripts/codemods/fold_integration_tests.py, then python3 scripts/record_new_files.py")
        return 1 if pending else 0
    for path in stale:
        path.write_text(writes[path])
        print(f"rewrote {path.relative_to(ROOT)}")
    for path in rewrite_manifest(folded, write=True):
        print(f"rewrote {path}")
    print(f"folded {len(folded)} test binaries of {len({c for c, _ in folded})} crates into one each")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Refused as error:
        print(f"fold_integration_tests: refused: {error}", file=sys.stderr)
        sys.exit(2)
