#!/usr/bin/env python3
"""Drill-only webhook receiver (Part 2 S5-monitor): stands in for the chat / SMS / dead-man
webhooks in CI. POST bodies (Alertmanager webhook JSON) are appended to /data/received.jsonl;
GET /received returns them as a JSON list. Never deployed on a college server."""
import json
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

LOG = "/data/received.jsonl"


class H(BaseHTTPRequestHandler):
    def do_POST(self):
        body = self.rfile.read(int(self.headers.get("content-length", 0)))
        try:
            doc = json.loads(body)
        except ValueError:
            doc = {"raw": body.decode("utf-8", "replace")}
        with open(LOG, "a") as f:
            f.write(json.dumps({"path": self.path, "body": doc}) + "\n")
        self.send_response(200)
        self.end_headers()

    def do_GET(self):
        try:
            with open(LOG) as f:
                items = [json.loads(line) for line in f if line.strip()]
        except FileNotFoundError:
            items = []
        out = json.dumps(items).encode()
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.end_headers()
        self.wfile.write(out)

    def log_message(self, *a):
        pass


ThreadingHTTPServer(("0.0.0.0", 8000), H).serve_forever()
