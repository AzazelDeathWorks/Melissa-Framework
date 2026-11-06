"""Owned synthetic full-stack fixture: real HTML/JS, HTTP API, SQLite, SW and HMR."""
import base64
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
import sqlite3
import struct
import threading
import time
from urllib.parse import urlparse, parse_qs

BAD_A11Y = False
BACKGROUND = '#ffffff'
VERSION = 1
SOCKETS = []
LOCK = threading.Lock()

HTML = '''<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width"><title>Synthetic notes</title>
<style>body{font:16px Arial,sans-serif;margin:24px;color:#111;background:BACKGROUND}main{max-width:700px}input,button{font:inherit;padding:8px;margin:4px}form{display:flex;flex-wrap:wrap}label{display:block}ul{padding:16px}h1{font-size:28px}</style></head><body><main><h1>Synthetic notes</h1>
<form id="form"><label for="name">Note name</label><input id="name" required><button id="save">Save note</button></form>
<p id="loading" role="status" hidden>Loading</p><p id="empty" hidden>No notes yet</p><ul id="items" aria-label="Saved notes"></ul>
<button id="slow">Refresh slowly</button><button id="fail">Show error</button><p id="error" role="alert" hidden></p>
<p id="version">module-v1</p><button id="hmr">Update module</button><p id="swresult">not requested</p></main>
<script>
const $=id=>document.getElementById(id);
async function load(suffix=''){$('loading').hidden=false;$('error').hidden=true;try{const r=await fetch('/items'+suffix);if(!r.ok)throw Error('Request failed');const items=await r.json();$('items').replaceChildren(...items.map(item=>{const li=document.createElement('li');li.className='row';li.textContent=item.name;return li;}));$('empty').hidden=items.length>0;}catch(e){$('error').textContent='Could not load notes';$('error').hidden=false;}finally{$('loading').hidden=true;}}
$('form').onsubmit=async e=>{e.preventDefault();await fetch('/items',{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify({name:$('name').value})});await load();};
$('slow').onclick=()=>load('?delay=1');$('fail').onclick=()=>load('?fail=1');load();
if(location.pathname==='/capabilities'){
 $('hmr').disabled=true;const socket=new WebSocket('ws://'+location.host+'/live');socket.onopen=()=>{$('hmr').disabled=false;};socket.onmessage=async event=>{const update=JSON.parse(event.data);const module=await import('/module.js?v='+update.version);module.apply();};
 $('hmr').onclick=()=>fetch('/hmr',{method:'POST'});
 navigator.serviceWorker.register('/sw.js').then(async()=>{await navigator.serviceWorker.ready;if(!navigator.serviceWorker.controller)await new Promise(resolve=>navigator.serviceWorker.addEventListener('controllerchange',resolve,{once:true}));$('swresult').textContent=await(await fetch('/sw-probe')).text();}).catch(()=>{$('swresult').textContent='service-worker-blocked';});
}
</script></body></html>'''

def connection():
    return sqlite3.connect(os.environ['MELISSA_FIXTURE_DB'], timeout=5)


class Handler(BaseHTTPRequestHandler):
    def send(self, status, body=b'', content_type='application/json'):
        self.send_response(status)
        self.send_header('content-type', content_type)
        self.send_header('content-length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        u = urlparse(self.path)
        q = parse_qs(u.query)
        if u.path == '/health': return self.send(200, b'{"ready":true}')
        if u.path == '/items':
            if 'delay' in q: time.sleep(2)
            if 'fail' in q: return self.send(500, b'{"error":"synthetic-failure"}')
            with connection() as db: items=[{'name':row[0]} for row in db.execute('SELECT name FROM items ORDER BY id')]
            return self.send(200,json.dumps(items).encode())
        if u.path in ('/', '/capabilities'):
            if 'reset' in q:
                with connection() as db: db.execute('DELETE FROM items')
            html=HTML.replace('BACKGROUND',BACKGROUND)
            if BAD_A11Y: html=html.replace('<label for="name">Note name</label>','<div>Note name</div>')
            return self.send(200,html.encode(),'text/html; charset=utf-8')
        if u.path=='/module.js':
            return self.send(200,("export function apply(){document.getElementById('version').textContent='module-v%d'}"%VERSION).encode(),'text/javascript')
        if u.path=='/sw.js':
            return self.send(200,b"self.addEventListener('install',()=>self.skipWaiting());self.addEventListener('activate',e=>e.waitUntil(self.clients.claim()));self.addEventListener('fetch',e=>{if(new URL(e.request.url).pathname==='/sw-probe')e.respondWith(new Response('service-worker-live'));});",'text/javascript')
        if u.path=='/sw-probe': return self.send(404,b'network-not-worker','text/plain')
        if u.path=='/live' and self.headers.get('upgrade','').lower()=='websocket':
            key=base64.b64encode(hashlib.sha1((self.headers['sec-websocket-key']+'258EAFA5-E914-47DA-95CA-C5AB0DC85B11').encode()).digest()).decode()
            self.send_response(101);self.send_header('Upgrade','websocket');self.send_header('Connection','Upgrade');self.send_header('Sec-WebSocket-Accept',key);self.end_headers()
            with LOCK: SOCKETS.append(self.connection)
            try:
                while self.connection.recv(1024): pass
            except OSError: pass
            finally:
                with LOCK:
                    if self.connection in SOCKETS: SOCKETS.remove(self.connection)
            return
        if u.path=='/favicon.ico':return self.send(204)
        self.send(404,b'{"error":"missing"}')

    def do_POST(self):
        global VERSION
        raw=self.rfile.read(int(self.headers.get('content-length','0')))
        if self.path=='/items':
            value=json.loads(raw)
            if not isinstance(value.get('name'),str) or not value['name']:return self.send(422,b'{"error":"name"}')
            with connection() as db:db.execute('INSERT INTO items(name) VALUES(?)',(value['name'],))
            return self.send(201,b'{"saved":true}')
        if self.path=='/hmr':
            VERSION+=1;data=json.dumps({'version':VERSION}).encode();frame=bytes([0x81,len(data)])+data
            with LOCK:
                for sock in SOCKETS:
                    try:sock.sendall(frame)
                    except OSError:pass
            return self.send(200,b'{"updated":true}')
        self.send(404)

ThreadingHTTPServer(('127.0.0.1',int(os.environ['PORT'])),Handler).serve_forever()
