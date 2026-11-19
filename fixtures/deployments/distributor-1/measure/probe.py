#!/usr/bin/env python3
"""After a load: what landed, what was held back and by which law, how the
cross-system holdings resolved (#1402), the questions and their latency, the
governed actions with approval, and an MCP agent's reconciliation.

The MCP agent is a deterministic script over the cell's MCP tools (no model
credentials enter the test VM): it does what an LLM agent would do through the
same tool surface (query receipts and invoices, propose matches by amount and
date, call the reconcile action), so the tools and their refusals are what is
measured, not a model's judgement.

Usage: probe.py PROJECT_DIR WORLD_JSON NATIVE_IDS_JSON OUT_JSON
"""
from __future__ import annotations

import json
import statistics
import sys
import time
import urllib.request
from collections import Counter
from decimal import Decimal
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from norn import Norn, rss_kb  # noqa: E402

TYPES = ["canon.Party", "canon.PartyRole", "canon.Item", "canon.Holding", "canon.OrderLine", "canon.Delivery", "canon.Posting", "canon.SettlementObservation",
         "distributor.CustomerProfile", "distributor.SalesInvoice", "distributor.CustomerOrder", "distributor.DocumentLine", "distributor.BankReceipt",
         "distributor.StockLedger", "distributor.StockMove", "distributor.PriceListEntry", "distributor.ReceivableBalance", "distributor.CreditRequest", "p2p.Case", "p2p.Event"]


def props(o: dict) -> dict:
    return o.get("properties") or o.get("props") or o


def oid(o: dict) -> str:
    return o.get("id") or o.get("objectId") or o.get("subject") or ""


def section(fn):
    def run(*a):
        try:
            return fn(*a)
        except Exception as e:
            return {"error": f"{type(e).__name__}: {e}"[:600]}
    return run


@section
def counts(n: Norn) -> dict:
    out = {}
    for t in TYPES:
        objs, code = n.objects(t)
        qcode, q, _ = n.call("GET", f"/v1/objects/{t}:quarantined", query={"limit": 500})
        held = (q.get("objects") or q.get("items") or []) if isinstance(q, dict) else []
        out[t] = {"objects": len(objs), "status": code, "quarantined": len(held), "quarantinedStatus": qcode}
    return out


@section
def inbox(n: Norn) -> dict:
    code, body, dt = n.call("GET", "/v1/inbox", query={"limit": 1000})
    rules = Counter()
    items = (body.get("items") or body.get("rules") or body.get("entries") or []) if isinstance(body, dict) else []
    for it in items:
        rules[it.get("rule") or it.get("law") or it.get("code") or it.get("reason") or json.dumps(it)[:80]] += int(it.get("count") or 1)
    return {"status": code, "ms": round(dt * 1000, 1), "byRule": dict(rules.most_common(40)), "sample": json.dumps(body)[:1500]}


@section
def holdings(n: Norn, world: dict) -> dict:
    """#1402: the same canonical holding reported by ERPNext (on hand), Saleor (available) and the WMS."""
    objs, _ = n.objects("canon.Holding")
    closing = {(f"d1/holding/{s['wh']}/{s['item']}"): s["qty"] for s in world["closing_stock"]}
    drift = {(f"d1/holding/{s['wh']}/{s['item']}"): s["qty"] for s in world["legacy_drift"]}
    winners, disagree = Counter(), 0
    for o in objs:
        p, i = props(o), oid(o)
        q = p.get("quantity")
        basis = p.get("quantity_basis")
        truth = closing.get(i)
        try:
            qd = Decimal(str(q))
        except Exception:
            continue
        if truth is None:
            continue
        if i in drift and qd == drift[i]:
            winners["legacy-wms (drifted value)"] += 1
        elif basis == "available":
            winners["saleor (available, not on hand)"] += 1
        elif qd == truth:
            winners["agrees with ledger truth"] += 1
        else:
            winners["other / stale"] += 1
        f = n.call("GET", f"/v1/objects/canon.Holding/{urllib.request.quote(i, safe='')}/findings")[1]
        if isinstance(f, dict) and json.dumps(f).count("SOURCE-AGREE"):
            disagree += 1
    return {"holdings": len(objs), "winner": dict(winners), "withSourceAgreeFinding": disagree}


def metric_for(metrics: list, type_: str, prop: str) -> str | None:
    for m in metrics:
        if m.get("objectType") == type_ and prop in json.dumps(m):
            return m["name"]
    return None


