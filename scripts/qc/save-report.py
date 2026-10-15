#!/usr/bin/env python3
"""Save a turin-qc workflow result as docs/qc/<run>-<slug>.md and print the lead report."""
import json, sys
src, run, slug, title = sys.argv[1:5]
raw = open(src).read()
d = json.loads(raw[raw.find('{'):])
r = d.get('result', d)
if isinstance(r, str):
    r = json.loads(r)
out = [f"# QC {run} — {title}", "", "Reviewer: Claude Opus 5.5 QC workflow (dimension reviewers, 2 skeptics per finding).",
       f"Confirmed: {len(r['confirmed'])} · refuted: {r['rejectedCount']}", "", "## Lead report", "", r['report'], "", "## Confirmed findings (raw)", ""]
for f in r['confirmed']:
    out.append(f"### [{f['severity']}] {f['title']}\n- `{f['file']}:{f.get('line','')}` · dimension: {f['dimension']} · upheld {f['upheld']}/{f['votes']}\n- Evidence: {f['evidence']}\n- Fix: {f['fix']}\n")
path = f"docs/qc/{run}-{slug}.md"
open(path, 'w').write("\n".join(out))
sev = {}
for f in r['confirmed']:
    sev[f['severity']] = sev.get(f['severity'], 0) + 1
print(path, sev)
print(r['report'])
