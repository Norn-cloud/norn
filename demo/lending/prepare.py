# Prepare the lending demo from the public BPIC 2017 log: per-application rows,
# offers, a few applications' events, minute-10 features, an offline logistic
# regression (trained on applications outside the demo subset) and its scores.
import hashlib, json, sys
import numpy as np, pandas as pd
from sklearn.linear_model import LogisticRegression
from sklearn.metrics import roc_auc_score, brier_score_loss
from sklearn.pipeline import make_pipeline
from sklearn.preprocessing import StandardScaler

N = int(sys.argv[1]) if len(sys.argv) > 1 else 1000
EVENT_APPS = int(sys.argv[2]) if len(sys.argv) > 2 else 20
OUT = "/home/user/data/out"
MODEL_VERSION = "completion-lr@2026-11-14.1"
import os; os.makedirs(OUT, exist_ok=True)

C = pd.read_parquet("/home/user/data/cases.parquet").rename(columns={"concept:name": "case"})
E = pd.read_parquet("/home/user/data/events.parquet")
C["RequestedAmount"] = C.RequestedAmount.astype(float)
E = E.sort_values(["case", "ts"], kind="stable")
act = E["concept:name"]
first = lambda a: E[act == a].groupby("case").ts.min()
created = first("A_Create Application")
comp, val = first("A_Complete"), first("A_Validating")
sent = E[act.str.startswith("O_Sent")].groupby("case").ts.min()
pend, den, canc = first("A_Pending"), first("A_Denied"), first("A_Cancelled")
last = E.groupby("case").ts.max()
C = C.set_index("case")
C["created"] = created; C["last"] = last
C["complete"] = comp; C["validated"] = val; C["sent"] = sent
C["outcome_at"] = pend.combine_first(den).combine_first(canc)
C["outcome"] = np.where(C.index.isin(pend.index), "accepted", np.where(C.index.isin(den.index), "denied", np.where(C.index.isin(canc.index), "cancelled", "open")))
C["stage"] = np.where(C.outcome != "open", C.outcome, np.where(C.validated.notna(), "validated", np.where(C.sent.notna(), "offered", "submitted")))
mins = lambda a, b: ((a - b).dt.total_seconds() // 60)
C["m_last"] = mins(C["last"], C.created); C["m_comp"] = mins(C.complete, C.created); C["m_out"] = mins(C.outcome_at, C.created)
C["y"] = (C.m_comp.notna() & (C.m_comp <= 24 * 60)).astype(int)
C["n_events"] = E.groupby("case").size()

# Minute-10 features: only what was known 10 minutes after creation.
E10 = E.join(created.rename("c0"), on="case")
E10 = E10[E10.ts <= E10.c0 + pd.Timedelta(minutes=10)]
g = E10.groupby("case")
F = pd.DataFrame(index=C.index)
F["ev10"] = g.size(); F["w10"] = g["concept:name"].apply(lambda s: s.str.startswith("W_").sum())
F["offers10"] = g["concept:name"].apply(lambda s: (s == "O_Create Offer").sum())
F["online"] = g["concept:name"].apply(lambda s: (s == "A_Submitted").any()).astype(float)
F["auto_share10"] = g["org:resource"].apply(lambda s: (s == "User_1").mean())
F = F.fillna(0)
F["log_amount"] = np.log1p(C.RequestedAmount); F["amount_zero"] = (C.RequestedAmount == 0).astype(float)
F["limit_raise"] = (C.ApplicationType == "Limit raise").astype(float)
F["hour"] = C.created.dt.hour; F["weekend"] = (C.created.dt.dayofweek >= 5).astype(float)
for goal in C.LoanGoal.value_counts().index[:8]:
    F["goal_" + goal.lower().replace(" ", "_").replace(",", "")[:20]] = (C.LoanGoal == goal).astype(float)

rng = np.random.default_rng(20261114)
demo = pd.Index(rng.choice(C.index.to_numpy(), size=N, replace=False))
train = C.index.difference(demo)
model = make_pipeline(StandardScaler(), LogisticRegression(max_iter=2000))
model.fit(F.loc[train], C.y.loc[train])
p = model.predict_proba(F.loc[demo])[:, 1]
auc, brier = roc_auc_score(C.y.loc[demo], p), brier_score_loss(C.y.loc[demo], p)
print(json.dumps({"train": len(train), "demo": N, "base_rate_train": round(C.y.loc[train].mean(), 4), "auc_demo": round(auc, 4), "brier_demo": round(brier, 4), "features": list(F.columns)}))

iso = lambda t: t.strftime("%Y-%m-%dT%H:%M:%SZ")
D = C.loc[demo]
apps = []
for cid, r in D.iterrows():
    apps.append({"dataset": "Application", "record": {
        "id": cid, "loan_goal": r.LoanGoal, "application_type": r.ApplicationType, "requested_amount": float(r.RequestedAmount),
        "created_at": iso(r.created), "last_event_at": iso(r["last"]), "stage": r.stage, "outcome": r.outcome,
        "offer_count": int((E[(E.case == cid) & (act == "O_Create Offer")]).shape[0]) if False else 0,
        "event_count": int(r.n_events), "completed_within_24h": bool(r.y),
        "minutes_to_last_event": int(r.m_last),
        "minutes_to_complete": None if pd.isna(r.m_comp) else int(r.m_comp),
        "minutes_to_outcome": None if pd.isna(r.m_out) else int(r.m_out)}})
# Offers of the demo applications, with their last state.
o = E[(act == "O_Create Offer") & E.case.isin(demo)].copy()
for k in ["OfferedAmount", "MonthlyCost", "FirstWithdrawalAmount", "NumberOfTerms", "CreditScore"]: o[k] = o[k].astype(float)
st = E[E.OfferID.notna() & act.isin(["O_Sent (mail and online)", "O_Sent (online only)", "O_Returned", "O_Accepted", "O_Refused", "O_Cancelled"])]
laststate = st.groupby("OfferID")["concept:name"].last().map(lambda a: {"O_Accepted": "accepted", "O_Refused": "refused", "O_Cancelled": "cancelled", "O_Returned": "returned"}.get(a, "sent"))
ocount = o.groupby("case").size()
for a in apps: a["record"]["offer_count"] = int(ocount.get(a["record"]["id"], 0))
offers = [{"dataset": "Offer", "record": {
    "id": r.EventID, "application_id": r.case, "offered_amount": r.OfferedAmount, "monthly_cost": r.MonthlyCost,
    "first_withdrawal": r.FirstWithdrawalAmount, "terms": int(r.NumberOfTerms), "credit_score": int(r.CreditScore),
    "selected": r.Selected == "true", "status": laststate.get(r.EventID, "created"), "created_at": iso(r.ts)}} for r in o.itertuples()]
# Events of a few applications (the click-through set), every lifecycle step.
ev_apps = [a["record"]["id"] for a in apps if a["record"]["requested_amount"] > 0][:EVENT_APPS]
ev = E[E.case.isin(ev_apps)]
events = [{"dataset": "Application Event", "record": {
    "id": r["EventID"], "application_id": r["case"], "activity": r["concept:name"],
    "origin": r["EventOrigin"], "lifecycle": r["lifecycle:transition"], "resource": r["org:resource"],
    "offer_id": None if pd.isna(r["OfferID"]) else r["OfferID"], "occurred_at": iso(r["ts"])}} for _, r in ev.iterrows()]
scores = []
for cid, prob in zip(demo, p):
    feats = json.dumps({k: float(F.loc[cid, k]) for k in F.columns}, sort_keys=True)
    scores.append({"dataset": "Model Score", "record": {
        "id": f"{cid}@{MODEL_VERSION}", "application_id": cid, "model_version": MODEL_VERSION, "score": round(float(prob), 4),
        "scored_at": iso(C.created[cid] + pd.Timedelta(minutes=10)),
        "features_digest": "sha256:" + hashlib.sha256(feats.encode()).hexdigest()}})
held = [a for a in apps if a["record"]["requested_amount"] <= 0]
ok = [a for a in apps if a["record"]["requested_amount"] > 0]
log = lambda rows: {"source": "public-loan-log", "mapping": "lending.log.mapping", "connectorInstance": "demo", "snapshot": rows}
sc = lambda rows: {"source": "offline-model", "mapping": "lending.scores.mapping", "connectorInstance": "demo", "snapshot": rows}
for name, doc in {"apps-ok": log(ok), "apps-held": log(held), "offers": log(offers), "events": log(events), "scores": sc(scores)}.items():
    json.dump(doc, open(f"{OUT}/{name}.json", "w"))
# Threshold comparison on the demo holdout: who gets help, and how many of them would not have completed.
cmp = {}
yd = C.y.loc[demo].to_numpy()
for t in (0.5, 0.7):
    flag = p < t
    cmp[str(t)] = {"helped": int(flag.sum()), "helped_not_completing": int((flag & (yd == 0)).sum()), "helped_completing_anyway": int((flag & (yd == 1)).sum()), "missed_not_completing": int((~flag & (yd == 0)).sum())}
summary = {"model_version": MODEL_VERSION, "auc": auc, "brier": brier, "coef": dict(zip(F.columns, model[-1].coef_[0].round(3).tolist())),
           "counts": {"apps_ok": len(ok), "apps_held": len(held), "offers": len(offers), "events": len(events), "scores": len(scores)},
           "funnel": D.stage.value_counts().to_dict(), "outcome": D.outcome.value_counts().to_dict(), "thresholds": cmp,
           "event_apps": ev_apps}
json.dump(summary, open(f"{OUT}/summary.json", "w"), indent=1, default=float)
print(json.dumps(summary["counts"]), json.dumps(cmp), json.dumps(summary["funnel"]))
