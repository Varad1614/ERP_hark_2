#!/usr/bin/env python3
"""Part 2 · S5-capacity: turns the evidence of one capacity run into capacity-results.{md,json}
and enforces deploy/capacity/thresholds.json per phase AND per endpoint (exit 1 on any breach).

  analyze.py <out-dir> [--scale 1]

Reads (all in <out-dir>): k6-summary.json, docker-stats.jsonl, pg-samples.jsonl,
pg-statements.json, snapshot-*.json, seed-report.json, runner.json, load-start.epoch.
Every number is labelled with the runner it was measured on (runner.json) — GitHub-hosted runner
numbers are NOT college hardware.
"""
import json
import os
import re
import sys
from collections import defaultdict

out = sys.argv[1]
scale = float(sys.argv[sys.argv.index("--scale") + 1]) if "--scale" in sys.argv else 1.0
HERE = os.path.dirname(os.path.abspath(__file__))
TH = json.load(open(os.path.join(HERE, "thresholds.json")))
PHASES = [("steady", 0, 10), ("results", 10, 15), ("fees", 15, 21.5)]


def load(name, default=None):
    p = os.path.join(out, name)
    if not os.path.exists(p):
        return default
    with open(p) as f:
        return json.load(f)


def jsonl(name):
    p = os.path.join(out, name)
    rows = []
    if os.path.exists(p):
        for line in open(p):
            line = line.strip()
            if line:
                try:
                    rows.append(json.loads(line))
                except ValueError:
                    pass
    return rows


k6 = load("k6-summary.json", {"metrics": {}})
runner = load("runner.json", {})
label = runner.get("label", "GitHub-hosted runner — NOT college hardware")
M = k6.get("metrics", {})
breaches = []


def sub(metric, tags):
    key = metric + "{" + ",".join(f"{k}:{v}" for k, v in tags) + "}"
    m = M.get(key)
    return m.get("values") if m else None


def f(x, d=1):
    return "—" if x is None else (f"{x:.{d}f}" if isinstance(x, (int, float)) else str(x))


phase_secs = {p: (b - a) * 60 * scale for p, a, b in PHASES}
res = {"runner": runner, "phases": {}, "endpoints": [], "breaches": breaches}

# ------------------------------------------------------------------ per phase
for p, _, _ in PHASES:
    reqs = (sub("http_reqs", [("phase", p)]) or {}).get("count", 0)
    failed = (sub("http_req_failed", [("phase", p)]) or {}).get("rate")
    login = sub("login_flow", [("phase", p)]) or {}
    kinds = {}
    for k in ["read", "write", "file", "login"]:
        v = sub("http_req_duration", [("phase", p), ("kind", k)]) or {}
        kinds[k] = {"p50": v.get("med"), "p95": v.get("p(95)"), "p99": v.get("p(99)"), "count": v.get("count")}
        if k in TH and v.get("count"):
            for q in ["p95", "p99"]:
                lim = TH[k][q]
                val = v.get("p(95)" if q == "p95" else "p(99)")
                if val is not None and val >= lim:
                    breaches.append(f"phase {p}: {k} {q} {val:.0f} ms >= {lim} ms")
    if failed is not None and failed >= TH["errorRate"]:
        breaches.append(f"phase {p}: error rate {100 * failed:.2f} % >= {100 * TH['errorRate']:.2f} %")
    lp95 = login.get("p(95)")
    if lp95 is not None and lp95 >= TH["loginFlow"][p]["p95"]:
        breaches.append(f"phase {p}: sign-in flow p95 {lp95:.0f} ms >= {TH['loginFlow'][p]['p95']} ms")
    res["phases"][p] = {
        "requests": reqs, "rps": reqs / phase_secs[p] if phase_secs[p] else None, "errorRate": failed,
        "kinds": kinds, "login": {"p50": login.get("med"), "p95": lp95, "p99": login.get("p(99)"), "count": login.get("count")},
        "logins": (sub("logins", [("phase", p)]) or {}).get("count"),
    }
checks = M.get("checks", {}).get("values", {}).get("rate")
if checks is not None and checks <= TH["checksRate"]:
    breaches.append(f"checks {100 * checks:.2f} % <= {100 * TH['checksRate']:.2f} %")

# ------------------------------------------------------------------ per endpoint
pat = re.compile(r"^http_req_duration\{phase:([a-z]+),name:([^}]+)\}$")
kind_of = {}
src = open(os.path.join(HERE, "k6", "college-day.js")).read()
for k, body in re.findall(r"\n  (read|write|file|login): \[([^\]]*)\]", src):
    for n in re.findall(r'"([^"]+)"', body):
        kind_of[n] = k
