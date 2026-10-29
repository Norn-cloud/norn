#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
tool_root="${NORN_ONTOLOGY_TOOL_ROOT:-$repo_root/target/ontology-tools}"
venv_root="$tool_root/venv"
requirements="$repo_root/verification/ontology/tools/requirements.lock"

if [[ ! -x "$venv_root/bin/python" ]]; then
  python3 -m venv "$venv_root"
fi

"$venv_root/bin/python" -m pip install \
  --disable-pip-version-check \
  --no-input \
  --require-hashes \
  --requirement "$requirements"

"$venv_root/bin/python" - <<'PY'
from importlib.metadata import version

expected = {
    "owlrl": "7.6.2",
    "pyshacl": "0.40.1",
    "rdflib": "7.6.0",
}
for package, wanted in expected.items():
    actual = version(package)
    if actual != wanted:
        raise SystemExit(f"{package} version mismatch: expected {wanted}, got {actual}")
    print(f"{package:8} {actual}")
PY
