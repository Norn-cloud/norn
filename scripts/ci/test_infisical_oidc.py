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


def run(secrets: list, names=("A_KEY", "B_KEY"), identity="id", flags=()) -> tuple[int, str]:
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
        result = subprocess.run(["bash", str(HELPER), *flags, "prod", "/x", *names], env=env, timeout=30,
                                capture_output=True, text=True)
        global LAST_STDOUT
        LAST_STDOUT = result.stdout
        return result.returncode, output.read_text()


LAST_STDOUT = ""


def entry(key, value):
    return {"secretKey": key, "secretValue": value}


code, written = run([entry("A_KEY", "alpha"), entry("B_KEY", "beta")])
assert code == 0 and written == "a_key=alpha\nb_key=beta\n", (code, written)
for name, secrets, kwargs in (
    ("missing", [entry("A_KEY", "alpha")], {}),
    ("empty", [entry("A_KEY", "alpha"), entry("B_KEY", "")], {}),
    ("line feed", [entry("A_KEY", "alpha"), entry("B_KEY", "beta\nc_key=injected")], {}),
    ("trailing line feed", [entry("A_KEY", "alpha"), entry("B_KEY", "beta\n")], {}),
    ("NUL", [entry("A_KEY", "alpha"), entry("B_KEY", "be\u0000ta")], {}),
    ("carriage return", [entry("A_KEY", "alpha"), entry("B_KEY", "beta\rx")], {}),
    ("duplicate", [entry("A_KEY", "alpha"), entry("B_KEY", "beta"), entry("B_KEY", "other")], {}),
    ("not a string", [entry("A_KEY", "alpha"), entry("B_KEY", 7)], {}),
    ("multi-line identity", [entry("A_KEY", "alpha"), entry("B_KEY", "beta")], {"identity": "id\nx"}),
    ("bad name", [entry("A_KEY", "alpha")], {"names": ("A_KEY", "b=c")}),
):
    code, written = run(secrets, **kwargs)
    assert code != 0 and written == "", f"{name}: exit {code}, wrote {written!r}"
# The runner percent-decodes command data, so a mask carries `%` escaped.
code, written = run([entry("A_KEY", "a%0Ab"), entry("B_KEY", "beta")])
assert code == 0 and "::add-mask::a%250Ab" in LAST_STDOUT.splitlines(), LAST_STDOUT
# --optional: a failure warns and exits 0 with nothing written.
code, written = run([entry("A_KEY", "alpha")], flags=("--optional",))
assert code == 0 and written == "" and "::warning::" in LAST_STDOUT, (code, written, LAST_STDOUT)
code, written = run([entry("A_KEY", "alpha"), entry("B_KEY", "beta")], identity="", flags=("--optional",))
assert code == 0 and written == "", (code, written)
print("infisical-oidc tests: ok")
