#!/usr/bin/env bash
# Verifies stored backups without restoring them (Part 2 · Stage 5) — run weekly
# (deploy/backup/systemd/sutraerp-backup-verify.timer) and before every restore.
#
#   sutra-backup-verify.sh [<set-dir> …]     default: every set under $SUTRA_BACKUP_DIR/sets + the
#                                            encrypted WAL directory SUTRA_WAL_OFFSITE_DIR
# For each set: the *.age files equal SHA256SUMS (nothing missing, nothing extra, every checksum
# matches). With SUTRA_BACKUP_AGE_IDENTITY set (only on the operator's restore workstation — the
# identity is never stored on the server) it also decrypts every file and checks PLAIN.SHA256.
# Exit 1 on the first damaged set, naming the file.
set -euo pipefail
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
envf=${SUTRA_BACKUP_ENV_FILE:-/etc/sutraerp/backup.env}
if [[ -f "$envf" ]]; then set -a; . "$envf"; set +a; fi
# shellcheck source=set-lib.sh
. "$here/set-lib.sh"
sets=("$@")
if [[ ${#sets[@]} -eq 0 ]]; then
  root=${SUTRA_BACKUP_DIR:-/var/backups/sutraerp}
  mapfile -t sets < <(ls -1d "$root"/sets/sutra-* 2>/dev/null)
fi
[[ ${#sets[@]} -gt 0 ]] || set_die "no backup sets found"
for s in "${sets[@]}"; do
  verify_set "$s"
  if [[ -n "${SUTRA_BACKUP_AGE_IDENTITY:-}" ]]; then
    tmp=$(mktemp -d)
    decrypt_set "$s" "$tmp" >/dev/null
    rm -rf "$tmp"
    printf 'ok decrypt-test %s\n' "$(basename "$s")"
  fi
done
if [[ $# -eq 0 && -n "${SUTRA_WAL_OFFSITE_DIR:-}" && -f "${SUTRA_WAL_OFFSITE_DIR}/SHA256SUMS" ]]; then
  (cd "$SUTRA_WAL_OFFSITE_DIR" && sha256sum --quiet --strict -c SHA256SUMS) || set_die "WAL archive: checksum mismatch"
  printf 'ok wal archive (%s files verified)\n' "$(wc -l <"$SUTRA_WAL_OFFSITE_DIR/SHA256SUMS")"
fi
