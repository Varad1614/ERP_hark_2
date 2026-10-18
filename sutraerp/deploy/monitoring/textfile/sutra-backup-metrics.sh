#!/usr/bin/env bash
# SutraERP backup metrics wrapper (Part 2 S5-monitor) — node_exporter textfile collector.
#
#   sutra-backup-metrics.sh [--kind logical] [--dir /var/lib/sutraerp/node-exporter] \
#       [--size-of <path>] -- <backup command> [args...]
#
# Runs the backup command unchanged (its exit code is returned) and atomically rewrites
# <dir>/sutra_backup_<kind>.prom with:
#   sutra_backup_last_run_success{kind}                1 = the last run exited 0, 0 = it failed
#   sutra_backup_last_run_timestamp_seconds{kind}      end of the last run
#   sutra_backup_last_duration_seconds{kind}           duration of the last run
#   sutra_backup_last_success_timestamp_seconds{kind}  end of the last SUCCESSFUL run (kept across failures)
#   sutra_backup_last_size_bytes{kind}                 size of --size-of after the last successful run
# Alerts: SutraBackupFailed (last run failed), SutraBackupStale (no success for 26 h),
# SutraBackupMissing (no metrics at all) — docs/readiness/MONITORING_AND_SUPPORT.md.
set -uo pipefail

kind=logical
dir="${SUTRA_BACKUP_METRICS_DIR:-/var/lib/sutraerp/node-exporter}"
size_of=""
while [ $# -gt 0 ]; do
  case "$1" in
    --kind) kind="$2"; shift 2 ;;
    --dir) dir="$2"; shift 2 ;;
    --size-of) size_of="$2"; shift 2 ;;
    --) shift; break ;;
    *) echo "usage: $0 [--kind K] [--dir D] [--size-of PATH] -- command [args...]" >&2; exit 64 ;;
  esac
done
[ $# -gt 0 ] || { echo "$0: no backup command given" >&2; exit 64; }
case "$kind" in *[!a-z0-9_]*|"") echo "$0: --kind must be [a-z0-9_]+" >&2; exit 64 ;; esac
mkdir -p "$dir" || exit 73

prom="$dir/sutra_backup_${kind}.prom"
last_success=""
last_size=""
if [ -f "$prom" ]; then
  last_success="$(sed -n "s/^sutra_backup_last_success_timestamp_seconds{kind=\"${kind}\"} \([0-9]*\)$/\1/p" "$prom")"
  last_size="$(sed -n "s/^sutra_backup_last_size_bytes{kind=\"${kind}\"} \([0-9]*\)$/\1/p" "$prom")"
fi

start="$(date +%s)"
"$@"
rc=$?
end="$(date +%s)"

ok=0
if [ "$rc" -eq 0 ]; then
  ok=1
  last_success="$end"
  if [ -n "$size_of" ] && [ -e "$size_of" ]; then
    last_size="$(du -sb "$size_of" 2>/dev/null | cut -f1)"
  fi
fi

tmp="$(mktemp "$dir/.sutra_backup_${kind}.XXXXXX")" || exit 73
{
  echo "# HELP sutra_backup_last_run_success 1 if the last backup run exited 0, else 0."
  echo "# TYPE sutra_backup_last_run_success gauge"
  echo "sutra_backup_last_run_success{kind=\"${kind}\"} ${ok}"
  echo "# HELP sutra_backup_last_run_timestamp_seconds End of the last backup run (Unix seconds)."
  echo "# TYPE sutra_backup_last_run_timestamp_seconds gauge"
  echo "sutra_backup_last_run_timestamp_seconds{kind=\"${kind}\"} ${end}"
  echo "# HELP sutra_backup_last_duration_seconds Duration of the last backup run."
  echo "# TYPE sutra_backup_last_duration_seconds gauge"
  echo "sutra_backup_last_duration_seconds{kind=\"${kind}\"} $((end - start))"
  if [ -n "$last_success" ]; then
    echo "# HELP sutra_backup_last_success_timestamp_seconds End of the last successful backup run (Unix seconds)."
    echo "# TYPE sutra_backup_last_success_timestamp_seconds gauge"
    echo "sutra_backup_last_success_timestamp_seconds{kind=\"${kind}\"} ${last_success}"
  fi
  if [ -n "$last_size" ]; then
    echo "# HELP sutra_backup_last_size_bytes Size of the last successful backup."
    echo "# TYPE sutra_backup_last_size_bytes gauge"
    echo "sutra_backup_last_size_bytes{kind=\"${kind}\"} ${last_size}"
  fi
} > "$tmp"
chmod 0644 "$tmp"
mv -f "$tmp" "$prom"
echo "sutra-backup-metrics: kind=${kind} rc=${rc} duration=$((end - start))s -> ${prom}" >&2
exit "$rc"
