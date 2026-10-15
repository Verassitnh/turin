#!/usr/bin/env python3
"""Refresh the checked-in Codex protocol inputs at the exact upstream pin."""
import json
import pathlib
import urllib.request

REF = "687a119f0fcaace47e1f1abcc77cec6c813fd6da"
ROOT = pathlib.Path(__file__).resolve().parents[1] / "schema-pinned"
BASE = "https://api.github.com/repos/openai/codex/contents/codex-rs/app-server-protocol/schema/"
HEADERS = {"User-Agent": "turin-codex-app-server-generator"}
TYPESCRIPT = {"ClientRequest.ts", "ClientNotification.ts", "ServerRequest.ts", "ServerNotification.ts"}

for folder in ("json", "json/v1", "json/v2", "typescript"):
    request = urllib.request.Request(f"{BASE}{folder}?ref={REF}", headers=HEADERS)
    entries = json.load(urllib.request.urlopen(request))
    for entry in entries:
        if entry["type"] != "file" or (folder == "typescript" and entry["name"] not in TYPESCRIPT):
            continue
        target = ROOT / folder / entry["name"]
        target.parent.mkdir(parents=True, exist_ok=True)
        source = urllib.request.Request(entry["download_url"], headers=HEADERS)
        target.write_bytes(urllib.request.urlopen(source).read())
        print(target.relative_to(ROOT))
