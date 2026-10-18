#!/usr/bin/env bash
# Offsite copy of encrypted backup sets and the encrypted WAL archive (Part 2 · Stage 5).
#
#   offsite.sh push <set-dir>          copy one set (already age-encrypted) to SUTRA_OFFSITE_TARGET
#   offsite.sh push-wal <wal-dir>      sync the encrypted WAL directory (sutra-wal-ship.sh output)
#   offsite.sh pull <set-name> <dest>  fetch a set back (disaster recovery), then verify checksums
#   offsite.sh pull-wal <dest>         fetch the encrypted WAL archive back, verify checksums
#
# SUTRA_OFFSITE_TARGET forms:
#   /mnt/offsite/sutraerp              a second disk / NFS / USB mount (cp -a)
#   user@backup-host:/srv/sutraerp     another machine over SSH (rsync -a; key-based, no password)
#   s3://bucket/prefix                 an S3-compatible store in ANOTHER location (aws s3 sync;
#                                      SUTRA_OFFSITE_S3_ENDPOINT, SUTRA_OFFSITE_ACCESS_KEY_ID,
#                                      SUTRA_OFFSITE_SECRET_ACCESS_KEY, SUTRA_OFFSITE_REGION)
# Everything shipped is already encrypted (age) and carries SHA256SUMS, so the offsite store never
# sees plaintext and a pull is verified before it is used. Use an offsite credential that can
# write but not delete (S3 object lock / an append-only rsync module) so ransomware on the server
# cannot erase the copies; offsite expiry is done by the store's lifecycle rules (runbook).
set -euo pipefail
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=set-lib.sh
. "$here/set-lib.sh"
load_backup_env   # the caller's environment wins over backup.env
t=${SUTRA_OFFSITE_TARGET:?SUTRA_OFFSITE_TARGET}

s3() {
  export AWS_ACCESS_KEY_ID=${SUTRA_OFFSITE_ACCESS_KEY_ID:?SUTRA_OFFSITE_ACCESS_KEY_ID}
  export AWS_SECRET_ACCESS_KEY=${SUTRA_OFFSITE_SECRET_ACCESS_KEY:?SUTRA_OFFSITE_SECRET_ACCESS_KEY}
  export AWS_DEFAULT_REGION=${SUTRA_OFFSITE_REGION:-us-east-1}
  aws ${SUTRA_OFFSITE_S3_ENDPOINT:+--endpoint-url "$SUTRA_OFFSITE_S3_ENDPOINT"} --only-show-errors s3 "$@"
}
copy() { # src-dir target-relative-path
  local src=$1 rel=$2
  case "$t" in
    s3://*) s3 sync "$src" "${t%/}/$rel" ;;
    *:*) rsync -a "$src/" "${t%/}/$rel/" ;;
    *) mkdir -p "${t%/}/$rel" && cp -a "$src/." "${t%/}/$rel/" ;;
  esac
}
fetch() { # target-relative-path dest-dir
  local rel=$1 dst=$2
  mkdir -p "$dst"
  case "$t" in
    s3://*) s3 sync "${t%/}/$rel" "$dst" ;;
    *:*) rsync -a "${t%/}/$rel/" "$dst/" ;;
    *) cp -a "${t%/}/$rel/." "$dst/" ;;
  esac
}

case "${1:-}" in
  push)
    d=${2:?set dir}
    verify_set "$d" >/dev/null
    copy "$d" "sets/$(basename "$d")"
    echo "ok offsite push $(basename "$d") → $t"
    ;;
  push-wal) copy "${2:?wal dir}" wal; echo "ok offsite push-wal → $t" ;;
  pull) fetch "sets/${2:?set name}" "${3:?dest}"; verify_set "$3"; echo "ok offsite pull $2" ;;
  pull-wal)
    fetch wal "${2:?dest}"
    (cd "$2" && sha256sum --quiet --strict -c SHA256SUMS) || set_die "offsite WAL: checksum mismatch"
    echo "ok offsite pull-wal ($(wc -l <"$2/SHA256SUMS") files verified)"
    ;;
  *) set_die "usage: offsite.sh push|push-wal|pull|pull-wal …" ;;
esac
