"""Readiness smoke; replay/checkpoint integration is added separately."""
import json
import pathlib
import sys
import time
from rest import API
from setup import provision


def connect():
    config = json.loads(pathlib.Path('/home/user/parity-runtime/credentials.json').read_text())
    deadline = time.monotonic() + 60
    while True:
        try:
            return API(**config)
        except OSError:
            if time.monotonic() >= deadline:
                raise
            time.sleep(1)


if __name__ == '__main__':
    api = connect()
    traces = json.loads(pathlib.Path(sys.argv[1]).read_text())
    results = []
    for trace in traces:
        fixture = provision(api, trace)
        results.append({'case': trace['id'], 'method': trace['method'], 'status': 'unsupported',
                        'reason': 'readiness-only: replay adapter not yet connected',
                        'warehouses': sorted(fixture['warehouses'])})
    pathlib.Path(sys.argv[2]).write_text(json.dumps({'format': 'norn.stock-readiness/1',
        'versions': json.loads(pathlib.Path('/home/user/parity-runtime/versions.json').read_text()),
        'cases': results}, indent=2) + '\n')
