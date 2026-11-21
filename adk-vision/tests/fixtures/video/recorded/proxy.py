#!/usr/bin/env python3
"""A recording proxy for video calls: forwards each GET and POST to UPSTREAM and saves the
exchange as <out>/<n>.json -- the method, the path, the request body with every video or
image reduced to its SHA-256 and size, and the response status, content type, upload URL and
body. Request headers are never written, and the proxy refuses to save a record that contains
a credential it was sent. A resumable-upload URL in a response is rewritten to come back
through the proxy, so the upload is recorded too.

    python3 -I proxy.py <port> <upstream-base-url> <out-dir>

Based on examples/vision/fixtures/recorded/proxy.py, which records JSON POSTs only.
"""

import base64
import hashlib
import http.server
import json
import re
import sys
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

PORT, UPSTREAM, OUT = int(sys.argv[1]), sys.argv[2].rstrip("/"), Path(sys.argv[3])
OUT.mkdir(parents=True, exist_ok=True)
DROP = {"host", "content-length", "connection", "accept-encoding"}
SECRET_HEADERS = {"authorization", "x-api-key", "x-goog-api-key"}
UPSTREAM_HOST = urllib.parse.urlsplit(UPSTREAM).netloc


def digest(data, mime):
    return f"{{{{media:{mime}:sha256:{hashlib.sha256(data).hexdigest()}:{len(data)}}}}}"


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
        return digest(data, m.group(1) if m else "base64")
    return value


def saved_body(body, content_type):
    if not body:
        return None
    if content_type.startswith("multipart/form-data"):
        boundary = content_type.split("boundary=", 1)[1].strip('"').encode()
        fields = {}
        for part in body.split(b"--" + boundary)[1:-1]:
            head, _, data = part.partition(b"\r\n\r\n")
            data = data[:-2] if data.endswith(b"\r\n") else data
            name = re.search(rb'name="([^"]+)"', head).group(1).decode()
            if b"filename=" in head:
                mime = re.search(rb"Content-Type: ([^\r\n]+)", head, re.I)
                fields[name] = digest(data, mime.group(1).decode() if mime else "binary")
            else:
                fields[name] = data.decode("utf-8", "replace")
        return {"multipart": fields}
    try:
        return scrub(json.loads(body))
    except ValueError:
        return digest(body, content_type or "binary")


class Handler(http.server.BaseHTTPRequestHandler):
    def forward(self, method):
        n = len(list(OUT.glob("*.json")))
        body = self.rfile.read(int(self.headers.get("content-length", 0))) if method == "POST" else None
        headers = {k: v for k, v in self.headers.items() if k.lower() not in DROP}
        target = self.path if self.path.startswith("http") else UPSTREAM + self.path
        request = urllib.request.Request(target, data=body, headers=headers, method=method)
        try:
            with urllib.request.urlopen(request, timeout=600) as response:
                status, response_headers, raw = response.status, response.headers, response.read()
        except urllib.error.HTTPError as error:
            status, response_headers, raw = error.code, error.headers, error.read()
        upload = response_headers.get("x-goog-upload-url")
        if upload:
            parts = urllib.parse.urlsplit(upload)
            upload = f"http://127.0.0.1:{PORT}{parts.path}?{parts.query}"
        text = raw.decode("utf-8", "replace")
        try:
            saved_response = json.loads(text)
        except ValueError:
            saved_response = text
        path = re.sub(r"upload_id=[^&]+", "upload_id=<upload>", self.path)
        record = {"method": method, "path": path,
                  "request": saved_body(body, self.headers.get("content-type", "")),
                  "status": status, "contentType": response_headers.get("content-type"),
                  "uploadUrl": re.sub(r"upload_id=[^&]+", "upload_id=<upload>", upload) if upload else None,
                  "response": saved_response}
        serialised = json.dumps(record, indent=2)
        for name, value in self.headers.items():
            if name.lower() in SECRET_HEADERS:
                assert value.split(" ")[-1] not in serialised, "a credential would be saved"
        (OUT / f"{n:03d}.json").write_text(serialised + "\n")
        self.send_response(status)
        for name in ("content-type", "retry-after", "request-id", "x-goog-upload-status"):
            if response_headers.get(name):
                self.send_header(name, response_headers.get(name))
        if upload:
            self.send_header("x-goog-upload-url", upload)
        self.send_header("content-length", str(len(raw)))
        self.end_headers()
        self.wfile.write(raw)

    def do_GET(self):
        self.forward("GET")

    def do_POST(self):
        self.forward("POST")

    def log_message(self, *args):
        pass


http.server.ThreadingHTTPServer(("127.0.0.1", PORT), Handler).serve_forever()
