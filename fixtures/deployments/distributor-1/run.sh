#!/usr/bin/env bash
# distributor-1 deployment test, in phases, on one disposable Linux VM with
# Docker. Every phase logs to $RUN/logs/<phase>.log and leaves JSON results in
# $RUN/results; a failing step is recorded and the phase goes on.
#
#   REPO=/path/to/norn RUN=/path/to/run bash run.sh setup|systems|seed|extract|norn|chaos|bpic|studio|pack|all
#
# Never run on a shared host: it starts ERPNext, Odoo, Saleor, two Postgres,
# MariaDB, a release Rust build and a browser.
set -uo pipefail
PHASE="${1:-all}"
REPO="${REPO:-$(cd "$(dirname "$0")/../../.." && pwd)}"
D1="$REPO/fixtures/deployments/distributor-1"
RUN="${RUN:-$HOME/d1run}"
SCALE="${SCALE:-3}"
BPIC_BUDGET_S="${BPIC_BUDGET_S:-1200}"
mkdir -p "$RUN"/{logs,results,files,proof}
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/norn-target}" CARGO_INCREMENTAL=0
export NORN_DEV_HOST_BIN="$CARGO_TARGET_DIR/release/norn-dev-host"
export NORN_CHECKER_WASM_PKG="$RUN/checker-wasm"
export D1_FILES="$RUN/files"
DC="docker compose -f $D1/compose.yaml --project-directory $D1"
note() { printf '%s %s\n' "$(date -u +%H:%M:%S)" "$*"; }
timed() { local name=$1; shift; local t=$SECONDS; "$@"; local rc=$?; printf '{"step":"%s","rc":%d,"seconds":%d}\n' "$name" "$rc" "$((SECONDS - t))" >> "$RUN/results/steps.jsonl"; return $rc; }
wait_http() { for _ in $(seq 1 "${3:-180}"); do curl -fsS -o /dev/null -H "Host: ${2:-localhost}" "$1" && return 0; sleep 5; done; return 1; }

