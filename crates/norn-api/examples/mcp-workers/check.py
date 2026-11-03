#!/usr/bin/env python3
"""Real celld HTTP versus the identical native Rust request-value service."""
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import urllib.error
import urllib.request

project = Path(os.environ['NORN_MCP_PROJECT']).resolve()
os.environ['NORN_WORKERS_SQL_PROJECT'] = str(project)
# Reuse the qualified bounded daemon lifecycle without running its probe body.
source = Path(__file__).parents[3] / 'norn-cell/examples/workers-sql/check.py'
spec = importlib.util.spec_from_loader('lifecycle', loader=None)
lifecycle = importlib.util.module_from_spec(spec)
exec(compile(source.read_text().split('version = subprocess.check_output')[0], str(source), 'exec'), lifecycle.__dict__)
assert subprocess.check_output(['celld', '--version'], text=True).strip() == 'celld 0.6.3'
reference = subprocess.Popen([os.environ['NORN_MCP_REFERENCE']], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
child = lifecycle.start()
session = None
count = 0


def send(method='POST', path='/mcp', message=None, headers=None, status=200):
    global session, count
    headers = dict(headers or {})
    headers.setdefault('Norn-Org', 'demo')
    headers.setdefault('Norn-Environment', 'dev')
    headers.setdefault('MCP-Protocol-Version', '2025-11-25')
    headers['Content-Type'] = 'application/json'
    if session:
        headers.setdefault('Mcp-Session-Id', session)
    body = '' if message is None else json.dumps(message)
    req = urllib.request.Request(lifecycle.URL + path, data=body.encode() if method == 'POST' else None, method=method, headers=headers)
    try:
        response = urllib.request.urlopen(req, timeout=15)
    except urllib.error.HTTPError as error:
        response = error
    with response:
        raw = response.read().decode()
        actual = {'status': response.code, 'body': raw, 'session': response.headers.get('Mcp-Session-Id')}
    envelope = dict(method=method, path=path, origin=headers.get('Origin'), org=headers['Norn-Org'], environment=headers['Norn-Environment'], session=headers.get('Mcp-Session-Id'), protocol=headers['MCP-Protocol-Version'], body=body)
    reference.stdin.write(json.dumps([envelope, actual['session'] or 'unused', 1]) + '\n')
    reference.stdin.flush()
    expected = json.loads(reference.stdout.readline())
    assert actual == expected, (actual, expected)
    assert actual['status'] == status, actual
    session = actual['session'] or session
    count += 1
    return json.loads(raw) if raw else None


def rpc(method, params=None):
    return dict(jsonrpc='2.0', id=1, method=method, params=params or {})


try:
    send(message=rpc('tools/list'), status=400)
    send(message=rpc('initialize', {'protocolVersion': '2025-11-25'}))
    catalog = send('GET', '/catalog')
    assert catalog == send(message=rpc('tools/list'))['result']
    # Closed capability + complete typed argument schema, not a name blacklist.
    assert len(catalog['tools']) == 1
    tool = catalog['tools'][0]
    assert tool['name'] == 'norn.check'
    schema = tool['inputSchema']
    assert schema['additionalProperties'] is False
    assert set(schema['properties']) == {'artifacts', 'context'}
    assert all(v['type'] == 'string' for v in schema['properties'].values())
    checked = send(message=rpc('tools/call', {'name': 'norn.check', 'arguments': {'artifacts': '[]', 'context': '{}'}}))
    assert checked['result']['structuredContent']['admitted'] is True
    for name in ['sql', 'tables', 'columns', 'schema', 'norn.sql', 'norn.tables']:
        assert 'error' in send(message=rpc('tools/call', {'name': name, 'arguments': {'sql': 'SELECT * FROM sqlite_master'}}))
    refused = send(message=rpc('tools/call', {'name': 'norn.check', 'arguments': {'sql': 'SELECT 1'}}))
    assert refused['result']['isError'] is True
    assert refused['result']['structuredContent']['code'] == 'request.invalid'
    send(message={'jsonrpc': '2.0', 'method': 'notifications/initialized'}, status=202)
    send('GET', '/mcp', status=405)
    send('GET', '/schema', status=404)
    send(message=rpc('tools/list'), headers={'Origin': 'https://invalid.example'}, status=403)
    send(message=rpc('tools/list'), headers={'Norn-Org': 'other'}, status=403)
    send(message=rpc('tools/list'), headers={'Norn-Org': ''}, status=400)
    send(message=rpc('tools/list'), headers={'Mcp-Session-Id': 'unknown'}, status=404)
    send(message=rpc('tools/list'), headers={'MCP-Protocol-Version': 'unsupported'}, status=400)
    send(message={'padding': 'x' * 1048576}, status=413)
    lifecycle.stop(child)
    child = lifecycle.start()
    reference.stdin.close()
    reference.wait(timeout=10)
    reference = subprocess.Popen([os.environ['NORN_MCP_REFERENCE']], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
    # Protocol sessions are ephemeral; they must not survive DO eviction/restart.
    send(message=rpc('tools/list'), status=404)
    session = None
    reference.stdin.close()
    reference.wait(timeout=10)
    print(f'PASS: {count} real-celld/native correspondences; negative catalog, raw refusal, checker, scope, origin, bounds, restart')
finally:
    lifecycle.stop(child)
    if reference.poll() is None:
        reference.stdin.close()
        reference.wait(timeout=10)
