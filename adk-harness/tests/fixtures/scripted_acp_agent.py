"""A scripted ACP v1 coding agent over stdio for the harness team tests.

On each prompt it asks permission to read `src/lib.rs`, then to edit the file named by
the first argument (default `REVIEW.md`, relative to the working directory). It writes the
file only when the client selects an allow option, appends one line to `acp-edits.log` per
write, and answers with what happened. Paths are relative to the session's `cwd`.
"""
import json
import os
import sys

TARGET = sys.argv[1] if len(sys.argv) > 1 else "REVIEW.md"
next_id = 900


def send(message):
    sys.stdout.write(json.dumps(message) + "\n")
    sys.stdout.flush()


def read_message():
    while True:
        line = sys.stdin.readline()
        if not line:
            sys.exit(0)
        line = line.strip()
        if line:
            return json.loads(line)


def ask(kind, title, raw_input):
    global next_id
    next_id += 1
    request_id = next_id
    send({"jsonrpc": "2.0", "id": request_id, "method": "session/request_permission",
          "params": {"sessionId": "s1",
                     "toolCall": {"toolCallId": f"t{request_id}", "title": title, "kind": kind,
                                  "rawInput": raw_input},
                     "options": [{"optionId": "allow", "name": "Allow", "kind": "allow_once"},
                                 {"optionId": "reject", "name": "Reject", "kind": "reject_once"}]}})
    while True:
        message = read_message()
        if message.get("id") == request_id and "method" not in message:
            outcome = (message.get("result") or {}).get("outcome") or {}
            return outcome.get("outcome") == "selected" and outcome.get("optionId") == "allow"
        handle(message)


def handle(message):
    method = message.get("method")
    request_id = message.get("id")
    if method == "initialize":
        send({"jsonrpc": "2.0", "id": request_id,
              "result": {"protocolVersion": 1, "agentCapabilities": {}, "authMethods": []}})
    elif method == "session/new":
        os.chdir((message.get("params") or {}).get("cwd") or ".")
        send({"jsonrpc": "2.0", "id": request_id, "result": {"sessionId": "s1"}})
    elif method == "session/prompt":
        read_ok = ask("read", "Read src/lib.rs", {"path": "src/lib.rs"})
        edit_ok = ask("edit", f"Write {TARGET}", {"path": TARGET, "content": "reviewed"})
        if edit_ok:
            with open(TARGET, "w") as handle_file:
                handle_file.write("reviewed\n")
            with open("acp-edits.log", "a") as log:
                log.write(TARGET + "\n")
        text = f"read: {'allowed' if read_ok else 'denied'}; edit: {'written' if edit_ok else 'denied'}"
        send({"jsonrpc": "2.0", "method": "session/update",
              "params": {"sessionId": "s1",
                         "update": {"sessionUpdate": "agent_message_chunk",
                                    "content": {"type": "text", "text": text}}}})
        send({"jsonrpc": "2.0", "id": request_id, "result": {"stopReason": "end_turn"}})
    elif method == "session/close":
        send({"jsonrpc": "2.0", "id": request_id, "result": {}})
    elif request_id is not None and method is not None:
        send({"jsonrpc": "2.0", "id": request_id,
              "error": {"code": -32601, "message": "method not found"}})


while True:
    handle(read_message())
