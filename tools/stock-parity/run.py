"""Replay validated shared traces; preserve every native checkpoint and refusal."""
import json
import pathlib
import sys
import time
from rest import API
from setup import provision
from driver import Driver
from extract import checkpoint


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
        if trace['format'] != 'norn.stock-trace/1':
            raise ValueError('Requires validated stock-trace/1')
        result = dict(case=trace['id'], method=trace['method'], status='complete', reason=None, checkpoints=[])
        try:
            fixture = provision(api, trace)
            driver = Driver(api, fixture)
            result['initial'] = checkpoint(driver, -1, [])
            for index, step in enumerate(trace['steps']):
                attempts = driver.step(step)
                try:
                    observed = checkpoint(driver, index, attempts)
                except Exception:
                    result['checkpoints'].append(dict(step=index, attempts=attempts, status='unsupported'))
                    raise
                result['checkpoints'].append(observed)
        except Exception as error:
            result.update(status='unsupported', reason=type(error).__name__ + ': incomplete oracle execution')
        results.append(result)
    pathlib.Path(sys.argv[2]).write_text(json.dumps({'format': 'norn.stock-dump/1', 'engine': 'erpnext',
        'versions': json.loads(pathlib.Path('/home/user/parity-runtime/versions.json').read_text()),
        'cases': results}, indent=2) + '\n')
