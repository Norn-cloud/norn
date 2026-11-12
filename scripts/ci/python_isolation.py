#!/usr/bin/env python3
"""Base-owned Python isolation policy over a checkout read as data (#513).

Python puts the script's directory (or, for `-c` and `-`, the working directory) first on
`sys.path`, and reuses bytecode from `__pycache__` next to a source, even bytecode that is
never checked against that source. Either lets an ungated file replace gated code. The
structural guarantees, which this check pins:

- the trusted lanes start Python as `python3 -I -B -X pycache_prefix=<fresh directory>`
  (`TRUSTED_WORKFLOW_RUN`, and `PYTHON` in `validate-runner-policy.py`): no script directory
  or cwd on `sys.path`, no `PYTHON*` environment, no bytecode read or written next to any
  source, standard library included;
- each gated shell script starts with the `HEADER` exports (`PYTHONSAFEPATH`,
  `PYTHONDONTWRITEBYTECODE`, a fresh `PYTHONPYCACHEPREFIX`) and the assertion that they
  took effect;
- gated Python (files, and the heredoc and `-c` bodies of the gated shell scripts) never
  touches `sys.path`, imports only the standard library or `THIRD_PARTY`, loads siblings
  only through the pinned `_sibling` (compiled from source, never from bytecode or by name
  alone), and every file it loads by a path this check can resolve is a gate file;
- no tracked file is bytecode or sits under `__pycache__`, and none is importable under a
  standard-library or `THIRD_PARTY` name.

Best effort on top: `ENV_DROPS` lists shell constructs that would drop the header's
variables, and workflow lines are scanned for `python` without `-I`. Both are text
patterns, not a shell or YAML parser.
"""

from __future__ import annotations

import argparse
import ast
import re
import subprocess
import sys
from pathlib import Path, PurePosixPath

REPO = Path(__file__).resolve().parents[2]
GATE_SOURCE = "crates/norn-skeleton-guard/src/ci/mod.rs"
SHELL_GATES = ("scripts/ci/core.sh", "scripts/verify-promoted-subsystems.sh")
SHEBANG = "#!/usr/bin/env bash"
HEADER = (
    "export PYTHONSAFEPATH=1",
    "export PYTHONDONTWRITEBYTECODE=1",
    'export PYTHONPYCACHEPREFIX="$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/norn-pycache.XXXXXX")"',
    "python3 -c 'import sys; sys.exit(0 if sys.flags.safe_path and sys.dont_write_bytecode"
    " and sys.pycache_prefix else 1)' || { echo \"Python isolation not honoured\" >&2; exit 1; }",
)
HEADER_VARIABLES = ("PYTHONSAFEPATH", "PYTHONDONTWRITEBYTECODE", "PYTHONPYCACHEPREFIX")
TRUSTED_WORKFLOWS = (".github/workflows/ci-policy.yml", ".github/workflows/trusted-merge-group.yml")
TRUSTED_WORKFLOW_RUN = ('python3 -I -B -X pycache_prefix="$(mktemp -d)" trusted/scripts/ci/validate-runner-policy.py'
                        ' --root candidate --maintainer-evidence "$RUNNER_TEMP/maintainer-approval.json"')
RUNNER = "scripts/ci/validate-runner-policy.py"
RUNNER_PYTHON = ('PYTHON = [sys.executable, "-I", "-B", "-X", '
                 'f"pycache_prefix={tempfile.mkdtemp(prefix=\'norn-pycache-\')}"]')
# Third-party packages gated Python may import; the CI venv pins their versions (core.sh).
THIRD_PARTY = frozenset({"jsonschema", "yaml", "_yaml"})
IMPORTABLE_SUFFIXES = frozenset({"py", "pyc", "pyo", "pyw", "pyd", "so"})
# The one loader gated Python uses for siblings; every copy must be exactly this.
SIBLING_HELPER = '''
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
'''
# Wrappers whose path argument is followed at each call site instead of inside the wrapper.
PATH_WRAPPERS = {"load_module": 1}
_VARIABLES = "|".join(HEADER_VARIABLES)
ENV_DROPS = (
    (re.compile(r"\benv\s+(?:-\w*[iu]\w*|-(?=\s|$)|--ignore-environment\b|--unset\b)"), "`env -i`/`env -u`/`env -`"),
    (re.compile(r"\b(?:sudo|doas)\b"), "`sudo`/`doas`"),
    (re.compile(r"--pure\b"), "`--pure`"),
    (re.compile(r"\bexec\s+-\w*c"), "`exec -c`"),
    (re.compile(rf"\b(?:unset|export\s+-n|declare\s+[+-]\w*|typeset\s+[+-]\w*|local|readonly)\b[^#\n]*\b(?:{_VARIABLES})\b"),
     "unsetting, unexporting or shadowing an isolation variable"),
    (re.compile(rf"\b(?:{_VARIABLES})="), "reassigning an isolation variable"),
    (re.compile(r"(?:^|[\s;&|(\"'])(?:[\w./${}-]*/)?python3?(?:\.\d+)?\"?\s+-[A-Za-z]*E"), "`python -E`"),
)
WORKFLOW_PYTHON = re.compile(r"(?:^|[\s;&|(\"'`])((?:[\w./${}-]*/)?python3?(?:\.\d+)?)\"?(?=\s|$)[ \t]*(\S*)")
# A heredoc fed to python, the command possibly continued over several `\` lines.
HEREDOC = re.compile(r"python3?\b(?:[^\n]*\\\n)*[^\n]*<<-?\s*(['\"]?)(\w+)\1[^\n]*\n(.*?)\n\2\n", re.S)
DASH_C = re.compile(r"python3?\s+-c\s+'([^']*)'")


