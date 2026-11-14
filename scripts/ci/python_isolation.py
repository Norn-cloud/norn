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
- no tracked file is bytecode or sits under `__pycache__`, and none is importable under a
  standard-library or `THIRD_PARTY` name;
- `_sibling` is only ever the pinned `def` (compiled from source, never from bytecode or by
  name alone), in every tracked Python file, gated or not;
- every file named in `GATE_FILES` or under `GATE_DIRECTORIES` changes only with an
  approved gate edit, so anything below that a gated file could do differently needs one.

Best effort on top, as defence in depth behind those guarantees and the review of every
gated edit; none of it is a parser of shell, YAML or Python semantics:

- gated Python (files, and the heredoc and `-c` bodies of the gated shell scripts) is
  refused if it touches `sys.path`, imports anything but the standard library or
  `THIRD_PARTY`, aliases, rebinds or passes a followed loader or `exec`/`eval`/`compile`,
  names `builtins`, calls `getattr` on a loader module by its plain name, writes through
  `globals()`/`locals()`/`vars()`, uses the dynamic import machinery, `exec`s or `eval`s
  anything but a string literal, or calls `exec`/`eval`/`compile` with a file read in its
  own arguments; and a `_sibling`, `spec_from_file_location`, `run_path` or `PATH_WRAPPERS`
  load must resolve, through literals, `__file__` and names bound once, to a tracked gate
  file. Known forms this does not catch: a wrapper rebinding its own path parameter; a
  once-bound name rebound through `sys.modules[__name__]`, `setattr` or `__dict__`;
  `getattr` on a loader module imported under another name; `runpy.__dict__[...]` or
  `vars(runpy)[...]`; and `compile` of a non-literal run through `types.FunctionType`;
- shell: `ENV_DROPS` and `python_flag_errors` refuse constructs that drop the header's
  variables or hide Python input (`-I`/`-E` anywhere in a `python` command's flags, quoted
  or not, `-X pycache_prefix`, here-strings on stdin, double-quoted `-c`);
  `python_heredocs` and the base's `HEREDOC` matcher read heredoc bodies fed to a command
  naming `python`. An interpreter reached only through a variable (`"$PY"`) is not
  recognised;
