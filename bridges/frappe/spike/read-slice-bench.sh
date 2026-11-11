#!/usr/bin/env bash
# Read slice (#1085): Norn-defined types shown in Frappe, read from a running
# cell host, with the cell's permissions. Run on the bench host:
#
#   read-slice-bench.sh start-cell DIR   # serve the cell build in DIR (pkg/, worker.mjs, wrangler.json)
#   read-slice-bench.sh apply BUNDLE     # stage a generated bundle and run bench migrate
#   read-slice-bench.sh users            # demo users, one per Norn reader role
#   read-slice-bench.sh show             # what Frappe shows each user
#
# Expects the site `demo.localhost` in container demo-backend-1 with
# norn_bridge installed, `celld` and `esbuild` in $TOOLS, and the web server on :8000.
set -uo pipefail
SITE=demo.localhost
C=demo-backend-1
B="docker exec -u frappe -w /home/frappe/frappe-bench $C"
CELL_URL=http://127.0.0.1:9877
step() { printf '\n### %s\n' "$*"; }
SHOW='import json,sys
d=json.load(sys.stdin)
if "exc_type" in d or "exception" in d:
    msgs=[json.loads(m).get("message") for m in json.loads(d.get("_server_messages") or "[]")]
    print("REFUSED", d.get("exc_type"), "|", msgs)
else:
    print(json.dumps(d.get("message", d), sort_keys=True)[:1500])'

cellget() { # GET a cell route from inside the container: status and body
  docker exec $C python3 -c 'import sys,urllib.request,urllib.error
try:
    r=urllib.request.urlopen(sys.argv[1],timeout=5); print(r.status, r.read().decode()[:300])
except urllib.error.HTTPError as e: print(e.code, e.read().decode()[:300])' "$CELL_URL$1"
}
login() { # login USER PASSWORD -> cookie jar path
  local jar=/tmp/rs.$1; rm -f "$jar"
  curl -s -H "Host: $SITE" -c "$jar" -X POST http://127.0.0.1:8000/api/method/login -d "usr=$1" -d "pwd=$2" > /dev/null
  echo "$jar"
}
raw() { # raw JAR METHOD ARGS... -> the untruncated JSON response
  local jar=$1 method=$2; shift 2
  curl -s -H "Host: $SITE" -b "$jar" -G "http://127.0.0.1:8000/api/method/$method" "$@"
}
api() { # api JAR METHOD ARGS...
  local jar=$1 method=$2; shift 2
  curl -s -H "Host: $SITE" -b "$jar" -G "http://127.0.0.1:8000/api/method/$method" "$@" | python3 -c "$SHOW"
}

case "${1:-}" in
start-cell)
  dir=$2
  # Stop the previous cell host and its node: every process running in the cell dir.
  docker exec $C bash -c 'for p in /proc/[0-9]*; do [ "$(readlink $p/cwd)" = /home/frappe/cell ] && kill ${p#/proc/}; done; for _ in $(seq 30); do (exec 3<>/dev/tcp/127.0.0.1/9877) 2>/dev/null || exit 0; sleep 1; done; exit 1' || true
  sleep 1
  docker exec $C rm -rf /home/frappe/cell
  docker cp "$dir" $C:/home/frappe/cell
  docker cp "$TOOLS/." $C:/usr/local/bin/
  docker exec $C chown -R frappe:frappe /home/frappe/cell
  # Host configuration: the cell trusts exactly this issuer and JWKS URL, and
  # gets a fresh stage lineage key. The JWKS URL names the site (a *.localhost
  # name resolves to loopback): Frappe picks the site from the Host header,
  # which a Worker fetch cannot set. .dev.vars is the celld dev variable file.
  docker exec -i -u frappe $C sh -c 'umask 077; cat > /home/frappe/cell/.dev.vars' <<VARS
NORN_HOST_ISSUER=frappe:$SITE
NORN_HOST_JWKS_URL=http://$SITE:8000/api/method/norn_bridge.auth.jwks
NORN_LINEAGE_KEY=$(head -c 32 /dev/urandom | base64 -w0)
VARS
  # --no-watch: the build is fixed; watching would restart on the cell's own state writes.
  docker exec -d -u frappe -w /home/frappe/cell $C bash -lc 'celld dev --no-watch --port 9877 > /home/frappe/cell/celld.log 2>&1'
  for _ in $(seq 120); do cellget /v1/definitions > /dev/null 2>&1 && break; sleep 1; done
  docker exec $C sh -c 'celld --version 2>/dev/null | head -1'
  echo "cell build: $(docker exec $C sha256sum /home/frappe/cell/pkg/norn_cell_do_bg.wasm | cut -c1-16)"
  ;;
