#!/usr/bin/env python3
"""Generate the decision-endpoint table of APPROVAL_CONTROL_MATRIX.md (S3-appr).

Port of apps/api/tests/approval_matrix.rs (handler lookup + guard detection) plus:
checker permission (handler's auth.require*), maker permission (manage permissions required by
other POST handlers of the same file), guard kind, and the tests that call the endpoint.
Usage (from sutraerp/): python3 docs/readiness/tools/approval_matrix_table.py .  -> prints markdown table rows + stats to stdout.
"""
import json, os, re, sys
from collections import Counter

root = sys.argv[1]
spec = json.load(open(os.path.join(root, "openapi/openapi.json")))
ops = {}
for path, item in spec["paths"].items():
    post = item.get("post")
    if not post:
        continue
    last = path.rsplit("/", 1)[-1]
    if (last == "decision" or last.endswith("-decision") or last.startswith("approve-")
            or last in ("authorise", "approve", "approval", "reject")):
        ops[post["operationId"]] = path

files = []
for d, _, fs in os.walk(os.path.join(root, "modules")):
    for f in fs:
        if f.endswith(".rs"):
            p = os.path.join(d, f)
            files.append((p, open(p).read()))
files.sort()

src = open(os.path.join(root, "apps/api/tests/approval_matrix.rs")).read()
allow = dict(re.findall(r'\("([a-z0-9_]+)",\s*"((?:[^"\\]|\\.)*)"\)', src.split("const GUARDS")[0]))
GUARDS = ["guard_decision", "ensure_independent_decider"]


def block(s, frm, o, c):
    i = s.find(o, frm)
    if i < 0:
        return None
    depth = 0
    for k in range(i, len(s)):
        if s[k] == o:
            depth += 1
        elif s[k] == c:
            depth -= 1
            if depth == 0:
                return (i, k + 1)
    return None


def fn_body(text, name):
    frm = 0
    while True:
        i = text.find("fn " + name, frm)
        if i < 0:
            return None
        at = i + 3 + len(name)
        if at < len(text) and text[at] in "(<":
            b = block(text, at, "{", "}")
            return text[b[0]:b[1]] if b else None
        frm = at


def calls(text):
    out = []
    for m in re.finditer(r"(?:([A-Za-z0-9_]+)::)?([A-Za-z0-9_]+)\(", text):
        out.append((m.group(1), m.group(2)))
    return out


def handler(op):
    direct = f'operation_id = "{op}"'
    for p, t in files:
        i = t.find(direct)
        if i >= 0:
            f = t.find("async fn ", i)
            b = block(t, f, "{", "}")
            name = re.match(r"async fn ([a-z0-9_]+)", t[f:]).group(1)
            return p, t[b[0]:b[1]], t, name
    lit = f'"{op}",'
    for p, t in files:
        i = t.find(lit)
        if i < 0:
            continue
        head = t[:i]
        bang = head.rfind("!(")
        if bang < 0:
            continue
        k = bang
        while k > 0 and (head[k - 1].isalnum() or head[k - 1] == "_"):
            k -= 1
        mac = head[k:bang]
        d = t.find("macro_rules! " + mac)
        if d < 0:
            continue
        b = block(t, bang, "(", ")")
        out = t[b[0]:b[1]]
        m = block(t, d, "{", "}")
        if m:
            out += t[m[0]:m[1]]
        return p, out, t, mac + "!"
    return None


def guard_kind(text):
    if "guard_decision" in text:
        return "ledger"
    if "ensure_independent_decider" in text:
        return "history"
    return None


def guarded(p, text):
    g = guard_kind(text)
    if g:
        return g
    crate_src = p[: p.index("/src/") + 5]
    d = os.path.dirname(p)
    for q, name in calls(text):
        cands = [(fp, t) for fp, t in files if fp.startswith(crate_src)]
        if q and q not in ("crate", "super", "self"):
            cands = [(fp, t) for fp, t in cands
                     if os.path.splitext(os.path.basename(fp))[0] == q or os.path.basename(os.path.dirname(fp)) == q]
        same = [c for c in cands if os.path.dirname(c[0]) == d]
        pool = same or cands
        for _, t in pool:
            b = fn_body(t, name)
            if b:
                g = guard_kind(b)
                if g:
                    return g
    return None


PERM = re.compile(r"Permission::([A-Za-z]+)")


def checker_perm(text):
    m = re.findall(r"require(?:_any)?\(\s*(?:&\[)?([^;]*?)\)\??", text)
    perms = []
    for x in m:
        perms += PERM.findall(x)
    if not perms:
        perms = PERM.findall(text)
    seen = []
    for x in perms:
        if x not in seen:
            seen.append(x)
    return seen[:2]


def maker_perm(full, checker):
    c = Counter()
    for m in re.finditer(r"#\[utoipa::path\(\s*post[\s\S]*?async fn [a-z0-9_]+[\s\S]*?\{", full):
        b = block(full, m.end() - 1, "{", "}")
        if not b:
            continue
        body = full[b[0]:b[1]]
        for x in PERM.findall(body[:600]):
            if x not in checker and ("Manage" in x or "Request" in x or "Submit" in x):
                c[x] += 1
    return [x for x, _ in c.most_common(2)]