@section
def questions(n: Norn) -> dict:
    code, body, _ = n.call("GET", "/v1/questions:metrics")
    metrics = body.get("metrics", []) if isinstance(body, dict) else []
    out = {"metricsStatus": code, "metrics": len(metrics), "metricNames": [m.get("name") for m in metrics][:60]}
    asks = {
        "revenue_by_region_month": ("distributor.SalesInvoice", "total", [{"path": {"property": "region"}}, {"grain": "month", "path": {"property": "posted_at"}}]),
        "stock_by_location": ("canon.Holding", "quantity", [{"path": {"property": "location"}}]),
        "p2p_worth_by_activity_month": ("p2p.Event", "net_worth_eur", [{"path": {"property": "activity"}}, {"grain": "month", "path": {"property": "occurred_at"}}]),
    }
    for name, (t, prop, by) in asks.items():
        m = metric_for(metrics, t, prop)
        if not m:
            out[name] = {"error": f"no metric over {t}.{prop}"}
            continue
        q = {"irVersion": "0.1", "name": f"d1.{name}", "objectSet": t, "aggregate": [{"metric": m}], "groupBy": by, "filter": [], "calendar": "iso_utc",
             "freshness": {"kind": "live", "maxAgeMs": 60000}, "window": {"start": "2018-01-01T00:00:00Z", "end": "2027-01-01T00:00:00Z"}}
        lat, last = [], None
        for _ in range(5):
            c, b, dt = n.call("POST", "/v1/questions:evaluate", {"question": q})
            lat.append(dt * 1000)
            last = (c, b)
        c, b = last
        rows = (b.get("rows") or b.get("answer", {}).get("rows") or []) if isinstance(b, dict) else []
        out[name] = {"metric": m, "status": c, "rows": len(rows) if isinstance(rows, (list, dict)) else None, "p50ms": round(statistics.median(lat), 1), "maxms": round(max(lat), 1),
                     "sample": json.dumps(b, ensure_ascii=False)[:900]}
    return out


@section
def actions(n: Norn, reviewer: Norn) -> dict:
    out = {}
    profiles, _ = n.objects("distributor.CustomerProfile", limit=5, max_pages=1)
    pid = props(profiles[0]).get("profile_id") if profiles else "erpnext/none"
    me = "local:developer"
    for rid, limit in (("CR-SMALL-1", "50000.00"), ("CR-LARGE-1", "750000.00")):
        c1, b1, _ = n.call("POST", "/v1/actions/distributor.credit.request", {"input": {"request_id": rid, "profile_id": pid, "new_limit": limit, "requester": me}, "idempotencyKey": f"{rid}-create"})
        c1b, b1b, _ = n.call("POST", "/v1/actions/distributor.credit.request", {"input": {"request_id": rid, "profile_id": pid, "new_limit": limit, "requester": me}, "idempotencyKey": f"{rid}-create"})
        c2, b2, _ = n.call("POST", "/v1/actions/distributor.credit.submit", {"input": {"request_id": rid}, "idempotencyKey": f"{rid}-submit"})
        decision = None
        if c2 == 202 and isinstance(b2, dict):
            did = (b2.get("decision") or {}).get("id")
            c3, b3, _ = reviewer.call("POST", f"/v1/decisions/{did}:decide", {"choice": "approve", "note": "finance ok", "idempotencyKey": f"{rid}-decide"})
            decision = {"status": c3, "body": json.dumps(b3)[:300]}
        c4, b4, _ = reviewer.call("POST", "/v1/actions/distributor.credit.approve", {"input": {"request_id": rid}, "idempotencyKey": f"{rid}-approve"})
        c5, b5, _ = n.call("POST", "/v1/actions/distributor.credit.approve", {"input": {"request_id": rid}, "idempotencyKey": f"{rid}-self-approve"})
        out[rid] = {"create": [c1, json.dumps(b1)[:300]], "createReplay": [c1b, json.dumps(b1b)[:200]], "submit": [c2, json.dumps(b2)[:300]], "decision": decision,
                    "approveByReviewer": [c4, json.dumps(b4)[:300]], "approveBySelf": [c5, json.dumps(b5)[:300]]}
    # a schema refusal: an unknown currency on the reconcile action
    c6, b6, _ = n.call("POST", "/v1/actions/distributor.receipt.reconcile", {"input": {"receipt_id": "x", "bank": "BANK-Z"}, "idempotencyKey": "bad-1"})
    out["schemaRefusal"] = [c6, json.dumps(b6)[:400]]
    return out


