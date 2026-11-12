#!/usr/bin/env python3
"""Canaries for the base-owned Python isolation policy (scripts/ci/python_isolation.py, #513)."""

from __future__ import annotations

import importlib.util
import os
import py_compile
import shutil
import subprocess
import sys
import tempfile
import types
import unittest
from pathlib import Path


def _sibling(name, directory=None):
    """Load sibling module `name` from its source by explicit path: never through sys.path,
    never from a bytecode cache, never a same-named module loaded some other way (#513)."""
    import pathlib
    import sys
    import types

    path = (directory or pathlib.Path(__file__).resolve().parent) / f"{name}.py"
    loaded = sys.modules.get(name)
    if getattr(loaded, "__norn_sibling__", None) == str(path):
        return loaded
    module = types.ModuleType(name)
    module.__file__ = str(path)
    module.__norn_sibling__ = str(path)
    sys.modules[name] = module
    try:
        exec(compile(path.read_bytes(), str(path), "exec", dont_inherit=True), module.__dict__)
    except BaseException:
        del sys.modules[name]
        raise
    return module


isolation = _sibling("python_isolation")
REPO = isolation.REPO
GATE_SOURCE = (REPO / isolation.GATE_SOURCE).read_text(encoding="utf-8")
PLANT = "import sys\nprint('PLANTED')\nsys.exit(0)\n"
# Interpreters without the isolation the gates set up: the control runs of each canary.
OPEN = {"PYTHONSAFEPATH": "", "PYTHONDONTWRITEBYTECODE": "", "PYTHONPYCACHEPREFIX": ""}


def git_tree(files: dict[str, str]) -> tempfile.TemporaryDirectory:
    directory = tempfile.TemporaryDirectory()
    root = Path(directory.name)
    for path, text in files.items():
        (root / path).parent.mkdir(parents=True, exist_ok=True)
        (root / path).write_text(text, encoding="utf-8")
    for args in (["init", "-q"], ["add", "-f", "."]):
        subprocess.run(["git", "-C", str(root), *args], check=True)
    return directory


def default_cache(source: Path) -> Path:
    """Where Python looks for `source`'s bytecode with no pycache prefix: `__pycache__` beside it."""
    return source.parent / "__pycache__" / f"{source.stem}.{sys.implementation.cache_tag}.pyc"


def run_python(args: list[str], env: dict[str, str] | None = None, cwd: Path | None = None) -> str:
    done = subprocess.run([sys.executable, *args], capture_output=True, text=True, cwd=cwd,
                          env={**os.environ, **OPEN, **(env or {})})
    return done.stdout + done.stderr


class RealTree(unittest.TestCase):
    def test_the_repository_passes(self) -> None:
        self.assertEqual(isolation.check(REPO, GATE_SOURCE), [])


class Workflows(unittest.TestCase):
    def test_every_workflow_python_needs_isolation(self) -> None:
        for line in ("run: python3 scripts/x.py", "  python scripts/x.py --flag",
                     'run: "$RUNNER_TEMP/venv/bin/python" -m pip install x', "run: python3.12 -P x.py",
                     "  cat x | python3", "  python3"):
            self.assertTrue(isolation.workflow_errors("w.yml", line), line)
        for line in ("run: python3 -I scripts/x.py", "language: [rust, python, actions]", "# python3 x.py"):
            self.assertEqual(isolation.workflow_errors("w.yml", line), [], line)

    def test_the_trusted_runner_invocation_is_pinned(self) -> None:
        for path in isolation.TRUSTED_WORKFLOWS:
            text = (REPO / path).read_text(encoding="utf-8")
            self.assertEqual(isolation.workflow_errors(path, text), [], path)
            for label, before, after in (("no -B", " -B ", " "), ("no prefix", ' -X pycache_prefix="$(mktemp -d)"', ""),
                                         ("shared prefix", '"$(mktemp -d)"', "/tmp/cache")):
                mutated = text.replace(isolation.TRUSTED_WORKFLOW_RUN,
                                       isolation.TRUSTED_WORKFLOW_RUN.replace(before, after))
                self.assertTrue(any("trusted runner" in e for e in isolation.workflow_errors(path, mutated)), label)
            twice = text + f"      - run: {isolation.TRUSTED_WORKFLOW_RUN}\n"
            self.assertTrue(any("exactly once" in e for e in isolation.workflow_errors(path, twice)))


class Runner(unittest.TestCase):
    def test_child_pythons_start_from_the_pinned_list(self) -> None:
        text = (REPO / isolation.RUNNER).read_text(encoding="utf-8")
        self.assertEqual(isolation.runner_errors(text), [])
        for label, mutated in (
                ("no -B", text.replace('"-I", "-B", ', '"-I", ')),
                ("no prefix", text.replace(', "-X", f"pycache_prefix={tempfile.mkdtemp(prefix=\'norn-pycache-\')}"', "")),
                ("a second interpreter", text + "\nOTHER = [sys.executable, 'x.py']\n"),
                ("a literal interpreter", text + "\nOTHER = ['python3', 'x.py']\n")):
            self.assertTrue(isolation.runner_errors(mutated), label)


