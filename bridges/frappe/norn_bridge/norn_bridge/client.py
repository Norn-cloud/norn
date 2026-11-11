"""The cell's HTTP routes, called as the Frappe user's principal."""

from __future__ import annotations

import json
import urllib.error
import urllib.parse
import urllib.request

import frappe

TIMEOUT = 5


class Refused(Exception):
    def __init__(self, status: int, document: dict):
        self.status = status
        self.document = document
        super().__init__(document.get("message") or f"HTTP {status}")


def _scope() -> dict:
    return {
        "orgId": frappe.conf.get("norn_org_id"),
        "environmentId": frappe.conf.get("norn_environment_id"),
    }


def call(method: str, path: str, query: dict | None = None, body: dict | None = None) -> dict:
    from norn_bridge import auth

    base = frappe.conf.get("norn_cell_url")
    if not base:
        frappe.throw("norn_cell_url is not configured for this site")
    params = {**_scope(), **{k: v for k, v in (query or {}).items() if v is not None}}
    url = f"{base.rstrip('/')}{path}?{urllib.parse.urlencode(params)}"
    data = None if body is None else json.dumps(body).encode()
    request = urllib.request.Request(url, data=data, method=method)
    request.add_header("accept", "application/json")
    if data is not None:
        request.add_header("content-type", "application/json")
    bearer = auth.token()
    if bearer:
        request.add_header("authorization", f"Bearer {bearer}")
    try:
        with urllib.request.urlopen(request, timeout=TIMEOUT) as response:
            return json.loads(response.read() or b"{}")
    except urllib.error.HTTPError as error:
        try:
            document = json.loads(error.read() or b"{}")
        except ValueError:
            document = {}
        raise Refused(error.code, document) from None
    except (urllib.error.URLError, TimeoutError, ConnectionError) as error:
        raise Refused(503, {"code": "bridge.cell_unavailable", "message": f"Ledger unavailable: {error}"}) from None


def segment(text: str) -> str:
    return urllib.parse.quote(text, safe="")


def get_object(object_type: str, object_id: str) -> dict:
    return call("GET", f"/v1/objects/{segment(object_type)}/{segment(object_id)}")


def query_objects(object_type: str, **query) -> dict:
    return call("GET", f"/v1/objects/{segment(object_type)}", query)


def act(action: str, body: dict) -> dict:
    """An action write (#1085 §4). The host does not serve actions yet (#805)."""
    return call("POST", f"/v1/actions/{segment(action)}", body=body)
