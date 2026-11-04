"""Normalize REST observations, retaining raw ledger legs and native identities."""
import json
import time
from decimal import Decimal


def decimal(value):
    number = Decimal(str(value))
    if not number.is_finite():
        raise ValueError('Non-finite oracle amount')
    text = format(number, 'f')
    return '0' if number == 0 else text.rstrip('0').rstrip('.') if '.' in text else text


def checkpoint(driver, step, attempts, timeout=60):
    api, f = driver.api, driver.f
    deadline = time.monotonic() + timeout
    while list(api.rows('Repost Item Valuation', {'company': f['company'], 'status': ['in', ['Queued', 'In Progress']]}, ['name'])):
        if time.monotonic() >= deadline:
            raise TimeoutError('Valuation queue did not settle')
        time.sleep(1)
    failed = list(api.rows('Repost Item Valuation', {'company': f['company'], 'status': 'Failed'}, ['name']))
    if failed:
        raise RuntimeError('Valuation repost failed')
    requests = {d['name']: key for key, d in driver.docs.items()}
    warehouses = {value: key for key, value in f['warehouses'].items()}
    accounts = {value: key for key, value in f['accounts'].items()}
    stock = []
    for row in api.rows('Stock Ledger Entry', {'company': f['company'], 'item_code': f['item']},
                        ['name', 'voucher_no', 'warehouse', 'actual_qty', 'stock_value_difference',
                         'qty_after_transaction', 'stock_value', 'incoming_rate', 'valuation_rate',
                         'stock_queue', 'is_cancelled']):
        queue = row['stock_queue']
        fifo = None if queue is None else [[decimal(q), decimal(r)] for q, r in json.loads(queue, parse_float=Decimal)]
        stock.append(dict(id=row['name'], requestId=requests[row['voucher_no']],
                          line=requests[row['voucher_no']]+':1', warehouse=warehouses[row['warehouse']],
                          quantityDelta=decimal(row['actual_qty']), valueDelta=decimal(row['stock_value_difference']),
                          quantity=decimal(row['qty_after_transaction']), value=decimal(row['stock_value']),
                          incomingRate=decimal(row['incoming_rate']), valuationRate=decimal(row['valuation_rate']),
                          fifo=fifo, cancelled=bool(row['is_cancelled'])))
    gl = []
    for row in api.rows('GL Entry', {'company': f['company']},
                        ['name', 'voucher_no', 'account', 'account_currency', 'debit', 'credit', 'is_cancelled']):
        gl.append(dict(id=row['name'], requestId=requests[row['voucher_no']],
                       account=accounts.get(row['account'], 'unmapped:'+row['account']),
                       currency=row['account_currency'], debit=decimal(row['debit']),
                       credit=decimal(row['credit']), cancelled=bool(row['is_cancelled'])))
    documents = [dict(requestId=key, status=api.get(doc['doctype'], doc['name'])['docstatus'])
                 for key, doc in driver.docs.items()]
    return dict(step=step, attempts=attempts, stock=stock, gl=gl, documents=documents)