- workflow lines are scanned for `python` without `-I`.
"""

from __future__ import annotations

import argparse
import ast
import re
import shlex
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
RUNNER_ISOLATED = ('ISOLATED = "import sys; sys.exit(0 if sys.flags.isolated and sys.dont_write_bytecode '
                   'and sys.pycache_prefix else 1)"')
# The assertion gates everything after it: it is the first statement of `main` that runs a
# command (before rustup), and a failure returns before any other command starts.
RUNNER_GATE = ('if subprocess.run([*PYTHON, "-c", ISOLATED], cwd=REPO, env=env).returncode != 0:\n'
               '    print("child Python is not isolated (-I, -B, pycache_prefix)", file=sys.stderr)\n'
               '    return 1')
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
    (re.compile(r"\b(?:unset|export\s+-n)\b[^#\n]*[$`]"), "unsetting or unexporting a computed name (`${!PYTHON@}`)"),
    (re.compile(r"-X\s*['\"]?pycache_prefix"), "a `-X pycache_prefix` override (an empty one turns the prefix off)"),
    (re.compile(r"python3?\b[^#\n]*(?:(?<![<\d])|(?<![\w])0)<<<"), "a here-string as Python's input (not read by this check)"),
    (re.compile(r"python3?(?:\.\d+)?[\"'}]*\s+(?:-\w+\s+)*-[A-Za-z]*I"),
     "`python -I` in a gated script (it ignores PYTHONPYCACHEPREFIX, so bytecode beside sources is read)"),
    (re.compile(r"python3?(?:\.\d+)?\s+(?:-\w+\s+)*-c\s*\""), "a double-quoted `python -c` body (not read by this check)"),
)
WORKFLOW_PYTHON = re.compile(r"(?:^|[\s;&|(\"'`])((?:[\w./${}-]*/)?python3?(?:\.\d+)?)\"?(?=\s|$)[ \t]*(\S*)")
HEREDOC_OPERATOR = re.compile(r"(?<!<)\d*<<(?!<)(-?)\s*(['\"]?)(\w+)\2")
PYTHON_WORD = re.compile(r"(?<![\w])python3?(?:\.\d+)?(?![\w])")
# The matcher before the line-based reader, kept so the two together read a superset of what
# either reads: `python` anywhere before `<<` on the command, continued over `\` lines.
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
        if re.search(r"-X\s*['\"]?pycache_prefix", line) and line.strip() != f"run: {TRUSTED_WORKFLOW_RUN}":
            errors.append(f"{path}:{number}: a `-X pycache_prefix` outside the pinned trusted run is refused")
    if path in TRUSTED_WORKFLOWS:
        runs = [line.strip() for line in lines if "validate-runner-policy.py" in line and not line.lstrip().startswith("#")]
        if runs != [f"run: {TRUSTED_WORKFLOW_RUN}"]:
            errors.append(f"{path}: the trusted runner must run exactly once, as `{TRUSTED_WORKFLOW_RUN}`")
    return errors


# Every way the runner could start a process before its isolation assertion.
PROCESS_CALLS = frozenset({"subprocess.run", "subprocess.call", "subprocess.check_call", "subprocess.check_output",
                           "subprocess.Popen", "subprocess.getoutput", "subprocess.getstatusoutput", "os.system",
                           "os.popen", "os.execv", "os.execve", "os.execvp", "os.execvpe", "os.execl", "os.execle",
                           "os.execlp", "os.execlpe", "os.spawnv", "os.spawnve", "os.spawnvp", "os.spawnvpe",
                           "os.spawnl", "os.spawnle", "os.spawnlp", "os.spawnlpe", "os.posix_spawn", "os.posix_spawnp"})


def runner_errors(text: str) -> list[str]:
    tree = ast.parse(text)
    uses = [node for node in ast.walk(tree) if isinstance(node, ast.Attribute) and node.attr == "executable"
            and isinstance(node.value, ast.Name) and node.value.id == "sys"]
    pinned = [node for node in tree.body if isinstance(node, ast.Assign) and ast.unparse(node) == ast.unparse(ast.parse(RUNNER_PYTHON).body[0])]
    errors = []
    if len(pinned) != 1 or len(uses) != 1:
        errors.append(f"{RUNNER}: every child Python must start from the one pinned `{RUNNER_PYTHON}`")
    asserted = [node for node in tree.body if isinstance(node, ast.Assign)
                and ast.unparse(node) == ast.unparse(ast.parse(RUNNER_ISOLATED).body[0])]
    checks = [node for node in ast.walk(tree) if isinstance(node, ast.List) and len(node.elts) == 3
              and ast.unparse(node) == '[*PYTHON, \'-c\', ISOLATED]']
    gate = ast.unparse(ast.parse(RUNNER_GATE).body[0])
    main = next((node for node in tree.body if isinstance(node, ast.FunctionDef) and node.name == "main"), None)
    runs = [statement for statement in (main.body if main else [])
            if any(isinstance(n, ast.Call) and ast.unparse(n.func) in PROCESS_CALLS for n in ast.walk(statement))]
    gated = bool(runs) and ast.unparse(runs[0]) == gate
    if len(asserted) != 1 or len(checks) != 1 or not gated:
        errors.append(f"{RUNNER}: the first command `main` runs must be the pinned isolation assertion, "
                      "returning 1 before anything else when it fails")
    for node in ast.walk(tree):
        if (isinstance(node, ast.List) and node.elts and isinstance(node.elts[0], ast.Constant)
                and re.fullmatch(r"(?:.*/)?python3?(?:\.\d+)?", str(node.elts[0].value))):
            errors.append(f"{RUNNER}:{node.lineno}: a child Python must start with `*PYTHON`")
    return errors


FLAGS_WITH_ARGUMENT = frozenset({"-X", "-W", "--check-hash-based-pycs"})


def python_flag_errors(line: str) -> list[str]:
    """Refusals in the interpreter flags of each `python` command on one shell line, read as a
    set wherever they sit (`-X dev -I`, `-W error -I`, `-B -E`, quoted `"-I"`), and
    here-strings feeding the command's stdin (`<<<`, `0<<<`, `00<<<`, before or after it)."""
    errors = []
    for match in PYTHON_WORD.finditer(line):
        rest = line[match.end():].lstrip("\"'}")
        try:
            words = shlex.split(rest, comments=True)
        except ValueError:
            words = rest.split()
        index = 0
        while index < len(words):
            word = words[index]
            if not word.startswith("-") or word in ("-", "--") or word in ("-c", "-m") or \
                    (word.startswith("-") and not word.startswith("--") and ("c" in word[1:] or "m" in word[1:])
                     and word[1:].isalpha()):
                break
            value = None
            if word in FLAGS_WITH_ARGUMENT:
                value = words[index + 1] if index + 1 < len(words) else ""
                index += 1
            elif word.startswith(("-X", "-W")) and len(word) > 2:
                value = word[2:]
            letters = word[1:] if not word.startswith("--") and value is None else ""
            if "I" in letters or "E" in letters:
                errors.append(f"`python {word}` (-I/-E) would drop Python isolation in a gated script")
            if word.startswith("-X") and (value or "").startswith("pycache_prefix"):
                errors.append("a `-X pycache_prefix` override would drop Python isolation")
            index += 1
    if PYTHON_WORD.search(line):
        for here in re.finditer(r"(?<!<)(\d*)<<<", line):
            if not here.group(1) or int(here.group(1)) == 0:
                errors.append("a here-string on Python's stdin is not read by this check")
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
        errors += [f"{path}:{number}: {error}" for error in python_flag_errors(line)]
    bodies = dict((body, number) for number, body in python_heredocs(path, lines, errors))
    for match in HEREDOC.finditer(text):
        bodies.setdefault(match.group(3), text.count("\n", 0, match.start(3)) + 1)
    for body, number in bodies.items():
        errors += python_errors(root, f"{path}:{number}", body, PurePosixPath("."),
                                files, directories, present)
    for match in DASH_C.finditer(text):
        number = text.count("\n", 0, match.start()) + 1
        errors += python_errors(root, f"{path}:{number}", match.group(1), PurePosixPath("."),
                                files, directories, present)
    return errors


