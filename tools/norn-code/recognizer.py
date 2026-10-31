"""Norn Code: deterministic framework fingerprints, never source execution.

Usage: python tools/norn-code/recognizer.py ROOT --revision COMMIT
Output is versioned typed JSON, not admitted ontology or IR. Confidence is a
rule weight, not a calibrated probability. No code-object NornQL ingestion
surface currently exists in this tool.
"""
import argparse
import ast
from dataclasses import asdict, dataclass
import hashlib
import json
import os
from pathlib import Path
import re
import tomllib
from typing import Literal
from syntax import parse, walk, string

Framework = Literal['frappe', 'odoo', 'xstate', 'temporal', 'effect', 'shopify_graphql']
PACKAGES = {
    'frappe': 'frappe', 'odoo': 'odoo', 'xstate': 'xstate',
    'temporalio': 'temporal', '@temporalio/workflow': 'temporal',
    '@temporalio/client': 'temporal', '@temporalio/worker': 'temporal',
    'effect': 'effect', '@effect/io': 'effect',
    '@shopify/admin-api-client': 'shopify_graphql',
    '@shopify/storefront-api-client': 'shopify_graphql',
    '@shopify/shopify-api': 'shopify_graphql',
}
SKIP = {'.git', 'node_modules', '.venv', '__pycache__', 'target', 'dist', 'build'}


@dataclass(frozen=True)
class Evidence:
    file: str
    line: int
    source_hash: str
    revision: str
    level: Literal['declared', 'analyzed']


@dataclass(frozen=True)
class Recognition:
    framework: Framework
    confidence: float
    signal: str
    evidence: Evidence


def evidence(path, data, revision, line=1, level='analyzed'):
    return Evidence(path, line, hashlib.sha256(data).hexdigest(), revision, level)


def framework(package):
    return PACKAGES.get(package) or next((v for k, v in PACKAGES.items() if (package.startswith(k + '/') or (k in {'frappe', 'odoo', 'temporalio'} and package.startswith(k + '.')))), None)


