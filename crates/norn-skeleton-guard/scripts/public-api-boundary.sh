#!/usr/bin/env bash
# SKELETON.md boundary: no `zen-*` type may appear in the public API of
# norn-expr or norn-decisions; the kernel consumes only Norn-owned types.
#
# Uses `cargo public-api` (nightly rustdoc JSON). `cargo public-api` renders
# re-exports of external items as `pub use <crate>::Name`, which hides their
# zen origin, so any external `pub use` in these crates also fails.
#
# Usage: public-api-boundary.sh [toolchain]   (default nightly-2026-08-18)
set -euo pipefail

toolchain="${1:-nightly-2026-08-18}"
status=0
for crate in norn-expr norn-decisions; do
  api="$(cargo "+${toolchain}" public-api -p "${crate}" --simplified)"
  bad="$(printf '%s\n' "${api}" | grep -E '(^|[^A-Za-z0-9_])zen_[A-Za-z0-9_]*::|^pub use ' || true)"
  if [[ -n "${bad}" ]]; then
    printf '::error::%s public API exposes zen-* or external re-exports:\n%s\n' "${crate}" "${bad}"
    status=1
  else
    printf '%s: public API is Norn-owned (%s items)\n' "${crate}" "$(printf '%s\n' "${api}" | wc -l)"
  fi
done
exit "${status}"
