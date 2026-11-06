"""Black-box Frappe REST transport; no ERPNext imports or schema exports."""
import http.cookiejar
import json
import urllib.error
import urllib.parse
import urllib.request
from decimal import Decimal


def encode(value):
    if isinstance(value, Decimal):
        if not value.is_finite():
            raise ValueError('Non-finite REST number')
        return format(value, 'f')
    if isinstance(value, dict):
        return '{' + ','.join(json.dumps(k) + ':' + encode(v) for k, v in value.items()) + '}'
    if isinstance(value, list):
        return '[' + ','.join(encode(v) for v in value) + ']'
    return json.dumps(value, allow_nan=False)


class NativeError(Exception):
    def __init__(self, status, code):
        super().__init__(f'{status}: {code}')
        self.status, self.code = status, code


class API:
    def __init__(self, url, password):
        self.url = url
        self.cookies = http.cookiejar.CookieJar()
        self.opener = urllib.request.build_opener(
            urllib.request.HTTPCookieProcessor(self.cookies))
        self.call('POST', '/api/method/login', {'usr': 'Administrator', 'pwd': password})

    def call(self, method, path, body=None):
        data = None if body is None else encode(body).encode()
        headers = {'Content-Type': 'application/json', 'Host': 'parity.localhost'}
        # Frappe checks session writes against the CSRF token its login cookie carries.
        token = next((cookie.value for cookie in self.cookies if cookie.name == 'csrf_token'), None)
        if token:
            headers['X-Frappe-CSRF-Token'] = token
        req = urllib.request.Request(self.url + path, data=data, method=method, headers=headers)
        try:
            with self.opener.open(req, timeout=60) as response:
                result = json.load(response, parse_float=Decimal)
                return result.get('data', result.get('message'))
        except urllib.error.HTTPError as error:
            try:
                code = json.load(error).get('exc_type', 'HTTPError')
            except (ValueError, AttributeError):
                code = 'HTTPError'
            raise NativeError(error.code, code) from None

    def resource(self, doctype, name=None):
        return '/api/resource/' + urllib.parse.quote(doctype, safe='') + (
            '/' + urllib.parse.quote(name, safe='') if name else '')

    def create(self, doctype, **fields):
        return self.call('POST', self.resource(doctype), fields)

    def get(self, doctype, name):
        return self.call('GET', self.resource(doctype, name))

    def update(self, doctype, name, **fields):
        return self.call('PUT', self.resource(doctype, name), fields)

    def rows(self, doctype, filters, fields):
        start = 0
        while True:
            query = urllib.parse.urlencode({'filters': json.dumps(filters), 'fields': json.dumps(fields),
                                            'limit_start': start, 'limit_page_length': 100,
                                            'order_by': 'creation asc, name asc'})
            page = self.call('GET', self.resource(doctype) + '?' + query)
            yield from page
            if len(page) < 100:
                return
            start += len(page)

    def submit(self, doc):
        # Preserve JSON numeric tokens accepted at the REST boundary.
        return self.call('POST', '/api/method/frappe.client.submit', {'doc': doc})

    def cancel(self, doctype, name):
        return self.call('POST', '/api/method/frappe.client.cancel', {'doctype': doctype, 'name': name})