# tests calling an endpoint: test fn names in apps/api/tests/*.rs whose body mentions the
# resource segment and the decision segment of the path on one line
tests_src = []
tdir = os.path.join(root, "apps/api/tests")
for f in sorted(os.listdir(tdir)):
    if f.endswith(".rs"):
        tests_src.append((f[:-3], open(os.path.join(tdir, f)).read()))
e2e_dir = os.path.join(root, "apps/web/e2e")
e2e_src = []
for d, _, fs in os.walk(e2e_dir):
    for f in fs:
        if f.endswith(".mjs"):
            e2e_src.append((os.path.relpath(os.path.join(d, f), e2e_dir), open(os.path.join(d, f)).read()))


def tests_for(path):
    segs = [s for s in path.split("/") if s and not s.startswith("{")]
    last = segs[-1]
    res = segs[-2] if len(segs) >= 2 else ""
    pat = re.compile(r"/" + re.escape(res) + r"/[^\"\s]*?/" + re.escape(last) + r"\b")
    found = []
    for mod, t in tests_src:
        for m in pat.finditer(t):
            head = t[: m.start()]
            fn = re.findall(r"#\[tokio::test\]\s*async fn ([a-z0-9_]+)", head)
            if fn:
                name = f"{mod}::{fn[-1]}"
                if name not in found:
                    found.append(name)
    for mod, t in e2e_src:
        if pat.search(t) or (res + "/${" in t and "/" + last in t and res in t):
            if pat.search(t):
                found.append(f"e2e {mod}")
    return found


# Stage 2 matrix (maker / checker columns) as a fallback for the heuristics
s2 = {}
s2_path = {}
for line in open(os.path.join(root, "docs/readiness/APPROVAL_CONTROL_MATRIX.md")):
    cols = [c.strip() for c in line.strip().strip("|").split("|")]
    if len(cols) >= 4 and cols[0].startswith("`") and cols[1].startswith("`/"):
        s2[cols[0].strip("`")] = (cols[2], cols[3])
    elif len(cols) >= 4 and cols[1].startswith("`POST /"):
        s2_path[cols[1].strip("`").split(" ")[1]] = (cols[2], cols[3])
MANUAL_TESTS = {
    "finance_adjustment_decide": ["db_p1_s3_appr::finance_adjustment_is_decided_by_an_independent_human_only"],
    "people_payroll_adjustment_decide": ["db_p1_s3_appr::payroll_adjustment_and_final_settlement_need_an_independent_approver"],
    "people_settlement_decide": ["db_p1_s3_appr::payroll_adjustment_and_final_settlement_need_an_independent_approver"],
    "people_payroll_run_decide": ["db_p1_s3_appr::payroll_adjustment_and_final_settlement_need_an_independent_approver", "db_b7c::await_decision_keeps_payroll_steps_pending"],
}
rows = []
stats = Counter()
for op in sorted(ops):
    path = ops[op].replace("/api/v1", "")
    h = handler(op)
    if not h:
        rows.append(f"| `{op}` | `POST {path}` | ? | ? | **handler not found** | |")
        stats["missing"] += 1
        continue
    p, text, full, name = h
    g = guarded(p, text)
    chk = checker_perm(text)
    mk = maker_perm(full, chk)
    rel = os.path.relpath(p, root)
    if g and op in allow:
        sod = "**guarded but allowlisted (stale)**"
        stats["stale"] += 1
    elif g == "ledger":
        sod = "Y (approval ledger `guard_decision*`)"
        stats["ledger"] += 1
    elif g == "history":
        sod = "Y (`ensure_independent_decider`: status history + maker columns)"
        stats["history"] += 1
    elif op in allow:
        sod = "N — allowlisted: " + allow[op]
        stats["allow"] += 1
    else:
        sod = "**N — UNGUARDED**"
        stats["unguarded"] += 1
    t = MANUAL_TESTS.get(op, []) + tests_for(ops[op])
    fb = s2.get(op) or s2_path.get(path)
    if fb:
        if not mk:
            mk = [fb[0]]
        if not chk:
            chk = [fb[1]]
    t = ["`approval_matrix::every_decision_endpoint_is_guarded_or_documented`"] + [f"`{x}`" for x in t[:5]]
    rows.append(
        f"| `{op}` | `POST {path}` | {', '.join(mk) or ('same slice permission: ' + ', '.join(x for x in chk if 'Read' not in x))} | {', '.join(chk) or '?'} | {sod} | {'; '.join(t)} | `{rel}` |"
    )

print("| Operation | Endpoint | Maker permission | Checker permission | SoD | Tests | Handler |")
print("|---|---|---|---|---|---|---|")
print("\n".join(rows))
print()
print(f"<!-- stats: total={len(ops)} " + " ".join(f"{k}={v}" for k, v in sorted(stats.items())) + " -->")