class ShellGates(unittest.TestCase):
    def errors(self, path: str, text: str) -> list[str]:
        files, directories = isolation.gates(GATE_SOURCE)
        present = set(isolation.tracked(REPO))
        return isolation.shell_errors(REPO, path, text, files, directories, present)

    def test_header_and_environment(self) -> None:
        for path in isolation.SHELL_GATES:
            text = (REPO / path).read_text(encoding="utf-8")
            self.assertEqual(self.errors(path, text), [], path)
            for line in isolation.HEADER:
                removed = text.replace(line + "\n", "", 1)
                self.assertTrue(any("isolation header" in e for e in self.errors(path, removed)), line)
            moved = text.replace(isolation.HEADER[0] + "\n", "true\n" + isolation.HEADER[0] + "\n", 1)
            self.assertTrue(any("isolation header" in e for e in self.errors(path, moved)))
            self.assertTrue(any(":1: must be" in e for e in self.errors(path, "set -e\n" + text)))
            for line in ("env -i PATH=$PATH python3 x.py", "env -u PYTHONSAFEPATH python3 x.py",
                         "env -uPYTHONSAFEPATH python3 x.py", "env - python3 x.py", "sudo python3 x.py",
                         "doas python3 x.py", "nix-shell --pure --run 'python3 x.py'", "exec -c python3 x.py",
                         "unset PYTHONSAFEPATH", "PYTHONSAFEPATH= python3 x.py", "export -n PYTHONSAFEPATH",
                         "declare +x PYTHONDONTWRITEBYTECODE", "typeset +x PYTHONSAFEPATH",
                         "local PYTHONPYCACHEPREFIX", "PYTHONPYCACHEPREFIX=/tmp/shared python3 x.py",
                         "python3 -E x.py", '"$upstream/venv/bin/python" -Es x.py'):
                self.assertTrue(self.errors(path, text + line + "\n"), line)
            for body in ("import sys\nsys.path.insert(0, 'scripts')\n", "import coverage_manifest\n"):
                for heredoc in (f"python3 - <<'PY'\n{body}PY\n", f"python3 - \\\n  a \\\n  b <<'PY'\n{body}PY\n",
                                f"python3 - <<PY\n{body}PY\n", f'python3 - <<"EOF"\n{body}EOF\n'):
                    self.assertTrue(self.errors(path, text + heredoc), heredoc)
            bodies = isolation.HEREDOC.findall(text)
            self.assertTrue(bodies, path)
            self.assertTrue(self.errors(path, text + "python3 -c 'import sys; sys.path.append(\"x\")'\n"))

    def test_the_assertion_refuses_without_isolation(self) -> None:
        assertion = isolation.HEADER[-1]
        full = {"PYTHONSAFEPATH": "1", "PYTHONDONTWRITEBYTECODE": "1", "PYTHONPYCACHEPREFIX": tempfile.gettempdir()}
        for drop in (None, *full):
            env = {**os.environ, **{k: v for k, v in full.items() if k != drop}, **({drop: ""} if drop else {})}
            done = subprocess.run(["bash", "-c", assertion], capture_output=True, env=env)
            self.assertEqual(done.returncode, 0 if drop is None else 1, drop)


class Shadows(unittest.TestCase):
    """The refusal is repo-wide, so it needs no directory list."""

    def test_standard_library_third_party_and_bytecode(self) -> None:
        for planted in ("scripts/ci/argparse.py", "scripts/json/__init__.py", "verification/coverage-v1/types.pyc",
                        "subprocess.py", "scripts/yaml.py", "scripts/jsonschema/__init__.py",
                        "scripts/__pycache__/coverage_manifest.cpython-312.pyc", "scripts/coverage_manifest.pyc",
                        "a/__pycache__/README", "scripts/x.pyo"):
            self.assertTrue(isolation.shadow_errors([planted]), planted)
        self.assertEqual(isolation.shadow_errors(["scripts/ci/argparse_helpers.py", "docs/json/notes.md",
                                                  ".github/workflows/x.yaml"]), [])


