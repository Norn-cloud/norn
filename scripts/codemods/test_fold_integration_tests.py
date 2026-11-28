#!/usr/bin/env python3
"""Unit tests for scripts/codemods/fold_integration_tests.py."""

from __future__ import annotations

import importlib.util
import json
import tempfile
import unittest
from pathlib import Path

_SPEC = importlib.util.spec_from_file_location(
    "fold_integration_tests", Path(__file__).with_name("fold_integration_tests.py")
)
assert _SPEC is not None and _SPEC.loader is not None
fold = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(fold)


class RustCratePathRewriteTests(unittest.TestCase):
    def test_rewrites_code_paths_and_is_idempotent(self) -> None:
        src = "use crate::helpers::setup;\nfn f() { crate::other::run(); crate::suite_a::done(); }\n"
        once = fold.rewrite_crate_paths(src, "suite_a")
        self.assertEqual(
            once,
            "use crate::suite_a::helpers::setup;\n"
            "fn f() { crate::suite_a::other::run(); crate::suite_a::done(); }\n",
        )
        self.assertEqual(fold.rewrite_crate_paths(once, "suite_a"), once)

    def test_preserves_crate_paths_inside_comments_strings_raw_strings_and_chars(self) -> None:
        fixture = (
            "// Valid qualified `impl crate::tzdb::TzdbBundle` produces zero violations.\n"
            "/* nested /* crate::inside_nested */ crate::inside_block */\n"
            'const FIXTURE: &str = "impl crate::tzdb::TzdbBundle { \\"crate::escaped\\" }";\n'
            'const RAW: &str = r#"impl crate::raw::Thing { "crate::still_raw" }"#;\n'
            'const BYTES: &[u8] = b"crate::bytes";\n'
            'const RAW_BYTES: &[u8] = br##"crate::raw_bytes"# still raw"##;\n'
            "const QUOTE: char = '\"';\n"
            "use crate::real_helper::ok;\n"
        )
        rewritten = fold.rewrite_crate_paths(fixture, "skeleton")
        self.assertIn("`impl crate::tzdb::TzdbBundle`", rewritten)
        self.assertIn("/* nested /* crate::inside_nested */ crate::inside_block */", rewritten)
        self.assertIn('"impl crate::tzdb::TzdbBundle { \\"crate::escaped\\" }"', rewritten)
        self.assertIn('r#"impl crate::raw::Thing { "crate::still_raw" }"#', rewritten)
        self.assertIn('b"crate::bytes"', rewritten)
        self.assertIn('br##"crate::raw_bytes"# still raw"##', rewritten)
        self.assertIn("use crate::skeleton::real_helper::ok;", rewritten)
        self.assertNotIn("crate::skeleton::tzdb", rewritten)

    def test_has_code_crate_path_ignores_literals_and_comments(self) -> None:
        only_literals = (
            '// crate::comment\n'
            'const S: &str = "crate::string";\n'
            'const R: &str = r#"crate::raw"#;\n'
        )
        self.assertFalse(fold.has_code_crate_path(only_literals))
        self.assertTrue(fold.has_code_crate_path("let _ = crate::helper();\n"))

    def test_lifetime_does_not_swallow_following_code(self) -> None:
        src = "fn f<'a>(x: &'a str) -> &'a str { let _ = crate::util::id(x); x }\n"
        self.assertEqual(
            fold.rewrite_crate_paths(src, "alpha"),
            "fn f<'a>(x: &'a str) -> &'a str { let _ = crate::alpha::util::id(x); x }\n",
        )

    def test_handles_spaced_commented_crate_paths_and_unicode_identifiers(self) -> None:
        spaced = (
            "use crate ::helper;\n"
            "use crate /* comment */ :: /* inner */ nested::item;\n"
            "use crate /* c */ :: alpha :: already_rooted;\n"
            "use écrate::unicode_ident;\n"
            "use e\u0301crate::combining_prefix;\n"
            "use crate\u0301::combining_suffix;\n"
        )
        self.assertTrue(fold.has_code_crate_path("use crate ::helper;\n"))
        self.assertTrue(fold.has_code_crate_path("use crate /* c */ ::helper;\n"))
        self.assertFalse(fold.has_code_crate_path("use écrate::unicode_ident;\n"))
        self.assertFalse(fold.has_code_crate_path("use e\u0301crate::combining_prefix;\n"))
        self.assertFalse(fold.has_code_crate_path("use crate\u0301::combining_suffix;\n"))
        rewritten = fold.rewrite_crate_paths(spaced, "alpha")
        self.assertEqual(
            rewritten,
            "use crate ::alpha::helper;\n"
            "use crate /* comment */ ::alpha:: /* inner */ nested::item;\n"
            "use crate /* c */ :: alpha :: already_rooted;\n"
            "use écrate::unicode_ident;\n"
            "use e\u0301crate::combining_prefix;\n"
            "use crate\u0301::combining_suffix;\n",
        )
        self.assertEqual(fold.rewrite_crate_paths(rewritten, "alpha"), rewritten)