for key, m in M.items():
    mm = pat.match(key)
    if not mm:
        continue
    p, name = mm.groups()
    v = m["values"]
    n = v.get("count") or 0
    if not n:
        continue
    err = (sub("http_req_failed", [("phase", p), ("name", name)]) or {}).get("rate")
    kind = kind_of.get(name, "read")
    row = {"phase": p, "name": name, "kind": kind, "count": n, "rps": n / phase_secs[p], "p50": v.get("med"), "p95": v.get("p(95)"), "p99": v.get("p(99)"), "max": v.get("max"), "errorRate": err}
    res["endpoints"].append(row)
    if n >= TH["minSamples"] and kind in TH:
        if v.get("p(95)", 0) >= TH[kind]["p95"]:
            breaches.append(f"{p} {name}: p95 {v['p(95)']:.0f} ms >= {TH[kind]['p95']} ms ({n} requests)")
        if v.get("p(99)", 0) >= TH[kind]["p99"]:
            breaches.append(f"{p} {name}: p99 {v['p(99)']:.0f} ms >= {TH[kind]['p99']} ms ({n} requests)")
    if n >= TH["minSamples"] and err is not None and err >= TH["errorRate"]:
        breaches.append(f"{p} {name}: errors {100 * err:.2f} % >= {100 * TH['errorRate']:.2f} % ({n} requests)")
res["endpoints"].sort(key=lambda r: (["steady", "results", "fees"].index(r["phase"]), -r["count"]))

# ------------------------------------------------------------------ containers
t0 = None
try:
    t0 = int(open(os.path.join(out, "load-start.epoch")).read().strip())
except (OSError, ValueError):
    pass


def phase_at(ts):
    if t0 is None:
        return "all"
    m_ = (ts - t0) / 60 / scale
    for p, a, b in PHASES:
        if a <= m_ < b:
            return p
    return "after" if m_ >= 0 else "before"


def mib(s):
    s = s.split("/")[0].strip()
    num = float(re.findall(r"[\d.]+", s)[0]) if re.findall(r"[\d.]+", s) else 0
    unit = re.findall(r"[A-Za-z]+", s)
    u = unit[0] if unit else "B"
    return num * {"B": 1 / 1048576, "KiB": 1 / 1024, "kB": 1 / 1024, "MiB": 1, "MB": 1, "GiB": 1024, "GB": 1024}.get(u, 1)


cont = defaultdict(lambda: defaultdict(lambda: {"cpu": [], "mem": []}))
for r in jsonl("docker-stats.jsonl"):
    s = r.get("s", {})
    name = re.sub(r"^sutracap-|-1$", "", s.get("Name", "?"))
    ph = phase_at(r["ts"])
    try:
        cont[name][ph]["cpu"].append(float(s.get("CPUPerc", "0%").rstrip("%")))
        cont[name][ph]["mem"].append(mib(s.get("MemUsage", "0B")))
    except ValueError:
        pass
res["containers"] = {
    n: {ph: {"cpuAvg": sum(v["cpu"]) / len(v["cpu"]), "cpuMax": max(v["cpu"]), "memAvgMiB": sum(v["mem"]) / len(v["mem"]), "memMaxMiB": max(v["mem"])} for ph, v in d.items() if v["cpu"]}
    for n, d in cont.items()
}

# ------------------------------------------------------------------ PostgreSQL
pgs = jsonl("pg-samples.jsonl")
pgp = defaultdict(lambda: {"conns": [], "active": [], "waiting": [], "tps": []})
prev = None
for r in pgs:
    ph = phase_at(r["ts"])
    conns = r.get("conns") or {}
    pgp[ph]["conns"].append(sum(conns.values()))
    pgp[ph]["active"].append(conns.get("active", 0))
    pgp[ph]["waiting"].append(r.get("waiting") or 0)
    if prev and r.get("xact_commit") and prev.get("xact_commit") and r["ts"] > prev["ts"]:
        pgp[ph]["tps"].append((r["xact_commit"] - prev["xact_commit"]) / (r["ts"] - prev["ts"]))
    prev = r
res["postgres"] = {ph: {"connsMax": max(v["conns"] or [0]), "activeMax": max(v["active"] or [0]), "lockWaitersMax": max(v["waiting"] or [0]), "commitsPerSecAvg": (sum(v["tps"]) / len(v["tps"])) if v["tps"] else None} for ph, v in pgp.items()}
res["statements"] = load("pg-statements.json", {})
snaps = {}
for fn in sorted(os.listdir(out)):
    if fn.startswith("snapshot-") and fn.endswith(".json"):
        try:
            snaps[fn[9:-5]] = load(fn)
        except ValueError:
            pass
res["snapshots"] = snaps
res["seed"] = load("seed-report.json", {})