def python_heredocs(path: str, lines: list[str], errors: list[str]) -> list[tuple[int, str]]:
    """Each heredoc body a gated shell script feeds to Python, with its first line number.

    Best effort: a command (joined over `\\` continuation lines) that names `python` anywhere,
    `cat <<PY | python3 -` included, has its heredocs read as Python; `<<-` terminators may be
    tab-indented; a body that never terminates is refused. An interpreter reached only
    through a variable (`"$PY"`) is not recognised."""
    bodies = []
    index = 0
    while index < len(lines):
        start = index
        command = lines[index]
        while command.endswith("\\") and index + 1 < len(lines):
            index += 1
            command = command[:-1] + " " + lines[index]
        index += 1
        if command.lstrip().startswith("#"):
            continue
        is_python = bool(PYTHON_WORD.search(command))
        for operator in HEREDOC_OPERATOR.finditer(command):
            strip, delimiter = operator.group(1), operator.group(3)
            body_start = index
            while index < len(lines) and (lines[index].lstrip("\t") if strip else lines[index]) != delimiter:
                index += 1
            if index >= len(lines):
                errors.append(f"{path}:{start + 1}: heredoc `{delimiter}` never terminates")
                return bodies
            if is_python:
                bodies.append((body_start + 1, "\n".join(lines[body_start:index])))
            index += 1
    return bodies


class Paths:
    """Repository-relative paths of the expressions a Python source builds from literals and
    `__file__`; `None` where the value is computed."""

    def __init__(self, file: PurePosixPath | None, tree: ast.Module):
        self.file = file
        # A name resolves only if it is bound exactly once in the whole source, module-level or
        # function-local, by a simple `name = value`; a name bound twice or any other way (a
        # parameter, loop, `with`, augmented or walrus target) resolves to nothing.
        bindings: dict[str, int] = {}
        simple: dict[str, ast.AST] = {}
        for node in ast.walk(tree):
            if isinstance(node, ast.Name) and isinstance(node.ctx, ast.Store):
                bindings[node.id] = bindings.get(node.id, 0) + 1
            elif isinstance(node, ast.arg):
                bindings[node.arg] = bindings.get(node.arg, 0) + 1
            if isinstance(node, ast.Assign) and len(node.targets) == 1 and isinstance(node.targets[0], ast.Name):
                simple[node.targets[0].id] = node.value
        self.names = {name: value for name, value in simple.items() if bindings.get(name) == 1}
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


DYNAMIC_IMPORTS = frozenset({"SourceFileLoader", "SourcelessFileLoader", "ExtensionFileLoader", "__import__",
                             "import_module", "run_module", "zipimporter", "get_importer", "iter_importers",
                             "PathFinder", "FileFinder", "find_spec", "find_loader", "path_hooks",
                             "path_importer_cache", "meta_path"})
