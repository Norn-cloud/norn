#!/bin/sh
# The container's entry point: deploy the cell Worker and Studio to the fleet bucket, apply Studio's
# database migrations, serve, then set up the first owner and their cell (setup.ts). Every step is
# idempotent, so a restart repeats them safely. Secrets are generated once into /data/secrets.
set -eu

: "${NORN_PUBLIC_URL:=http://localhost:8080}"
: "${CELLD_BUCKET:?set CELLD_BUCKET (compose.yaml does)}"
export CELLD_BUCKET
STAGE=self
DB="norn-studio-auth-${STAGE}"
SECRETS=/data/secrets
umask 077
mkdir -p "$SECRETS" /data/celld /data/deploy

secret() { # secret NAME: a 32-byte hex value, created on first use
  [ -s "$SECRETS/$1" ] || od -An -N32 -tx1 /dev/urandom | tr -d ' \n' > "$SECRETS/$1"
  cat "$SECRETS/$1"
}
AUTH_SECRET=$(secret better-auth-secret)
LINEAGE_KEY=$(secret lineage-key)

# The bundled store has no accounts on its private network; create the fleet bucket there. Any
# other S3-compatible store: create the bucket yourself and leave NORN_CREATE_BUCKET unset.
if [ "${NORN_CREATE_BUCKET:-0}" = 1 ]; then
  bucket=${CELLD_BUCKET#s3://}; bucket=${bucket%%/*}
  for attempt in $(seq 1 60); do
    code=$(curl -s -o /dev/null -w '%{http_code}' -X PUT "${S3_ENDPOINT}/${bucket}" || true)
    case "$code" in 200|409) break ;; esac
    [ "$attempt" = 60 ] && { echo "the store at ${S3_ENDPOINT} did not create bucket ${bucket} (HTTP ${code})" >&2; exit 1; }
    sleep 1
  done
fi

# The cell Worker. It trusts only the host tokens Studio signs (issuer and JWKS are pinned here,
# never read from a request); the JWKS is fetched over this node's own listener.
CELL=/data/deploy/cell
rm -rf "$CELL" && mkdir -p "$CELL" && cp -R /opt/norn/cell/. "$CELL/"
cat > "$CELL/wrangler.json" <<JSON
{
  "name": "norn-cell",
  "main": "worker.mjs",
  "compatibility_date": "2026-11-01",
  "durable_objects": { "bindings": [{ "name": "CELLS", "class_name": "Cell" }] },
  "migrations": [{ "tag": "v1", "new_sqlite_classes": ["Cell"] }],
  "vars": {
    "NORN_HOST_ISSUER": "${NORN_PUBLIC_URL}",
    "NORN_HOST_JWKS_URL": "http://127.0.0.1:8080/api/auth/jwks",
    "NORN_LINEAGE_KEY": "${LINEAGE_KEY}",
    "NORN_TENANT_ENVIRONMENTS": "prod"
  }
}
JSON
celld deploy "$CELL"

# Studio, deployed last: celld serves the last deployed script as the fleet's application.
STUDIO=/opt/norn/studio
rm -f "$STUDIO/wrangler.json"
STUDIO_STAGE=$STAGE STUDIO_BUILD=$(cat /opt/norn/commit) BETTER_AUTH_URL=$NORN_PUBLIC_URL \
  BETTER_AUTH_SECRET=$AUTH_SECRET NORN_HOST_SERVICE=norn-cell STUDIO_DEFAULT_ENVIRONMENT_ID=prod \
  STUDIO_CLIENT_IP_HEADER=x-real-ip \
  bun "$STUDIO/celld-config.ts" "$STUDIO"
celld deploy "$STUDIO"
celld d1 migrations apply "$DB" "$STUDIO"
# The first account owns this install; after it, sign-up is by invitation (migration 0009).
celld d1 execute "$DB" --command "insert into studio_setting (key, value) values ('signup', 'first-owner') on conflict (key) do nothing"
rm -f "$STUDIO/wrangler.json"

# Serve: one node, durable in the bucket. The public listener is reached only through the proxy,
# which sets X-Real-Ip from the connection (Caddyfile).
CELLD_DURABILITY=bucket CELLD_ADDR=0.0.0.0:8080 CELLD_INTERNAL_ADDR=127.0.0.1:8081 celld &
NODE=$!
trap 'kill -TERM "$NODE" 2>/dev/null; wait "$NODE"' TERM INT
NORN_PUBLIC_URL=$NORN_PUBLIC_URL bun /opt/norn/setup.ts || { kill -TERM "$NODE"; wait "$NODE"; exit 1; }
wait "$NODE"