# ------------------------------------------------------------------ markdown
L = []
w = L.append
w(f"## Capacity run — {label}\n")
w(f"Runner: `{runner.get('cpuModel', '?')}`, {runner.get('vcpus', '?')} vCPU, {runner.get('ramGiB', '?')} GiB RAM, {runner.get('os', '')}; load generator (k6 {runner.get('k6', '')}) on the SAME host.\n")
w("### Phases\n")
w("| Phase | Requests | Throughput (req/s) | Errors | Sign-ins | Sign-in p50 / p95 (ms) | Read p50 / p95 / p99 (ms) | Write p50 / p95 / p99 (ms) | File p95 (ms) |")
w("|---|---|---|---|---|---|---|---|---|")
for p, _, _ in PHASES:
    r = res["phases"][p]
    k = r["kinds"]
    w(f"| {p} | {r['requests']} | {f(r['rps'])} | {f(100 * r['errorRate'] if r['errorRate'] is not None else None, 2)} % | {r['logins'] or 0} | {f(r['login']['p50'], 0)} / {f(r['login']['p95'], 0)} | {f(k['read']['p50'], 0)} / {f(k['read']['p95'], 0)} / {f(k['read']['p99'], 0)} | {f(k['write']['p50'], 0)} / {f(k['write']['p95'], 0)} / {f(k['write']['p99'], 0)} | {f(k['file']['p95'], 0)} |")
w("\n### Endpoints (per phase)\n")
w("| Phase | Endpoint | Kind | Requests | req/s | p50 | p95 | p99 | max | Errors |")
w("|---|---|---|---|---|---|---|---|---|---|")
for r in res["endpoints"]:
    w(f"| {r['phase']} | `{r['name']}` | {r['kind']} | {r['count']} | {f(r['rps'], 2)} | {f(r['p50'], 0)} | {f(r['p95'], 0)} | {f(r['p99'], 0)} | {f(r['max'], 0)} | {f(100 * r['errorRate'] if r['errorRate'] is not None else None, 2)} % |")
w("\n### Containers (docker stats, sampled every ~5 s; CPU % of one core)\n")
w("| Container | Phase | CPU avg % | CPU max % | RAM avg MiB | RAM max MiB |")
w("|---|---|---|---|---|---|")
for n in sorted(res["containers"]):
    for ph in ["before", "steady", "results", "fees", "after"]:
        v = res["containers"][n].get(ph)
        if v:
            w(f"| {n} | {ph} | {f(v['cpuAvg'])} | {f(v['cpuMax'])} | {f(v['memAvgMiB'], 0)} | {f(v['memMaxMiB'], 0)} |")
w("\n### PostgreSQL\n")
w("| Phase | Connections max | Active max | Lock waiters max | Commits/s avg |")
w("|---|---|---|---|---|")
for ph in ["steady", "results", "fees"]:
    v = res["postgres"].get(ph)
    if v:
        w(f"| {ph} | {v['connsMax']} | {v['activeMax']} | {v['lockWaitersMax']} | {f(v['commitsPerSecAvg'])} |")
if snaps:
    w("\n| Snapshot | sutra DB | keycloak DB | flowable DB | cache hit ratio | deadlocks | temp files | volumes (MiB) |")
    w("|---|---|---|---|---|---|---|---|")
    for lab, s in snaps.items():
        c = s.get("cache") or {}
        vols = ", ".join(f"{k.replace('sutracap_', '')} {v / 1048576:.0f}" for k, v in (s.get("volumes") or {}).items())
        w(f"| {lab} | {s.get('dbBytes', 0) / 1048576:.0f} MiB | {s.get('keycloakDbBytes', 0) / 1048576:.0f} MiB | {s.get('flowableDbBytes', 0) / 1048576:.0f} MiB | {c.get('ratio')} | {c.get('deadlocks')} | {c.get('tempFiles')} | {vols} |")
st = res["statements"] or {}
if st.get("byTotal"):
    w(f"\nStatements during the load: {st.get('totals', {}).get('calls')} calls, {st.get('totals', {}).get('total_ms')} ms total.\n")
    w("| Top queries by total time | calls | total ms | mean ms | max ms |")
    w("|---|---|---|---|---|")
    for r in st["byTotal"][:15]:
        w(f"| `{r['query'][:160].replace('|', '/')}` | {r['calls']} | {r['total_ms']} | {r['mean_ms']} | {r['max_ms']} |")
seed = res["seed"]
if seed.get("phases"):
    w("\n### Seed (production-mode API, real approvals)\n")
    w("| Phase | Status | Seconds | Detail |")
    w("|---|---|---|---|")
    for p in seed["phases"]:
        w(f"| {p['name']} | {p['status']} | {p['ms'] / 1000:.1f} | {json.dumps(p.get('detail', {}))[:300].replace('|', '/')} |")
w("\n### Threshold verdict\n")
if breaches:
    w("**FAILED** — " + str(len(breaches)) + " breach(es):\n")
    for b in breaches:
        w(f"- {b}")
else:
    w("**PASSED** — every phase and every endpoint with ≥ %d requests is within deploy/capacity/thresholds.json." % TH["minSamples"])
open(os.path.join(out, "capacity-results.md"), "w").write("\n".join(L) + "\n")
json.dump(res, open(os.path.join(out, "capacity-results.json"), "w"), indent=1, default=str)
print("\n".join(L[:12]))
print(f"... {len(res['endpoints'])} endpoint rows; {len(breaches)} breach(es)")
for b in breaches:
    print("BREACH", b)
sys.exit(1 if breaches else 0)
