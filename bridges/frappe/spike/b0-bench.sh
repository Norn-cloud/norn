#!/usr/bin/env bash
# B0 mechanics spike: run on the bench host. Expects a site `demo.localhost`
# with norn_bridge installed, the stub cell on :8787 and this directory at $SPIKE.
set -uo pipefail
SITE=demo.localhost
B="docker exec -u frappe -w /home/frappe/frappe-bench demo-backend-1"
step() { printf '\n### %s\n' "$*"; }

step "stage the hand-written bundle and run bench migrate"
docker cp "$SPIKE/demo-repair.bundle.json" demo-backend-1:/tmp/b0.bundle.json
$B bench --site $SITE execute norn_bridge.sync.stage --args '["/tmp/b0.bundle.json"]'
$B bench --site $SITE migrate 2>&1 | grep -E "norn_bridge|Orphan|rror" || true
$B bench --site $SITE execute frappe.db.get_value --args '["DocType","Demo Repair",["name","module","custom","is_virtual"]]'

step "the controller resolves through the import finder"
$B bench --site $SITE execute norn_bridge.sync.status

step "bench migrate again: the generated DocType survives the orphan sweep"
$B bench --site $SITE migrate 2>&1 | grep -E "norn_bridge|Orphan|rror" || true
$B bench --site $SITE execute frappe.db.exists --args '["DocType","Demo Repair"]'

step "where Frappe resists: a native Workflow insert on a Virtual DocType"
docker cp "$SPIKE/probe_native_workflow.py" demo-backend-1:/tmp/probe_native_workflow.py
docker exec -u frappe -w /home/frappe/frappe-bench/sites demo-backend-1 ../env/bin/python -W ignore /tmp/probe_native_workflow.py $SITE

step "a technician user (role Demo Technician only)"
$B bench --site $SITE execute frappe.client.insert --args '[{"doctype":"User","email":"tech@demo.localhost","first_name":"Tech","send_welcome_email":0,"new_password":"tech-demo-pw-1","roles":[{"role":"Demo Technician"}]}]' >/dev/null 2>&1 || true

step "start the stub cell and the web server"
docker exec demo-backend-1 bash -c 'for p in /proc/[0-9]*; do grep -qa -e gunicorn -e stub_cell $p/cmdline 2>/dev/null && kill ${p#/proc/}; done' || true; sleep 2
docker cp "$SPIKE/stub_cell.py" demo-backend-1:/tmp/stub_cell.py
docker exec -d -u frappe demo-backend-1 bash -lc 'python3 /tmp/stub_cell.py 8787 > /tmp/stub.log 2>&1'
$B bench --site $SITE set-config norn_cell_url http://127.0.0.1:8787 >/dev/null
docker exec -d -u frappe -w /home/frappe/frappe-bench/sites demo-backend-1 ../env/bin/gunicorn --bind=0.0.0.0:8000 --threads=4 --workers=1 --timeout=60 frappe.app:application
for _ in $(seq 60); do curl -s -o /dev/null http://127.0.0.1:8000/api/method/ping && break; sleep 1; done
J=/tmp/b0.cookies; rm -f $J
H=(-s -H "Host: $SITE" -b $J -c $J)
curl "${H[@]}" -X POST http://127.0.0.1:8000/api/method/login -d usr=Administrator -d pwd=admin; echo
SHOW='import json,sys
d=json.load(sys.stdin)
if "exc_type" in d or "exception" in d:
    msgs=[json.loads(m).get("message") for m in json.loads(d.get("_server_messages") or "[]")]
    print("REFUSED", d.get("exc_type"), "|", (d.get("exception") or "").splitlines()[-1][:160], "|", msgs)
else:
    print(json.dumps(d.get("message", d), sort_keys=True)[:1500])'
api() { curl "${H[@]}" -G "http://127.0.0.1:8000/api/method/$1" "${@:2}" | python3 -c "$SHOW"; }
post() { curl "${H[@]}" -X POST "http://127.0.0.1:8000/api/method/$1" "${@:2}" | python3 -c "$SHOW"; }

step "list view (frappe.desk.reportview.get)"
api frappe.desk.reportview.get --data-urlencode doctype="Demo Repair" --data-urlencode 'fields=["`tabDemo Repair`.`name`","`tabDemo Repair`.`title`","`tabDemo Repair`.`status`"]' --data-urlencode 'filters=[["Demo Repair","customer","=","Synthetic Customer A"]]' --data-urlencode 'order_by=`tabDemo Repair`.`modified` desc' --data-urlencode page_length=20

