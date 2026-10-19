#!/usr/bin/env python3
"""Upstream coverage census from `//! Port of upstream/<path>` headers.

Usage (repo root): scripts/coverage.py [--branches] [--list none|partial|branch]
Reads docs/inventory/upstream-coverage.md (KEEP + UNASSIGNED rows).
  main     direct `Port of upstream/<path>` header on main
  partial  path or basename named in a doc comment on main, not as a direct header
  branch   only on some task/* branch
  none     nowhere (may still be a logged cut: check PORTING.md)
"""
import re, subprocess, sys, collections
args = sys.argv[1:]
want_branches = "--branches" in args
list_kind = args[args.index("--list") + 1] if "--list" in args else None
def git(*a): return subprocess.run(["git", *a], capture_output=True, text=True).stdout
rows, cur = [], None
for line in open("docs/inventory/upstream-coverage.md"):
    m = re.match(r"### `([^`]+)`", line)
    if m: cur = m.group(1); continue
    if cur and line.startswith("| ") and "---" not in line:
        c = [x.strip() for x in line.strip().strip("|").split("|")]
        if len(c) >= 4 and c[2] in ("KEEP", "UNASSIGNED"):
            try: rows.append((cur + c[0], int(c[1].replace(",", "")), c[2], c[3]))
            except ValueError: pass
def doc_lines(ref): return git("grep", "-h", "-E", r"^\s*//[!/].*upstream/|Port of", ref, "--", "crates/**.rs").splitlines()
def classify(lines):
    direct, mention = set(), set()
    for l in lines:
        for m in re.finditer(r"Port of (?:upstream/)?([\w./@-]+\.(?:tsx?|mts))", l): direct.add(m.group(1))
        if "ort of" in l:
            for m in re.finditer(r"([\w.@-]+\.(?:tsx?|mts))", l): mention.add("base:" + m.group(1).split("/")[-1])
        for m in re.finditer(r"upstream/([\w./@-]+\.(?:tsx?|mts))", l): mention.add(m.group(1))
    return direct, mention
md, mm = classify(doc_lines("main"))
bd = set()
if want_branches:
    for ref in git("for-each-ref", "--format=%(refname:short)", "refs/heads/task/").split():
        d, x = classify(doc_lines(ref)); bd |= d | x
def status(p):
    b = "base:" + p.split("/")[-1]
    if p in md: return "main"
    if p in mm or b in mm: return "partial"
    if p in bd or b in bd: return "branch"
    return "none"
by = collections.defaultdict(lambda: [0, 0]); listed = []
for p, n, cls, task in rows:
    s = status(p); by[s][0] += 1; by[s][1] += n
    if list_kind == s: listed.append((n, p, task))
tot = sum(v[1] for v in by.values())
print(f"KEEP+UNASSIGNED: {sum(v[0] for v in by.values())} files / {tot} lines")
for s in ("main", "partial", "branch", "none"):
    print(f"  {s:8} {by[s][0]:5} files {by[s][1]:8} lines {100*by[s][1]/max(tot,1):5.1f}%")
for n, p, task in sorted(listed, reverse=True): print(f"{n:6} {task:6} {p}")
