#!/usr/bin/env python3
"""Unit tests for scripts/codemods/fold_integration_tests.py."""

from __future__ import annotations

import importlib.util
import json
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
            "norn-datalog",
            "norn-testkit",
        ):
            crate = fold.Crate(crate_dir)
            self.assertFalse(fold._needs_suite_allows(crate))
            generated = fold.suites_source(crate, crate.foldable(), {})
            self.assertNotIn("#![allow(", generated)
        self.assertTrue(fold._needs_suite_allows(fold.Crate("norn-api")))

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


if __name__ == "__main__":
    unittest.main()
