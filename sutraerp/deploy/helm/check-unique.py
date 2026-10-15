#!/usr/bin/env python3
"""INT (Batch 9 integration): fail when a rendered manifest holds two objects with the same
kind + namespace + name (helm install would fail with "already exists").
Usage: check-unique.py <rendered.yaml>..."""
import collections
import sys

import yaml

bad = False
for path in sys.argv[1:]:
    with open(path, encoding="utf-8") as f:
        docs = [d for d in yaml.safe_load_all(f) if d]
    seen = collections.Counter(
        (d.get("kind"), d.get("metadata", {}).get("namespace", ""), d.get("metadata", {}).get("name"))
        for d in docs
    )
    for key, n in seen.items():
        if n > 1:
            print(f"{path}: {key[0]} {key[2]} rendered {n} times", file=sys.stderr)
            bad = True
sys.exit(1 if bad else 0)
