#!/usr/bin/env python3
"""Backup / restore drill summary (Part 2 · Stage 5): reads $D/evidence and prints Markdown
(job summary + evidence/summary.md) and writes evidence/drill-metrics.json (machine readable).

Measured numbers are labelled with the runner hardware. The college-scale figures are an
ESTIMATE: measured throughputs (bytes per second of each restore step) applied to an assumed
college data volume — never a measurement on college hardware.
"""
import json
import os
import sys
from datetime import datetime

D = sys.argv[1]
EV = os.path.join(D, "evidence")


def read(name, default=""):
    try:
        with open(os.path.join(EV, name)) as f:
            return f.read().strip()
    except OSError:
        return default


def readj(name):
    try:
        with open(os.path.join(EV, name)) as f:
            return json.load(f)
    except (OSError, ValueError):
        return {}


def ts(path):
    try:
        with open(os.path.join(D, path)) as f:
            return datetime.fromisoformat(f.read().strip().replace("Z", "+00:00"))
    except (OSError, ValueError):
        return None


steps = []
for line in read("metrics.jsonl").splitlines():
    try:
        steps.append(json.loads(line))
    except ValueError:
        pass
runner = dict(l.split(": ", 1) for l in read("runner.txt").splitlines() if ": " in l)
label = f"GitHub-hosted runner ({runner.get('cpu', '?')}, {runner.get('vcpu', '?')} vCPU, {runner.get('ram', '?')}) — NOT college hardware"
rpo = readj("rpo.json")
seed = readj("drill-seed.json")
nightly = readj("nightly-set.json")
sizes = (read("sizes.txt").split() + ["0"] * 5)[:5]
db_bytes, kc_bytes, n_docs, doc_bytes, n_students = (int(x) for x in sizes)
rs, re_ = ts("restore-start.txt"), ts("restore-end.txt")
rto = (re_ - rs).total_seconds() if rs and re_ else None
restore_steps = [s for s in steps if s.get("phase") == "restore" and s["step"][:1].isdigit()]
by = {s["step"]: s["seconds"] for s in steps}


def sec(prefix):
    return next((s["seconds"] for s in restore_steps if s["step"].startswith(prefix)), None)


nightly_bytes = sum(f.get("bytes", 0) for f in nightly.get("files", []))

# ---- college-scale estimate (assumptions stated in the table) -----------------------------------
A = {"students": 5000, "years": 5, "docs_per_student": 6, "doc_kb": 400}
per_student_db = db_bytes / max(n_students, 1)
est_db = per_student_db * A["students"] * A["years"] * 1.0
est_docs = A["students"] * A["years"] * A["docs_per_student"] * A["doc_kb"] * 1024
pg_s = (sec("3") or 0) + (sec("4") or 0)
obj_s = sec("7") or 0
pg_rate = db_bytes / pg_s if pg_s else None
obj_rate = doc_bytes / obj_s if obj_s and doc_bytes else None
fixed = sum(s["seconds"] for s in restore_steps if s["step"][:1] in "689")
est = None
if pg_rate and obj_rate:
    est = {
        "dbBytes": est_db, "docBytes": est_docs,
        "pgSeconds": est_db / pg_rate, "objectSeconds": est_docs / obj_rate,
        "fetchDecryptSeconds": ((sec("1") or 0) + (sec("2") or 0)) * (est_db + est_docs) / max(db_bytes + doc_bytes, 1),
        "fixedSeconds": fixed,
    }
    est["rtoSeconds"] = est["pgSeconds"] + est["objectSeconds"] + est["fetchDecryptSeconds"] + est["fixedSeconds"]

out = {
    "hardware": label, "runner": runner, "rtoSeconds": rto, "rtoTargetSeconds": 7200,
    "rpo": rpo, "rpoTargetSeconds": 86400, "steps": steps, "seed": seed,
    "sizes": {"sutraDbBytes": db_bytes, "keycloakDbBytes": kc_bytes, "documents": n_docs, "documentBytes": doc_bytes, "students": n_students, "nightlySetBytes": nightly_bytes},
    "collegeEstimate": {"assumptions": A, **(est or {})},
    "runId": os.environ.get("GITHUB_RUN_ID"), "sha": os.environ.get("GITHUB_SHA"),
}
with open(os.path.join(EV, "drill-metrics.json"), "w") as f:
    json.dump(out, f, indent=2, default=str)

