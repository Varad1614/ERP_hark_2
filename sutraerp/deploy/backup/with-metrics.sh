#!/usr/bin/env bash
# node_exporter textfile metrics around a backup job (Part 2 · Stage 5, hook for S5-monitor).
#
#   with-metrics.sh <kind> <command> [args…]      kind: nightly | base | wal_ship | verify
#
# When the monitoring wrapper is installed (SUTRA_BACKUP_METRICS_WRAPPER, default
# /opt/sutraerp/monitoring/sutra-backup-metrics.sh — deploy/monitoring/textfile/ on part2/s5-monitor)
# it is used unchanged:  sutra-backup-metrics.sh --kind <kind> --size-of $SUTRA_BACKUP_DIR -- <command>.
# Otherwise the SAME metric names are written here, so the SutraBackupFailed / SutraBackupStale /
# SutraBackupMissing alerts work either way:
#   sutra_backup_last_run_success{kind}, sutra_backup_last_run_timestamp_seconds{kind},
#   sutra_backup_last_duration_seconds{kind}, sutra_backup_last_success_timestamp_seconds{kind},
#   sutra_backup_last_size_bytes{kind}
# into $SUTRA_BACKUP_METRICS_DIR/sutra_backup_<kind>.prom (default /var/lib/sutraerp/node-exporter).
# The exit code of <command> is returned.
set -uo pipefail
kind=${1:?kind}; shift
[[ $# -gt 0 ]] || { echo "with-metrics.sh: no command" >&2; exit 64; }
envf=${SUTRA_BACKUP_ENV_FILE:-/etc/sutraerp/backup.env}
if [[ -f "$envf" ]]; then set -a; . "$envf"; set +a; fi
wrapper=${SUTRA_BACKUP_METRICS_WRAPPER:-/opt/sutraerp/monitoring/sutra-backup-metrics.sh}
size_of=${SUTRA_BACKUP_DIR:-/var/backups/sutraerp}
[[ "$kind" == wal_ship ]] && size_of=${SUTRA_WAL_OFFSITE_DIR:-$size_of}
if [[ -x "$wrapper" ]]; then
  exec "$wrapper" --kind "$kind" --size-of "$size_of" -- "$@"
fi
dir=${SUTRA_BACKUP_METRICS_DIR:-/var/lib/sutraerp/node-exporter}
mkdir -p "$dir" || exit 73
prom="$dir/sutra_backup_${kind}.prom"
last_success=$(sed -n "s/^sutra_backup_last_success_timestamp_seconds{kind=\"$kind\"} \([0-9]*\)$/\1/p" "$prom" 2>/dev/null)
last_size=$(sed -n "s/^sutra_backup_last_size_bytes{kind=\"$kind\"} \([0-9]*\)$/\1/p" "$prom" 2>/dev/null)
start=$(date +%s)
"$@"
rc=$?
end=$(date +%s)
okv=0
if [[ $rc -eq 0 ]]; then
  okv=1; last_success=$end
  last_size=$(du -sb "$size_of" 2>/dev/null | cut -f1)
fi
tmp=$(mktemp "$dir/.sutra_backup_${kind}.XXXXXX") || exit 73
{
  echo "# TYPE sutra_backup_last_run_success gauge"
  echo "sutra_backup_last_run_success{kind=\"$kind\"} $okv"
  echo "# TYPE sutra_backup_last_run_timestamp_seconds gauge"
  echo "sutra_backup_last_run_timestamp_seconds{kind=\"$kind\"} $end"
  echo "# TYPE sutra_backup_last_duration_seconds gauge"
  echo "sutra_backup_last_duration_seconds{kind=\"$kind\"} $((end - start))"
  if [[ -n "$last_success" ]]; then
    echo "# TYPE sutra_backup_last_success_timestamp_seconds gauge"
    echo "sutra_backup_last_success_timestamp_seconds{kind=\"$kind\"} $last_success"
  fi
  if [[ -n "$last_size" ]]; then
    echo "# TYPE sutra_backup_last_size_bytes gauge"
    echo "sutra_backup_last_size_bytes{kind=\"$kind\"} $last_size"
  fi
} >"$tmp"
chmod 0644 "$tmp"
mv -f "$tmp" "$prom"
exit $rc
