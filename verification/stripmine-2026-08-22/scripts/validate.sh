#!/usr/bin/env bash
set -euo pipefail

pack_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
sources_file="$pack_dir/sources.json"
obligations_file="$pack_dir/obligations.json"
fixtures_file="$pack_dir/fixtures/quantity-ledger.json"
repo_root="$(cd "$pack_dir/../.." && pwd)"
python3 "$repo_root/scripts/proof_enrollment.py"

# Mutation tests may supply an isolated registry explicitly. The promoted gate
# invokes this script without arguments, so ambient environment variables can
# never substitute the canonical security registry.
if [[ "${1:-}" == "--test-obligations-file" ]]; then
  if [[ $# -ne 2 ]]; then
    printf 'usage: %s [--test-obligations-file PATH]\n' "$0" >&2
    exit 2
  fi
  obligations_file="$2"
elif [[ $# -gt 0 ]]; then
  printf 'usage: %s [--test-obligations-file PATH]\n' "$0" >&2
  exit 2
fi

for required_tool in jq rg python3; do
  if ! command -v "$required_tool" >/dev/null 2>&1; then
    printf 'required verification-manifest tool is unavailable: %s\n' "$required_tool" >&2
    exit 1
  fi
done

for json_file in "$sources_file" "$obligations_file" "$fixtures_file" "$pack_dir"/schemas/*.json; do
  jq -e . "$json_file" >/dev/null
done

# Exercise the checked-in JSON Schemas themselves. The jq checks below remain
# useful for cross-file reference integrity, which JSON Schema cannot express.
python3 - \
  "$pack_dir/schemas/source-manifest.schema.json" "$sources_file" \
  "$pack_dir/schemas/obligation-registry.schema.json" "$obligations_file" <<'PY'
import json
import sys

try:
    from jsonschema import Draft202012Validator, FormatChecker
except ImportError as error:
    raise SystemExit("python jsonschema is required for verification-pack validation") from error

arguments = sys.argv[1:]
for schema_path, instance_path in zip(arguments[0::2], arguments[1::2], strict=True):
    with open(schema_path, encoding="utf-8") as schema_file:
        schema = json.load(schema_file)
    with open(instance_path, encoding="utf-8") as instance_file:
        instance = json.load(instance_file)
    Draft202012Validator.check_schema(schema)
    errors = sorted(
        Draft202012Validator(schema, format_checker=FormatChecker()).iter_errors(instance),
        key=lambda item: tuple(str(part) for part in item.absolute_path),
    )
    if errors:
        rendered = "\n".join(
            f"{instance_path}:{'/'.join(map(str, error.absolute_path))}: {error.message}"
            for error in errors
        )
        raise SystemExit(rendered)

# This is intentionally independent of the coverage validator's copy. The
# registry is a security control, so self-consistent deletion or substitution
# must not silently redefine the set of obligations that the pack validates.
required_obligation_ids = frozenset(
    {
        "INV-AUTH-ATOMIC-COMMIT",
        "INV-AUTH-STALE-REJECT",
        "INV-AUTH-IDEMPOTENT-RESULT",
        "INV-AUTH-CLOSED-CUSTOM-EFFECT",
        "INV-AUTH-HARD-INVARIANT-OWNERSHIP",
        "INV-AUTH-DOMAIN-ADMISSION-PREDICATE",
        "INV-AUTH-CAPABILITY-NOW",
        "INV-AUTH-SCOPE-CLOSED",
        "INV-ART-AUTHORITY-PROMOTION",
        "INV-ART-DIRECT-WRITE-PREDICATE",
        "INV-QL-CONSERVATION",
        "INV-QL-CERTIFICATE-POSTING-BINDING",
        "INV-QL-UNIT-DIMENSION",
        "INV-QL-EXACT-ARITHMETIC",
        "INV-QL-PENDING-RESOLUTION",
        "INV-QL-BATCH-ATOMIC",
        "INV-QL-AUTHORITY-BRIDGE-ATOMIC",
        "INV-QL-APPEND-REVERSAL",
        "INV-ONT-MAPPING-PROVENANCE",
        "INV-ONT-OPEN-CLOSED-SEPARATION",
        "INV-ONT-RELATION-AUTHORITY",
        "INV-QL-REFERENCE-BALANCE-ALGEBRA",
        "INV-RULE-CLOSED-DETERMINISTIC-EVALUATION",
        "INV-RULE-HIT-POLICY-SAFETY",
        "INV-RULE-HIT-POLICY-ADMISSION-PREDICATE",
        "INV-VER-DIFFERENTIAL-REFINEMENT",
        "INV-VER-NO-PROOF-HOLES",
    }
)
with open(sys.argv[4], encoding="utf-8") as obligation_file:
    obligation_document = json.load(obligation_file)
obligation_entries = obligation_document["obligations"]
obligation_ids = {item["id"] for item in obligation_entries}
if (
    len(obligation_entries) != len(required_obligation_ids)
    or obligation_ids != required_obligation_ids
):
    missing = sorted(required_obligation_ids - obligation_ids)
    unexpected = sorted(obligation_ids - required_obligation_ids)
    raise SystemExit(
        "central obligation registry must contain exactly the retained 27-ID closure; "
        f"missing={missing}, unexpected={unexpected}"
    )
PY

jq -e '
  .manifest_version == "norn.verification.sources.v1" and
  (.sources | length > 0) and
  ((.sources | length) == ([.sources[].id] | unique | length)) and
  all(.sources[];
    (.id | test("^SRC-[A-Z0-9-]+$")) and
    (.title | length > 0) and
    (.kind | length > 0) and
    (.locator | length > 0) and
    (.version | length > 0) and
    (.license | length > 0) and
    (.refs | length > 0) and
    (.caveats | type == "array")) and
  all(.sources[] | select(.kind == "repository" or .kind == "tool");
    has("commit") and (.commit | test("^[0-9a-f]{40}$"))) and
  all(.sources[] | select(has("commit")); .commit | test("^[0-9a-f]{40}$"))
' "$sources_file" >/dev/null

jq -e '
  .registry_version == "norn.verification.obligations.v1" and
  (.obligations | length > 0) and
  ((.obligations | length) == ([.obligations[].id] | unique | length)) and
  all(.obligations[];
    (.id | test("^INV-[A-Z0-9-]+$")) and
    (.priority == "P0" or .priority == "P1" or .priority == "P2") and
    (.statement | length > 0) and
    (.owner | length > 0) and
    (.assumptions | type == "array") and
    (.sources | length > 0) and
    (.checkers | length > 0) and
    all(.checkers[]; (.artifact | length > 0) and (.acceptance | length > 0)) and
    (.counterexamples | length > 0) and
    (.implementation_bindings | length > 0))
' "$obligations_file" >/dev/null

source_ids=$(mktemp)
referenced_ids=$(mktemp)
cleanup() {
  rm -f "$source_ids" "$referenced_ids"
}
trap cleanup EXIT

jq -r '.sources[].id' "$sources_file" | sort -u >"$source_ids"
jq -r '.obligations[].sources[].source_id' "$obligations_file" | sort -u >"$referenced_ids"
if ! comm -13 "$source_ids" "$referenced_ids" | (! read -r); then
  comm -13 "$source_ids" "$referenced_ids" >&2
  exit 1
fi

jq -e '
  (.cases | length > 0) and
  ((.cases | length) == ([.cases[].id] | unique | length)) and
  (.sequence_cases | length > 0) and
  ((.sequence_cases | length) == ([.sequence_cases[].id] | unique | length))
' "$fixtures_file" >/dev/null

jq -e '
  any(.sources[]; .id == "SRC-CALA-FORK-BASE" and .commit == "d0a4be8daf43b19813441cf19a5149604c72f062") and
  any(.sources[]; .id == "SRC-CALA-LATEST" and .commit == "68d7755acc7b2e8d9740d0d6132332938d4628e2")
' "$sources_file" >/dev/null

if rg -n --glob '*.lean' '(^|[;[:space:]])(axiom|sorry|admit)([;[:space:]]|$)' "$pack_dir/proofs/lean"; then
  exit 1
fi

if rg -n --glob '*.fst' '^[[:space:]]*(assume|admit|axiom)([[:space:]]|$)' "$pack_dir/proofs/fstar"; then
  exit 1
fi

printf 'validated %s sources, %s obligations, %s fixtures, %s sequences\n' \
  "$(jq '.sources | length' "$sources_file")" \
  "$(jq '.obligations | length' "$obligations_file")" \
  "$(jq '.cases | length' "$fixtures_file")" \
  "$(jq '.sequence_cases | length' "$fixtures_file")"