apply)
  docker cp "$2" $C:/tmp/bundle.json
  step "stage $(basename "$2") and bench migrate"
  $B bench --site $SITE execute norn_bridge.sync.stage --args '["/tmp/bundle.json"]'
  $B bench --site $SITE migrate 2>&1 | grep -E "norn_bridge|Orphan|rror" || true
  $B bench --site $SITE execute norn_bridge.sync.status
  ;;
users)
  for spec in "viewer@demo.localhost:Norn Viewer" "member@demo.localhost:Norn Member" "nobody@demo.localhost:Desk User"; do
    $B bench --site $SITE execute frappe.client.insert --args "[{\"doctype\":\"User\",\"email\":\"${spec%%:*}\",\"first_name\":\"${spec%%@*}\",\"send_welcome_email\":0,\"new_password\":\"demo-pass-1234\",\"roles\":[{\"role\":\"${spec#*:}\"}]}]" > /dev/null 2>&1 \
      && echo "user ${spec%%:*} with role ${spec#*:}"
  done
  ;;
show)
  admin=$(login Administrator admin)
  step "the cell refuses a caller without a host token"
  cellget "/v1/objects/canon.Item?orgId=org_demo&environmentId=dev"
  step "generated DocType meta (fields Frappe shows for Canon Item)"
  raw "$admin" frappe.desk.form.load.getdoctype --data-urlencode doctype="Canon Item" | python3 -c 'import json,sys; d=json.load(sys.stdin); print(json.dumps(d) if "docs" not in d else [(f["fieldname"], f["fieldtype"]) for f in d["docs"][0]["fields"]])'
  step "generated Workflow for Core Analytics Restatement"
  raw "$admin" frappe.client.get --data-urlencode doctype=Workflow --data-urlencode "name=Core Analytics Restatement status lifecycle" | python3 -c 'import json,sys; d=json.load(sys.stdin)["message"]; print("states:", [s["state"] for s in d["states"]]); print("transitions:", [(t["state"], t["action"], t["next_state"], t["allowed"]) for t in d["transitions"]])'
  for who in Administrator:admin viewer@demo.localhost:demo-pass-1234 member@demo.localhost:demo-pass-1234 nobody@demo.localhost:demo-pass-1234; do
    jar=$(login "${who%%:*}" "${who#*:}")
    step "as ${who%%:*}: Canon Item list, count and one form"
    api "$jar" frappe.desk.reportview.get --data-urlencode doctype="Canon Item" --data-urlencode 'fields=["`tabCanon Item`.`name`","`tabCanon Item`.`code`","`tabCanon Item`.`label`"]' --data-urlencode page_length=3
    api "$jar" frappe.desk.reportview.get_count --data-urlencode doctype="Canon Item" --data-urlencode 'filters=[]'
    curl -s -H "Host: $SITE" -b "$jar" -G http://127.0.0.1:8000/api/method/frappe.desk.form.load.getdoc --data-urlencode doctype="Canon Item" --data-urlencode name=SYN-ITEM-02 | python3 -c 'import json,sys; d=json.load(sys.stdin); doc=(d.get("docs") or [None])[0]; print({k: doc.get(k) for k in ("name","code","label","weight_g") if k in doc} if doc else ("REFUSED", d.get("exc_type")))'
  done
  step "Canon Party with a filter on a generated Select (as member)"
  jar=$(login member@demo.localhost demo-pass-1234)
  api "$jar" frappe.desk.reportview.get --data-urlencode doctype="Canon Party" --data-urlencode 'fields=["`tabCanon Party`.`name`","`tabCanon Party`.`display_name`","`tabCanon Party`.`kind`"]' --data-urlencode 'filters=[["Canon Party","kind","=","organization"]]'
  api "$jar" frappe.desk.reportview.get_sidebar_stats --data-urlencode doctype="Canon Party" --data-urlencode 'stats=["kind"]'
  step "what the cell host logged"
  docker exec $C sh -c 'grep -vE "^\s*$" /home/frappe/cell/celld.log | tail -5'
  ;;
*) echo "usage: $0 start-cell DIR | apply BUNDLE | users | show" >&2; exit 2 ;;
esac
