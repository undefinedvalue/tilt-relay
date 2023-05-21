#!/usr/bin/env python3

from http.server import BaseHTTPRequestHandler, HTTPServer

class TestHandler(BaseHTTPRequestHandler):
    def do_POST(self):
        print(self.requestline)
        print(self.headers)
        print(self.rfile.read(int(self.headers.get('Content-Length'))).decode('utf-8'))
        self.protocol_version = "HTTP/1.1"
        self.send_response(200)
        self.send_header('Content-type','text/html')
        self.end_headers()

        message = '{ "result": "success" }'
        self.wfile.write(bytes(message, "utf8"))

with HTTPServer(('', 8000), TestHandler) as server:
    server.serve_forever()
