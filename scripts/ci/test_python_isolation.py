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
        for line in ("run: python3 -I -X pycache_prefix= scripts/x.py", 'run: python3 -I -X pycache_prefix="" x.py'):
            self.assertTrue(any("pycache_prefix" in e for e in isolation.workflow_errors("w.yml", line)), line)

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
                ("a literal interpreter", text + "\nOTHER = ['python3', 'x.py']\n"),
                ("no child assertion", text.replace('[*PYTHON, "-c", ISOLATED]', '[*PYTHON, "-c", "pass"]')),
                ("a weaker assertion", text.replace("sys.flags.isolated and ", "")),
                ("the result ignored", text.replace('    if subprocess.run([*PYTHON, "-c", ISOLATED], cwd=REPO, env=env).returncode != 0:\n'
                                                    '        print("child Python is not isolated (-I, -B, pycache_prefix)", file=sys.stderr)\n'
                                                    '        return 1\n',
                                                    '    subprocess.run([*PYTHON, "-c", ISOLATED], cwd=REPO, env=env)\n')),
                ("os.system first", text.replace('    env = {**os.environ, "RUSTUP_TOOLCHAIN": TOOLCHAIN}\n',
                                                 '    env = {**os.environ, "RUSTUP_TOOLCHAIN": TOOLCHAIN}\n    os.system("true")\n')),
                ("check_call first", text.replace('    env = {**os.environ, "RUSTUP_TOOLCHAIN": TOOLCHAIN}\n',
                                                  '    env = {**os.environ, "RUSTUP_TOOLCHAIN": TOOLCHAIN}\n    subprocess.check_call(["true"])\n')),
                ("not first", text.replace('    env = {**os.environ, "RUSTUP_TOOLCHAIN": TOOLCHAIN}\n',
                                           '    env = {**os.environ, "RUSTUP_TOOLCHAIN": TOOLCHAIN}\n    subprocess.run(["true"])\n'))):
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
                         "python3 -E x.py", '"$upstream/venv/bin/python" -Es x.py',
                         "unset ${!PYTHON@}", 'export -n "$name"', "unset `compgen -v PYTHON`",
                         "python3 -X pycache_prefix= x.py", 'python3 -X pycache_prefix="" x.py',
                         "python3 -X 'pycache_prefix=' x.py", 'python3 -X "pycache_prefix=" x.py',
                         'python3 - <<< "$code"', 'python3 - 0<<<"$code"', 'python3 -c "import coverage_manifest"',
                         "python3 -I x.py", "python3 -IB x.py", '"$v/bin/python" -I -m pip', "python3 -B -I x.py",
                         "python3 -X dev -I x.py", "python3 -W error -I x.py", "python3 -B -E x.py",
                         'python3 "-I" x.py', "python3 '-E' x.py", "python3 \"-X\" 'pycache_prefix=' x.py",
                         'python3 -X pycache_prefix=/tmp/x x.py', '<<<"$code" python3 -', 'python3 - 00<<<"$code"'):
                self.assertTrue(self.errors(path, text + line + "\n"), line)
            for body in ("import sys\nsys.path.insert(0, 'scripts')\n", "import coverage_manifest\n"):
                tabbed = body.replace("\n", "\n\t")
                for heredoc in (f"python3 - <<'PY'\n{body}PY\n", f"python3 - \\\n  a \\\n  b <<'PY'\n{body}PY\n",
                                f"python3 - <<PY\n{body}PY\n", f'python3 - <<"EOF"\n{body}EOF\n',
                                f"python3 - <<-'PY'\n\t{tabbed}PY\n", f"cat <<'PY' | python3 -\n{body}PY\n",
                                f"python3 - <<'PY'\n{body}PY"):
                    self.assertTrue(self.errors(path, text + heredoc), heredoc)
            self.assertTrue(any("never terminates" in e for e in self.errors(path, text + "python3 - <<'PY'\nimport json\n")))
            # `readonly` alone (no assignment) is refused by its own pattern.
            self.assertTrue(any("shadowing" in e for e in self.errors(path, text + "readonly PYTHONSAFEPATH\n")))
            # Every heredoc form the line-based reader's predecessor read is still read.
            for body in ("import coverage_manifest\n",):
                for heredoc in (f"python3 - 0<<'PY'\n{body}PY\n", f'"${{PYTHON:-python3}}" - <<\'PY\'\n{body}PY\n',
                                f"PY=python3; $PY - <<'PY'\n{body}PY\n"):
                    self.assertTrue(any("neither standard library" in e for e in self.errors(path, text + heredoc)), heredoc)
            # A heredoc fed to anything else is not read as Python, and its body is skipped.
            self.assertEqual(self.errors(path, text + "cat <<'TXT'\nimport coverage_manifest\nTXT\n"), [])
            self.assertTrue(self.errors(path, text + "python3 -c 'import sys; sys.path.append(\"x\")'\n"))

    def test_the_assertion_refuses_without_isolation(self) -> None:
        assertion = isolation.HEADER[-1]
        full = {"PYTHONSAFEPATH": "1", "PYTHONDONTWRITEBYTECODE": "1", "PYTHONPYCACHEPREFIX": tempfile.gettempdir()}
        for drop in (None, *full):
            env = {**os.environ, **{k: v for k, v in full.items() if k != drop}, **({drop: ""} if drop else {})}
            done = subprocess.run(["bash", "-c", assertion], capture_output=True, env=env)
            self.assertEqual(done.returncode, 0 if drop is None else 1, drop)