setup() {
  command -v docker >/dev/null || { note "installing docker"; curl -fsSL https://get.docker.com | sudo sh && sudo usermod -aG docker "$USER"; }
  docker info >/dev/null 2>&1 || sudo chmod 666 /var/run/docker.sock
  # resolve image tags once and record them
  ERPNEXT_TAG=$(curl -fsS "https://hub.docker.com/v2/repositories/frappe/erpnext/tags?page_size=100&name=v15." | python3 -c 'import json,sys,re; t=[x["name"] for x in json.load(sys.stdin)["results"] if re.fullmatch(r"v15\.\d+\.\d+",x["name"])]; print(sorted(t,key=lambda s:[int(p) for p in s[1:].split(".")])[-1])' || echo v15.88.1)
  printf 'ERPNEXT_TAG=%s\nODOO_TAG=17.0\nSALEOR_TAG=3.20\n' "$ERPNEXT_TAG" > "$RUN/tags.env"
  note "tags: $(tr '\n' ' ' < "$RUN/tags.env")"
  (cd "$REPO" && nohup nice cargo build --locked --release -p norn-cell-do --bin norn-dev-host > "$RUN/logs/cargo.log" 2>&1; echo "exit=$?" >> "$RUN/logs/cargo.log") &
  (cd "$REPO" && bash crates/norn-checker-wasm/scripts/build-pkg.sh "$NORN_CHECKER_WASM_PKG" > "$RUN/logs/wasm.log" 2>&1; echo "exit=$?" >> "$RUN/logs/wasm.log") &
  (cd "$REPO/tools/norn-ts" && bun install --frozen-lockfile --backend copyfile > "$RUN/logs/bun-ts.log" 2>&1)
  set -a; . "$RUN/tags.env"; set +a
  timed pull $DC pull --quiet > "$RUN/logs/pull.log" 2>&1
}

systems() {
  set -a; . "$RUN/tags.env"; set +a
  timed world python3 -I "$D1/gen/world.py" "$RUN/world" --scale "$SCALE"
  timed files python3 -I "$D1/gen/files.py" "$RUN/world/world.json" "$RUN/files"
  timed compose_up $DC up -d
  note "waiting for ERPNext site"
  timed erpnext_up wait_http http://127.0.0.1:8000/api/method/ping d1.localhost 240
  timed erpnext_wizard $DC exec -T -e TERM=xterm erpnext bench --site d1.localhost execute frappe.desk.page.setup_wizard.setup_wizard.setup_complete --kwargs \
    '{"args":{"language":"English","country":"Egypt","timezone":"Africa/Cairo","currency":"EGP","full_name":"Admin","email":"admin@d1.example","company_name":"Distributor One","company_abbr":"D1","chart_of_accounts":"Standard","fy_start_date":"2025-01-01","fy_end_date":"2025-12-31","setup_demo":0}}'
  timed odoo_up wait_http http://127.0.0.1:8069/web/login localhost 240
  timed saleor_up wait_http http://127.0.0.1:8001/graphql/ localhost 240
  timed saleor_admin $DC exec -T -e DJANGO_SUPERUSER_EMAIL=admin@d1.example -e DJANGO_SUPERUSER_PASSWORD=d1-admin saleor python manage.py createsuperuser --noinput
  pkill -f proxy/hostile.py; pkill -f proxy/sink.py
  nohup python3 -I "$D1/proxy/hostile.py" "$D1/proxy/chaos.json" > "$RUN/logs/hostile.log" 2>&1 &
  mkdir -p "$RUN/exports"
  nohup python3 -I "$D1/proxy/sink.py" 18901 "$RUN/exports/webhooks.jsonl" > "$RUN/logs/sink.log" 2>&1 &
  sleep 2
}

toxics() {  # $1 = on|off
  python3 - "$1" "$D1/proxy/chaos.json" <<'PY'
import json, sys, urllib.request
on, cfg = sys.argv[1] == "on", json.load(open(sys.argv[2]))
for i, t in enumerate(cfg["toxics"]):
    name = f"t{i}"
    try:
        urllib.request.urlopen(urllib.request.Request(f"http://127.0.0.1:8474/proxies/{t['proxy']}/toxics/{name}", method="DELETE"), timeout=10)
    except Exception:
        pass
    if on:
        body = {"name": name, "type": t["type"], "stream": "downstream", "toxicity": t.get("toxicity", 1.0), "attributes": t["attributes"]}
        urllib.request.urlopen(urllib.request.Request(f"http://127.0.0.1:8474/proxies/{t['proxy']}/toxics", data=json.dumps(body).encode(), method="POST", headers={"Content-Type": "application/json"}), timeout=10)
print("toxics", "on" if on else "off")
PY
}

seed() {
  timed seed python3 -I "$D1/seed/seed.py" "$RUN/world/world.json" "$RUN/results"
}

extract() {
  toxics off
  timed extract_careful python3 -I "$D1/extract/extract.py" "$RUN/world/world.json" "$RUN/files" "$RUN/exports/careful" careful
  curl -fsS http://127.0.0.1:18000/__chaos > "$RUN/results/chaos-before.json" || true
  toxics on
  timed extract_naive python3 -I "$D1/extract/extract.py" "$RUN/world/world.json" "$RUN/files" "$RUN/exports/naive" naive
  curl -fsS http://127.0.0.1:18000/__chaos > "$RUN/results/chaos-counters.json" || true
  toxics off
}

project() {  # $1 = dir, $2 = port
  rm -rf "$1"
  (cd "$REPO" && bun tools/norn-ts/src/cli.ts init "$1" --template three-systems --org org_d1 --env dev --no-install > "$RUN/logs/init-$(basename "$1").log" 2>&1)
  cp -r "$RUN/definitions-d1" "$1/definitions/d1"
  python3 - "$1/norn.json" "$2" <<'PY'
import json, sys
p, port = sys.argv[1], int(sys.argv[2])
m = json.load(open(p)); m["sources"] = sorted(set(m["sources"]) | {"definitions/d1"}); m.setdefault("dev", {})["port"] = port
json.dump(m, open(p, "w"), indent=1)
PY
}

start_dev() {  # $1 = project dir
  (cd "$1" && nohup bun "$REPO/tools/norn-ts/src/cli.ts" dev --json > "$RUN/logs/dev-$(basename "$1").log" 2>&1 &)
  local url
  for _ in $(seq 1 150); do
    if test -f "$1/.norn/dev.json"; then
      url=$(python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); print(d.get("url") or "http://127.0.0.1:%s" % d.get("port"))' "$1/.norn/dev.json")
      curl -fsS -o /dev/null "$url/v1/health" 2>/dev/null || curl -sS -o /dev/null "$url/" 2>/dev/null && return 0
    fi
    sleep 2
  done
  return 1
}

LOAD_ORDER="pack_erpnext erpnext_docs pack_odoo odoo_docs pack_saleor saleor_docs legacy files saleor_webhooks"

norn() {
  grep -q exit=0 "$RUN/logs/cargo.log" || { note "waiting for cargo"; while ! grep -q '^exit=' "$RUN/logs/cargo.log"; do sleep 10; done; }
  grep -q exit=0 "$RUN/logs/wasm.log" || { while ! grep -q '^exit=' "$RUN/logs/wasm.log"; do sleep 10; done; }
  (cd "$D1/project" && rm -rf src/gen && python3 -I gen_mappings.py src/gen > "$RUN/results/gen.json")
  mkdir -p "$RUN/definitions-d1"
  timed assemble bun "$D1/project/assemble.ts" "$RUN/asm" "$RUN/results/assemble.json"
  rm -rf "$RUN/definitions-d1" && cp -r "$RUN/asm/definitions/d1" "$RUN/definitions-d1"
  project "$RUN/proj" 4180
  timed dev_start start_dev "$RUN/proj"
  local files=""; for e in $LOAD_ORDER; do test -f "$RUN/exports/careful/$e.json" && files="$files $RUN/exports/careful/$e.json"; done
  timed load_careful python3 -I "$D1/measure/load.py" "$RUN/proj" "$RUN/results/load-careful.json" $files
  timed probe python3 -I "$D1/measure/probe.py" "$RUN/proj" "$RUN/world/world.json" "$RUN/results/native-ids.json" "$RUN/results/probe.json"
  # replay the same exports: idempotency (every row should be alreadyLanded)
  timed load_replay python3 -I "$D1/measure/load.py" "$RUN/proj" "$RUN/results/load-replay.json" $files
}

chaos() {  # the naive (hostile-network) extract into its own cell
  project "$RUN/proj-naive" 4181
  timed dev_start_naive start_dev "$RUN/proj-naive"
  local files=""; for e in $LOAD_ORDER; do test -f "$RUN/exports/naive/$e.json" && files="$files $RUN/exports/naive/$e.json"; done
  timed load_naive python3 -I "$D1/measure/load.py" "$RUN/proj-naive" "$RUN/results/load-naive.json" $files
  timed probe_naive python3 -I "$D1/measure/probe.py" "$RUN/proj-naive" "$RUN/world/world.json" "$RUN/results/native-ids.json" "$RUN/results/probe-naive.json" counts inbox holdings
  pkill -f "proj-naive" || true
}

bpic() {
  $DC down > /dev/null 2>&1 || true   # free the systems' memory for the stress run
  mkdir -p "$RUN/bpic-raw" "$RUN/bpic"
  test -n "$(ls "$RUN"/bpic-raw/*.xes* 2>/dev/null)" || timed bpic_fetch python3 -I "$D1/measure/bpic.py" fetch "$RUN/bpic-raw" > "$RUN/logs/bpic-fetch.log" 2>&1
  local xes; xes=$(ls "$RUN"/bpic-raw/*.xes* 2>/dev/null | head -1)
  test -n "$xes" || { note "no BPIC file"; return 1; }
  timed bpic_convert python3 -I "$D1/measure/bpic.py" convert "$xes" "$RUN/bpic" > "$RUN/results/bpic-convert.json"
  timed bpic_load python3 -I "$D1/measure/load.py" "$RUN/proj" "$RUN/results/load-bpic.json" --budget-s "$BPIC_BUDGET_S" $(ls "$RUN"/bpic/bpic-*.json)
  timed probe_scale python3 -I "$D1/measure/probe.py" "$RUN/proj" "$RUN/world/world.json" "$RUN/results/native-ids.json" "$RUN/results/probe-scale.json" counts questions
}

studio() {
  (cd "$REPO/apps/studio" && bun install --frozen-lockfile --backend copyfile > "$RUN/logs/studio-install.log" 2>&1 && bun run checker > "$RUN/logs/studio-checker.log" 2>&1 && bun run build > "$RUN/logs/studio-build.log" 2>&1) || note "studio build failed"
  (cd "$REPO/apps/studio" && bunx playwright install --with-deps chromium > "$RUN/logs/playwright.log" 2>&1) || note "playwright install failed"
  (cd "$REPO/apps/studio" && nohup bun run local --project "$RUN/proj" --port 5180 > "$RUN/logs/studio-local.log" 2>&1 &)
  wait_http http://127.0.0.1:5180/ localhost 60
  (cd "$REPO/apps/studio" && timed shoot bun "$D1/measure/shoot.ts" http://127.0.0.1:5180 "$RUN/proof")
}

pack() {
  tar -C "$RUN" -czf "$HOME/d1-results.tgz" results logs proof tags.env exports/careful/extract-report.json exports/naive/extract-report.json 2>/dev/null
  ls -la "$HOME/d1-results.tgz"
}

case "$PHASE" in
  all) setup; systems; seed; extract; norn; chaos; bpic; studio; pack ;;
  *) "$PHASE" ;;
esac
