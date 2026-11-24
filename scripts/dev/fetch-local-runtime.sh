#!/usr/bin/env bash
# Fetch and verify prebuilt Norn local runtime artifacts (norn-checker-wasm,
# norn-dev-host, and norn CLI) into ~/.cache/norn/runtime/ via `norn setup`.
#
# Usage:
#   scripts/dev/fetch-local-runtime.sh [--version TAG] [--base-url URL] [--cache-dir DIR]
#
# Environment:
#   NORN_VERSION     release tag to install (default: latest)
#   NORN_BASE_URL    release directory URL holding SHA256SUMS and archives
#   NORN_REPO        GitHub owner/repo (default: Norn-cloud/norn)
#   NORN_CACHE_DIR   cache root override (default: ${XDG_CACHE_HOME:-$HOME/.cache}/norn)
set -euo pipefail

root=$(cd "$(dirname "$0")/../.." && pwd)
if ! command -v bun > /dev/null 2>&1; then
  echo "fetch-local-runtime.sh: bun is required to run norn setup" >&2
  exit 1
fi
exec bun "$root/tools/norn-ts/src/setup.ts" "$@"
