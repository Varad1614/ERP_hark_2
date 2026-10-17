#!/usr/bin/env python3
"""Drill-only: copy rule files with every `for:` capped at 20 s and group intervals at 10 s, so the
fault-injection drill finishes in minutes. The EXPRESSIONS are unchanged; the production `for:`
durations are proven by the promtool unit tests. Usage: ci-rules.py OUT_DIR RULE_FILE..."""
import os
import re
import sys

import yaml

CAP = 20


def secs(d):
    total = 0
    for n, u in re.findall(r"(\d+)([smhd])", d):
        total += int(n) * {"s": 1, "m": 60, "h": 3600, "d": 86400}[u]
    return total


out = sys.argv[1]
os.makedirs(out, exist_ok=True)
for path in sys.argv[2:]:
    doc = yaml.safe_load(open(path))
    capped = 0
    for g in doc["groups"]:
        g["interval"] = "10s"
        for r in g["rules"]:
            if "for" in r and secs(r["for"]) > CAP:
                r["for"] = f"{CAP}s"
                capped += 1
    dst = os.path.join(out, os.path.basename(path))
    with open(dst, "w") as f:
        f.write("# DRILL COPY (for: capped at %ds) of %s\n" % (CAP, path))
        yaml.safe_dump(doc, f, sort_keys=False, width=200)
    print(f"{dst}: {sum(len(g['rules']) for g in doc['groups'])} rules, {capped} capped")