def tracked(root: Path) -> list[str]:
    out = subprocess.run(["git", "-C", str(root), "ls-files", "-z"], capture_output=True, check=True)
    return [path for path in out.stdout.decode().split("\0") if path]


def gates(gate_source: str) -> tuple[set[str], tuple[str, ...]]:
    """`GATE_FILES` and `GATE_DIRECTORIES` as the base's guard declares them."""
    def block(name: str) -> list[str]:
        return re.findall(r'"([^"]+)"', gate_source.split(f"const {name}:", 1)[1].split("];", 1)[0])
    return set(block("GATE_FILES")), tuple(block("GATE_DIRECTORIES"))


def is_gated(path: str, files: set[str], directories: tuple[str, ...]) -> bool:
    return path in files or path.startswith(directories) or path.startswith(".github/workflows/")


def shadow_errors(paths: list[str]) -> list[str]:
    names = set(sys.stdlib_module_names) | THIRD_PARTY
    errors = []
    for path in paths:
        pure = PurePosixPath(path)
        if "__pycache__" in pure.parts or pure.suffix in (".pyc", ".pyo"):
            errors.append(f"{path}: tracked bytecode is refused; Python could run it instead of a source")
        stem, _, suffix = pure.name.partition(".")
        if stem in names and suffix.rsplit(".", 1)[-1] in IMPORTABLE_SUFFIXES:
            errors.append(f"{path}: importable as module `{stem}`, a standard-library or pinned third-party name")
        if pure.name.startswith("__init__.") and pure.parent.name in names:
            errors.append(f"{path}: makes `{pure.parent}` a package named like module `{pure.parent.name}`")
    return errors


def workflow_errors(path: str, text: str) -> list[str]:
    errors = []
    lines = text.splitlines()
    for number, line in enumerate(lines, 1):
        if line.lstrip().startswith("#"):
            continue
        for match in WORKFLOW_PYTHON.finditer(line):
            if match.group(2) != "-I":
                errors.append(f"{path}:{number}: `{match.group(1)}` runs without `-I`")
    if path in TRUSTED_WORKFLOWS:
        runs = [line.strip() for line in lines if "validate-runner-policy.py" in line and not line.lstrip().startswith("#")]
        if runs != [f"run: {TRUSTED_WORKFLOW_RUN}"]:
            errors.append(f"{path}: the trusted runner must run exactly once, as `{TRUSTED_WORKFLOW_RUN}`")
    return errors


def runner_errors(text: str) -> list[str]:
    tree = ast.parse(text)
    uses = [node for node in ast.walk(tree) if isinstance(node, ast.Attribute) and node.attr == "executable"
            and isinstance(node.value, ast.Name) and node.value.id == "sys"]
    pinned = [node for node in tree.body if isinstance(node, ast.Assign) and ast.unparse(node) == ast.unparse(ast.parse(RUNNER_PYTHON).body[0])]
    errors = []
    if len(pinned) != 1 or len(uses) != 1:
        errors.append(f"{RUNNER}: every child Python must start from the one pinned `{RUNNER_PYTHON}`")
    for node in ast.walk(tree):
        if (isinstance(node, ast.List) and node.elts and isinstance(node.elts[0], ast.Constant)
                and re.fullmatch(r"(?:.*/)?python3?(?:\.\d+)?", str(node.elts[0].value))):
            errors.append(f"{RUNNER}:{node.lineno}: a child Python must start with `*PYTHON`")
    return errors