class Flags(unittest.TestCase):
    def test_ordinary_invocations_pass(self) -> None:
        for line in ("python3 -m venv x", 'python3 - "$@" <<\'PY\'', "python3 -c 'import sys'",
                     'python3 - a b 3<<<"$data" <<\'PY\'', '"$v/bin/python" -m pip install x==1', "python3 -B -W error x.py"):
            self.assertEqual(isolation.python_flag_errors(line), [], line)


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

    def errors(self, body: str, ungated: str = "X = 1\n") -> list[str]:
        with git_tree({"scripts/g.py": body, "scripts/gated2.py": "X = 1\n", "scripts/ungated.py": ungated}) as root:
            return [e for e in isolation.check(Path(root), self.SOURCE) if e.startswith(("scripts/g.py", "scripts/ungated.py"))]

    def test_every_copy_of_the_loader_is_pinned_even_ungated(self) -> None:
        helper = isolation.SIBLING_HELPER
        drifted = helper.replace("dont_inherit=True", "dont_inherit=False")
        self.assertTrue(any(e.startswith("scripts/ungated.py") and "differs" in e
                            for e in self.errors("X = 1\n", ungated=drifted)))
        self.assertTrue(any(e.startswith("scripts/ungated.py") and "pinned `def`" in e
                            for e in self.errors("X = 1\n", ungated="_sibling = lambda name: None\n")))

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
                ('import runpy\nrunpy.run_path(input())\n', "cannot resolve"),
                (f'{helper}\nU = _sibling("gated2", somewhere())\n', "cannot resolve"),
                ('import runpy\nfrom pathlib import Path\ndef f():\n    p = Path(__file__).resolve().parent / "ungated.py"\n'
                 '    runpy.run_path(str(p))\n', "loads scripts/ungated.py"),
                ('import runpy\ndef g(x):\n    runpy.run_path(x)\n', "cannot resolve"),
                ('import runpy\nfrom pathlib import Path\nP = Path(__file__).parent / "gated2.py"\nP = Path("x.py")\n'
                 'runpy.run_path(P)\n', "cannot resolve"),
                (f'{helper}\nM = _sibling("missing")\n', "not a tracked file"),
                ("exec(open('scripts/gated2.py').read())\n", "of a file read is refused"),
                ("from pathlib import Path\nexec(compile(Path('scripts/gated2.py').read_text(), 'x', 'exec'))\n",
                 "of a file read is refused"),
                ("from importlib.machinery import SourceFileLoader\n", "dynamic import machinery"),
                ("import importlib.machinery\nL = importlib.machinery.SourcelessFileLoader('x', 'y')\n", "dynamic import machinery"),
                ("import importlib\nM = importlib.import_module('ungated')\n", "dynamic import machinery"),
                ("M = __import__('ungated')\n", "dynamic import machinery"),
                ("_sibling = lambda name: None\n", "only be bound by the pinned"),
                ("from runpy import run_path as rp\n", "under another name"),
                ("import runpy\nr = runpy.run_path\n", "may only be called directly"),
                ("from importlib.util import spec_from_file_location as s\n", "under another name"),
                ("import runpy\nlist(map(runpy.run_path, ['x']))\n", "may only be called directly"),
                ("import importlib\nf = getattr(importlib, 'import_' + 'module')\n", "`getattr` on `importlib`"),
                ("import runpy\nf = getattr(runpy, 'run_path')\n", "`getattr` on `runpy`"),
                ("import pkgutil\npkgutil.get_importer('x')\n", "dynamic import machinery"),
                ("from importlib.machinery import PathFinder\nPathFinder.find_spec('x')\n", "dynamic import machinery"),
                ("code = 'x = 1'\nexec(code)\n", "of anything but a string literal"),
                ("eval(input())\n", "of anything but a string literal"),
                ("globals()['x'] = 1\n", "writing through `globals()`"),
                ("e = exec\ne('x = 1')\n", "may only be called directly"),
                ("import builtins\nbuiltins.exec('x = 1')\n", "`builtins` is refused"),
                ("getattr(builtins, 'exec')('x = 1')\n", "`builtins` is refused"),
                ("__builtins__['exec']('x = 1')\n", "`builtins` is refused"),
                ("import runpy\n@runpy.run_path\ndef f():\n    pass\n", "may only be called directly"),
                ("locals().update(x=1)\n", "writing through `locals()`"),
                ("from json import loads as _sibling\n", "only be bound by the pinned")):
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
            # The pinned loader text itself, run in a fresh interpreter on a directory of its own,
            # with a same-named module already registered.
            probe = isolation.SIBLING_HELPER + (
                "\nimport sys, types\n"
                "sys.modules['norn_sibling_probe'] = types.ModuleType('norn_sibling_probe')\n"
                "first = _sibling('norn_sibling_probe', pathlib.Path(sys.argv[1]))\n"
                "print(first.VALUE, _sibling('norn_sibling_probe', pathlib.Path(sys.argv[1])) is first)\n")
            result = run_python(["-c", "import pathlib\n" + probe, str(folder)])
            self.assertEqual(result.strip(), "source True", result)


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
