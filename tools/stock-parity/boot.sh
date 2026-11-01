#!/usr/bin/env bash
# Execute inside a disposable Boat fork. All credentials are generated there.
set -Eeuo pipefail
set +x
umask 077
cd /home/user
mkdir -p parity-runtime
cd parity-runtime
docker load -i /home/user/.local/share/boat-cache/frappe/runtime-images.tar > image-load.log
python3 - <<'PY'
import json, secrets
password = secrets.token_hex(24)
image = 'sha256:c84e378fc8b589299ab232855af5a50ebe78d2df911347ccfd0488e5d7668d39'
services = {
    'db': {'image': 'mariadb:10.11', 'environment': {'MARIADB_ROOT_PASSWORD': password}, 'mem_limit': '1g'},
    'cache': {'image': 'redis:7-alpine', 'mem_limit': '128m'},
    'queue': {'image': 'redis:7-alpine', 'mem_limit': '128m'},
    'web': {'image': image, 'entrypoint': ['sleep', 'infinity'], 'mem_limit': '5g',
            'ports': ['127.0.0.1:8000:8000'], 'environment': {'PARITY_PASSWORD': password}},
}
json.dump({'services': services}, open('compose.json', 'w'))
json.dump({'url': 'http://127.0.0.1:8000', 'password': password}, open('credentials.json', 'w'))
PY
dc() { docker compose -p stock-parity -f compose.json "$@"; }
dc up -d --pull never > start.log 2>&1
for ((i=0; i<60; i++)); do
  if dc exec -T db sh -c 'mariadb --protocol=TCP -h db -P 3306 -uroot -p"$MARIADB_ROOT_PASSWORD" -e "SELECT 1"' >/dev/null 2>&1; then break; fi
  if (( i == 59 )); then echo 'Database readiness timeout' >&2; exit 1; fi
  sleep 1
done
dc exec -T web bash -lc '
set -e
cd /home/frappe/frappe-bench
printf "frappe\nerpnext\n" > sites/apps.txt
bench set-config -g db_host db
bench set-config -gp db_port 3306
bench set-config -g redis_cache redis://cache:6379
bench set-config -g redis_queue redis://queue:6379
bench new-site parity.localhost --no-mariadb-socket --mariadb-root-password "$PARITY_PASSWORD" --admin-password "$PARITY_PASSWORD" --db-password "$PARITY_PASSWORD" --install-app erpnext
bench use parity.localhost
bench --site parity.localhost set-config mute_emails 1
bench --site parity.localhost set-config host_name http://parity.localhost:8000
bench --site parity.localhost set-config --parse throttle_user_limit 10000
bench version --format json
' > setup.log 2>&1
dc exec -T web bash -lc 'cd /home/frappe/frappe-bench; bench version --format json' > versions.json
python3 - <<'PY'
import json
versions = {v['app']: v for v in json.load(open('versions.json'))}
json.dump(versions, open('versions.json', 'w'))
assert all(versions[a]['version'].startswith('16.') for a in ('frappe', 'erpnext')), 'Requires v16'
PY
dc exec -d web bash -lc 'cd /home/frappe/frappe-bench; bench serve --port 8000 > /tmp/parity-web.log 2>&1'
dc exec -d web bash -lc 'cd /home/frappe/frappe-bench; bench worker --queue short,default,long > /tmp/parity-worker.log 2>&1'
