"""Native test fixture, synthetic data only; not a shipped application template."""
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
import sqlite3
import re

BUG = False
PG = bool(os.environ.get('MELISSA_PG_SOCKET')) and os.environ.get('FIXTURE_ENGINE') == 'postgres'
# Native tests select the engine through a source marker, not a production DSN.
PG = 'POSTGRES_FIXTURE' in globals() or os.path.exists(os.environ.get('MELISSA_PG_SOCKET', '/nonexistent') + '/.s.PGSQL.37642')


def query(sql, args=()):
    if PG:
        import psycopg
        c = psycopg.connect(host=os.environ['MELISSA_PG_SOCKET'], port=37642, dbname='postgres', user='melissa_fixture', autocommit=True)
        sql = sql.replace('?', '%s')
    else:
        c = sqlite3.connect(os.environ['MELISSA_FIXTURE_DB'])
    try:
        result = c.execute(sql, args)
        values = result.fetchall() if result.description else []
        c.commit()
        return values
    finally:
        c.close()


class Handler(BaseHTTPRequestHandler):
    def send(self, status, data):
        raw = json.dumps(data).encode()
        self.send_response(status)
        self.send_header('content-type', 'application/json')
        self.send_header('content-length', str(len(raw)))
        self.end_headers()
        self.wfile.write(raw)

    def unsupported(self):
        self.send_response(405)
        self.send_header('allow', 'GET, POST' if self.path.split('?')[0] == '/items' else 'GET')
        self.send_header('content-length', '0')
        self.end_headers()

    do_TRACE = do_PUT = do_PATCH = do_DELETE = do_OPTIONS = do_HEAD = do_CONNECT = unsupported

    def __getattr__(self, name):
        if name.startswith('do_'):
            return self.unsupported
        raise AttributeError(name)

    def do_GET(self):
        path = self.path.split('?')[0]
        if path == '/health':
            query('SELECT 1')
            return self.send(200, {'ready': True})
        if path == '/items':
            return self.send(200, [{'id': row[0], 'name': row[1]} for row in query('SELECT id,name FROM items ORDER BY id')])
        if path.startswith('/items/'):
            try:
                identity = int(path.rsplit('/', 1)[1])
                if identity < 1 or identity > 1000:
                    raise ValueError()
            except ValueError:
                return self.send(422, {'error': 'invalid id'})
            rows = query('SELECT id,name FROM items WHERE id=?', (identity,))
            return self.send(200, {'id': rows[0][0], 'name': rows[0][1]}) if rows else self.send(404, {'error': 'not found'})
        return self.send(404, {'error': 'not found'})

    def do_POST(self):
        if self.path != '/items':
            return self.unsupported()
        try:
            if self.headers.get('content-type', '').split(';')[0] != 'application/json':
                return self.send(415, {'error': 'json required'})
            data = json.loads(self.rfile.read(int(self.headers.get('content-length', '0'))))
            valid = isinstance(data, dict) and set(data) == {'id', 'name'} and type(data['id']) is int and 1 <= data['id'] <= 1000 and isinstance(data['name'], str) and 1 <= len(data['name']) <= 20 and re.fullmatch(r'[a-zA-Z0-9_-]+', data['name'])
            if not valid:
                if BUG:
                    return self.send(201, {'id': 1, 'name': 'accepted-invalid'})
                return self.send(422, {'error': 'invalid payload'})
            if query('SELECT id FROM items WHERE id=?', (data['id'],)):
                return self.send(409, {'error': 'duplicate id'})
            query('INSERT INTO items(id,name) VALUES(?,?)', (data['id'], data['name']))
            return self.send(201, data)
        except (ValueError, TypeError):
            return self.send(422, {'error': 'invalid payload'})


ThreadingHTTPServer(('127.0.0.1', int(os.environ['PORT'])), Handler).serve_forever()
