"""Minimal ACP v1 agent over stdio for process-lifecycle tests.

The first argument selects when the process exits:
  normal            answer every prompt
  die_on_prompt     exit when a prompt arrives, without answering
  die_second_prompt answer the first prompt, exit on the second
  die_idle          exit shortly after creating the session
  permission_then_die  request permission, then exit before the answer
"""
import json
import sys
import time

MODE = sys.argv[1] if len(sys.argv) > 1 else "normal"
prompts = 0


def send(message):
    sys.stdout.write(json.dumps(message) + "\n")
    sys.stdout.flush()


for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    message = json.loads(line)
    method = message.get("method")
    request_id = message.get("id")
    if method == "initialize":
        send({"jsonrpc": "2.0", "id": request_id,
              "result": {"protocolVersion": 1, "agentCapabilities": {}, "authMethods": []}})
    elif method == "session/new":
        send({"jsonrpc": "2.0", "id": request_id, "result": {"sessionId": "s1"}})
        if MODE == "die_idle":
            time.sleep(0.3)
            sys.exit(0)
    elif method == "session/prompt":
        prompts += 1
        if MODE == "die_on_prompt" or (MODE == "die_second_prompt" and prompts == 2):
            sys.exit(1)
        if MODE == "permission_then_die":
            send({"jsonrpc": "2.0", "id": 900, "method": "session/request_permission",
                  "params": {"sessionId": "s1",
                             "toolCall": {"toolCallId": "t1", "title": "write file"},
                             "options": [{"optionId": "allow", "name": "Allow", "kind": "allow_once"},
                                         {"optionId": "reject", "name": "Reject", "kind": "reject_once"}]}})
            sys.exit(1)
        send({"jsonrpc": "2.0", "method": "session/update",
              "params": {"sessionId": "s1",
                         "update": {"sessionUpdate": "agent_message_chunk",
                                    "content": {"type": "text", "text": "hello"}}}})
        send({"jsonrpc": "2.0", "id": request_id, "result": {"stopReason": "end_turn"}})
    elif method == "session/close":
        send({"jsonrpc": "2.0", "id": request_id, "result": {}})
    elif request_id is not None and method is not None:
        send({"jsonrpc": "2.0", "id": request_id,
              "error": {"code": -32601, "message": "method not found"}})
