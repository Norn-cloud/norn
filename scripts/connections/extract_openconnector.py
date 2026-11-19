#!/usr/bin/env python3
"""Extract OAuth provider metadata from OpenConnector v1.9.4 into Norn's
connector auth form (`registry/connections/providers.json`).

Source: https://github.com/oomol-lab/open-connector at tag v1.9.4, commit
d89dc9f24f5a057bb500272d7dbf333837c77c68, Apache-2.0. Only that commit is
read: every input is fetched by its commit-pinned raw URL and must match the
SHA-256 pinned below, so a later commit can never be read by accident. The
licence file is checked to be the Apache License 2.0 and the upstream NOTICE
is carried into the output.

Only auth metadata is extracted (authorize/token/refresh/revocation URLs,
scopes, scope separator, client authentication, PKCE, static authorization
parameters, URL template parameters). No code, action or prose is copied.

Each entry's `auth` is a `ConnectorAuth::Oauth2` value (`kind: oauth2`, the
connection facet of the #1430 system contract); provider quirks travel in its
flattened fields.

Usage:
  python3 scripts/connections/extract_openconnector.py          # write
  python3 scripts/connections/extract_openconnector.py --check  # compare
  ... --from DIR   read the pinned files from DIR (same relative paths)
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
import urllib.request
from pathlib import Path

REPOSITORY = "https://github.com/oomol-lab/open-connector"
TAG = "v1.9.4"
COMMIT = "d89dc9f24f5a057bb500272d7dbf333837c77c68"
RAW = f"https://raw.githubusercontent.com/oomol-lab/open-connector/{COMMIT}/"

# Every input file at COMMIT and its SHA-256.
PINNED = {
    "LICENSE.txt": "c71d239df91726fc519c6eb72d318ec65820627232b2f796219e87dcf35d0ab4",
    "NOTICE.md": "d0890661bf25e7327808610f773b055cb87bf949ff6bba5943c7d62d94d1bf0f",
    "src/providers/github/definition.ts": "6cb34ae139caa89a7cbe6ecc1f46613bfda5ab39db03e1a0bd2a33ee93fafbf4",
    "src/providers/github/scopes.ts": "a977e244613e1ceb59e250c44806e190c91eb8727b96b68edd9d501e3a1993b4",
    "src/providers/gmail/definition.ts": "7b26f9cb3a90643908965be87130450bbb1b17b11f2ec250b335bd04f3862808",
    "src/providers/gmail/scopes.ts": "ada6b08c63d021780106cfd33d11a34f37b34c4aa4db75eb52130fbd4143bad1",
    "src/providers/shopify/definition.ts": "84328061f1d586ce47a50d8a83835c48cb50f140e9154a6fca4892f07c544b20",
    "src/providers/shopify/scopes.ts": "c4cb674b5c249367570dfb27b78741552d1a279e70243bef6d46fb30802b0ba4",
}

# (Norn provider id, system, OpenConnector service directory)
PROVIDERS = [
    ("github", "github", "github"),
    ("google.gmail", "google", "gmail"),
    ("shopify", "shopify", "shopify"),
]

OUTPUT = Path(__file__).resolve().parents[2] / "registry" / "connections" / "providers.json"


def fetch(path: str, source: Path | None) -> str:
    if source is not None:
        data = (source / path).read_bytes()
    else:
        with urllib.request.urlopen(RAW + path, timeout=30) as response:  # noqa: S310 (pinned https URL)
            data = response.read()
    digest = hashlib.sha256(data).hexdigest()
    if digest != PINNED[path]:
        raise SystemExit(f"{path}: sha256 {digest} is not the pinned {PINNED[path]}")
    return data.decode("utf-8")


def check_license(text: str) -> None:
    head = " ".join(text.split()[:6])
    if head != "Apache License Version 2.0, January 2004":
        raise SystemExit(f"LICENSE.txt at {COMMIT} is not the Apache License 2.0: {head!r}")


def constants(*sources: str) -> dict[str, object]:
    """String and string-array constants, references resolved."""
    raw: dict[str, str] = {}
    for source in sources:
        for match in re.finditer(
            r"(?:export\s+)?const\s+(\w+)(?:\s*:\s*[\w\[\]]+)?\s*=\s*(\"[^\"]*\"|\[[^\]]*\])\s*;",
            source,
        ):
            raw[match.group(1)] = match.group(2)

    def value(token: str, depth: int = 0) -> object:
        token = token.strip()
        if depth > 8:
            raise SystemExit(f"constant cycle at {token}")
        if token.startswith('"'):
            return json.loads(token)
        if token.startswith("["):
            out: list[str] = []
            for item in split_items(token[1:-1]):
                resolved = value(item, depth + 1)
                out.extend(resolved if isinstance(resolved, list) else [resolved])
            return out
        if token.startswith("..."):
            return value(token[3:], depth + 1)
        if token in raw:
            return value(raw[token], depth + 1)
        raise SystemExit(f"unresolved constant {token}")

    return {name: value(name) for name in raw}


def split_items(body: str) -> list[str]:
    body = re.sub(r"//[^\n]*", "", body)
    return [item.strip() for item in body.split(",") if item.strip()]


def oauth_block(definition: str) -> str:
    start = definition.index('type: "oauth2"')
    open_at = definition.rindex("{", 0, start)
    depth = 0
    for index in range(open_at, len(definition)):
        if definition[index] == "{":
            depth += 1
        elif definition[index] == "}":
            depth -= 1
            if depth == 0:
                return definition[open_at : index + 1]
    raise SystemExit("unterminated oauth2 block")


def field(block: str, name: str, consts: dict[str, object]) -> object | None:
    match = re.search(rf"\n\s*{name}\s*:\s*(\"[^\"]*\"|\[[^\]]*\]|\w+)\s*,", block)
    if match is None:
        return None
    token = match.group(1)
    if token.startswith('"'):
        return json.loads(token)
    if token.startswith("["):
        return [str(consts[item]) if item in consts else json.loads(item) for item in split_items(token[1:-1])]
    if token in consts:
        return consts[token]
    raise SystemExit(f"{name}: unresolved {token}")


def object_field(block: str, name: str) -> dict[str, str] | None:
    match = re.search(rf"\n\s*{name}\s*:\s*\{{([^}}]*)\}}", block)
    if match is None:
        return None
    return {
        key: value
        for key, value in re.findall(r"(\w+)\s*:\s*\"([^\"]*)\"", match.group(1))
    }


def authorization_options(block: str, consts: dict[str, object]) -> list[tuple[str, bool]]:
    match = re.search(r"authorizationOptions\s*:\s*\[", block)
    if match is None:
        return []
    out = []
    for option in re.finditer(r"\{([^{}]*)\}", block[match.end() :].split("\n      ],", 1)[0]):
        body = option.group(1)
        ident = re.search(r"\bid\s*:\s*(\w+|\"[^\"]*\")", body)
        selected = re.search(r"defaultSelected\s*:\s*(true|false)", body)
        if ident is None or selected is None:
            continue
        token = ident.group(1)
        scope = json.loads(token) if token.startswith('"') else consts[token]
        out.append((str(scope), selected.group(1) == "true"))
    return out


def extract(service: str, source: Path | None) -> dict[str, object]:
    definition = fetch(f"src/providers/{service}/definition.ts", source)
    scopes_ts = fetch(f"src/providers/{service}/scopes.ts", source)
    consts = constants(scopes_ts, definition)
    block = oauth_block(definition)
    declared = list(field(block, "scopes", consts) or [])
    optional = list(field(block, "optionalScopes", consts) or [])
    options = authorization_options(block, consts)
    if options:
        # Least privilege: the defaults are the options selected by default.
        scopes = [scope for scope, selected in options if selected]
        optional = [scope for scope in declared if scope not in scopes] + [
            scope for scope in optional if scope not in declared
        ]
    else:
        scopes = declared
    authorization_url = str(field(block, "authorizationUrl", consts))
    token_url = str(field(block, "tokenUrl", consts))
    auth: dict[str, object] = {
        "kind": "oauth2",
        "authId": f"{service}.oauth2",
        "authorizationUrl": authorization_url,
        "tokenUrl": token_url,
        "scopes": scopes,
        "optionalScopes": optional,
        "scopeSeparator": field(block, "scopeSeparator", consts) or " ",
        "tokenEndpointAuthMethod": field(block, "tokenEndpointAuthMethod", consts),
        "pkce": "S256",
        "pkceRequired": re.search(r"\n\s*pkce\s*:", block) is not None,
    }
    for name in ("refreshTokenUrl", "revocationUrl"):
        value = field(block, name, consts)
        if value is not None:
            auth[name] = value
    params = object_field(block, "authorizationParams")
    if params:
        auth["authorizationParams"] = params
    template = sorted(set(re.findall(r"\{(\w+)\}", authorization_url + token_url)))
    if template:
        auth["urlParameters"] = template
    return auth


def build(source: Path | None) -> dict[str, object]:
    check_license(fetch("LICENSE.txt", source))
    notice = fetch("NOTICE.md", source)
    providers = []
    for norn_id, system, service in PROVIDERS:
        providers.append(
            {
                "id": norn_id,
                "system": system,
                "upstream": f"src/providers/{service}/definition.ts",
                "auth": extract(service, source),
            }
        )
    return {
        "format": "norn.connection-providers/1",
        "generatedBy": "scripts/connections/extract_openconnector.py",
        "source": {
            "repository": REPOSITORY,
            "tag": TAG,
            "commit": COMMIT,
            "license": "Apache-2.0",
            "notice": notice.strip(),
        },
        "providers": providers,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n", 1)[0])
    parser.add_argument("--check", action="store_true", help="fail if the output is stale")
    parser.add_argument("--from", dest="source", type=Path, help="read pinned files from a directory")
    args = parser.parse_args()
    text = json.dumps(build(args.source), indent=2, ensure_ascii=False) + "\n"
    if args.check:
        if not OUTPUT.exists() or OUTPUT.read_text(encoding="utf-8") != text:
            print(f"{OUTPUT} is stale; run {Path(__file__).name}", file=sys.stderr)
            return 1
        return 0
    OUTPUT.parent.mkdir(parents=True, exist_ok=True)
    OUTPUT.write_text(text, encoding="utf-8")
    return 0


if __name__ == "__main__":
    sys.exit(main())