step "list count and sidebar stats"
api frappe.desk.reportview.get_count --data-urlencode doctype="Demo Repair" --data-urlencode 'filters=[]'
api frappe.desk.reportview.get_sidebar_stats --data-urlencode doctype="Demo Repair" --data-urlencode 'stats=["status"]'

step "form view (frappe.desk.form.load.getdoc)"
curl "${H[@]}" -G http://127.0.0.1:8000/api/method/frappe.desk.form.load.getdoc --data-urlencode doctype="Demo Repair" --data-urlencode name=R-001 | python3 -c 'import json,sys; d=json.load(sys.stdin); doc=(d.get("docs") or [{}])[0]; print({k: doc.get(k) for k in ("name","title","customer","status","estimate")} if doc else d)'

step "Number Card (Count) and Report Builder count reach get_list"
api frappe.client.get_list --data-urlencode doctype="Demo Repair" --data-urlencode 'fields=[{"COUNT":"*","as":"result"}]' --data-urlencode 'filters=[["status","=","draft"]]'
post frappe.desk.doctype.number_card.number_card.get_result --data-urlencode 'doc={"function":"Count","document_type":"Demo Repair"}' --data-urlencode 'filters=[["Demo Repair","status","in",["draft","received"]]]'
api frappe.desk.reportview.get --data-urlencode doctype="Demo Repair" --data-urlencode 'fields=["count(`tabDemo Repair`.`name`) as count"]' --data-urlencode 'filters=[]'
api frappe.client.get_list --data-urlencode doctype="Demo Repair" --data-urlencode 'fields=[{"SUM":"estimate","as":"result"}]'
api frappe.desk.reportview.get --data-urlencode doctype="Demo Repair" --data-urlencode 'fields=["`tabDemo Repair`.`status`","count(`tabDemo Repair`.`name`) as count"]' --data-urlencode 'group_by=`tabDemo Repair`.`status`'

step "Workflow transitions for R-001 and apply_workflow(Receive) -> db_update"
post frappe.model.workflow.get_transitions --data-urlencode "doc={\"doctype\":\"Demo Repair\",\"name\":\"R-001\"}"
post frappe.model.workflow.apply_workflow --data-urlencode "doc={\"doctype\":\"Demo Repair\",\"name\":\"R-001\"}" --data-urlencode action=Receive 
post frappe.model.workflow.apply_workflow --data-urlencode "doc={\"doctype\":\"Demo Repair\",\"name\":\"R-001\"}" --data-urlencode action=Deliver 

step "as the technician: Frappe offers no Receive (generated Workflow roles); the cell sees role=viewer"
T=/tmp/b0.tech; rm -f $T
curl -s -H "Host: $SITE" -c $T -X POST http://127.0.0.1:8000/api/method/login -d usr=tech@demo.localhost -d pwd=tech-demo-pw-1 >/dev/null
curl -s -H "Host: $SITE" -b $T -X POST http://127.0.0.1:8000/api/method/frappe.model.workflow.get_transitions --data-urlencode 'doc={"doctype":"Demo Repair","name":"R-003"}' | python3 -c "$SHOW"
curl -s -H "Host: $SITE" -b $T -X POST http://127.0.0.1:8000/api/method/frappe.model.workflow.apply_workflow --data-urlencode 'doc={"doctype":"Demo Repair","name":"R-002"}' --data-urlencode action=Repair | python3 -c "$SHOW"
curl -s -H "Host: $SITE" -b $T -X POST http://127.0.0.1:8000/api/method/frappe.model.workflow.apply_workflow --data-urlencode 'doc={"doctype":"Demo Repair","name":"R-001"}' --data-urlencode action=Receive | python3 -c "$SHOW"

step "DocType Action (Server Action) resolves through norn_bridge.act.__getattr__"
post norn_bridge.act.demo_repair_reopen --data-urlencode "doc={\"doctype\":\"Demo Repair\",\"name\":\"R-003\"}"
post norn_bridge.act.not_in_bundle --data-urlencode "doc={}" 

step "what reached the stub cell"
docker exec demo-backend-1 cat /tmp/stub.log
