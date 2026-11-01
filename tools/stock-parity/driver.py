"""REST lifecycle driver for validated norn.stock-trace/1 commands."""
from concurrent.futures import ThreadPoolExecutor
from threading import Barrier
from rest import API, NativeError


class Driver:
    def __init__(self, api, fixture, credentials):
        self.api, self.f, self.credentials = api, fixture, credentials
        self.docs, self.payloads = {}, {}

    def document(self, command):
        op, f = command['operation'], self.f
        kind = op['kind']
        if kind == 'cancel':
            return self.docs[op['target']]
        date, time = command['effectiveAt'].removesuffix('Z').split('T')
        line = dict(item_code=f['item'], qty=op['quantity'], uom='Unit', stock_uom='Unit',
                    conversion_factor=1, expense_account=f['accounts']['cogs'], cost_center=None)
        doc = dict(company=f['company'], posting_date=date, posting_time=time,
                   set_posting_time=1, currency='USD', conversion_rate=1)
        if kind in ('receipt', 'delivery', 'return'):
            doc['doctype'] = 'Purchase Receipt' if kind == 'receipt' else 'Delivery Note'
            doc['supplier' if kind == 'receipt' else 'customer'] = f['supplier' if kind == 'receipt' else 'customer']
            if kind == 'return':
                original = self.docs[op['delivery']]
                doc.update(is_return=1, return_against=original['name'])
                line.update(qty='-' + op['quantity'], warehouse=original['items'][0]['warehouse'],
                            dn_detail=original['items'][0]['name'], rate=original['items'][0]['rate'])
            else:
                line.update(warehouse=f['warehouses'][op['warehouse']], rate=op.get('rate', '1'))
            doc['items'] = [line]
        elif kind == 'transfer':
            doc.update(doctype='Stock Entry', stock_entry_type='Material Transfer', purpose='Material Transfer')
            line.update(s_warehouse=f['warehouses'][op['from']], t_warehouse=f['warehouses'][op['to']])
            doc['items'] = [line]
        elif kind == 'reconcile':
            doc.update(doctype='Stock Reconciliation', purpose='Stock Reconciliation',
                       expense_account=f['accounts']['adjustment'])
            doc['items'] = [dict(item_code=f['item'], warehouse=f['warehouses'][op['warehouse']],
                                 qty=op['quantity'], valuation_rate=op['rate'])]
        else:
            raise ValueError('Unsupported trace operation: ' + kind)
        return doc

    def prepare(self, command):
        key = command['requestId']
        if command['operation']['kind'] == 'cancel':
            return self.document(command)
        if key not in self.docs:
            doc = self.document(command)
            self.docs[key] = self.api.create(doc['doctype'], **{k: v for k, v in doc.items() if k != 'doctype'})
            self.payloads[key] = command
        return self.docs[key]

    def attempt(self, api, command, doc, attempt, barrier=None):
        if barrier:
            barrier.wait(timeout=60)
        result = dict(attempt=attempt, requestId=command['requestId'], nativeId=doc['name'],
                      outcome='accepted', nativeCode=None, reason=None)
        try:
            if command['operation']['kind'] == 'cancel':
                api.cancel(doc['doctype'], doc['name'])
            else:
                current = api.get(doc['doctype'], doc['name'])
                if command != self.payloads[command['requestId']]:
                    changed = self.document(command)
                    api.update(doc['doctype'], doc['name'], **{k: v for k, v in changed.items() if k != 'doctype'})
                    current = api.get(doc['doctype'], doc['name'])
                api.submit(current)
        except NativeError as error:
            result.update(outcome='refused', nativeCode=error.code, reason=error.code)
        # Connection/timeouts propagate: an uncertain write is incomplete, never blindly retried.
        return result

    def step(self, step):
        commands = [step['command']] if step['kind'] == 'serial' else step['commands']
        prepared, results = [], []
        for index, command in enumerate(commands):
            try:
                prepared.append((index, command, self.prepare(command)))
            except NativeError as error:
                results.append(dict(attempt=index, requestId=command['requestId'], nativeId=None,
                                    outcome='refused', nativeCode=error.code, reason=error.code))
        if step['kind'] == 'serial':
            results += [self.attempt(self.api, c, d, i) for i, c, d in prepared]
        elif prepared:
            # Each member has its own authenticated HTTP session, released together.
            sessions = [API(**self.credentials) for _ in prepared]
            barrier = Barrier(len(prepared))
            from concurrent.futures import as_completed
            with ThreadPoolExecutor(max_workers=len(prepared)) as pool:
                futures = [pool.submit(self.attempt, a, c, d, i, barrier)
                           for a, (i, c, d) in zip(sessions, prepared)]
                results += [future.result() for future in as_completed(futures)]
        for order, result in enumerate(results):
            result['completionOrder'] = order
        return results
