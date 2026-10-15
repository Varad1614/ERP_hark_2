#!/usr/bin/env bash
# Show SutraERP CI runs + jobs for a branch (re-skin v2). Read-only.
#   rs2-ci.sh [branch=reskin2/foundation] [runs=3]
# Run through Hark's shell with credentials [{"provider":"github"}] (the gateway adds the token).
# Prints run id, workflow, sha, status/conclusion, then each job's conclusion; failed jobs show the
# failing step. RS2_LOG=1 also prints the tail of each failed job's log of the newest run (RS2_TAIL=80).
set -euo pipefail
REPO=${RS2_REPO:-Varad1614/ERP_hark_2}
BR=${1:-reskin2/foundation}
N=${2:-3}
API=https://api.github.com/repos/$REPO
TMP=$(mktemp)
enc=$(python3 -c 'import sys,urllib.parse;print(urllib.parse.quote(sys.argv[1],safe=""))' "$BR")
curl -fsS -H "Accept: application/vnd.github+json" "$API/actions/runs?branch=$enc&per_page=$N" > "$TMP"
python3 - "$API" "$TMP" <<'PY'
import json, subprocess, sys
api, tmp = sys.argv[1], sys.argv[2]
runs = json.load(open(tmp))['workflow_runs']
if not runs: print('no runs'); sys.exit()
for r in runs:
    print(f"RUN {r['id']} {r['name']} sha={r['head_sha'][:8]} {r['status']}/{r['conclusion']} {r['created_at']} {r['html_url']}")
    jobs = json.loads(subprocess.check_output(['curl','-fsS','-H','Accept: application/vnd.github+json', f"{api}/actions/runs/{r['id']}/jobs?per_page=100"]))['jobs']
    for j in jobs:
        bad = [s['name'] for s in j.get('steps', []) if s.get('conclusion') == 'failure']
        print(f"   {(j['conclusion'] or j['status']):<11} {j['name']} (job {j['id']}){'  ✗ '+bad[-1] if bad else ''}")
PY
if [ "${RS2_LOG:-0}" = 1 ]; then
  for jid in $(python3 - "$API" "$TMP" <<'PY'
import json, subprocess, sys
api, tmp = sys.argv[1], sys.argv[2]
r = json.load(open(tmp))['workflow_runs'][0]
js = json.loads(subprocess.check_output(['curl','-fsS',f"{api}/actions/runs/{r['id']}/jobs?per_page=100"]))['jobs']
print(' '.join(str(j['id']) for j in js if j['conclusion'] == 'failure'))
PY
); do
    echo "===== log tail job $jid"; curl -fsSL "$API/actions/jobs/$jid/logs" | tail -n "${RS2_TAIL:-80}"
  done
fi
rm -f "$TMP"