class Mcp:
    def __init__(self, n: Norn):
        self.n, self.id = n, 0

    def rpc(self, method: str, params: dict) -> dict:
        self.id += 1
        url = f"{self.n.base}/v1/mcp?orgId={self.n.scope['orgId']}&environmentId={self.n.scope['environmentId']}"
        req = urllib.request.Request(url, data=json.dumps({"jsonrpc": "2.0", "id": self.id, "method": method, "params": params}).encode(), method="POST",
                                     headers={"Authorization": f"Bearer {self.n.token}", "Content-Type": "application/json", "Accept": "application/json, text/event-stream",
                                              "MCP-Protocol-Version": "2025-06-18"})
        with urllib.request.urlopen(req, timeout=300) as r:
            raw = r.read().decode()
        if raw.lstrip().startswith("{"):
            return json.loads(raw)
        for line in raw.splitlines():
            if line.startswith("data:"):
                return json.loads(line[5:])
        return {"raw": raw[:300]}

    def tool(self, name: str, args: dict):
        r = self.rpc("tools/call", {"name": name, "arguments": args})
        res = r.get("result") or {}
        text = "".join(c.get("text", "") for c in res.get("content", []) if isinstance(c, dict))
        sc = res.get("structuredContent")
        try:
            return sc if sc is not None else json.loads(text), res.get("isError", False)
        except ValueError:
            return text, res.get("isError", False)


@section
def mcp_agent(n: Norn, world: dict, native: dict) -> dict:
    m = Mcp(n)
    t0 = time.time()
    init = m.rpc("initialize", {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "d1-reconciler", "version": "0.1.0"}})
    tools = [t["name"] for t in m.rpc("tools/list", {}).get("result", {}).get("tools", [])]
    def all_of(t):
        got, _ = m.tool("norn_query_objects", {"type": t, "limit": 1000})
        return (got.get("objects") or got.get("items") or []) if isinstance(got, dict) else []
    receipts, invoices = all_of("distributor.BankReceipt"), all_of("distributor.SalesInvoice")
    by_amount: dict[str, list] = {}
    for inv in invoices:
        p = props(inv)
        by_amount.setdefault(str(Decimal(str(p.get("total", "0")))), []).append(p)
    # truth for precision: world payment -> world invoice -> native invoice name
    si = native.get("erpnext_si", {})
    truth = {p["id"]: si.get(p["invoice"]) for p in world["payments"] if p.get("invoice")}
    stats = Counter()
    for r in receipts:
        p = props(r)
        cands = by_amount.get(str(Decimal(str(p.get("amount", "0")))), [])
        cands = [c for c in cands if c.get("posted_at", "") <= p.get("booked_at", "") and c.get("currency") == p.get("currency")]
        if len(cands) != 1:
            stats["no_unique_candidate" if cands else "no_candidate"] += 1
            continue
        inv = cands[0]
        res, err = m.tool("norn_call_action", {"action": "distributor.receipt.reconcile", "input": {**{k: p.get(k) for k in ("receipt_id", "bank", "booked_at", "currency", "amount", "remittance")},
                                                                                                  "matched": True, "invoice_id": inv["invoice_id"]}})
        stats["action_refused" if err else "reconciled"] += 1
        if err and stats["action_refused"] <= 3:
            stats[f"refusal:{json.dumps(res)[:160]}"] += 1
        wid = p.get("receipt_id", "").split("-", 1)[-1]
        if not err and truth.get(wid):
            stats["correct" if inv["invoice_id"].endswith(truth[wid]) else "wrong_match"] += 1
    findings, _ = m.tool("norn_list_held", {"type": "distributor.BankReceipt"}) if "norn_list_held" in tools else ({}, False)
    return {"initialize": bool(init.get("result")), "tools": len(tools), "receipts": len(receipts), "invoices": len(invoices), "outcome": dict(stats),
            "heldReceipts": json.dumps(findings)[:600], "seconds": round(time.time() - t0, 1)}


def main() -> None:
    project, world, native, out = Path(sys.argv[1]), json.loads(Path(sys.argv[2]).read_text()), json.loads(Path(sys.argv[3]).read_text()) if Path(sys.argv[3]).exists() else {}, Path(sys.argv[4])
    n, rv = Norn(project), Norn(project, reviewer=True)
    which = set(sys.argv[5:]) or {"counts", "inbox", "holdings", "questions", "actions", "mcp"}
    report = json.loads(out.read_text()) if out.exists() else {}
    steps = {"counts": lambda: counts(n), "inbox": lambda: inbox(n), "holdings": lambda: holdings(n, world), "questions": lambda: questions(n),
             "actions": lambda: actions(n, rv), "mcp": lambda: mcp_agent(n, world, native)}
    for k in [k for k in steps if k in which]:
        t = time.time()
        report[k] = steps[k]()
        if isinstance(report[k], dict):
            report[k]["_seconds"] = round(time.time() - t, 1)
        print(k, json.dumps(report[k], ensure_ascii=False)[:600], flush=True)
    report["rssKb"] = rss_kb()
    out.write_text(json.dumps(report, indent=1, ensure_ascii=False))


if __name__ == "__main__":
    main()
