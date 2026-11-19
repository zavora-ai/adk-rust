#!/usr/bin/env python3
"""A recording proxy: forwards each POST to UPSTREAM and saves the exchange as
<out>/<n>.json — the request path, the request body with every inline image replaced by
its SHA-256 and size, and the response status, content type and body. Request headers are
never written, and the proxy refuses to save a record that contains a credential it was
sent.

    python3 proxy.py <port> <upstream-base-url> <out-dir>
"""

import base64
import hashlib
import http.server
import json
import re
import sys
import urllib.error
import urllib.request
from pathlib import Path

PORT, UPSTREAM, OUT = int(sys.argv[1]), sys.argv[2].rstrip("/"), Path(sys.argv[3])
OUT.mkdir(parents=True, exist_ok=True)
DROP = {"host", "content-length", "connection", "accept-encoding"}
SECRET_HEADERS = {"authorization", "x-api-key", "x-goog-api-key"}


def scrub(value):
    if isinstance(value, dict):
        return {k: scrub(v) for k, v in value.items()}
    if isinstance(value, list):
        return [scrub(v) for v in value]
    if isinstance(value, str) and len(value) > 512:
        m = re.match(r"^data:([^;]+);base64,(.*)$", value, re.S)
        raw = m.group(2) if m else value
        try:
            data = base64.b64decode(raw, validate=True)
        except Exception:
            return value
        mime = m.group(1) if m else "base64"
        return f"{{{{image:{mime}:sha256:{hashlib.sha256(data).hexdigest()}:{len(data)}}}}}"
    return value


class Handler(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        n = len(list(OUT.glob("*.json")))
        body = self.rfile.read(int(self.headers.get("content-length", 0)))
        headers = {k: v for k, v in self.headers.items() if k.lower() not in DROP}
        request = urllib.request.Request(UPSTREAM + self.path, data=body, headers=headers, method="POST")
        try:
            with urllib.request.urlopen(request, timeout=300) as response:
                status, response_headers, raw = response.status, response.headers, response.read()
        except urllib.error.HTTPError as error:
            status, response_headers, raw = error.code, error.headers, error.read()
        try:
            saved_request = scrub(json.loads(body))
        except ValueError:
            saved_request = None
        text = raw.decode("utf-8", "replace")
        try:
            saved_response = json.loads(text)
        except ValueError:
            saved_response = text
        record = {"path": self.path, "request": saved_request, "status": status,
                  "contentType": response_headers.get("content-type"), "response": saved_response}
        serialised = json.dumps(record, indent=2)
        for name, value in self.headers.items():
            if name.lower() in SECRET_HEADERS:
                assert value.split(" ")[-1] not in serialised, "a credential would be saved"
        (OUT / f"{n:03d}.json").write_text(serialised + "\n")
        self.send_response(status)
        for name in ("content-type", "retry-after", "request-id"):
            if response_headers.get(name):
                self.send_header(name, response_headers.get(name))
        self.send_header("content-length", str(len(raw)))
        self.end_headers()
        self.wfile.write(raw)

    def log_message(self, *args):
        pass


http.server.ThreadingHTTPServer(("127.0.0.1", PORT), Handler).serve_forever()