def shell_errors(root: Path, path: str, text: str, files: set[str], directories: tuple[str, ...],
                 present: set[str]) -> list[str]:
    lines = text.splitlines()
    errors = []
    if not lines or lines[0] != SHEBANG:
        errors.append(f"{path}:1: must be `{SHEBANG}`")
    statements = [line for line in lines[1:] if line.strip() and not line.lstrip().startswith("#")]
    if tuple(statements[:len(HEADER)]) != HEADER:
        errors.append(f"{path}: must start with the Python isolation header and its assertion")
    header_lines = set(HEADER)
    for number, line in enumerate(lines, 1):
        if line.lstrip().startswith("#") or line in header_lines:
            continue
        for pattern, label in ENV_DROPS:
            if pattern.search(line):
                errors.append(f"{path}:{number}: {label} would drop Python isolation")
    for match in HEREDOC.finditer(text):
        number = text.count("\n", 0, match.start(3)) + 1
        errors += python_errors(root, f"{path}:{number}", match.group(3), PurePosixPath("."),
                                files, directories, present)
    for match in DASH_C.finditer(text):
        number = text.count("\n", 0, match.start()) + 1
        errors += python_errors(root, f"{path}:{number}", match.group(1), PurePosixPath("."),
                                files, directories, present)
    return errors


class Paths:
    """Repository-relative paths of the expressions a Python source builds from literals and
    `__file__`; `None` where the value is computed."""

    def __init__(self, file: PurePosixPath | None, tree: ast.Module):
        self.file = file
        self.names = {target.id: node.value for node in tree.body if isinstance(node, ast.Assign)
                      for target in node.targets if isinstance(target, ast.Name)}
        self.seen: set[str] = set()

    @staticmethod
    def normal(path: PurePosixPath) -> PurePosixPath | None:
        parts: list[str] = []
        for part in path.parts:
            if part == "..":
                if not parts:
                    return None
                parts.pop()
            elif part != ".":
                parts.append(part)
        return PurePosixPath(*parts) if parts else PurePosixPath(".")

    def __call__(self, node: ast.AST) -> PurePosixPath | None:
        if isinstance(node, ast.Constant) and isinstance(node.value, str):
            return self.normal(PurePosixPath(node.value))
        if isinstance(node, ast.Name):
            if node.id == "__file__":
                return self.file
            if node.id in self.names and node.id not in self.seen:
                self.seen.add(node.id)
                try:
                    return self(self.names[node.id])
                finally:
                    self.seen.discard(node.id)
            return None
        if isinstance(node, ast.BinOp) and isinstance(node.op, ast.Div):
            left, right = self(node.left), self(node.right)
            return self.normal(left / right) if left is not None and right is not None else None
        if isinstance(node, ast.Attribute) and node.attr == "parent":
            base = self(node.value)
            return self.normal(base / "..") if base is not None else None
        if (isinstance(node, ast.Subscript) and isinstance(node.value, ast.Attribute)
                and node.value.attr == "parents" and isinstance(node.slice, ast.Constant)
                and isinstance(node.slice.value, int)):
            base = self(node.value.value)
            return self.normal(base / ("../" * (node.slice.value + 1))) if base is not None else None
        if isinstance(node, ast.Call):
            name = node.func.attr if isinstance(node.func, ast.Attribute) else getattr(node.func, "id", "")
            if name in ("Path", "PurePath", "PurePosixPath", "str", "fspath") and len(node.args) == 1:
                return self(node.args[0])
            if name in ("resolve", "absolute") and isinstance(node.func, ast.Attribute):
                return self(node.func.value)
            if name == "with_name" and isinstance(node.func, ast.Attribute) and node.args:
                base, leaf = self(node.func.value), self(node.args[0])
                return self.normal(base / ".." / leaf) if base is not None and leaf is not None else None
        return None