class AllowRatchetTests(unittest.TestCase):
    def test_new_folded_crates_refuse_legacy_suite_allows(self) -> None:
        for crate_dir in (
            "norn-authoring",
            "norn-builtins",
            "norn-canonical",
            "norn-checker-wasm",
            "norn-datalog",
            "norn-testkit",
        ):
            crate = fold.Crate(crate_dir)
            self.assertFalse(fold._needs_suite_allows(crate))
            generated = fold.suites_source(crate, crate.foldable(), {})
            self.assertNotIn("#![allow(", generated)
        self.assertTrue(fold._needs_suite_allows(fold.Crate("norn-api")))

    def test_deduplicated_crates_drop_duplicate_mod_allow(self) -> None:
        for crate_dir in (
            "norn-api",
            "norn-decisions",
            "norn-expr",
            "norn-formats",
            "norn-ir",
            "norn-projections",
        ):
            crate = fold.Crate(crate_dir)
            self.assertFalse(fold._needs_duplicate_mod_allow(crate))
            generated = fold.suites_source(crate, crate.foldable(), {})
            self.assertNotIn("clippy::duplicate_mod", generated)
        for crate_dir in ("norn-cell", "norn-cell-do", "norn-checker", "norn-kernel"):
            crate = fold.Crate(crate_dir)
            self.assertTrue(fold._needs_duplicate_mod_allow(crate))
            generated = fold.suites_source(crate, crate.foldable(), {})
            self.assertIn("clippy::duplicate_mod", generated)

    def test_pinned_target_stays_outside_foldable_targets(self) -> None:
        crate = fold.Crate("norn-checker-wasm")
        names = [target["name"] for target in crate.foldable()]
        self.assertNotIn("parity", names)
        self.assertEqual(names, ["admit_question", "restatement"])

    def test_shared_support_block_roundtrips_and_rejects_inner_attrs(self) -> None:
        sample = (
            "//! header\n"
            "#![allow(\n"
            "    dead_code,\n"
            "    unreachable_pub,\n"
            "    reason = \"each suite keeps the visibility it had as its own test binary\"\n"
            ")]\n\n"
            f"{fold.SHARED_SUPPORT_HEADER}\n"
            "#[allow(\n"
            "    clippy::expect_used,\n"
            "    reason = \"shared fixture\"\n"
            ")]\n"
            "#[path = \"support/hosted_read.rs\"]\n"
            "mod hosted_read;\n"
            "use hosted_read::Mapping;\n\n"
            "#[cfg(feature = \"native\")]\n"
            "#[path = \"alpha.rs\"]\n"
            "mod alpha;\n"
        )
        support = fold.parse_shared_support(sample)
        self.assertEqual(support[0], fold.SHARED_SUPPORT_HEADER)
        self.assertEqual(fold.shared_support_names(support), {"hosted_read", "Mapping"})
        parsed = fold.parse_suites(sample)
        self.assertEqual(list(parsed.keys()), ["alpha"])
        self.assertEqual(parsed["alpha"]["cfg"], 'feature = "native"')

        bad = f"{fold.SHARED_SUPPORT_HEADER}\n#![allow(warnings)]\nmod support;\n"
        with self.assertRaises(fold.Refused):
            fold.parse_shared_support(bad)

    def test_uses_extern_crate_detects_real_shadowing_but_allows_local_module_import(self) -> None:
        self.assertTrue(fold.uses_extern_crate("use geo::Point;\n", "geo"))
        self.assertTrue(fold.uses_extern_crate("use ::geo::Point;\n", "geo"))
        self.assertTrue(fold.uses_extern_crate("use r#geo;\n", "geo"))
        self.assertTrue(fold.uses_extern_crate("use ::r#geo::Point;\n", "geo"))
        self.assertTrue(fold.uses_extern_crate("use {geo};\n", "geo"))
        self.assertTrue(fold.uses_extern_crate("use {std::fmt, r#geo::Point};\n", "geo"))
        self.assertTrue(fold.uses_extern_crate("use ::{std::fmt, {geo}};\n", "geo"))
        self.assertTrue(fold.uses_extern_crate("extern crate geo;\n", "geo"))
        self.assertTrue(fold.uses_extern_crate("fn f() { let _ = geo::Point::new(0.0, 0.0); }\n", "geo"))
        self.assertTrue(fold.uses_extern_crate("fn f() { let _ = r#geo::Point::new(0.0, 0.0); }\n", "geo"))
        self.assertTrue(
            fold.uses_extern_crate(
                "#[cfg(any())]\nuse norn_builtins::geo;\nfn f() { let _ = geo::Point::new(0.0, 0.0); }\n",
                "geo",
            )
        )
        self.assertTrue(
            fold.uses_extern_crate(
                "const S: &str = stringify!(use foo::bar as geo;);\nfn f() { let _ = geo::Point::new(0.0, 0.0); }\n",
                "geo",
            )
        )
        self.assertTrue(
            fold.uses_extern_crate(
                "#[geo::some]\nfn f() {}\n",
                "geo",
            )
        )
        self.assertTrue(
            fold.uses_extern_crate(
                "use norn_builtins::geo::{self as g};\nfn f() { let _ = geo::Point::new(0.0, 0.0); }\n",
                "geo",
            )
        )
        self.assertTrue(
            fold.uses_extern_crate(
                "use norn_builtins::geo::{self, Foo};\nmod inner { fn f() { geo::call(); } }\n",
                "geo",
            )
        )
        self.assertFalse(
            fold.uses_extern_crate(
                "use norn_builtins::geo::{self, Foo};\nfn f() { geo::call(); }\n",
                "geo",
            )
        )
        self.assertFalse(
            fold.uses_extern_crate(
                "#![allow(unused)]\nuse norn_builtins::geo::{self, Foo};\nfn f() { geo::call(); }\n",
                "geo",
            )
        )
        self.assertTrue(
            fold.uses_extern_crate(
                "fn a() { use norn_builtins::geo; }\nfn b() { let _ = geo::Point::new(0.0, 0.0); }\n",
                "geo",
            )
        )
        self.assertFalse(fold.uses_extern_crate("use std::path::Path;\nfn f() {}\n", "geo"))