# Loaders this check follows: they may only be called directly, never aliased, rebound,
# passed or reached through `getattr`, so every call is one the follower sees.
LOADERS = frozenset({"_sibling", "spec_from_file_location", "run_path", "exec", "eval", "compile", *PATH_WRAPPERS})
LOADER_MODULES = frozenset({"importlib", "importlib.util", "importlib.machinery", "runpy", "pkgutil", "sys.modules"})
FILE_READS = frozenset({"open", "read_text", "read_bytes", "read"})


def sibling_errors(path: str, tree: ast.AST) -> list[str]:
    """`_sibling` is only ever the pinned `def`: never rebound (`_sibling = lambda ...`), imported
    or redefined differently. Checked in every tracked Python file, gated or not."""
    canonical = ast.unparse(ast.parse(SIBLING_HELPER).body[0])
    errors = []
    for node in ast.walk(tree):
        line = getattr(node, "lineno", 0)
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)) and node.name == "_sibling":
            if ast.unparse(node) != canonical:
                errors.append(f"{path}:{line}: `_sibling` differs from the pinned loader")
        elif isinstance(node, ast.Name) and node.id == "_sibling" and isinstance(node.ctx, (ast.Store, ast.Del)):
            errors.append(f"{path}:{line}: `_sibling` may only be bound by the pinned `def`")
        elif isinstance(node, ast.alias) and "_sibling" in (node.name, node.asname):
            errors.append(f"{path}:{line}: `_sibling` may only be bound by the pinned `def`")
    return errors


