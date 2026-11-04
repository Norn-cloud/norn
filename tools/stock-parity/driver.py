"""First oracle checkpoint: serial receipt and delivery through document REST."""
from rest import NativeError
from decimal import Decimal


class Driver:
    def __init__(self, api, fixture):
        self.api, self.f = api, fixture
        self.docs = {}

    def step(self, step):
        if step['kind'] != 'serial':
            raise ValueError('Concurrent traces are outside this checkpoint')
        command, f = step['command'], self.f
        op, key = command['operation'], command['requestId']
        if op['kind'] not in ('receipt', 'delivery') or key in self.docs:
            raise ValueError('Checkpoint supports fresh receipts and deliveries only')
        date, time = command['effectiveAt'].removesuffix('Z').split('T')
        receipt = op['kind'] == 'receipt'
        doctype = 'Purchase Receipt' if receipt else 'Delivery Note'
        doc = dict(company=f['company'], posting_date=date, posting_time=time, set_posting_time=1,
                   currency='USD', conversion_rate=1,
                   items=[dict(item_code=f['item'], qty=Decimal(op['quantity']), rate=Decimal(op.get('rate', '1')),
                               warehouse=f['warehouses'][op['warehouse']], uom='Unit', stock_uom='Unit',
                               conversion_factor=1, expense_account=f['accounts']['cogs'])])
        if not receipt:
            doc.update(selling_price_list='Parity Selling', price_list_currency='USD', plc_conversion_rate=1)
        doc['supplier' if receipt else 'customer'] = f['supplier' if receipt else 'customer']
        result = dict(attempt=0, requestId=key, nativeId=None, outcome='accepted',
                      nativeCode=None, reason=None, completionOrder=0)
        try:
            self.docs[key] = self.api.create(doctype, **doc)
            result['nativeId'] = self.docs[key]['name']
            self.api.submit(self.docs[key])
        except NativeError as error:
            result.update(outcome='refused', nativeCode=error.code, reason=error.code)
        return [result]
