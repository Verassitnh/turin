#!/usr/bin/env python3
"""Fake `claude` CLI for the Turin app tests (not a paid provider).

Replays the NDJSON transcript next to it (`transcript.ndjson`, or FAKE_FIXTURE)
over the stream-json protocol. Copied from crates/provider-claude/tests/session_replay.rs;
only the paths differ: no environment variables are needed.
"""
import json, os, sys

# Text generation (`claude -p ... --json-schema`): one structured reply, no session.
if "-p" in sys.argv or "--print" in sys.argv:
    sys.stdin.read()
    print(json.dumps({"type": "result", "subtype": "success", "is_error": False,
                      "structured_output": {"title": os.environ.get("FAKE_TITLE", "Fixture title")}}))
    sys.exit(0)
# Each launch appends its argv, so tests can check flags such as resume-at.
HERE = os.path.dirname(os.path.abspath(__file__))
ARGV = os.environ.get("FAKE_ARGV", os.path.join(HERE, "argv.log"))
with open(ARGV, "a") as log:
    log.write(json.dumps(sys.argv) + "\n")
pending = [json.loads(line) for line in open(os.environ.get("FAKE_FIXTURE", os.path.join(HERE, "transcript.ndjson"))) if line.strip()]

# The transcript names the prompt uuid it expects; the CLI echoes the uuid it was sent.
uuid_map = {}

def emit(value):
    text = json.dumps(value)
    for expected, actual in uuid_map.items():
        text = text.replace(expected, actual)
    print(text, flush=True)

def wait_response(request_id):
    # The CLI blocks a turn on a permission callback until the host answers.
    for line in sys.stdin:
        if line.strip() and json.loads(line).get("type") == "control_response":
            if json.loads(line)["response"].get("request_id") == request_id:
                return

def drain():
    while pending and pending[0]["type"] != "expect_outbound":
        entry = pending.pop(0)
        if entry["type"] == "emit_inbound":
            emit(entry["frame"])
            if entry["frame"].get("type") == "control_request":
                wait_response(entry["frame"]["request_id"])
        elif entry["type"] == "runtime_exit":
            sys.exit(0)

def consume(prefix, message=None):
    if pending and pending[0]["type"] == "expect_outbound" and pending[0]["label"].startswith(prefix):
        expect = pending.pop(0)
        expected = ((expect.get("frame") or {}).get("message") or {}).get("uuid")
        if expected and message and message.get("uuid"):
            uuid_map[expected] = message["uuid"]
        drain()

while pending and not (pending[0]["type"] == "expect_outbound" and pending[0]["label"].startswith("query.open")):
    pending.pop(0)
if pending:
    pending.pop(0)

for line in sys.stdin:
    if not line.strip():
        continue
    message = json.loads(line)
    if message.get("type") == "user":
        consume("prompt.offer", message)
    elif message.get("type") == "control_request":
        request = message.get("request") or {}
        # Tests read the control requests the CLI received from this log.
        with open(ARGV + ".control", "a") as log:
            log.write(json.dumps(request) + "\n")
        # FAKE_SILENT names control subtypes the CLI never answers.
        if request.get("subtype") in os.environ.get("FAKE_SILENT", "").split(","):
            continue
        body = {"models": [], "commands": [], "account": {}} if request.get("subtype") == "initialize" else {}
        emit({"type": "control_response", "response": {"subtype": "success", "request_id": message["request_id"], "response": body}})
        # An interrupt or a task stop is followed by the frames the CLI sends for it.
        if request.get("subtype") in ("interrupt", "stop_task"):
            consume("query." + request["subtype"])
