#!/usr/bin/env python3
"""Fake `opencode` for the Turin app tests (not a paid provider).

Launched the way the provider launches the real binary:

  opencode --version                    prints a v1 version and exits
  opencode serve --hostname=H --port=P  an HTTP + SSE server speaking the v1 API
                                        the provider uses (/global/health,
                                        /session, /session/:id/prompt_async,
                                        /session/:id/abort, /permission/:id/reply,
                                        GET /event), after the readiness line
                                        "opencode server listening on <url>"

The prompt text picks the behaviour:

  "hang"          the session goes busy and never finishes by itself; abort
                  makes it idle
  "Run pwd"       raises permission.asked, waits for /permission/:id/reply, then
                  streams a reply naming the answer
  anything else   streams "Respond with exactly: X" as X, then goes idle

argv goes to FAKE_ARGV (default argv.log next to the script); every request
line to FAKE_ARGV.rpc.
"""
import json, os, queue, sys, threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

HERE = os.path.dirname(os.path.abspath(__file__))
LOG = os.environ.get("FAKE_ARGV", os.path.join(HERE, "argv.log"))
with open(LOG, "a") as log:
    log.write(json.dumps(sys.argv) + "\n")

VERSION = "1.19.0"
if "--version" in sys.argv:
    print(VERSION)
    sys.exit(0)
if "serve" not in sys.argv:
    sys.exit(2)


def arg(name, default):
    for item in sys.argv:
        if item.startswith(f"--{name}="):
            return item.split("=", 1)[1]
    return default


SESSION = "ses_fake"
lock = threading.Lock()
subscribers = []        # one queue per open GET /event
hang = threading.Event()  # a hung turn is waiting for abort
permission = {"open": None, "reply": threading.Event(), "answer": None}


def publish(frame):
    with lock:
        for q in subscribers:
            q.put(frame)


def info(message_id, role, completed=False):
    value = {"id": message_id, "sessionID": SESSION, "role": role}
    if completed:
        value["time"] = {"created": 1760500000000, "completed": 1760500001000}
    return value


def event(kind, **properties):
    publish({"type": kind, "properties": properties})


def status(kind):
    event("session.status", sessionID=SESSION, status={"type": kind})


def run_turn(text):
    status("busy")
    event("message.updated", info=info("msg_user", "user"))
    event("message.part.updated", part={"id": "p_user", "sessionID": SESSION, "messageID": "msg_user",
                                        "type": "text", "text": text})
    event("message.updated", info=info("msg_asst", "assistant"))
    if "hang" in text.lower():
        event("message.part.updated", part={"id": "p_hang", "sessionID": SESSION, "messageID": "msg_asst",
                                            "type": "text", "text": "working", "time": {"start": 1760500000000}})
        hang.set()
        return
    if "Run pwd" in text:
        event("permission.asked", id="perm_fake", sessionID=SESSION, permission="bash", patterns=["pwd"])
        permission["reply"].wait(30)
        answer = permission["answer"] or "none"
        event("permission.replied", requestID="perm_fake", sessionID=SESSION, reply=answer)
        reply = f"permission {answer}"
    else:
        marker = "Respond with exactly:"
        reply = text.split(marker, 1)[1].strip() if marker in text else "fake opencode reply"
    half = max(1, len(reply) // 2)
    event("message.part.updated", part={"id": "p1", "sessionID": SESSION, "messageID": "msg_asst",
                                        "type": "text", "text": reply[:half], "time": {"start": 1760500000000}})
    event("message.part.delta", sessionID=SESSION, messageID="msg_asst", partID="p1", field="text",
          delta=reply[half:])
    event("message.updated", info=info("msg_asst", "assistant", completed=True))
    status("idle")


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *_):
        pass

    def body(self):
        length = int(self.headers.get("content-length") or 0)
        return json.loads(self.rfile.read(length) or b"null") if length else None

    def reply(self, status_code, value=None):
        data = b"" if value is None else json.dumps(value).encode()
        self.send_response(status_code)
        if value is not None:
            self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def record(self, body):
        with open(LOG + ".rpc", "a") as log:
            log.write(json.dumps({"method": self.command, "path": self.path, "body": body}) + "\n")

    def do_GET(self):
        path = self.path.split("?")[0]
        self.record(None)
        if path == "/global/health":
            self.reply(200, {"healthy": True, "version": VERSION})
        elif path == "/event":
            q = queue.Queue()
            with lock:
                subscribers.append(q)
            self.send_response(200)
            self.send_header("content-type", "text/event-stream")
            self.send_header("cache-control", "no-cache")
            self.end_headers()
            try:
                self.wfile.write(b'data: {"type":"server.connected","properties":{}}\n\n')
                self.wfile.flush()
                while True:
                    frame = q.get()
                    self.wfile.write(f"data: {json.dumps(frame)}\n\n".encode())
                    self.wfile.flush()
            except OSError:
                pass
            finally:
                with lock:
                    subscribers.remove(q)
            self.close_connection = True
        elif path == "/session/status":
            self.reply(200, {})
        elif path in ("/command", "/agent", "/skill", "/session"):
            self.reply(200, [])
        elif path == "/provider":
            self.reply(200, {"all": [], "default": {}, "connected": []})
        else:
            self.reply(200, {})

    def do_POST(self):
        path = self.path.split("?")[0]
        body = self.body()
        self.record(body)
        if path == "/session":
            self.reply(200, {"id": SESSION})
        elif path.endswith("/prompt_async"):
            text = "".join(part.get("text", "") for part in (body or {}).get("parts", []) if part.get("type") == "text")
            self.reply(204)
            threading.Thread(target=run_turn, args=(text,), daemon=True).start()
        elif path.endswith("/abort"):
            self.reply(200, True)
            if hang.is_set():
                hang.clear()
                event("session.error", sessionID=SESSION,
                      error={"name": "MessageAbortedError", "data": {"message": "The operation was aborted."}})
                status("idle")
        elif path.startswith("/permission/") and path.endswith("/reply"):
            permission["answer"] = (body or {}).get("reply")
            permission["reply"].set()
            self.reply(200, True)
        else:
            self.reply(200, {})


server = ThreadingHTTPServer((arg("hostname", "127.0.0.1"), int(arg("port", "0"))), Handler)
server.daemon_threads = True
host, port = server.server_address[:2]
print(f"opencode server listening on http://{host}:{port}", flush=True)
server.serve_forever()
