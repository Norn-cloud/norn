#!/usr/bin/env python3
"""Canaries for the base-owned Python isolation policy (scripts/ci/python_isolation.py, #513)."""

from __future__ import annotations

import os
import re
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


def _sibling(name, directory=None):
    """Load sibling module `name` by explicit path, never through sys.path."""
    import importlib.util
    import pathlib

    if name in sys.modules:
        return sys.modules[name]
    path = (directory or pathlib.Path(__file__).resolve().parent) / f"{name}.py"
    spec = importlib.util.spec_from_file_location(name, path)
    if spec is None or spec.loader is None:
        raise ImportError(f"cannot load {name} from {path}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


isolation = _sibling("python_isolation")
REPO = isolation.REPO
GATE_SOURCE = (REPO / isolation.GATE_SOURCE).read_text(encoding="utf-8")
PLANT = "import sys\nprint('PLANTED')\nsys.exit(0)\n"


def git_tree(files: dict[str, str]) -> tempfile.TemporaryDirectory:
    directory = tempfile.TemporaryDirectory()
    root = Path(directory.name)
    for path, text in files.items():
        (root / path).parent.mkdir(parents=True, exist_ok=True)
        (root / path).write_text(text, encoding="utf-8")
    for args in (["init", "-q"], ["add", "."]):
        subprocess.run(["git", "-C", str(root), *args], check=True)
    return directory


class RealTree(unittest.TestCase):
    def test_the_repository_passes(self) -> None:
        self.assertEqual(isolation.check(REPO, GATE_SOURCE), [])


class Workflows(unittest.TestCase):
    def test_every_workflow_python_needs_isolation(self) -> None:
        for line in ("run: python3 scripts/x.py", "  python scripts/x.py --flag",
                     'run: "$RUNNER_TEMP/venv/bin/python" -m pip install x', "run: python3.12 -P x.py"):
            self.assertTrue(isolation.workflow_errors("w.yml", line), line)
        for line in ("run: python3 -I scripts/x.py", "language: [rust, python, actions]", "# python3 x.py"):
            self.assertEqual(isolation.workflow_errors("w.yml", line), [], line)


class ShellGates(unittest.TestCase):
    def test_header_and_environment(self) -> None:
        for path in isolation.SHELL_GATES:
            text = (REPO / path).read_text(encoding="utf-8")
            self.assertEqual(isolation.shell_errors(path, text), [], path)
            for label, mutated in (
                    ("export removed", text.replace(isolation.SAFE_EXPORT + "\n", "", 1)),
                    ("assertion removed", text.replace(isolation.SAFE_ASSERT + "\n", "", 1)),
                    ("assertion after other code", text.replace(isolation.SAFE_EXPORT + "\n",
                                                                isolation.SAFE_EXPORT + "\ntrue\n", 1))):
                self.assertTrue(any("must start with" in e for e in isolation.shell_errors(path, mutated)), label)
            for line in ("env -i PATH=$PATH python3 x.py", "env -u PYTHONSAFEPATH python3 x.py",
                         "sudo python3 x.py", "nix-shell --pure --run 'python3 x.py'", "exec -c python3 x.py",
                         "unset PYTHONSAFEPATH", "PYTHONSAFEPATH= python3 x.py", "export -n PYTHONSAFEPATH",
                         'python3 -E x.py', '"$upstream/venv/bin/python" -Es x.py'):
                self.assertTrue(isolation.shell_errors(path, text + line + "\n")[-1:], line)


class StdlibShadows(unittest.TestCase):
    def directories(self) -> set[str]:
        """Every directory a gated script or workflow runs Python from, plus what gated Python loads."""
        found = {"scripts", "scripts/ci", "verification/coverage-v1", "verification/kernel-v2"}
        sources = [REPO / p for p in isolation.SHELL_GATES] + sorted((REPO / ".github/workflows").glob("*.y*ml"))
        for source in sources:
            for match in re.finditer(r"python3?\s+(?:-I\s+)?\"?(?:\$repo_root/|trusted/)?([\w./-]+)\.py",
                                     source.read_text(encoding="utf-8")):
                found.add(str(Path(match.group(1)).parent))
        return found

    def test_every_python_directory_refuses_a_stdlib_name(self) -> None:
        directories = self.directories()
        self.assertLessEqual({"scripts/metrics", "scripts/importers", "verification/ontology/scripts"}, directories)
        for directory in sorted(directories):
            for planted in (f"{directory}/argparse.py", f"{directory}/json/__init__.py", f"{directory}/types.pyc"):
                self.assertTrue(isolation.stdlib_shadows([planted]), planted)
        self.assertEqual(isolation.stdlib_shadows(["scripts/ci/argparse_helpers.py", "docs/json/notes.md"]), [])


class GatedPython(unittest.TestCase):
    SOURCE = ('const GATE_DIRECTORIES: [&str; 1] = ["crates/g/"];\n'
              'const GATE_FILES: [&str; 2] = ["scripts/g.py", "scripts/gated2.py"];\n')

    def errors(self, body: str) -> list[str]:
        with git_tree({"scripts/g.py": body, "scripts/gated2.py": "X = 1\n", "scripts/ungated.py": "X = 1\n",
                       "scripts/ci/core.sh": "#!/usr/bin/env bash\n" + isolation.SAFE_EXPORT + "\n"
                       + isolation.SAFE_ASSERT + "\n",
                       "scripts/verify-promoted-subsystems.sh": "#!/usr/bin/env bash\n" + isolation.SAFE_EXPORT
                       + "\n" + isolation.SAFE_ASSERT + "\n"}) as directory:
            return isolation.check(Path(directory), self.SOURCE)

    def test_loads(self) -> None:
        self.assertEqual(self.errors('import json\nimport yaml\nG = _sibling("gated2")\n'), [])
        for body, wanted in (
                ("import sys\nsys.path.insert(0, 'x')\n", "must not use `sys.path`"),
                ("from sys import path\n", "must not use `sys.path`"),
                ("import ungated\n", "neither standard library"),
                ("from . import gated2\n", "relative import"),
                ('U = _sibling("ungated")\n', "scripts/ungated.py, which is not a gate file"),
                ('U = ROOT / "scripts/ungated.py"\n', "scripts/ungated.py, which is not a gate file"),
                ('U = _sibling("gated2", somewhere())\n', "cannot be checked")):
            self.assertTrue(any(wanted in e for e in self.errors(body)), body)


class PlantedModules(unittest.TestCase):
    """A planted standard-library name beside a gated entrypoint changes nothing."""

    def run_python(self, args: list[str], env: dict[str, str] | None = None) -> str:
        done = subprocess.run([sys.executable, *args], capture_output=True, text=True,
                              env={**os.environ, "PYTHONSAFEPATH": "", **(env or {})})
        return done.stdout + done.stderr

    def test_trusted_runner_ignores_a_planted_argparse(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            runner = Path(directory) / "scripts/ci/validate-runner-policy.py"
            runner.parent.mkdir(parents=True)
            shutil.copy(REPO / "scripts/ci/validate-runner-policy.py", runner)
            (runner.parent / "argparse.py").write_text(PLANT, encoding="utf-8")
            self.assertIn("PLANTED", self.run_python([str(runner), "--help"]), "the canary must bite without -I")
            isolated = self.run_python(["-I", str(runner), "--help"])
            self.assertNotIn("PLANTED", isolated)
            self.assertIn("usage", isolated)

    def test_safe_path_ignores_a_planted_json(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            scripts = Path(directory) / "scripts"
            scripts.mkdir()
            for name in ("proof_enrollment.py", "coverage_manifest.py"):
                shutil.copy(REPO / "scripts" / name, scripts / name)
            (scripts / "json.py").write_text(PLANT, encoding="utf-8")
            entry = str(scripts / "proof_enrollment.py")
            self.assertIn("PLANTED", self.run_python([entry, "--help"], {"PYTHONSAFEPATH": ""}))
            safe = self.run_python([entry, "--help"], {"PYTHONSAFEPATH": "1"})
            self.assertNotIn("PLANTED", safe)
            self.assertIn("usage", safe)

    def test_the_shell_assertion_refuses_without_safe_path(self) -> None:
        for value, status in (("1", 0), ("", 1)):
            done = subprocess.run(["bash", "-c", isolation.SAFE_ASSERT], capture_output=True,
                                  env={**os.environ, "PYTHONSAFEPATH": value})
            self.assertEqual(done.returncode, status, value)


if __name__ == "__main__":
    unittest.main()
