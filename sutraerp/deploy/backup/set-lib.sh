#!/usr/bin/env bash
# Backup-set helpers (Part 2 · Stage 5): encryption at rest (age, X25519) and checksums.
#
# A backup SET is a directory  <name>/  holding
#   *.age          every artefact, encrypted to SUTRA_BACKUP_AGE_RECIPIENT (public key only on the
#                  server; the identity / private key is kept OFF the server — see the runbook)
#   SHA256SUMS     sha256 of every *.age file (what is stored / shipped offsite)
#   PLAIN.SHA256   sha256 of every plaintext artefact before encryption (checked after decrypting)
#   set.json       kind, createdAt, host, gitSha, files[{name, bytes, sha256}] (plain JSON, no secrets)
# Sourced by sutra-fullbackup.sh, sutra-backup-verify.sh, sutra-fullrestore.sh, sutra-wal-ship.sh.
set -euo pipefail

set_log() { printf '%s %s\n' "$(date -u +%H:%M:%SZ)" "$*" >&2; }
set_die() { set_log "ERROR: $*"; exit 1; }

set_need_age() {
  command -v age >/dev/null 2>&1 || set_die "missing tool: age (apt install age)"
}

age_recipient_args() {
  AGE_ARGS=()
  [[ -n "${SUTRA_BACKUP_AGE_RECIPIENT:-}" ]] && AGE_ARGS+=(-r "$SUTRA_BACKUP_AGE_RECIPIENT")
  [[ -n "${SUTRA_BACKUP_AGE_RECIPIENTS_FILE:-}" ]] && AGE_ARGS+=(-R "$SUTRA_BACKUP_AGE_RECIPIENTS_FILE")
  [[ ${#AGE_ARGS[@]} -gt 0 ]] \
    || set_die "set SUTRA_BACKUP_AGE_RECIPIENT (age public key) or SUTRA_BACKUP_AGE_RECIPIENTS_FILE"
}

# encrypt_into <plain-file> <set-dir>: writes <set-dir>/<basename>.age, records both checksums,
# removes the plaintext.
encrypt_into() {
  local f=$1 dir=$2 b
  b=$(basename "$f")
  set_need_age
  age_recipient_args
  (cd "$(dirname "$f")" && sha256sum "$b") >>"$dir/PLAIN.SHA256"
  age "${AGE_ARGS[@]}" -o "$dir/$b.age" "$f"
  (cd "$dir" && sha256sum "$b.age") >>"$dir/SHA256SUMS"
  rm -f "$f"
}

# write_set_json <set-dir> <kind> [extra-json-object]
write_set_json() {
  local dir=$1 kind=$2 extra=${3:-'{}'} files="" n sha bytes
  while read -r sha n; do
    n=${n#\*}
    bytes=$(stat -c %s "$dir/$n")
    files+="{\"name\":\"$n\",\"bytes\":$bytes,\"sha256\":\"$sha\"},"
  done <"$dir/SHA256SUMS"
  files="[${files%,}]"
  printf '{"format":"sutra-backup-set/v1","kind":"%s","name":"%s","createdAt":"%s","host":"%s","gitSha":"%s","encryption":"age-x25519","files":%s,"extra":%s}\n' \
    "$kind" "$(basename "$dir")" "$(date -u +%FT%TZ)" "$(hostname)" "${SUTRA_GIT_SHA:-unknown}" "$files" "$extra" >"$dir/set.json"
}

# verify_set <set-dir>: every *.age file listed and matching SHA256SUMS, nothing missing / extra.
verify_set() {
  local dir=$1 listed actual
  [[ -f "$dir/SHA256SUMS" ]] || set_die "$dir: SHA256SUMS missing"
  listed=$(awk '{print $2}' "$dir/SHA256SUMS" | sed 's/^\*//' | LC_ALL=C sort)
  actual=$(cd "$dir" && ls -1 -- *.age 2>/dev/null | LC_ALL=C sort || true)
  [[ "$listed" == "$actual" ]] || set_die "$dir: files differ from SHA256SUMS (listed: $(echo $listed); present: $(echo $actual))"
  (cd "$dir" && sha256sum --quiet --strict -c SHA256SUMS) >&2 || set_die "$dir: checksum mismatch (corrupted or tampered backup file)"
  printf 'ok set %s (%s files, checksums verified)\n' "$(basename "$dir")" "$(wc -l <"$dir/SHA256SUMS")"
}

# decrypt_set <set-dir> <out-dir>: verify, decrypt every file, check the plaintext checksums.
decrypt_set() {
  local dir=$1 out=$2 f
  verify_set "$dir" >&2
  [[ -n "${SUTRA_BACKUP_AGE_IDENTITY:-}" ]] || set_die "set SUTRA_BACKUP_AGE_IDENTITY (age identity file kept off the server)"
  set_need_age
  mkdir -p "$out"
  for f in "$dir"/*.age; do
    age -d -i "$SUTRA_BACKUP_AGE_IDENTITY" -o "$out/$(basename "${f%.age}")" "$f" \
      || set_die "$(basename "$f"): age decryption failed (wrong identity or damaged file)"
  done
  (cd "$out" && sha256sum --quiet --strict -c "$(cd "$dir" && pwd)/PLAIN.SHA256") >&2 \
    || set_die "$dir: plaintext checksum mismatch after decryption"
  printf 'ok decrypt %s → %s (%s files)\n' "$(basename "$dir")" "$out" "$(wc -l <"$dir/PLAIN.SHA256")"
}
