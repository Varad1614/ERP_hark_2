#!/usr/bin/env python3
"""Generated tables for docs/readiness/CAPACITY_TEST_REPORT.md (Part 2 · S5-capacity).

usage: cap-report.py LABEL=DIR [LABEL=DIR ...]   (DIR = an unpacked `capacity-evidence` artifact)

Prints markdown: a cross-run summary (runner, per-phase throughput / errors / latencies, peak
container CPU and RAM, PostgreSQL connections and lock waiters, volume growth, verdict). Every
number comes from capacity-results.json written by deploy/capacity/analyze.py — nothing typed by hand.
"""
import json
import os
import sys


def load(d):
    with open(os.path.join(d, "capacity-results.json")) as f:
        return json.load(f)


def ms(v):
    if v is None:
        return "–"
    return f"{v / 1000:.1f} s" if v >= 1000 else f"{v:.0f}"


def pct(v):
    return "–" if v is None else f"{v * 100:.2f} %"


def g(dct, *keys):
    for k in keys:
        if isinstance(dct, dict) and k in dct:
            return dct[k]
    return None


def main(args):
    runs = []
    for a in args:
        label, d = a.split("=", 1)
        runs.append((label, load(d)))
    out = []
    out.append("| Run | GitHub run id | Commit | Runner — NOT college hardware | Verdict |")
    out.append("|---|---|---|---|---|")
    for label, r in runs:
        rn = r["runner"]
        verdict = "PASSED" if not r.get("breaches") else f"FAILED ({len(r['breaches'])} breach(es))"
        out.append(
            f"| {label} | {rn.get('runId')} | `{str(rn.get('sha'))[:8]}` | GitHub-hosted runner "
            f"({rn.get('cpuModel')}, {rn.get('vcpus')} vCPU, {rn.get('ramGiB')} GiB RAM) | {verdict} |"
        )
    out.append("")
    out.append("| Run | Phase | Requests | req/s | Errors | Sign-in p50 / p95 | Read p50 / p95 / p99 | Write p50 / p95 / p99 | File p95 |")
    out.append("|---|---|---|---|---|---|---|---|---|")
    for label, r in runs:
        for ph in ("steady", "results", "fees"):
            p = r["phases"].get(ph) or {}
            kinds = p.get("kinds") or {}
            rd, wr, fl = (kinds.get(k) or {} for k in ("read", "write", "file"))
            lg = p.get("login") or {}
            out.append(
                f"| {label} | {ph} | {g(p, 'requests', 'count')} | {g(p, 'rps') or 0:.1f} | {pct(g(p, 'errorRate'))} | "
                f"{ms(lg.get('p50'))} / {ms(lg.get('p95'))} | {ms(rd.get('p50'))} / {ms(rd.get('p95'))} / {ms(rd.get('p99'))} | "
                f"{ms(wr.get('p50'))} / {ms(wr.get('p95'))} / {ms(wr.get('p99'))} | {ms(fl.get('p95'))} |"
            )
    out.append("")
    out.append("| Run | Container | Peak CPU % (one core = 100) | Peak RAM MiB |")
    out.append("|---|---|---|---|")
    for label, r in runs:
        for name, phases in sorted(r["containers"].items()):
            cpu = max((v.get("cpuMax") or 0) for v in phases.values() if isinstance(v, dict))
            ram = max((v.get("memMaxMiB") or 0) for v in phases.values() if isinstance(v, dict))
            out.append(f"| {label} | {name} | {cpu:.1f} | {ram:.0f} |")
    out.append("")
    out.append("| Run | PG connections max | Active max | Lock waiters max | pgdata before seed → after seed → after load (MiB) |")
    out.append("|---|---|---|---|---|")
    for label, r in runs:
        pg = r["postgres"]
        ph = [pg[p] for p in ("steady", "results", "fees") if p in pg]
        conn = max((x.get("connsMax") or 0) for x in ph) if ph else None
        act = max((x.get("activeMax") or 0) for x in ph) if ph else None
        lw = max((x.get("lockWaitersMax") or 0) for x in ph) if ph else None
        vols = []
        for s in ("before-seed", "after-seed", "after-load"):
            v = (r["snapshots"].get(s) or {}).get("volumes") or {}
            b = v.get("sutracap_pgdata")
            vols.append("–" if b is None else f"{b / 1048576:,.0f}")
        out.append(f"| {label} | {conn} | {act} | {lw} | {' → '.join(vols)} |")
    out.append("")
    for label, r in runs:
        br = r.get("breaches") or []
        out.append(f"**Threshold breaches — {label}: {len(br) if br else 'none'}**")
        out.append("")
        for b in br:
            out.append(f"- {b}")
        if br:
            out.append("")
    return "\n".join(out)


if __name__ == "__main__":
    print(main(sys.argv[1:]))