class GatedPython(unittest.TestCase):
    SOURCE = ('const GATE_DIRECTORIES: [&str; 1] = ["crates/g/"];\n'
              'const GATE_FILES: [&str; 2] = ["scripts/g.py", "scripts/gated2.py"];\n')

    def errors(self, body: str) -> list[str]:
        with git_tree({"scripts/g.py": body, "scripts/gated2.py": "X = 1\n", "scripts/ungated.py": "X = 1\n"}) as root:
            return [e for e in isolation.check(Path(root), self.SOURCE) if e.startswith("scripts/g.py")]

    def test_loads(self) -> None:
        helper = isolation.SIBLING_HELPER
        self.assertEqual(self.errors(f'import json\nimport yaml\n{helper}\nG = _sibling("gated2")\n'
                                     'from pathlib import Path\nROOT = Path(__file__).resolve().parents[1]\n'
                                     'P = ROOT / "scripts/gated2.py"\n'), [])
        for body, wanted in (
                ("import sys\nsys.path.insert(0, 'x')\n", "must not use `sys.path`"),
                ("from sys import path\n", "must not use `sys.path`"),
                ("import ungated\n", "neither standard library"),
                ("from . import gated2\n", "relative import"),
                (f'{helper}\nU = _sibling("ungated")\n', "scripts/ungated.py, which is not a gate file"),
                (helper.replace("dont_inherit=True", "dont_inherit=False") + '\nG = _sibling("gated2")\n',
                 "differs from the pinned loader"),
                ('from pathlib import Path\nHERE = Path(__file__).resolve().parent\nU = HERE / "ungated.py"\n',
                 "names scripts/ungated.py"),
                ('import importlib.util\nS = importlib.util.spec_from_file_location("u", "scripts/ungated.py")\n',
                 "loads scripts/ungated.py"),
                ('import runpy\nfrom pathlib import Path\nROOT = Path(__file__).resolve().parents[1]\n'
                 'runpy.run_path(str(ROOT / "scripts/ungated.py"))\n', "loads scripts/ungated.py"),
                ('import runpy\nrunpy.run_path(input())\n', "cannot be checked"),
                (f'{helper}\nU = _sibling("gated2", somewhere())\n', "cannot be checked")):
            self.assertTrue(any(wanted in e for e in self.errors(body)), (body, wanted, self.errors(body)))


class SiblingLoader(unittest.TestCase):
    def test_compiles_from_source_and_never_reuses_by_name(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            folder = Path(directory)
            (folder / "norn_sibling_probe.py").write_text("VALUE = 'source'\n", encoding="utf-8")
            cache = default_cache(folder / "norn_sibling_probe.py")
            cache.parent.mkdir()
            planted = folder / "planted.py"
            planted.write_text("VALUE = 'bytecode'\n", encoding="utf-8")
            py_compile.compile(str(planted), cfile=str(cache),
                               invalidation_mode=py_compile.PycInvalidationMode.UNCHECKED_HASH)
            # The pinned loader text itself, run on a directory of its own.
            pinned: dict = {}
            exec(isolation.SIBLING_HELPER, pinned)
            sys.modules["norn_sibling_probe"] = types.ModuleType("norn_sibling_probe")
            try:
                module = pinned["_sibling"]("norn_sibling_probe", folder)
                self.assertEqual(module.VALUE, "source")
                self.assertIs(pinned["_sibling"]("norn_sibling_probe", folder), module)
            finally:
                sys.modules.pop("norn_sibling_probe", None)


class PlantedModules(unittest.TestCase):
    """A planted module or bytecode beside a gated entrypoint changes nothing."""

    def test_trusted_runner_ignores_a_planted_argparse(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            runner = Path(directory) / "scripts/ci/validate-runner-policy.py"
            runner.parent.mkdir(parents=True)
            shutil.copy(REPO / "scripts/ci/validate-runner-policy.py", runner)
            (runner.parent / "argparse.py").write_text(PLANT, encoding="utf-8")
            self.assertIn("PLANTED", run_python([str(runner), "--help"]), "the canary must bite without -I")
            isolated = run_python(["-I", "-B", "-X", f"pycache_prefix={directory}/cache", str(runner), "--help"])
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
            self.assertIn("PLANTED", run_python([entry, "--help"]))
            safe = run_python([entry, "--help"], {"PYTHONSAFEPATH": "1"})
            self.assertNotIn("PLANTED", safe)
            self.assertIn("usage", safe)

    def test_a_planted_unchecked_pyc_is_never_run(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            scripts = Path(directory) / "scripts"
            scripts.mkdir()
            for name in ("proof_enrollment.py", "coverage_manifest.py"):
                shutil.copy(REPO / "scripts" / name, scripts / name)
            source = scripts / "coverage_manifest.py"
            cache = default_cache(source)
            cache.parent.mkdir()
            evil = Path(directory) / "evil.py"
            evil.write_text(PLANT, encoding="utf-8")
            py_compile.compile(str(evil), cfile=str(cache),
                               invalidation_mode=py_compile.PycInvalidationMode.UNCHECKED_HASH)
            # The old load (script directory on sys.path, plain import) runs the planted bytecode.
            self.assertIn("PLANTED", run_python(["-c", "import coverage_manifest"], cwd=scripts))
            entry = str(scripts / "proof_enrollment.py")
            for flags in ([], ["-I", "-B", "-X", f"pycache_prefix={directory}/cache"]):
                result = run_python([*flags, entry, "--help"])
                self.assertNotIn("PLANTED", result, flags)
                self.assertIn("usage", result, flags)


if __name__ == "__main__":
    unittest.main()
