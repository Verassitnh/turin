#!/usr/bin/env python3
"""Fake `cursor-agent acp` for the Turin app tests (not a paid provider).

Speaks the ACP JSON-RPC protocol over stdio, one JSON object per line, the way
crates/provider-cursor and crates/acp expect it (initialize, session/new,
cursor/list_available_models, session/set_mode, session/set_config_option,
session/prompt with streamed session/update notifications, session/cancel,
session/request_permission). The prompt text picks the behaviour:

  "hang"          streams one chunk, then waits for session/cancel and ends the
                  prompt with stopReason "cancelled" (the Stop test)
  "Run pwd"       asks session/request_permission, waits for the answer, then
                  streams a reply naming the chosen option (the approval test)
  anything else   streams "Respond with exactly: X" as X (else a fixed reply)
                  in two chunks and ends the turn
"""
import json, os, sys

HERE = os.path.dirname(os.path.abspath(__file__))
# Each launch appends its argv and every inbound frame, so tests can inspect the wire.
LOG = os.environ.get("FAKE_ARGV", os.path.join(HERE, "argv.log"))
with open(LOG, "a") as log:
    log.write(json.dumps(sys.argv) + "\n")


def record(frame):
    with open(LOG + ".rpc", "a") as log:
        log.write(json.dumps(frame) + "\n")


def send(frame):
    sys.stdout.write(json.dumps(frame) + "\n")
    sys.stdout.flush()


def respond(request_id, result):
    send({"jsonrpc": "2.0", "id": request_id, "result": result})


def update(session_id, body):
    send({"jsonrpc": "2.0", "method": "session/update",
          "params": {"sessionId": session_id, "update": body}})


def chunk(session_id, message_id, text):
    update(session_id, {"sessionUpdate": "agent_message_chunk", "messageId": message_id,
                        "content": {"type": "text", "text": text}})


SESSION = "fake-cursor-session"
PERMISSION_REQUEST_ID = 9001
prompt_id = None      # the open session/prompt request, if any
mode = None           # "hang" | "permission" | None


def prompt_text(params):
    return "".join(part.get("text", "") for part in params.get("prompt", []) if part.get("type") == "text")


def reply_for(text):
    marker = "Respond with exactly:"
    if marker in text:
        return text.split(marker, 1)[1].strip()
    return "fake cursor reply"


for line in sys.stdin:
    if not line.strip():
        continue
    frame = json.loads(line)
    record(frame)
    method = frame.get("method")
    request_id = frame.get("id")

    if method is None:
        # A response to a request this agent sent (the permission answer).
        if request_id == PERMISSION_REQUEST_ID and prompt_id is not None:
            outcome = (frame.get("result") or {}).get("outcome") or {}
            chosen = outcome.get("optionId") or outcome.get("outcome") or "none"
            update(SESSION, {"sessionUpdate": "tool_call_update", "toolCallId": "call-pwd",
                             "status": "completed" if chosen == "once" else "failed"})
            chunk(SESSION, "m-perm", f"permission {chosen}")
            respond(prompt_id, {"stopReason": "end_turn"})
            prompt_id, mode = None, None
        continue

    if method == "initialize":
        respond(request_id, {
            "protocolVersion": 1,
            "agentCapabilities": {"loadSession": True, "promptCapabilities": {"image": {}}},
            "authMethods": [{"id": "cursor_login"}],
        })
    elif method == "session/new":
        respond(request_id, {"sessionId": SESSION})
    elif method == "cursor/list_available_models":
        respond(request_id, {"models": []})
    elif method in ("session/set_mode", "session/set_config_option", "session/set_model"):
        respond(request_id, {})
    elif method == "session/prompt":
        text = prompt_text(frame.get("params") or {})
        prompt_id = request_id
        if "hang" in text.lower():
            mode = "hang"
            chunk(SESSION, "m-hang", "working")
        elif "Run pwd" in text:
            mode = "permission"
            update(SESSION, {"sessionUpdate": "tool_call", "toolCallId": "call-pwd", "title": "Run pwd",
                             "kind": "execute", "status": "pending", "rawInput": {"command": "pwd"}})
            send({"jsonrpc": "2.0", "id": PERMISSION_REQUEST_ID, "method": "session/request_permission",
                  "params": {"sessionId": SESSION,
                             "toolCall": {"toolCallId": "call-pwd", "title": "Run pwd", "kind": "execute",
                                          "rawInput": {"command": "pwd"}},
                             "options": [
                                 {"optionId": "once", "name": "Allow once", "kind": "allow_once"},
                                 {"optionId": "no", "name": "Reject", "kind": "reject_once"}]}})
        else:
            reply = reply_for(text)
            half = max(1, len(reply) // 2)
            chunk(SESSION, "m1", reply[:half])
            chunk(SESSION, "m1", reply[half:])
            respond(request_id, {"stopReason": "end_turn"})
            prompt_id = None
    elif method == "session/cancel":
        if prompt_id is not None and mode == "hang":
            respond(prompt_id, {"stopReason": "cancelled"})
            prompt_id, mode = None, None
    elif request_id is not None:
        respond(request_id, {})
