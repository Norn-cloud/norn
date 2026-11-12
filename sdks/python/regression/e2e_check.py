"""What the demo must show, against the running dev host: run twice, then after a write."""
import sys

from norn_regression import Cell, check_regression

cell = Cell.from_project(sys.argv[1])
args = dict(target="y", features=["x1", "x2", "x3"], order="t", cell=cell)


def status(result, test, subject=None):
    [check] = [c for c in result.checks if c.test.startswith(test) and (subject is None or c.subject == subject)]
    return check.status


first = check_regression("lab.Sample", **args)
assert status(first, "Breusch-Pagan") == "fail", first.frame()
assert all(status(first, "variance inflation factor", f) == "fail" for f in ("x1", "x2", "x3"))
assert status(first, "Durbin-Watson") == "fail"
assert first.status == "fail"

# Recorded in the cell, linked to the spec, with receipts.
specs = cell.rows("stats.ModelSpec")
checks = cell.rows("stats.AssumptionCheck")
assert list(specs["spec"]) == [first.spec["spec"]], specs
assert len(checks) == len(first.checks) and set(checks["spec"]) == {first.spec["spec"]}
assert set(checks["status"]) <= {"pass", "warn", "fail", "not_applicable"}
# Absence is a typed null, never "": a check without a p-value reads back as null.
vif = checks[checks["test"] == "variance inflation factor"]
assert len(vif) == 3 and vif["p_value"].isna().all(), vif[["test", "p_value"]]
assert checks[checks["test"] == "Breusch-Pagan (LM)"]["p_value"].notna().all()
assert not (checks["p_value"] == "").any()

# Rerunning on the same revision replays: the same checks and the same receipts, nothing new.
again = check_regression("lab.Sample", **args)
assert [c.as_dict() for c in again.checks] == [c.as_dict() for c in first.checks]
assert again.receipts == first.receipts
assert len(cell.rows("stats.ModelSpec")) == 1 and len(cell.rows("stats.AssumptionCheck")) == len(first.checks)

# A write to the dataset is a new revision, and a new spec beside the first.
cell.call("lab.sample.add", {"sample": "s99999", "t": 99999, "x1": "1.0000", "x2": "2.0000", "x3": "3.0000", "y": "4.0000"}, "extra-row")
later = check_regression("lab.Sample", **args)
assert later.spec["dataset_revision"] != first.spec["dataset_revision"] and later.spec["spec"] != first.spec["spec"]
assert len(cell.rows("stats.ModelSpec")) == 2
# One click in Studio: a queued request, run by the worker through the same governed actions.
import subprocess  # noqa: E402
# As Studio's form submits: only the person's choices; Norn derives status, spec and error.
cell.call("stats.checkrequest.submit", {"request": "rq-demo-1", "object_type": "lab.Sample", "target": "y", "features": "x1,x2",
                                        "order_field": "t"}, "rq-demo-1")
cell.call("stats.checkrequest.submit", {"request": "rq-demo-2", "object_type": "lab.Sample", "target": "y", "features": "nope",
                                        "order_field": None}, "rq-demo-2")
subprocess.run([sys.executable, "-m", "norn_regression.worker", "--project", sys.argv[1], "--once"], check=True)
requests = cell.rows("stats.CheckRequest").set_index("request")
assert requests.loc["rq-demo-1", "status"] == "done", requests
spec = requests.loc["rq-demo-1", "spec"]
assert spec in set(cell.rows("stats.ModelSpec")["spec"])
assert requests.loc["rq-demo-2", "status"] == "failed" and "nope" in requests.loc["rq-demo-2", "error"], requests
assert requests.loc["rq-demo-2", "spec"] is None or requests.loc["rq-demo-2", "spec"] != requests.loc["rq-demo-2", "spec"], requests  # null, not ""
assert requests.loc["rq-demo-1", "error"] is None or requests.loc["rq-demo-1", "error"] != requests.loc["rq-demo-1", "error"], requests
# The done request links to its spec ("result"), so Studio can follow request → spec → checks.
from norn_client.api.objects import get_object_links  # noqa: E402
links = get_object_links.sync("stats.CheckRequest", "rq-demo-1", client=cell.client, **cell.where)
assert any(edge.to_dict().get("link") == "result" for edge in links.items), [edge.to_dict() for edge in links.items]
# A second pass finds nothing queued.
subprocess.run([sys.executable, "-m", "norn_regression.worker", "--project", sys.argv[1], "--once"], check=True)
assert len(cell.rows("stats.CheckRequest")) == 2

print(f"regression demo: ok ({len(first.checks)} checks, {len(first.receipts)} receipts, replay identical, new revision recorded, worker ran one request and failed one with its reason)")
