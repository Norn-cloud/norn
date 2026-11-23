"""Audit the Actions secret inventory visible to the workflow's GITHUB_TOKEN.

The job grants Actions read for listing its repository's environments. The
workflow token cannot request repository Secrets, Environments, or organization
Secrets permissions. Access denials in those scopes are reported as UNVERIFIED.
The daily norn-ops credential audit uses an OIDC-backed GitHub App to verify the
full inventory across both repositories and both organizations.
"""

import json
import os
from urllib.error import HTTPError
from urllib.parse import quote
from urllib.request import Request, urlopen


def names(path, key, headers):
    seen = set()
    total = None
    for page in range(1, 51):
        try:
            request = Request(f"https://api.github.com{path}?per_page=100&page={page}", headers=headers)
            with urlopen(request, timeout=15) as response:
                body = json.load(response)
        except HTTPError as error:
            if page == 1:
                raise
            raise ValueError(f"page {page} returned HTTP {error.code}") from error

        count = body["total_count"]
        if type(count) is not int or count < 0 or (total is not None and count != total):
            raise ValueError("missing, invalid or changing total_count")
        total = count
        entries = body[key]
        if not isinstance(entries, list):
            raise ValueError(f"expected a {key} list")
        for entry in entries:
            name = entry["name"]
            if not isinstance(name, str) or not name or name in seen:
                raise ValueError("missing, invalid or repeated name")
            seen.add(name)
        if len(seen) == total:
            return sorted(seen)
        if len(seen) > total or not entries:
            break
    raise ValueError(f"incomplete inventory ({len(seen)} of {total})")


def main():
    owner, repo = os.environ["GITHUB_REPOSITORY"].split("/")
    base = f"/repos/{quote(owner, safe='')}/{quote(repo, safe='')}"
    headers = {
        "Accept": "application/vnd.github+json",
        "Authorization": f"Bearer {os.environ['GITHUB_TOKEN']}",
        "X-GitHub-Api-Version": "2022-11-28",
    }
    failed = False

    def check(path, key, *, required):
        nonlocal failed
        try:
            return names(path, key, headers)
        except HTTPError as error:
            reason = f"HTTP {error.code}"
            failed |= required or error.code not in (403, 404)
        except (OSError, ValueError, KeyError, TypeError) as error:
            reason = str(error)
            failed = True
        print(f"UNVERIFIED {path}: {reason}")
        return []

    scopes = [f"{base}/actions/secrets"]
    for org in ("Norn-cloud", "Soliman-Holdings"):
        scopes.append(f"/orgs/{org}/actions/secrets")
    for name in check(f"{base}/environments", "environments", required=True):
        scopes.append(f"{base}/environments/{quote(name, safe='')}/secrets")
    for path in scopes:
        for name in check(path, "secrets", required=False):
            print(f"FAIL {path} stores {name}")
            failed = True
    print("Full inventory is checked by the norn-ops daily credential audit.")
    return int(failed)


if __name__ == "__main__":
    raise SystemExit(main())
