"""`check_regression`: read a dataset from a cell, run the suite, record the results in the cell.

Everything goes through the client generated from the cell's OpenAPI
document (`norn_client`), as the caller: the rows are the objects the caller
may read, and the results are written by the governed actions
`stats.modelspec.record` and `stats.assumptioncheck.record`, so each one has a
receipt and shows in Studio like any other object.

Reproducible by construction: the model spec's id is a hash of the type, the
target, the features, the order field, the suite version and the dataset
revision (a hash of every row's id and version). Each action call's
idempotency key is derived from that id, so rerunning on the same revision
replays the same calls and gets the same receipts; any write to the dataset
changes the revision and records a new spec.
"""

from __future__ import annotations

import hashlib
import json
from dataclasses import dataclass, field
from pathlib import Path

import pandas as pd

from norn_client import AuthenticatedClient
from norn_client.api.actions import call_action
from norn_client.api.objects import query_objects
from norn_client.models import ActionRequest, Refusal

from .suite import Check, assumption_suite, environment, summary, SUITE_VERSION


class Refused(Exception):
    """The cell refused a read or a write; the message carries its refusal."""


@dataclass(frozen=True)
class Cell:
    client: AuthenticatedClient
    org_id: str
    environment_id: str

    @classmethod
    def from_project(cls, project: str | Path = ".") -> "Cell":
        """The local development host `norn dev` serves for a project, with its session token."""
        project = Path(project)
        session = json.loads((project / ".norn/dev.json").read_text())
        token = (project / ".norn/dev.token").read_text().strip()
        return cls(AuthenticatedClient(base_url=session["url"], token=token),
                   session["scope"]["orgId"], session["scope"]["environmentId"])

    @classmethod
    def from_env(cls) -> "Cell":
        """A hosted cell through Studio's proxy: NORN_URL (https://<studio>/api/host), ORG, ENV,
        and NORN_TOKEN_FILE holding a Studio API key, sent as `x-api-key`."""
        import os
        key = Path(os.environ["NORN_TOKEN_FILE"]).read_text().strip()
        client = AuthenticatedClient(base_url=os.environ["NORN_URL"].rstrip("/"), token=key, prefix="", auth_header_name="x-api-key")
        return cls(client, os.environ["ORG"], os.environ["ENV"])

    @property
    def where(self) -> dict:
        return {"org_id": self.org_id, "environment_id": self.environment_id}

    def export(self, object_type: str, columns: list[str]) -> pd.DataFrame | None:
        """The columns through the cell's analytical engine (`:export`, Parquet): every row the
        caller may read, nulls kept, `id` and `version` included. The answer's snapshot
        (`Norn-Snapshot`) is in `frame.attrs["snapshot"]`. None when the host has no such
        engine (404 or 501), so the caller falls back to the paged object route."""
        import io

        answer = self.client.get_httpx_client().get(
            f"/v1/objects/{object_type}:export",
            params={"columns": ",".join(columns), "format": "parquet", "orgId": self.org_id, "environmentId": self.environment_id},
        )
        if answer.status_code in (404, 501):
            return None
        if answer.status_code != 200:
            raise Refused(f"exporting {object_type} ({answer.status_code}): {answer.text}")
        frame = pd.read_parquet(io.BytesIO(answer.content))
        frame.attrs["snapshot"] = answer.headers.get("Norn-Snapshot")
        frame.attrs["withheld"] = int(answer.headers.get("Norn-Withheld-Rows", "0"))
        return frame

    def rows(self, object_type: str, columns: list[str] | None = None) -> pd.DataFrame:
        """Every object of the type the caller may read, as a dataframe with `id` and `version`.
        With `columns`, read through the analytical engine when the host has one."""
        if columns:
            exported = self.export(object_type, columns)
            if exported is not None:
                return exported
        rows, after = [], None
        while True:
            page = query_objects.sync(object_type, client=self.client, limit=200, **self.where, **({"after": after} if after else {}))
            if page is None or isinstance(page, Refusal):
                raise Refused(f"reading {object_type}: {page}")
            for record in page.records:
                if record.withheld:
                    continue
                values = record.values.to_dict() if record.values else {}
                rows.append({"id": record.id, "version": record.version, **values})
            after = page.next_
            if not after:
                return pd.DataFrame(rows)

    def call(self, action: str, values: dict, key: str) -> dict:
        body = ActionRequest.from_dict({"input": values, "idempotencyKey": key})
        answer = call_action.sync_detailed(action, client=self.client, body=body, **self.where)
        if answer.status_code != 200:
            raise Refused(f"{action} ({answer.status_code}): {answer.content.decode(errors='replace')}")
        return json.loads(answer.content)


@dataclass
class RegressionChecks:
    spec: dict
    checks: list[Check]
    plots: dict
    receipts: list[str] = field(default_factory=list)

    def frame(self) -> pd.DataFrame:
        return pd.DataFrame([check.as_dict() for check in self.checks])

    @property
    def status(self) -> str:
        return self.spec["summary"]


def revision(rows: pd.DataFrame) -> str:
    """The dataset revision the checks ran on: a hash of every row's id and version."""
    pairs = sorted(zip(rows["id"].astype(str), rows["version"].astype(int))) if len(rows) else []
    return hashlib.sha256(json.dumps(pairs, separators=(",", ":")).encode()).hexdigest()


def check_regression(object_type: str, target: str, features: list[str], *, cell: Cell, order: str | None = None,
                     request: str | None = None) -> RegressionChecks:
    """Run the assumption suite on one object type and record the results in the cell.
    `request`: the stats.CheckRequest this run answers (the worker passes it); the spec links to it."""
    wanted = [target, *features, *([order] if order else [])]
    rows = cell.rows(object_type, wanted)
    missing = [name for name in wanted if name not in rows.columns]
    if missing:
        raise ValueError(f"{object_type} has no field {', '.join(missing)} (fields: {', '.join(c for c in rows.columns if c not in ('id', 'version'))})")
    # The engine's snapshot when it answered (the same snapshot re-exports the same rows),
    # else a hash of every row's id and version.
    dataset = rows.attrs.get("snapshot") or revision(rows)
    identity = json.dumps([object_type, target, sorted(features), order, SUITE_VERSION, dataset, *([request] if request else [])], separators=(",", ":"))
    spec_id = "ms-" + hashlib.sha256(identity.encode()).hexdigest()[:24]
    checks, plots = assumption_suite(rows, target, list(features), order)
    spec = {"spec": spec_id, "object_type": object_type, "target": target, "features": ",".join(features),
            "order_field": order, "dataset_revision": dataset, "rows": int(len(rows)), "rows_used": int(plots["n"]),
            "method": environment(), "summary": summary(checks), "request": request}
    result = RegressionChecks(spec, checks, plots)
    # An absent value (no order field, no p-value) is recorded as null: the properties are nullable.
    action = "stats.modelspec.record_for_request" if request else "stats.modelspec.record"
    result.receipts.append(cell.call(action, spec, spec_id)["receipt"]["receiptHash"])
    for index, check in enumerate(checks):
        values = {"check": f"{spec_id}-{index:02d}", "spec": spec_id, "assumption": check.assumption, "test": check.test,
                  "subject": check.subject, "status": check.status, "statistic": check.statistic, "p_value": check.p_value,
                  "threshold": check.threshold, "threshold_source": check.threshold_source, "remediation": check.remediation}
        result.receipts.append(cell.call("stats.assumptioncheck.record", values, values["check"])["receipt"]["receiptHash"])
    return result