def python_errors(root: Path, path: str, source: str, here: PurePosixPath, files: set[str],
                  directories: tuple[str, ...], present: set[str]) -> list[str]:
    """Isolation errors in one gated Python source (a file, or a shell heredoc or `-c` body
    run from the repository root, `here`).

    What is followed: paths built from literals, `__file__` and names bound exactly once
    (`Paths`), in `_sibling`, `spec_from_file_location`, `run_path` and the `PATH_WRAPPERS`;
    such a load must resolve to a tracked gate file, and one that does not resolve is
    refused. `exec`/`eval`/`compile` of a file read outside `_sibling`, and the dynamic import
    machinery (`DYNAMIC_IMPORTS`), are refused outright."""
    try:
        tree = ast.parse(source, filename=path)
    except SyntaxError as error:
        return [f"{path}: gated Python does not parse: {error}"]
    allowed = set(sys.stdlib_module_names) | THIRD_PARTY
    canonical = ast.unparse(ast.parse(SIBLING_HELPER).body[0])
    paths = Paths(PurePosixPath(path) if path.endswith(".py") else None, tree)
    errors = sibling_errors(path, tree)

    def load(label: str, node: ast.AST | None, line: int) -> None:
        target = paths(node) if node is not None else None
        if target is None:
            errors.append(f"{path}:{line}: {label} from a path this check cannot resolve is refused")
        elif str(target) == path:
            return
        elif str(target) not in present:
            errors.append(f"{path}:{line}: loads {target}, which is not a tracked file")
        elif not is_gated(str(target), files, directories):
            errors.append(f"{path}:{line}: loads {target}, which is not a gate file")

    parents: dict[ast.AST, ast.AST] = {child: parent for parent in ast.walk(tree) for child in ast.iter_child_nodes(parent)}
    for node in ast.walk(tree):
        line = getattr(node, "lineno", 0)
        named = node.id if isinstance(node, ast.Name) else node.attr if isinstance(node, ast.Attribute) else None
        parent = parents.get(node)
        # A `_sibling` store is already refused by `sibling_errors`; nothing else is exempt.
        if named in LOADERS and not (isinstance(parent, ast.Call) and parent.func is node) \
                and not (isinstance(node, ast.Name) and isinstance(node.ctx, ast.Store) and named == "_sibling"):
            errors.append(f"{path}:{line}: `{named}` may only be called directly (no alias, rebinding or passing)")
        if (isinstance(node, ast.Name) and node.id in ("builtins", "__builtins__")) or \
                (isinstance(node, ast.alias) and node.name.split(".", 1)[0] == "builtins"):
            errors.append(f"{path}:{line}: `builtins` is refused in gated Python")
        if isinstance(node, ast.alias) and node.name.rsplit(".", 1)[-1] in LOADERS and node.asname:
            errors.append(f"{path}:{line}: `{node.name}` may not be imported under another name")
        if isinstance(node, ast.Call) and getattr(node.func, "id", "") == "getattr" and node.args \
                and ast.unparse(node.args[0]) in LOADER_MODULES:
            errors.append(f"{path}:{line}: `getattr` on `{ast.unparse(node.args[0])}` is refused")
        if isinstance(node, ast.Call) and getattr(node.func, "id", "") in ("exec", "eval") and not (
                node.args and isinstance(node.args[0], ast.Constant) and isinstance(node.args[0].value, str)):
            function = parents.get(node)
            while function is not None and not isinstance(function, ast.FunctionDef):
                function = parents.get(function)
            if not (function and function.name == "_sibling" and ast.unparse(function) == canonical):
                errors.append(f"{path}:{line}: `{node.func.id}` of anything but a string literal is refused")
        if isinstance(node, ast.Call) and getattr(node.func, "id", "") in ("globals", "locals", "vars"):
            use = parents.get(node)
            if (isinstance(use, ast.Subscript) and isinstance(use.ctx, (ast.Store, ast.Del))) or \
                    (isinstance(use, ast.Attribute) and use.attr in ("update", "setdefault", "__setitem__")):
                errors.append(f"{path}:{line}: writing through `{node.func.id}()` is refused")
    enclosing: dict[ast.AST, ast.AST] = {}
    for function in ast.walk(tree):
        if isinstance(function, (ast.FunctionDef, ast.AsyncFunctionDef, ast.Lambda)):
            for inner in ast.walk(function):
                if inner is not function:
                    enclosing[inner] = function  # walk order leaves the innermost function last
    for node in ast.walk(tree):
        line = getattr(node, "lineno", 0)
        function = enclosing.get(node)
        in_loader = (isinstance(function, ast.FunctionDef) and function.name == "_sibling"
                     and ast.unparse(function) == canonical)
        if (isinstance(node, ast.Attribute) and node.attr == "path"
                and isinstance(node.value, ast.Name) and node.value.id == "sys"):
            errors.append(f"{path}:{line}: gated Python must not use `sys.path`")
        elif isinstance(node, ast.ImportFrom) and node.module == "sys" and any(a.name == "path" for a in node.names):
            errors.append(f"{path}:{line}: gated Python must not use `sys.path`")
        if (isinstance(node, ast.Name) and node.id in DYNAMIC_IMPORTS) or \
                (isinstance(node, ast.Attribute) and node.attr in DYNAMIC_IMPORTS) or \
                (isinstance(node, ast.alias) and node.name.rsplit(".", 1)[-1] in DYNAMIC_IMPORTS):
            errors.append(f"{path}:{line}: dynamic import machinery is refused in gated Python")
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
        if name in ("exec", "eval", "compile") and not in_loader and any(
                isinstance(inner, ast.Call) and (inner.func.attr if isinstance(inner.func, ast.Attribute)
                                                 else getattr(inner.func, "id", "")) in FILE_READS
                for argument in node.args for inner in ast.walk(argument)):
            errors.append(f"{path}:{line}: `{name}` of a file read is refused; load gated code with `_sibling`")
        if name == "_sibling" and node.args:
            module = paths(node.args[0])
            if module is None:
                errors.append(f"{path}:{line}: `_sibling` of a computed name is refused")
            elif len(node.args) == 1 and not node.keywords:
                base = PurePosixPath(path).parent if path.endswith(".py") else here
                load("`_sibling`", ast.Constant(str(base / f"{module}.py")), line)
            else:
                directory = node.args[1] if len(node.args) > 1 else node.keywords[0].value
                load("`_sibling`", ast.BinOp(directory, ast.Div(), ast.Constant(f"{module}.py")), line)
        elif name in ("spec_from_file_location", "run_path") or name in PATH_WRAPPERS:
            index = PATH_WRAPPERS.get(name, 1 if name == "spec_from_file_location" else 0)
            argument = node.args[index] if len(node.args) > index else None
            if (isinstance(argument, ast.Name) and isinstance(function, ast.FunctionDef)
                    and function.name in PATH_WRAPPERS
                    and len(function.args.args) > PATH_WRAPPERS[function.name]
                    and function.args.args[PATH_WRAPPERS[function.name]].arg == argument.id):
                continue  # the wrapper's own path parameter: followed at the wrapper's call sites
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
        if path.endswith(".py") and not is_gated(path, files, directories):
            try:
                errors += sibling_errors(path, ast.parse((root / path).read_text(encoding="utf-8"), filename=path))
            except (OSError, SyntaxError, UnicodeDecodeError):
                pass  # an ungated file that does not parse loads nothing through `_sibling`
            continue
        if path.endswith(".py") and not path.startswith(".github/"):
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
