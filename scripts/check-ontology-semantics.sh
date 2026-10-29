#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
tool_root="${NORN_ONTOLOGY_TOOL_ROOT:-$repo_root/target/ontology-tools}"
python="$tool_root/venv/bin/python"

if [[ ! -x "$python" ]]; then
  echo "pinned ontology tools are missing; run scripts/bootstrap-ontology-tools.sh" >&2
  exit 1
fi

exec "$python" "$repo_root/verification/ontology/scripts/validate_semantics.py"
