#!/usr/bin/env python3
"""Fail closed unless the constrained Verus spike uses the production body.

This is textual source correspondence, not Rust/Verus compiler equivalence.
The standalone production module has no imports or conditional compilation.
"""
from pathlib import Path
import re

ROOT = Path(__file__).resolve().parents[1]
production = (ROOT / "crates/norn-kernel/src/version_check.rs").read_text()
verus = (ROOT / "verification/kernel-l1/version_verus.rs").read_text()
# Deliberately recognize only this complete, dependency-free production module.
pattern = (
    r"//! Pure machine-version successor shared by commit admission and proof tools\.\s*"
    r"/// Return the strictly next version, refusing exhaustion rather than wrapping\.\s*"
    r"#\[must_use\]\s*"
    r"pub\(crate\) const fn checked_next_version\(version: u64\) -> Option<u64> \{([^{}]+)\}\s*"
)
source = re.fullmatch(pattern, production)
annotated = re.search(
    r"pub fn checked_next_version\(version: u64\) -> \(result: Option<u64>\).*?"
    r"\n\{\n([^{}]+)\n\}\n\}\nfn main\(\) \{\}\s*\Z", verus, re.S
)
if source is None or annotated is None:
    raise SystemExit("version spike changed shape: review the source correspondence")
normalize = lambda body: re.sub(r"\s+", "", body)
if normalize(source.group(1)) != normalize(annotated.group(1)):
    raise SystemExit("Verus executable body differs from production")
print("kernel-l1: Verus/production version body matches exactly modulo whitespace")
