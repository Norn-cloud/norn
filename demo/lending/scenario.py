"""The lending demo's governed-action scenario, as the officer and the assistant (an API key).

Runs against a hosted cell with the lending pack installed and its data loaded.
`officer(action, input, key)` posts as a signed-in admin, and `agent(...)` posts
with the member API key (a `service:` principal). See registry/packs/lending.
"""
MV = "completion-lr@2026-11-14.1"
A, B = "Application_295469559", "Application_1548597068"   # scores 0.4311 and 0.5850
out = {}
def run(label, r): show(label, r); out[label] = r
# The assistant at minute 10 on A (score 0.43, below the active 0.50 policy).
run("agent proposes offer_help on A", agent("lending.proposal.submit", {"id": f"prop-{A}-help", "application_id": A, "kind": "offer_help", "rationale": "Score 0.43 at minute 10: below the 0.50 policy. Offer a call to help finish the application."}, f"prop-{A}-help"))
run("agent offers help on A under assist@1", agent("lending.assistance.offer", {"id": f"help-{A}", "application_id": A, "score_id": f"{A}@{MV}", "policy_id": "assist@1", "channel": "call", "message": "Can we help you finish your application? A specialist can call you today."}, f"help-{A}"))
run("agent proposes decline on A", agent("lending.proposal.submit", {"id": f"prop-{A}-decline", "application_id": A, "kind": "decline", "rationale": "Low completion likelihood; decline to save handling time."}, f"prop-{A}-decline"))
run("agent proposes price on A", agent("lending.proposal.submit", {"id": f"prop-{A}-price", "application_id": A, "kind": "price", "rationale": "Offer a lower rate to raise completion."}, f"prop-{A}-price"))
run("agent declines A directly", agent("lending.application.decline", {"id": A}, f"agent-decline-{A}"))
# B (0.585) is above the active 0.50 threshold: the assistant may not help yet.
run("agent offers help on B under assist@1", agent("lending.assistance.offer", {"id": f"help-{B}", "application_id": B, "score_id": f"{B}@{MV}", "policy_id": "assist@1", "channel": "email", "message": "Can we help you finish your application?"}, f"help-{B}-v1"))
# The officer retires v1 and promotes v2 (0.70): a versioned policy change, each step with a receipt.
run("officer retires assist@1", officer("lending.policy.retire", {"id": "assist@1"}, "retire-assist-1"))
run("officer promotes assist@2", officer("lending.policy.promote", {"id": "assist@2"}, "promote-assist-2"))
run("agent offers help on B under assist@2", agent("lending.assistance.offer", {"id": f"help-{B}-v2", "application_id": B, "score_id": f"{B}@{MV}", "policy_id": "assist@2", "channel": "email", "message": "Can we help you finish your application?"}, f"help-{B}-v2"))
json.dump({k: [v[0], v[1]] for k, v in out.items()}, open("scenario.json", "w"), indent=1)
