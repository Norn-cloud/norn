"""Tests for scripts/ci/infisical-oidc.sh against a fake curl: it writes every output or none,
and an empty, multi-line, duplicate or missing value fails before anything is written."""
import json
import os
import pathlib
import subprocess
import tempfile

HELPER = pathlib.Path(__file__).resolve().parent / "infisical-oidc.sh"
FAKE_CURL = """#!/bin/sh
for arg; do last=$arg; done
case "$last" in
  *audience=*) echo '{"value":"jwt"}' ;;
  */oidc-auth/login) cat > /dev/null; echo '{"accessToken":"tok"}' ;;
  */secrets/raw) printf '%s' "$FAKE_SECRETS" ;;
  *) exit 22 ;;
esac
"""


def run(secrets: list, names=("A_KEY", "B_KEY"), identity="id") -> tuple[int, str]:
    with tempfile.TemporaryDirectory() as tmp:
        root = pathlib.Path(tmp)
        (root / "curl").write_text(FAKE_CURL)
        (root / "curl").chmod(0o755)
        output = root / "output"
        output.write_text("")
        env = {**os.environ, "PATH": f"{root}:{os.environ['PATH']}", "GITHUB_OUTPUT": str(output),
               "ACTIONS_ID_TOKEN_REQUEST_URL": "https://token.invalid/?x=1", "ACTIONS_ID_TOKEN_REQUEST_TOKEN": "t",
               "INFISICAL_IDENTITY_ID": identity, "INFISICAL_PROJECT_ID": "project",
               "FAKE_SECRETS": json.dumps({"secrets": secrets})}
        code = subprocess.run(["bash", str(HELPER), "prod", "/x", *names], env=env, timeout=30,
                              stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode
        return code, output.read_text()


def entry(key, value):
    return {"secretKey": key, "secretValue": value}


code, written = run([entry("A_KEY", "alpha"), entry("B_KEY", "beta")])
assert code == 0 and written == "a_key=alpha\nb_key=beta\n", (code, written)
for name, secrets, kwargs in (
    ("missing", [entry("A_KEY", "alpha")], {}),
    ("empty", [entry("A_KEY", "alpha"), entry("B_KEY", "")], {}),
    ("line feed", [entry("A_KEY", "alpha"), entry("B_KEY", "beta\nc_key=injected")], {}),
    ("carriage return", [entry("A_KEY", "alpha"), entry("B_KEY", "beta\rx")], {}),
    ("duplicate", [entry("A_KEY", "alpha"), entry("B_KEY", "beta"), entry("B_KEY", "other")], {}),
    ("not a string", [entry("A_KEY", "alpha"), entry("B_KEY", 7)], {}),
    ("multi-line identity", [entry("A_KEY", "alpha"), entry("B_KEY", "beta")], {"identity": "id\nx"}),
    ("bad name", [entry("A_KEY", "alpha")], {"names": ("A_KEY", "b=c")}),
):
    code, written = run(secrets, **kwargs)
    assert code != 0 and written == "", f"{name}: exit {code}, wrote {written!r}"
print("infisical-oidc tests: ok")