class CommandAndFilterRewriteTests(unittest.TestCase):
    def test_rewrite_command_filter_and_drops(self) -> None:
        folded = {("norn-testkit", "harness"): "norn-testkit"}
        self.assertEqual(
            fold.rewrite_command("cargo test -p norn-testkit --test harness smoke -- --exact", folded, fold.SEP),
            "cargo test -p norn-testkit --test suites harness::smoke -- --exact",
        )
        self.assertEqual(
            fold.rewrite_filter("binary_id(norn-testkit::harness) & test(=smoke)", folded),
            "binary_id(norn-testkit::suites) & test(=harness::smoke)",
        )
        drops = json.dumps({"d1": {"norn-testkit::harness": ["smoke"]}})
        self.assertEqual(
            json.loads(fold.rewrite_drops(drops, folded)),
            {"d1": {"norn-testkit::suites": ["harness::smoke"]}},
        )

    def test_module_cfg_handles_single_and_multiple_features(self) -> None:
        self.assertIsNone(fold.module_cfg({}))
        self.assertEqual(fold.module_cfg({"required-features": ["duckdb"]}), 'feature = "duckdb"')
        self.assertEqual(
            fold.module_cfg({"required-features": ["duckdb", "native"]}),
            'all(feature = "duckdb", feature = "native")',
        )

    def test_shared_support_rejects_blank_line_use_and_grouped_use(self) -> None:
        split_text = (
            f"{fold.SHARED_SUPPORT_HEADER}\n"
            "mod query_support;\n"
            "\n"
            "use query_support::governed;\n"
            '#[path = "a.rs"]\n'
            "mod a;\n"
        )
        with self.assertRaises(fold.Refused):
            fold.split_suites_sections(split_text)
        before_marker = (
            "use query_support::governed;\n"
            f"{fold.SHARED_SUPPORT_HEADER}\n"
            "mod query_support;\n"
        )
        with self.assertRaises(fold.Refused):
            fold.split_suites_sections(before_marker)
        helper_after_blank = (
            f"{fold.SHARED_SUPPORT_HEADER}\n"
            "mod query_support;\n"
            "\n"
            "mod extra_helper;\n"
            '#[path = "a.rs"]\n'
            "mod a;\n"
        )
        with self.assertRaises(fold.Refused):
            fold.split_suites_sections(helper_after_blank)
        with self.assertRaises(fold.Refused):
            fold.shared_support_names([fold.SHARED_SUPPORT_HEADER, "use query_support::{governed, other};"])
        with self.assertRaises(fold.Refused):
            fold.shared_support_names([fold.SHARED_SUPPORT_HEADER, "#[cfg(test)] use fixture::{Helper, Other};"])
        self.assertEqual(
            fold.shared_support_names([fold.SHARED_SUPPORT_HEADER, "#[cfg(test)] mod helper;"]),
            {"helper"},
        )
        for bad in (
            '#[cfg(\n    test\n)] use std::sync::Once;\n#[path = "a.rs"]\nmod a;\n',
            f'#![allow(unused_imports)] use std::sync::Once;\n{fold.SHARED_SUPPORT_HEADER}\nmod h;\n',
            f'{fold.SHARED_SUPPORT_HEADER}\nmod h;\n\n#[cfg(test)] #[test] fn smoke() {{}}\n#[path = "a.rs"]\nmod a;\n',
            f'{fold.SHARED_SUPPORT_HEADER}\nmod h;\n\n#[path = "a.rs"]\n#[cfg(feature = "extra")] mod a;\n',
            '#! [allow(\n dead_code\n) ]\n#[test]\nfn smoke() {}\n/*\n)]\n//! */\n#[path = "a.rs"]\nmod a;\n',
            '#[allow(\n dead_code\n) ]\n#[test]\nfn smoke() {}\n/*\n)]\n//! */\n#[path = "a.rs"]\nmod a;\n',
            '#!allow_bad\n',
            '#[cfg(test)] #[test] fn smoke() {} #[cfg(test)]\n#[path = "a.rs"]\nmod a;\n',
            '#[cfg(feature = "a")]\n#[path = "a.rs"]\nmod a;\n#[cfg(not(feature = "a"))]\n#[path = "a.rs"]\nmod a;\n',
        ):
            with self.assertRaises(fold.Refused):
                fold.split_suites_sections(bad)
        with self.assertRaises(fold.Refused):
            fold.split_suites_sections(
                '#![allow(\n dead_code\n) ]\n#[test]\nfn smoke() {}\n/*\n)]\n//! */\n#[path = "a.rs"]\nmod a;\n'
            )
        with self.assertRaises(fold.Refused):
            fold.split_suites_sections(
                '#![allow(\n    reason = "x\\"\n)]\n//! ")] #[test] fn smoke() {}\n#[path = "a.rs"]\nmod a;\n'
            )
        with self.assertRaises(fold.Refused):
            fold.split_suites_sections(
                '#[cfg(test /* )]\n#[path = "a.rs"]\nmod a;\n// */ )] #[test] fn smoke() {}\n#[path = "b.rs"]\nmod b;\n'
            )
        with self.assertRaises(fold.Refused):
            fold.split_suites_sections(
                '// comment\x0c#[cfg(any())]\n#[path = "a.rs"]\nmod a;\n'
            )
        with self.assertRaises(fold.Refused):
            fold.shared_support_names(
                [fold.SHARED_SUPPORT_HEADER, "use helpers::first; use helpers::second;"]
            )

    def test_pinned_target_requires_explicit_manifest_entry(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            crate_dir = root / "crates" / "norn-checker-wasm"
            (crate_dir / "tests").mkdir(parents=True)
            (crate_dir / "tests" / "parity.rs").write_text("#[test] fn p() {}\n")
            (crate_dir / "tests" / "suites.rs").write_text("")
            (crate_dir / "Cargo.toml").write_text(
                '[package]\nname = "norn-checker-wasm"\nautotests = false\n\n'
                '[[test]]\nname = "suites"\npath = "tests/suites.rs"\n'
            )
            orig_root = fold.ROOT
            try:
                fold.ROOT = root
                crate = fold.Crate("norn-checker-wasm")
                with self.assertRaises(fold.Refused):
                    crate.foldable()
            finally:
                fold.ROOT = orig_root


if __name__ == "__main__":
    unittest.main()
