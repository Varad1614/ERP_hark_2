#!/usr/bin/env bash
# ZAP baseline verdict (Batch 9 · 9D): fail on any High-risk alert (riskcode 3); list Low+
# alerts in the job summary for review in docs/security/findings.md.
# Usage: verdict.sh <label> <report_json.json> <out-dir>
set -euo pipefail
label="$1" report="$2" out="$3"
[[ -f "$report" ]] || { echo "✗ ZAP report $report not found" >&2; exit 1; }
mkdir -p "$out"
cp "$report" "$out/zap-$label.json"
high="$(jq '[.site[]?.alerts[]? | select(.riskcode == "3")] | length' "$report")"
table="$(
  echo "#### ZAP baseline \`$label\` — target $(jq -r '[.site[]?."@name"] | join(", ")' "$report")"
  echo "| Risk | Alert | Plugin | Instances |"
  echo "|---|---|---|---|"
  jq -r '.site[]?.alerts[]? | select((.riskcode|tonumber) >= 1)
         | "| \(["Info","Low","Medium","High"][.riskcode|tonumber]) | \(.name) | \(.pluginid) | \(.count // (.instances|length)) |"' "$report" | sort -r
  echo
)"
echo "$table"
[[ -n "${GITHUB_STEP_SUMMARY:-}" ]] && echo "$table" >> "$GITHUB_STEP_SUMMARY"
jq -r '.site[]?.alerts[]? | "\(.riskdesc)\t\(.pluginid)\t\(.name)"' "$report" | sort -r
if [[ "$high" != 0 ]]; then
  echo "✗ ZAP $label: $high High-risk alert(s)" >&2
  exit 1
fi
echo "ok zap-$label (no High-risk alerts)"
