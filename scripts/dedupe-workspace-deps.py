#!/usr/bin/env python3
"""Drop repeated keys in [workspace.dependencies] (keep the first).

Union-style merges of parallel task branches append the same dependency line
twice, which cargo rejects ("duplicate key"). PM runs this after each merge.
"""
import re, sys
path = sys.argv[1] if len(sys.argv) > 1 else "Cargo.toml"
out, seen, section, dropped = [], set(), None, []
for line in open(path):
    m = re.match(r"\s*\[([^\]]+)\]", line)
    if m:
        section = m.group(1)
    elif section == "workspace.dependencies":
        k = re.match(r"\s*([A-Za-z0-9_-]+)\s*=", line)
        if k:
            if k.group(1) in seen:
                dropped.append(line.strip())
                continue
            seen.add(k.group(1))
    out.append(line)
open(path, "w").writelines(out)
for d in dropped:
    print("dropped:", d)
