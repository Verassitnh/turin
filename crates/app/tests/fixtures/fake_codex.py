#!/usr/bin/env python3
"""Fake `codex app-server` for the Turin app tests (not a paid provider).

Speaks the Codex app-server JSON-RPC dialect over stdio (one JSON object per
line, no "jsonrpc" member), with frame shapes copied from upstream's recorded
fixtures (multi_turn, turn_interrupt, tool_call_read_only_on_request). The
prompt text picks the behaviour:

  "hang"          starts a turn that never ends by itself; `turn/interrupt`
                  answers `{}` and completes the turn as "interrupted"
  "Run pwd"       raises `item/commandExecution/requestApproval`, waits for the
                  decision, then reports it in the reply
  anything else   streams "Respond with exactly: X" as X and completes the turn

Every inbound frame is appended to FAKE_ARGV.rpc (default argv.log.rpc next to
the script); argv goes to FAKE_ARGV.
"""
import json, os, sys

HERE = os.path.dirname(os.path.abspath(__file__))
LOG = os.environ.get("FAKE_ARGV", os.path.join(HERE, "argv.log"))
with open(LOG, "a") as log:
    log.write(json.dumps(sys.argv) + "\n")

# `codex --version` style probes: answer and exit.
if "--version" in sys.argv or "-V" in sys.argv:
    print("codex-cli 0.156.1")
    sys.exit(0)


def send(frame):
    sys.stdout.write(json.dumps(frame) + "\n")
    sys.stdout.flush()


def respond(request_id, result):
    send({"id": request_id, "result": result})


def notify(method, params):
    send({"method": method, "params": params})


THREAD = "01a0d5ed-187d-70c2-b4b6-b6881a68de09"
CWD = os.getcwd()
TURN = "01a0d5ed-1957-7a22-8399-d0172af342fd"
APPROVAL_ID = 0

thread = {
    "id": THREAD, "environments": [{"environmentId": "local", "cwd": CWD, "runtimeWorkspaceRoots": [CWD]}],
    "extra": None, "sessionId": THREAD, "forkedFromId": None, "parentThreadId": None, "preview": "",
    "ephemeral": False, "section": None, "sectionEnteredAt": None, "projectId": None,
    "historyMode": "paginated", "modelProvider": "openai", "model": "gpt-6-luna", "reasoningEffort": None,
    "createdAt": 1790295480, "updatedAt": 1790295480, "recencyAt": 1790295480, "status": {"type": "idle"},
    "path": CWD + "/rollout.jsonl", "cwd": CWD, "cliVersion": "0.156.1", "originator": "t3code_desktop",
    "source": "vscode", "canAcceptDirectInput": True, "threadSource": None, "agentNickname": None,
    "agentRole": None, "gitInfo": None, "name": None, "daybreakEnabled": None, "turns": [],
}


def turn(status, items=None):
    return {"id": TURN, "items": items or [], "itemsView": "summary" if items else "notLoaded",
            "status": status, "error": None,
            "startedAt": 1790295480, "completedAt": None if status == "inProgress" else 1790295485,
            "durationMs": None if status == "inProgress" else 10}


def agent_item(text, item_id="msg_fake"):
    return {"type": "agentMessage", "id": item_id, "text": text, "phase": "final_answer",
            "memoryCitation": None, "delivery": None, "questions": None}


def ids(**extra):
    return {"threadId": THREAD, "turnId": TURN, **extra}


def start_turn(text):
    notify("turn/started", {"threadId": THREAD, "turn": turn("inProgress")})
    user = {"type": "userMessage", "id": "user-fake", "clientId": None,
            "content": [{"type": "text", "text": text, "text_elements": []}]}
    notify("item/started", ids(item=user, startedAtMs=1790295480000))
    notify("item/completed", ids(item=user, completedAtMs=1790295480001))


def reply(text):
    notify("item/started", ids(item=agent_item(""), startedAtMs=1790295481000))
    half = max(1, len(text) // 2)
    for part in (text[:half], text[half:]):
        notify("item/agentMessage/delta", ids(itemId="msg_fake", delta=part))
    notify("item/completed", ids(item=agent_item(text), completedAtMs=1790295482000))
    notify("turn/completed", {"threadId": THREAD, "turn": turn("completed", [agent_item(text)])})


def command_item(status):
    return {"type": "commandExecution", "id": "exec-fake", "pluginId": None, "scriptPath": None,
            "command": "pwd", "cwd": CWD, "processId": None, "source": "agent", "status": status,
            "commandActions": [{"type": "unknown", "command": "pwd"}], "aggregatedOutput": None,
            "exitCode": None if status == "inProgress" else 0, "durationMs": None if status == "inProgress" else 0}


mode = None  # "hang" | "approval" | None

for line in sys.stdin:
    if not line.strip():
        continue
    frame = json.loads(line)
    with open(LOG + ".rpc", "a") as log:
        log.write(json.dumps(frame) + "\n")
    method = frame.get("method")
    request_id = frame.get("id")

    if method is None:
        # The decision for the approval this agent raised.
        if request_id == APPROVAL_ID and mode == "approval":
            decision = (frame.get("result") or {}).get("decision", "none")
            notify("serverRequest/resolved", {"threadId": THREAD, "requestId": APPROVAL_ID})
            notify("item/completed", ids(item=command_item("completed" if decision == "accept" else "declined"),
                                          completedAtMs=1790295481500))
            reply(f"approval {decision}")
            mode = None
        continue

    if method == "initialize":
        respond(request_id, {"userAgent": "fake-codex/0.156.1", "codexHome": os.path.join(CWD, ".codex"),
                             "platformFamily": "unix", "platformOs": "macos"})
    elif method == "initialized":
        pass
    elif method == "thread/start":
        respond(request_id, {"thread": thread, "model": "gpt-6-luna", "modelProvider": "openai", "serviceTier": None,
                             "disabledPluginIds": [], "cwd": CWD, "runtimeWorkspaceRoots": [CWD],
                             "instructionSources": [], "approvalPolicy": "on-request", "approvalsReviewer": "user",
                             "sandbox": {"type": "workspaceWrite", "writableRoots": [], "networkAccess": False,
                                         "excludeTmpdirEnvVar": False, "excludeSlashTmp": False},
                             "activePermissionProfile": {"id": ":workspace", "extends": None},
                             "reasoningEffort": None, "multiAgentMode": "explicitRequestOnly"})
    elif method == "turn/start":
        text = "".join(part.get("text", "") for part in (frame.get("params") or {}).get("input", []))
        respond(request_id, {"turn": turn("inProgress")})
        start_turn(text)
        if "hang" in text.lower():
            mode = "hang"
        elif "Run pwd" in text:
            mode = "approval"
            notify("item/started", ids(item=command_item("inProgress"), startedAtMs=1790295481000))
            send({"method": "item/commandExecution/requestApproval", "id": APPROVAL_ID,
                  "params": ids(kind="command", itemId="exec-fake", startedAtMs=1790295481000, environmentId="local",
                                reason="May I run pwd?", command="pwd", cwd=CWD,
                                commandActions=[{"type": "unknown", "command": "pwd"}],
                                availableDecisions=["accept", "acceptForSession", "decline", "cancel"])})
        else:
            marker = "Respond with exactly:"
            reply(text.split(marker, 1)[1].strip() if marker in text else "fake codex reply")
    elif method == "turn/interrupt":
        respond(request_id, {})
        if mode == "hang":
            notify("turn/completed", {"threadId": THREAD, "turn": turn("interrupted")})
            mode = None
    elif request_id is not None:
        respond(request_id, {})
