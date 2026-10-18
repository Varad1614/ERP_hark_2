#!/usr/bin/env bash
# Installs the SutraERP backup scripts + systemd timers on a single-server installation
# (Part 2 · Stage 5). Run as root from the unpacked release bundle / checkout:
#   sudo bash deploy/backup/systemd/install.sh
# Then edit /etc/sutraerp/backup.env (from host/backup.env.example) and test:
#   sudo systemctl start sutraerp-backup.service && journalctl -u sutraerp-backup -n 50
# SUTRA_INSTALL_ROOT (default /) installs into another root (used by the CI drill to validate units).
set -euo pipefail
src=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
R=${SUTRA_INSTALL_ROOT:-}
install -d -m 0755 "$R/opt/sutraerp/backup" "$R/etc/systemd/system"
for f in lib.sh set-lib.sh roles.sql sutra-backup.sh sutra-restore.sh sutra-fullbackup.sh sutra-fullrestore.sh \
         sutra-objects.sh sutra-wal-ship.sh sutra-backup-verify.sh fingerprint.sh inventory.sh rotate.sh offsite.sh with-metrics.sh; do
  install -m 0755 "$src/$f" "$R/opt/sutraerp/backup/"
done
install -d -m 0700 "$R/etc/sutraerp"
[[ -f "$R/etc/sutraerp/backup.env" ]] || install -m 0600 "$src/host/backup.env.example" "$R/etc/sutraerp/backup.env"
for u in sutraerp-backup.service sutraerp-backup.timer sutraerp-wal-ship.service sutraerp-wal-ship.timer \
         sutraerp-backup-verify.service sutraerp-backup-verify.timer sutraerp-backup-alert@.service; do
  install -m 0644 "$src/systemd/$u" "$R/etc/systemd/system/"
done
if [[ -n "$R" ]]; then
  echo "installed into $R (no systemctl calls)"
  exit 0
fi
# shellcheck disable=SC1091
. /etc/sutraerp/backup.env
install -d -m 0700 "${SUTRA_BACKUP_DIR:-/var/backups/sutraerp}"
install -d -o 999 -g 999 -m 0700 "${SUTRA_WAL_ARCHIVE_DIR:-/var/lib/sutraerp/wal-archive}" "${SUTRA_PG_STAGING_DIR:-/var/backups/sutraerp/staging}"
systemctl daemon-reload
systemctl enable --now sutraerp-backup.timer sutraerp-wal-ship.timer sutraerp-backup-verify.timer
systemctl list-timers 'sutraerp-*' --no-pager
echo "installed — edit /etc/sutraerp/backup.env and add deploy/backup/compose.backup.yml to COMPOSE_FILE"
