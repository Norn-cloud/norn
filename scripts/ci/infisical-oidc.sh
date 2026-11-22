#!/usr/bin/env bash
# The one reviewed way a workflow acquires a credential (credential policy, norn-skeleton-guard
# ci::credentials): it reads secrets from Infisical over GitHub OIDC into this step's outputs. No
# workflow mints an OIDC token or calls the Infisical API itself, and a credential-named input may
# only come from a step that runs this script (or from an OIDC action). Usage, in a job with
# `permissions: id-token: write`:
#   INFISICAL_IDENTITY_ID=… INFISICAL_PROJECT_ID=… bash scripts/ci/infisical-oidc.sh <env> <path> NAME…
# Each NAME becomes the output `name` (lower case). Every value is checked (non-empty, one line) and
# masked before any is written, and the outputs are written at once or not at all; bearers reach
# curl on stdin, never argv. Fails closed: a missing variable, token or secret exits 1.
# `Norn-cloud/norn-ops` holds the same script in `ci/infisical-oidc.sh`.
set -euo pipefail
fail() { echo "::error::infisical-oidc: $1" >&2; exit 1; }
one_line() { [[ -n $1 && $1 != *$'\n'* && $1 != *$'\r'* ]]; }
[ $# -ge 3 ] || fail "usage: infisical-oidc.sh <env> <path> NAME…"
environment=$1 path=$2
shift 2
for name in "$@"; do [[ $name =~ ^[A-Z][A-Z0-9_]*$ ]] || fail "not a secret name: $name"; done
one_line "${INFISICAL_IDENTITY_ID:-}" && one_line "${INFISICAL_PROJECT_ID:-}" ||
  fail "set INFISICAL_IDENTITY_ID and INFISICAL_PROJECT_ID (the OIDC identity and its project), one line each"
[ -n "${ACTIONS_ID_TOKEN_REQUEST_URL:-}" ] || fail "no OIDC token: the job needs permissions: id-token: write (a fork gets none)"
api=https://app.infisical.com/api
jwt=$(printf 'header = "Authorization: bearer %s"\n' "$ACTIONS_ID_TOKEN_REQUEST_TOKEN" |
  curl -fsS --max-time 30 -K - "$ACTIONS_ID_TOKEN_REQUEST_URL&audience=https://github.com/Norn-cloud" | jq -er .value) ||
  fail "the GitHub OIDC token request failed"
one_line "$jwt" || fail "the GitHub OIDC token is not one line"
echo "::add-mask::$jwt"
token=$(jq -cn --arg id "$INFISICAL_IDENTITY_ID" --arg jwt "$jwt" '{identityId: $id, jwt: $jwt}' |
  curl -fsS --max-time 30 -H 'content-type: application/json' --data @- "$api/v1/auth/oidc-auth/login" | jq -er .accessToken) ||
  fail "the Infisical OIDC login failed"
one_line "$token" || fail "the Infisical access token is not one line"
echo "::add-mask::$token"
secrets=$(printf 'header = "Authorization: Bearer %s"\n' "$token" |
  curl -fsS --max-time 30 -K - -G --data-urlencode "workspaceId=$INFISICAL_PROJECT_ID" --data-urlencode "environment=$environment" \
    --data-urlencode "secretPath=$path" "$api/v3/secrets/raw") || fail "the Infisical read of $environment $path failed"
# The whole set is checked in jq on the raw JSON first: each name exactly once, a non-empty string
# with no CR or LF. Nothing is masked or written unless every value passes, and no value can be
# changed by the shell (command substitution would strip a trailing LF).
jq -e --args '. as $raw | ($raw.secrets | type) == "array" and all($ARGS.positional[];
  . as $key | [$raw.secrets[] | select(.secretKey == $key) | .secretValue]
  | length == 1 and (.[0] | type) == "string" and (.[0] | length) > 0 and (.[0] | test("[\r\n]") | not))' \
  "$@" <<<"$secrets" > /dev/null ||
  fail "each of $* must be in Infisical $environment $path exactly once, a non-empty single line"
outputs=''
values=()
for name in "$@"; do
  values+=("$(jq -r --arg key "$name" '.secrets[] | select(.secretKey == $key) | .secretValue' <<<"$secrets")")
done
for value in "${values[@]}"; do echo "::add-mask::$value"; done
for i in "${!values[@]}"; do
  name=${@:i+1:1}
  outputs+="$(tr '[:upper:]' '[:lower:]' <<<"$name")=${values[i]}"$'\n'
done
printf '%s' "$outputs" >> "${GITHUB_OUTPUT:?}"
echo "infisical-oidc: read $# secret(s) from $environment $path"
