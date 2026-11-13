#!/bin/sh
# The container's entry point: deploy the cell Worker and Studio to the fleet bucket, apply Studio's
# database migrations, serve, then set up the first owner and their cell (setup.ts). Every step is
# idempotent, so a restart repeats them safely. Secrets are generated once into /data/secrets.
set -eu

: "${NORN_PUBLIC_URL:=http://localhost:8080}"
: "${CELLD_BUCKET:?set CELLD_BUCKET (compose.yaml does)}"
export CELLD_BUCKET
STAGE=self
# The client address Studio's sign-in limit counts comes from a header only the edge can set:
# x-real-ip from Caddy, or cf-connecting-ip from Cloudflare's tunnel with nothing else published.
CLIENT_IP_HEADER=${NORN_CLIENT_IP_HEADER:-x-real-ip}
case "$CLIENT_IP_HEADER/${NORN_CADDY:-1}/${NORN_CLOUDFLARE_TUNNEL:-0}" in
  x-real-ip/1/0 | cf-connecting-ip/0/1) ;;
  *) echo "refused: NORN_CLIENT_IP_HEADER=$CLIENT_IP_HEADER needs its own edge alone (x-real-ip: NORN_CADDY=1 NORN_CLOUDFLARE_TUNNEL=0; cf-connecting-ip: NORN_CADDY=0 NORN_CLOUDFLARE_TUNNEL=1)" >&2; exit 1 ;;
esac
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

# The bundled store has no accounts on its private network; create the fleet bucket there. Your
# own storage: the bucket must already exist, and its credentials must be able to write to it.
if [ "${S3_ENDPOINT}" = "http://s3:8333" ]; then
  bucket=${CELLD_BUCKET#s3://}; bucket=${bucket%%/*}
  for attempt in $(seq 1 120); do
    code=$(curl -s -o /dev/null -w '%{http_code}' -X PUT "${S3_ENDPOINT}/${bucket}" || true)
    case "$code" in 200|409) break ;; esac
    [ "$attempt" = 120 ] && { echo "the store at ${S3_ENDPOINT} did not create bucket ${bucket} (HTTP ${code})" >&2; exit 1; }
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
  STUDIO_CLIENT_IP_HEADER=$CLIENT_IP_HEADER \
  bun "$STUDIO/celld-config.ts" "$STUDIO"
celld deploy "$STUDIO"
rm -f "$STUDIO/wrangler.json"
# Studio's database, for celld d1 (migrations and operator SQL). No secrets; kept in the data volume:
#   docker compose exec norn celld d1 execute norn-studio-auth-self --command "<sql>" /data/deploy/ops
OPS=/data/deploy/ops
rm -rf "$OPS" && mkdir -p "$OPS" && cp -R "$STUDIO/worker/migrations" "$OPS/migrations"
cat > "$OPS/wrangler.json" <<JSON
{ "name": "norn-studio-${STAGE}-ops", "d1_databases": [{ "binding": "AUTH_DB", "database_name": "${DB}", "migrations_dir": "migrations" }] }
JSON

# One node, durable in the bucket. It first listens on loopback only, so nobody reaches Studio until
# its database is migrated, the sign-up policy is set and the owner exists; then it serves publicly,
# reached only through the proxy, which sets X-Real-Ip from the connection (Caddyfile).
serve() { CELLD_DURABILITY=bucket CELLD_ADDR="$1" CELLD_INTERNAL_ADDR=127.0.0.1:8081 celld & NODE=$!; }
serve 127.0.0.1:8080
for attempt in $(seq 1 120); do
  celld d1 migrations apply "$DB" "$OPS" 2>/tmp/migrate.err && break
  [ "$attempt" = 120 ] && { cat /tmp/migrate.err >&2; kill -TERM "$NODE"; exit 1; }
  sleep 1
done
# The first account owns this install; after it, sign-up is by invitation (migration 0009).
celld d1 execute "$DB" --command "insert into studio_setting (key, value) values ('signup', 'first-owner') on conflict (key) do nothing" "$OPS"
NORN_PUBLIC_URL=$NORN_PUBLIC_URL bun /opt/norn/setup.ts || { kill -TERM "$NODE"; wait "$NODE"; exit 1; }
kill -TERM "$NODE"; wait "$NODE" || true

serve 0.0.0.0:8080
until curl -fs -o /dev/null http://127.0.0.1:8080/; do sleep 1; done
echo "Norn is ready at ${NORN_PUBLIC_URL}"
trap 'kill -TERM "$NODE" 2>/dev/null; wait "$NODE"' TERM INT
wait "$NODE"