def python_errors(root: Path, path: str, source: str, here: PurePosixPath, files: set[str],
                  directories: tuple[str, ...], present: set[str]) -> list[str]:
    """Isolation errors in one gated Python source (a file, or a shell heredoc or `-c` body
    run from the repository root, `here`)."""
    try:
        tree = ast.parse(source, filename=path)
    except SyntaxError as error:
        return [f"{path}: gated Python does not parse: {error}"]
    allowed = set(sys.stdlib_module_names) | THIRD_PARTY
    canonical = ast.unparse(ast.parse(SIBLING_HELPER).body[0])
    paths = Paths(PurePosixPath(path) if path.endswith(".py") else None, tree)
    errors = []

    def load(label: str, node: ast.AST | None, line: int) -> None:
        target = paths(node) if node is not None else None
        if target is None:
            errors.append(f"{path}:{line}: {label} from a computed path cannot be checked")
        elif str(target) != path and str(target) in present and not is_gated(str(target), files, directories):
            errors.append(f"{path}:{line}: loads {target}, which is not a gate file")
        elif str(target) not in present and (root / str(target)).exists():
            errors.append(f"{path}:{line}: loads {target}, which is untracked")

    parameters: dict[ast.AST, set[str]] = {}
    for function in ast.walk(tree):
        if isinstance(function, (ast.FunctionDef, ast.AsyncFunctionDef)):
            names = {argument.arg for argument in function.args.args}
            for inner in ast.walk(function):
                parameters.setdefault(inner, set()).update(names)
            if function.name == "_sibling" and ast.unparse(function) != canonical:
                errors.append(f"{path}:{function.lineno}: `_sibling` differs from the pinned loader")
    for node in ast.walk(tree):
        line = getattr(node, "lineno", 0)
        if (isinstance(node, ast.Attribute) and node.attr == "path"
                and isinstance(node.value, ast.Name) and node.value.id == "sys"):
            errors.append(f"{path}:{line}: gated Python must not use `sys.path`")
        elif isinstance(node, ast.ImportFrom) and node.module == "sys" and any(a.name == "path" for a in node.names):
            errors.append(f"{path}:{line}: gated Python must not use `sys.path`")
        if isinstance(node, (ast.Import, ast.ImportFrom)):
            if isinstance(node, ast.ImportFrom) and node.level:
                errors.append(f"{path}:{line}: relative import in gated Python")
            modules = [alias.name for alias in node.names] if isinstance(node, ast.Import) else [node.module or ""]
            for module in modules:
                if module.split(".", 1)[0] not in allowed:
                    errors.append(f"{path}:{line}: `{module}` is neither standard library nor pinned "
                                  "third-party; load gated siblings with `_sibling`")
        if not isinstance(node, ast.Call):
            if (isinstance(node, ast.BinOp) and isinstance(node.op, ast.Div)
                    and isinstance(node.right, ast.Constant) and str(node.right.value).endswith(".py")):
                target = paths(node)
                if target is not None and str(target) in present and str(target) != path \
                        and not is_gated(str(target), files, directories):
                    errors.append(f"{path}:{line}: names {target}, which is not a gate file")
            continue
        name = node.func.attr if isinstance(node.func, ast.Attribute) else getattr(node.func, "id", "")
        own = parameters.get(node, set())
        if name == "_sibling" and node.args:
            module = paths(node.args[0])
            if module is None:
                errors.append(f"{path}:{line}: `_sibling` of a computed name cannot be checked")
            elif len(node.args) == 1 and not node.keywords:
                base = PurePosixPath(path).parent if path.endswith(".py") else here
                load("`_sibling`", ast.Constant(str(base / f"{module}.py")), line)
            else:
                directory = node.args[1] if len(node.args) > 1 else node.keywords[0].value
                load("`_sibling`", ast.BinOp(directory, ast.Div(), ast.Constant(f"{module}.py")), line)
        elif name in ("spec_from_file_location", "run_path") or name in PATH_WRAPPERS:
            index = PATH_WRAPPERS.get(name, 1 if name == "spec_from_file_location" else 0)
            argument = node.args[index] if len(node.args) > index else None
            if isinstance(argument, ast.Name) and argument.id in own:
                continue  # a wrapper's parameter: followed at the wrapper's call sites
            load(f"`{name}`", argument, line)
    return errors


def check(root: Path, gate_source: str) -> list[str]:
    paths = tracked(root)
    present = set(paths)
    files, directories = gates(gate_source)
    errors = shadow_errors(paths)
    for path in paths:
        if path.startswith(".github/workflows/") and path.endswith((".yml", ".yaml")):
            errors += workflow_errors(path, (root / path).read_text(encoding="utf-8"))
    for path in TRUSTED_WORKFLOWS:
        if path not in present:
            errors.append(f"{path}: trusted workflow is missing")
    errors += runner_errors((root / RUNNER).read_text(encoding="utf-8")) if RUNNER in present \
        else [f"{RUNNER}: trusted runner is missing"]
    for path in SHELL_GATES:
        errors += shell_errors(root, path, (root / path).read_text(encoding="utf-8"), files, directories, present) \
            if path in present else [f"{path}: gated shell script is missing"]
    for path in sorted(present):
        if path.endswith(".py") and is_gated(path, files, directories) and not path.startswith(".github/"):
            try:
                source = (root / path).read_text(encoding="utf-8")
            except (OSError, UnicodeDecodeError) as error:
                errors.append(f"{path}: gated Python is unreadable: {error}")
                continue
            errors += python_errors(root, path, source, PurePosixPath(path).parent, files, directories, present)
    return errors


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--root", default=str(REPO), help="the checkout checked as data (default: this one)")
    args = parser.parse_args()
    # The gate list is always this checkout's: in the trusted lane, the base's.
    errors = check(Path(args.root).resolve(), (REPO / GATE_SOURCE).read_text(encoding="utf-8"))
    for error in errors:
        print(error, file=sys.stderr)
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main())
