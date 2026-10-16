#!/usr/bin/env python3
"""Part 2 · S5 (DEF-P2-CAP-007): heuristic scan for pool hold-and-wait in the Rust modules.

A request path that takes a pooled connection (`pool.acquire()` / `pool.begin()`) and then, while
it still holds it, awaits a call that takes ANOTHER pooled connection (a port / repository /
guard called with the state or `&...pool`) deadlocks the pool once as many such requests overlap
as the pool has connections (DEF-P2-CAP-002, -006, -007). This script lists candidates: the line
that takes the connection and the later awaited call that is passed the shared state or pool
instead of the held connection. It is a reading aid, not a proof: every hit needs a human look
(some calls only touch the cache, or take a lock-free path).

    python3 docs/readiness/tools/hold-and-wait-scan.py            # from sutraerp/
"""
import os
import re
import sys

ACQ = re.compile(r"let\s+(mut\s+)?(\w+)\s*=\s*([\w\.]*pool\w*)\s*\.\s*(acquire|begin)\(\)")
SHARED = re.compile(
    r"(&\s*st\b|&\s*state\b|\bst\.\w+\.\w+\(|\bstate\.\w+\.\w+\(|\bself\.\w+\.\w+\(|&self\.pool"
    r"|&st\.pool|\.pool\)|\(st,|\(state,|&self\b|self\.\w+\()"
)


def scan(root: str):
    for base, _, files in os.walk(root):
        if "/target" in base or "/tests" in base:
            continue
        for name in files:
            if not name.endswith(".rs") or name.endswith("_tests.rs"):
                continue
            path = os.path.join(base, name)
            lines = open(path, encoding="utf-8").read().split("\n")
            for i, line in enumerate(lines):
                m = ACQ.search(line)
                if not m:
                    continue
                var = m.group(2)
                indent = len(line) - len(line.lstrip())
                for j in range(i + 1, min(i + 400, len(lines))):
                    s = lines[j]
                    if s.strip().startswith("}") and len(s) - len(s.lstrip()) < indent:
                        break
                    if re.search(rf"\b(drop\({var}\)|{var}\.commit\(\)|{var}\.rollback\(\))", s):
                        break
                    awaited = ".await" in s or (j + 1 < len(lines) and lines[j + 1].strip().startswith(".await"))
                    if awaited and SHARED.search(s) and var not in s:
                        yield f"{path}:{i + 1} -> {j + 1}: {s.strip()[:110]}"


if __name__ == "__main__":
    hits = list(scan(sys.argv[1] if len(sys.argv) > 1 else "modules"))
    print("\n".join(hits))
    print(f"{len(hits)} candidate(s)", file=sys.stderr)
