#!/usr/bin/env python3
"""Real celld HTTP versus the identical native Rust request-value service."""
import json
import os
from pathlib import Path
import subprocess
import signal
import time
import urllib.error
import urllib.request

project = Path(os.environ['NORN_MCP_PROJECT']).resolve()
URL = 'http://127.0.0.1:9876'
assert subprocess.check_output(['celld', '--version'], text=True).strip() == 'celld 0.6.3'
transcript, observed = [], []


def stop(child):
    for sig in (signal.SIGTERM, signal.SIGKILL):
        if child.poll() is None:
            assert Path(os.readlink(f'/proc/{child.pid}/cwd')) == project
            os.killpg(child.pid, sig)
        try:
            child.wait(timeout=15)
            return
        except subprocess.TimeoutExpired:
            if sig == signal.SIGKILL:
                raise


def start():
    child = subprocess.Popen(['celld', 'dev', '--port', '9876'], cwd=project, start_new_session=True)
    try:
        for _ in range(100):
            assert child.poll() is None, 'celld exited'
            try:
                urllib.request.urlopen(URL, timeout=1).close()
                return child
            except urllib.error.HTTPError:
                return child
            except OSError:
                time.sleep(0.2)
        raise RuntimeError('celld startup timeout')
    except BaseException:
        stop(child)
        raise


child = start()
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
    req = urllib.request.Request(URL + path, data=body.encode() if method == 'POST' else None, method=method, headers=headers)
    try:
        response = urllib.request.urlopen(req, timeout=15)
    except urllib.error.HTTPError as error:
        response = error
    with response:
        raw = response.read().decode()
        actual = {'status': response.code, 'body': raw, 'session': response.headers.get('Mcp-Session-Id')}
    envelope = dict(method=method, path=path, headers={k.lower(): v for k, v in headers.items()}, body=body)
    transcript.append([envelope, actual['session'] or 'unused', 1])
    observed.append(actual)
    assert actual['status'] == status, actual
    session = actual['session'] or session
    count += 1
    return json.loads(raw) if raw else None


def rpc(method, params=None):
    return dict(jsonrpc='2.0', id=1, method=method, params=params or {})


def host_catalog_excludes_raw_storage(catalog):
    # Closed capability + complete typed argument schema, not a name blacklist.
    assert len(catalog['tools']) == 1
    tool = catalog['tools'][0]
    assert tool['name'] == 'norn.check'
    schema = tool['inputSchema']
    assert schema['additionalProperties'] is False
    assert set(schema['properties']) == {'artifacts', 'context'}
    assert all(v['type'] == 'string' for v in schema['properties'].values())

try:
    send(message=rpc('tools/list'), status=400)
    send(message=rpc('initialize', {'protocolVersion': '2025-11-25'}))
    catalog = send('GET', '/catalog')
    assert catalog == send(message=rpc('tools/list'))['result']
    host_catalog_excludes_raw_storage(catalog)
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
    stop(child)
    child = start()
    expected = json.loads(subprocess.check_output([os.environ['NORN_MCP_REFERENCE']], input=json.dumps(transcript).encode()))
    assert observed == expected, (observed, expected)
    transcript, observed = [], []
    # Protocol sessions are ephemeral; they must not survive DO eviction/restart.
    send(message=rpc('tools/list'), status=404)
    session = None
    assert observed == json.loads(subprocess.check_output([os.environ['NORN_MCP_REFERENCE']], input=json.dumps(transcript).encode()))
    print(f'PASS: {count} real-celld/native correspondences; negative catalog, raw refusal, checker, scope, origin, bounds, restart')
finally:
    stop(child)