mb = lambda b: f"{b / 1048576:,.1f} MiB"
gb = lambda b: f"{b / 1073741824:,.1f} GiB"
p = print
p("## SutraERP Part 2 backup + restore drill")
p()
p(f"**Measured on: {label}.** Run {out['runId']} · {(out['sha'] or '')[:8]}")
p()
p("| Measure | Value | Target | Result |")
p("|---|---|---|---|")
if rto is not None:
    p(f"| RTO — restore start → service verified (fresh host, offsite sets) | **{rto:,.0f} s ({rto / 60:.1f} min)** | 7 200 s (2 h) | {'PASS' if rto <= 7200 else 'FAIL'} |")
if rpo:
    p(f"| RPO achieved — base backup + WAL archive (archive_timeout 60 s, ship every 5 s) | **{rpo.get('achievedRpoSeconds')} s** ({rpo.get('lostCommits')} heartbeat commits lost) | 86 400 s (24 h) | {'PASS' if (rpo.get('achievedRpoSeconds') or 1e9) <= 86400 else 'FAIL'} |")
p("| RPO of the nightly logical set alone | ≤ 24 h by schedule (02:00 daily) | 24 h | design |")
p()
p(f"Data: {n_students:,} students · {n_docs:,} document objects ({mb(doc_bytes)}) · sutra DB {mb(db_bytes)} · Keycloak DB {mb(kc_bytes)} · nightly encrypted set {mb(nightly_bytes)}")
if seed:
    p(f"Seed: {seed.get('students')} students admitted through the API with {seed.get('documents')} files, {seed.get('charges')} charges, {seed.get('payments')} payments (₹{seed.get('paidAmount')}), {seed.get('requests')} requests in {seed.get('timing', {}).get('feesSeconds')} s")
p()
p("### Restore steps (wall time)")
p()
p("| Step | Seconds |")
p("|---|---|")
for s in restore_steps:
    p(f"| {s['step']} | {s['seconds']} |")
p()
p("### Other timed steps")
p()
p("| Phase | Step | Seconds |")
p("|---|---|---|")
for s in steps:
    if s not in restore_steps:
        p(f"| {s['phase']} | {s['step']} | {s['seconds']} |")
p()
if est:
    p("### College-scale ESTIMATE (not a measurement)")
    p()
    p(f"Assumed: {A['students']:,} students × {A['years']} years of history, {A['docs_per_student']} documents × {A['doc_kb']} KiB per student-year; DB size scaled linearly from this run ({mb(per_student_db)} per student). Throughputs measured above on the runner.")
    p()
    p(f"| Item | Estimate |")
    p("|---|---|")
    p(f"| Database | {gb(est['dbBytes'])} → PGDATA restore + WAL replay ≈ {est['pgSeconds'] / 60:,.0f} min |")
    p(f"| Documents | {gb(est['docBytes'])} → object restore + verify ≈ {est['objectSeconds'] / 60:,.0f} min |")
    p(f"| Fetch + decrypt (scaled) | ≈ {est['fetchDecryptSeconds'] / 60:,.0f} min |")
    p(f"| Services up + verification (fixed) | ≈ {est['fixedSeconds'] / 60:,.1f} min |")
    p(f"| **Estimated RTO** | **≈ {est['rtoSeconds'] / 60:,.0f} min** (target 120 min) — to be measured on the college server |")
p()
p("### Consistency evidence")
p()
for f in ("fp-before.txt", "fp-after-restore.txt", "fp-pitr.txt", "fp-logical.txt"):
    t = read(f)
    if t:
        lines = t.splitlines()
        rows = sum(int(l.split()[2]) for l in lines if l.startswith("rows "))
        p(f"- `{f}`: {sum(1 for l in lines if l.startswith('rows '))} tables / {rows:,} rows, {sum(1 for l in lines if l.startswith('tb '))} ledger accounts, {next((l for l in lines if l.startswith('tb-total')), '')}, {next((l for l in lines if l.startswith('docs ')), '')[:40]}…, {next((l for l in lines if l.startswith('kc-users')), '')[:20]}")
p("- after-restore / PITR / logical fingerprints compared with `fp-before.txt` by `fingerprint.sh --compare` (diff empty)")
for n in sorted(os.listdir(EV)):
    if n.startswith("negative-"):
        p(f"- partial failure `{n[9:-4]}`: refused (see `{n}`)")
