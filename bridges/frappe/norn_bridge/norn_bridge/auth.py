"""Frappe session -> cell principal (#1085 §5), with no permission logic here.

The bridge is a token issuer the cell trusts by configuration: the cell pins
this site's issuer string and JWKS URL. Per request the bridge signs a short
EdDSA host token naming the Frappe user and the membership role that the
bundle's declarative role map assigns to the user's Frappe roles. The cell
verifies it and alone decides what that role may read.
"""

from __future__ import annotations

import base64
import json
import secrets
import time

import frappe

AUDIENCE = "norn-host"
LIFETIME = 60


def _b64(raw: bytes) -> str:
    return base64.urlsafe_b64encode(raw).rstrip(b"=").decode()


def _key():
    from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

    seed = frappe.conf.get("norn_bridge_signing_seed")
    if not seed:
        frappe.throw("norn_bridge_signing_seed is not configured for this site")
    return Ed25519PrivateKey.from_private_bytes(base64.b64decode(seed))


def _kid(public: bytes) -> str:
    import hashlib

    return hashlib.sha256(public).hexdigest()[:16]


def _public() -> bytes:
    from cryptography.hazmat.primitives import serialization

    return _key().public_key().public_bytes(
        serialization.Encoding.Raw, serialization.PublicFormat.Raw
    )


def issuer() -> str:
    return frappe.conf.get("norn_bridge_issuer") or f"frappe:{frappe.local.site}"


def membership_role(user: str) -> str | None:
    """The first entry of the bundle's role map that the user holds."""
    from norn_bridge import bundle

    held = set(frappe.get_roles(user))
    for frappe_role, norn_role in (bundle.load() or {}).get("principal", {}).get("roleMap", []):
        if frappe_role in held:
            return norn_role
    return None


def token(user: str | None = None) -> str | None:
    user = user or frappe.session.user
    role = membership_role(user)
    if role is None or user == "Guest":
        return None
    now = int(time.time())
    public = _public()
    header = {"alg": "EdDSA", "typ": "JWT", "kid": _kid(public)}
    claims = {
        "iss": issuer(),
        "aud": AUDIENCE,
        "sub": user,
        "iat": now,
        "exp": now + LIFETIME,
        "jti": secrets.token_hex(8),
        "norn": {
            "org": frappe.conf.get("norn_org_id"),
            "kind": "user",
            "amr": "password",
            "role": role,
        },
    }
    signed = ".".join(
        _b64(json.dumps(part, separators=(",", ":")).encode()) for part in (header, claims)
    )
    return f"{signed}.{_b64(_key().sign(signed.encode()))}"


@frappe.whitelist(allow_guest=True, methods=["GET"])
def jwks():
    """The issuer's public key set, at the top level of the response."""
    public = _public()
    frappe.local.response.update(
        {"keys": [{"kty": "OKP", "crv": "Ed25519", "kid": _kid(public), "x": _b64(public), "use": "sig"}]}
    )
