"""Negative tests for scripts/ci/secret-scan.sh: each way of hiding a committed secret must fail.

The fixture is a GitHub-token-shaped string assembled at run time, so this file holds no literal the scan would flag.
"""
import os
import pathlib
import subprocess
import tempfile

SCAN = pathlib.Path(__file__).resolve().parent / "secret-scan.sh"
FIXTURE = "ghp_" + "R8x2Qm7Lp4Vz9Kc1" + "Wt6Yb3Nd5Hf0Js8Ge2Ua"
assert len(FIXTURE) == 40


def repo(root: pathlib.Path, files: dict[str, str]) -> pathlib.Path:
    path = root / "repo"
    path.mkdir()
    git = ["git", "-C", str(path), "-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"]
    subprocess.run(git[:3] + ["init", "-q"], check=True)
    for name, text in files.items():
        (path / name).parent.mkdir(parents=True, exist_ok=True)
        (path / name).write_text(text)
    subprocess.run(git + ["add", "-A"], check=True)
    subprocess.run(git + ["commit", "-qm", "fixture"], check=True)
    return path


def scan(path: pathlib.Path, env: dict[str, str] | None = None) -> int:
    return subprocess.run([str(SCAN), str(path)], env={**os.environ, **(env or {})},
                          stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=300).returncode


def case(name: str, files: dict[str, str], want_fail: bool, env_for=None) -> None:
    with tempfile.TemporaryDirectory() as tmp:
        root = pathlib.Path(tmp)
        path = repo(root, files)
        code = scan(path, env_for(root) if env_for else None)
        assert (code != 0) == want_fail, f"{name}: exit {code}, expected {'failure' if want_fail else 'success'}"


def allow_all(root: pathlib.Path) -> dict[str, str]:
    config = root / "allow.toml"
    config.write_text('[extend]\nuseDefault = true\n[allowlist]\npaths = [".*"]\nregexes = [".*"]\n')
    return {"GITLEAKS_CONFIG": str(config), "GITLEAKS_CONFIG_TOML": config.read_text()}


def fake_gitleaks(root: pathlib.Path) -> dict[str, str]:
    bin_dir = root / "bin"
    bin_dir.mkdir()
    fake = bin_dir / "gitleaks"
    fake.write_text("#!/bin/sh\n[ \"$1\" = version ] && echo 8.28.0\nexit 0\n")
    fake.chmod(0o755)
    return {"PATH": f"{bin_dir}:{os.environ['PATH']}"}


leaky = f"value = \"{FIXTURE}\"\n"
case("clean tree", {"a.txt": "nothing here\n"}, want_fail=False)
case("plain secret", {"a.txt": leaky}, want_fail=True)
case("export-ignored secret", {".gitattributes": "hidden.txt export-ignore\n", "hidden.txt": leaky}, want_fail=True)
case("secret behind a GITLEAKS_CONFIG allowlist", {"a.txt": leaky}, want_fail=True, env_for=allow_all)
case("secret with a fake gitleaks on PATH", {"a.txt": leaky}, want_fail=True, env_for=fake_gitleaks)
case("tracked .gitleaksignore", {"a.txt": "x\n", ".gitleaksignore": "a.txt\n"}, want_fail=True)
case("inline allow comment", {"a.txt": f"value = \"{FIXTURE}\" # gitleaks:allow\n"}, want_fail=True)
print("secret-scan negative tests: ok")
