#!/usr/bin/env python3
"""Recompute Turin upstream coverage from `//! Port of upstream/<path>` headers.

Usage: scripts/coverage-progress.py [--branches] [--list none|partial|branch] [--area SUBSTR]
Reads docs/inventory/upstream-coverage.md (KEEP + UNASSIGNED rows), then greps
doc comments on HEAD (and, with --branches, every local task/* branch).
Status per file:
  direct   direct `Port of upstream/<path>` header on HEAD
  partial  path mentioned in a doc comment on HEAD, but not as a direct header
  branch   direct header only on some task/* branch (not merged yet)
  none     nowhere
"""
import re, subprocess, sys, collections

args = sys.argv[1:]
want_branches = "--branches" in args
list_kind = args[args.index("--list") + 1] if "--list" in args else None
area = args[args.index("--area") + 1] if "--area" in args else None

def git(*a):
    return subprocess.run(["git", *a], capture_output=True, text=True).stdout

rows = []
cur = None
for line in open("docs/inventory/upstream-coverage.md"):
    m = re.match(r"### `([^`]+)`", line)
    if m:
        cur = m.group(1)
        continue
    if cur and line.startswith("| ") and not line.startswith("| File") and "---" not in line:
        cells = [c.strip() for c in line.strip().strip("|").split("|")]
        if len(cells) >= 4 and cells[2] in ("KEEP", "UNASSIGNED"):
            try:
                n = int(cells[1].replace(",", ""))
            except ValueError:
                continue
            rows.append((cur + cells[0], n, cells[2], cells[3]))

def doc_lines(ref):
    out = git("grep", "-h", "-E", r"^\s*//[!/].*upstream/|Port of", ref, "--", "crates/**.rs")
    return out.splitlines()

def classify(lines):
    """direct: full upstream paths in a `Port of` header. mention: full paths in any doc
    comment, plus basenames named anywhere in a `Port of` line (e.g. "Port of Foo.tsx")."""
    direct, mention = set(), set()
    blob = "\n".join(lines)
    for l in lines:
        for m in re.finditer(r"Port of (?:upstream/)?([\w./@-]+\.(?:tsx?|mts))", l):
            direct.add(m.group(1))
        if "Port of" in l or "port of" in l:
            for m in re.finditer(r"([\w.@-]+\.(?:tsx?|mts))", l):
                mention.add("base:" + m.group(1).split("/")[-1])
    for m in re.finditer(r"upstream/([\w./@-]+\.(?:tsx?|mts))", blob):
        mention.add(m.group(1))
    return direct, mention

main_direct, main_mention = classify(doc_lines("HEAD"))
branch_direct = set()
if want_branches:
    for ref in git("for-each-ref", "--format=%(refname:short)", "refs/heads/task/").split():
        d, mm = classify(doc_lines(ref))
        branch_direct |= d | mm

def status(p):
    if p in main_direct:
        return "direct"
    if p in main_mention or "base:" + p.split("/")[-1] in main_mention:
        return "partial"
    if p in branch_direct or "base:" + p.split("/")[-1] in branch_direct:
        return "branch"
    return "none"

by = collections.defaultdict(lambda: [0, 0])
per_area = collections.defaultdict(lambda: collections.defaultdict(lambda: [0, 0]))
listed = []
for p, n, cls, task in rows:
    if area and area not in p:
        continue
    s = status(p)
    by[s][0] += 1; by[s][1] += n
    top = "/".join(p.split("/")[:4])
    per_area[top][s][0] += 1; per_area[top][s][1] += n
    if list_kind and s == list_kind:
        listed.append((n, p, cls, task))

tot_n = sum(v[1] for v in by.values())
print(f"files/lines in scope (KEEP+UNASSIGNED): {sum(v[0] for v in by.values())} / {tot_n}")
for s in ("direct", "partial", "branch", "none"):
    f, n = by[s]
    print(f"  {s:8} {f:5} files {n:8} lines  {100*n/max(tot_n,1):5.1f}%")
if not list_kind:
    print("\nby area (none lines):")
    for top, d in sorted(per_area.items(), key=lambda kv: -kv[1]["none"][1]):
        if d["none"][1]:
            print(f"  {top:50} none {d['none'][0]:4}/{d['none'][1]:6}  partial {d['partial'][0]:3}  branch {d['branch'][0]:3}  main {d['main'][0]:4}")
for n, p, cls, task in sorted(listed, reverse=True):
    print(f"{n:6} {cls[:4]} {task:6} {p}")