def analyze(sources: dict[str, bytes], revision='working-tree'):
    matches, unknown = [], []
    for path, data in sorted(sources.items()):
        name = Path(path).name
        def emit(package, signal, confidence, line=1, level='analyzed'):
            found = framework(package)
            if found:
                matches.append(Recognition(found, confidence, signal, evidence(path, data, revision, line, level)))
        try:
            if name == 'package.json':
                manifest = json.loads(data)
                for group in ('dependencies', 'devDependencies', 'peerDependencies', 'optionalDependencies'):
                    for package in manifest.get(group, {}):
                        group_match = re.search(rb'"' + group.encode() + rb'"\s*:', data)
                        key_match = re.search(rb'"' + re.escape(package.encode()) + rb'"\s*:', data[group_match.end():]) if group_match else None
                        line = data[:group_match.end() + key_match.start()].count(b'\n') + 1 if key_match else 1
                        emit(package, 'manifest:' + package, .98, line, level='declared')
            elif name == 'pyproject.toml':
                manifest = tomllib.loads(data.decode())
                for dep in manifest.get('project', {}).get('dependencies', []):
                    package = re.split(r'[\s\[<>=!~;]', dep)[0].lower().replace('_', '-')
                    offset = data.find(dep.encode(), data.find(b'[project]'))
                    line = data[:offset].count(b'\n') + 1 if offset >= 0 else 1
                    emit(package, 'manifest:' + package, .98, line, level='declared')
            elif name in ('shopify.app.toml', 'shopify.web.toml'):
                tomllib.loads(data.decode())
                # App config alone does not establish GraphQL client use.
                unknown.append({'reason': 'shopify_config_without_client_evidence', 'evidence': asdict(evidence(path, data, revision))})
            elif path.endswith('.py'):
                tree = ast.parse(data)
                if name == '__manifest__.py':
                    manifest = ast.literal_eval(tree.body[0].value)
                    if isinstance(manifest, dict) and 'depends' in manifest:
                        emit('odoo', 'config:odoo_manifest', .9, level='declared')
                def bound_names(nodes):
                    names = set()
                    pending = list(nodes)
                    while pending:
                        item = pending.pop()
                        if isinstance(item, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)):
                            names.add(item.name)
                        elif isinstance(item, ast.Import):
                            names.update(alias.asname or alias.name.split('.')[0] for alias in item.names)
                        elif isinstance(item, ast.ImportFrom):
                            names.update(alias.asname or alias.name for alias in item.names)
                        else:
                            if isinstance(item, ast.Name) and isinstance(item.ctx, ast.Store):
                                names.add(item.id)
                            pending.extend(ast.iter_child_nodes(item))
                    return names
                def decorators(statements, inherited):
                    aliases = dict(inherited)
                    for node in statements:
                        if isinstance(node, ast.Import):
                            for alias in node.names:
                                aliases[alias.asname or alias.name.split('.')[0]] = alias.name if alias.asname else alias.name.split('.')[0]
                        elif isinstance(node, ast.ImportFrom):
                            for alias in node.names:
                                aliases[alias.asname or alias.name] = (node.module + '.' + alias.name) if node.module and node.level == 0 else 'local'
                        elif isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)):
                            for decorator in node.decorator_list:
                                expr = decorator.func if isinstance(decorator, ast.Call) else decorator
                                parts = ast.unparse(expr).split('.')
                                qualified = '.'.join([aliases.get(parts[0], 'local'), *parts[1:]])
                                if qualified in {'frappe.whitelist', 'odoo.api.constrains', 'odoo.api.depends', 'temporalio.workflow.defn', 'temporalio.activity.defn'}:
                                    emit(qualified, 'decorator:' + qualified, .97, decorator.lineno)
                            child = dict(aliases)
                            if not isinstance(node, ast.ClassDef):
                                child.update({key: 'local' for key in bound_names(node.body)})
                                child.update({arg.arg: 'local' for arg in ast.walk(node.args) if isinstance(arg, ast.arg)})
                            decorators(node.body, child)
                            aliases[node.name] = 'local'
                        else:
                            # Conditional/reassigned bindings need semantic resolution.
                            aliases.update({key: 'local' for key in bound_names([node])})
                decorators(tree.body, {})
                for node in ast.walk(tree):
                    if isinstance(node, ast.Import):
                        for alias in node.names:
                            emit(alias.name, 'import:' + alias.name, .95, node.lineno)
                    elif isinstance(node, ast.ImportFrom) and node.module and node.level == 0:
                        emit(node.module, 'import:' + node.module, .95, node.lineno)
                if name == 'hooks.py':
                    settings = [n for n in ast.walk(tree) if isinstance(n, ast.Name) and isinstance(n.ctx, ast.Store) and n.id in {'doc_events', 'scheduler_events', 'app_name'}]
                    if settings:
                        emit('frappe', 'config:hooks', .8, min(n.lineno for n in settings), level='declared')
            elif Path(path).suffix in {'.ts', '.tsx', '.js', '.jsx', '.mjs', '.cjs'}:
                tree = parse(data, path.endswith(('.tsx', '.jsx')))
                if tree.has_error:
                    raise ValueError('syntax error')
                for node in walk(tree):
                    if node.type in {'import_statement', 'export_statement'}:
                        package = string(node.child_by_field_name('source'))
                        type_only = any(child.type == 'type' for child in node.children)
                        specifiers = [child for child in walk(node) if child.type in {'import_specifier', 'export_specifier'}]
                        clause = next((c for c in node.named_children if c.type == 'import_clause'), None)
                        default_or_namespace = clause is not None and any(c.type in {'identifier', 'namespace_import'} for c in clause.named_children)
                        only_type_specifiers = not default_or_namespace and bool(specifiers) and all(any(c.type == 'type' for c in child.children) for child in specifiers)
                        if package and not type_only and not only_type_specifiers:
                            emit(package, 'import:' + package, .95, node.start_point.row + 1)
                    elif node.type == 'call_expression':
                        fn = node.child_by_field_name('function')
                        args = node.child_by_field_name('arguments')
                        if fn and fn.text in {b'require', b'import'} and args and len(args.named_children) == 1:
                            package = string(args.named_children[0])
                            if package:
                                emit(package, 'import:' + package, .9, node.start_point.row + 1)
        except (ValueError, SyntaxError, TypeError, AttributeError, IndexError, RecursionError) as error:
            unknown.append({'reason': 'unreadable_syntax', 'evidence': asdict(evidence(path, data, revision)), 'detail': type(error).__name__})
    matched_paths = {m.evidence.file for m in matches}
    diagnosed_paths = {u['evidence']['file'] for u in unknown}
    for path, data in sorted(sources.items()):
        if path not in matched_paths | diagnosed_paths:
            unknown.append({'reason': 'no_supported_fingerprint', 'evidence': asdict(evidence(path, data, revision))})
    return {'schema_version': 'norn-code/1', 'recognized': [asdict(m) for m in matches],
            'extracted': [], 'unknown': unknown,
            'coverage': {'files': len(sources), 'recognized_files': len(matched_paths), 'extracted': 0},
            'gaps': ['code facts are typed JSON; ontology ingestion and NornQL projection are not implemented']}


def collect(root: Path):
    if root.is_symlink() or any(p.is_symlink() for p in root.absolute().parents):
        raise ValueError('symlink source root')
    if not root.is_dir():
        raise ValueError('source root must be a directory')
    sources = {}
    def traversal_error(error):
        raise error
    for directory, dirs, files in os.walk(root, followlinks=False, onerror=traversal_error):
        dirs[:] = sorted(d for d in dirs if d not in SKIP)
        for name in [*dirs, *sorted(files)]:
            path = Path(directory) / name
            relative = path.relative_to(root)
            if path.is_symlink():
                raise ValueError('symlink source: ' + str(relative))
            if path.is_file() and (path.suffix in {'.py', '.ts', '.tsx', '.js', '.jsx', '.mjs', '.cjs'} or path.name in {'package.json', 'pyproject.toml', 'shopify.app.toml', 'shopify.web.toml'}):
                sources[relative.as_posix()] = path.read_bytes()
    return sources


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('root', type=Path)
    parser.add_argument('--revision', default='working-tree')
    args = parser.parse_args()
    print(json.dumps(analyze(collect(args.root), args.revision), sort_keys=True, indent=2))
