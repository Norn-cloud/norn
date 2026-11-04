"""Synthetic fixtures created only through the public document REST API."""
import uuid
from rest import NativeError


def ensure(api, doctype, identity, **fields):
    try:
        return api.get(doctype, identity)
    except NativeError as error:
        if error.status != 404:
            raise
        return api.create(doctype, **fields)


def provision(api, trace):
    suffix = uuid.uuid4().hex[:10]
    company = 'Parity ' + suffix
    ensure(api, 'Currency', 'USD', currency_name='USD', enabled=1, fraction='Cent', fraction_units=100)
    api.update('Currency', 'USD', enabled=1)
    ensure(api, 'UOM', 'Unit', uom_name='Unit', must_be_whole_number=0)
    api.update('UOM', 'Unit', must_be_whole_number=0)
    ensure(api, 'Item Group', 'All Item Groups', item_group_name='All Item Groups', is_group=1)
    ensure(api, 'Supplier Group', 'All Supplier Groups', supplier_group_name='All Supplier Groups', is_group=1)
    ensure(api, 'Customer Group', 'All Customer Groups', customer_group_name='All Customer Groups', is_group=1)
    ensure(api, 'Customer Group', 'Parity Customers', customer_group_name='Parity Customers',
           parent_customer_group='All Customer Groups', is_group=0)
    ensure(api, 'Territory', 'All Territories', territory_name='All Territories', is_group=1)
    api.update('System Settings', 'System Settings', float_precision=6, currency_precision=2, time_zone='UTC', language='en')
    api.update('Stock Settings', 'Stock Settings', allow_negative_stock=0)
    ensure(api, 'Price List', 'Parity Selling', price_list_name='Parity Selling',
           currency='USD', selling=1, enabled=1)
    ensure(api, 'Warehouse Type', 'Transit', name='Transit')
    api.create('Company', company_name=company, abbr=suffix, default_currency='USD',
               country='United States', chart_of_accounts='Standard', enable_perpetual_inventory=1)
    accounts = list(api.rows('Account', {'company': company}, ['name', 'account_type', 'is_group', 'root_type']))
    def account(kind):
        return next(a['name'] for a in accounts if a['account_type'] == kind and not a['is_group'])
    roles = {'srbnb': account('Stock Received But Not Billed'), 'cogs': account('Cost of Goods Sold'),
             'adjustment': account('Stock Adjustment'), 'rounding': account('Round Off')}
    warehouses = {}
    parent = next(a['name'] for a in accounts if a['root_type'] == 'Asset' and a['is_group'])
    for alias in ('a', 'b'):
        name = api.create('Account', account_name='Stock ' + alias, company=company,
                          parent_account=parent, account_type='Stock', is_group=0)['name']
        roles['stock-' + alias] = name
        warehouses[alias] = api.create('Warehouse', warehouse_name='Parity ' + alias,
                                       company=company, account=name)['name']
    api.update('Company', company, default_inventory_account=roles['stock-a'],
               stock_received_but_not_billed=roles['srbnb'], default_expense_account=roles['cogs'],
               stock_adjustment_account=roles['adjustment'], round_off_account=roles['rounding'])
    item = api.create('Item', item_code='parity-' + suffix, item_name='Synthetic stock',
                      item_group='All Item Groups', stock_uom='Unit', is_stock_item=1,
                      valuation_method={'fifo': 'FIFO', 'moving-average': 'Moving Average'}[trace['method']])['name']
    supplier = api.create('Supplier', supplier_name='Supplier ' + suffix,
                          supplier_group='All Supplier Groups', supplier_type='Company')['name']
    customer = api.create('Customer', customer_name='Customer ' + suffix, customer_type='Company',
                          customer_group='Parity Customers', territory='All Territories')['name']
    years = {c['effectiveAt'][:4] for s in trace['steps'] for c in
             ([s['command']] if s['kind'] == 'serial' else s['commands'])}
    for year in years:
        ensure(api, 'Fiscal Year', year, year=year, year_start_date=year+'-01-01', year_end_date=year+'-12-31')
    return dict(company=company, item=item, warehouses=warehouses, accounts=roles,
                supplier=supplier, customer=customer)
