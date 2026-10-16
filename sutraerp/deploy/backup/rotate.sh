#!/usr/bin/env bash
# Backup rotation (Part 2 · Stage 5) — grandfather-father-son retention of backup sets and WAL
# pruning behind the oldest kept base backup.
#
#   rotate.sh <sets-dir> [--wal-dir DIR] [--dry-run]
#
# Nightly sets (sutra-nightly-<YYYYMMDD>T…Z): keep the newest SUTRA_KEEP_DAILY (14) days, the newest
# set of each of the last SUTRA_KEEP_WEEKLY (8) ISO weeks and of the last SUTRA_KEEP_MONTHLY (12)
# months. Base sets (sutra-base-…): keep the newest SUTRA_KEEP_BASE (2). With --wal-dir, WAL
# segments older than the oldest kept base backup's start segment are deleted (point-in-time
# recovery is possible from the oldest kept base backup to now). The newest set of each kind is
# never deleted. Prints "keep|delete <set>" lines; --dry-run deletes nothing.
set -euo pipefail
dir=${1:?usage: rotate.sh <sets-dir> [--wal-dir DIR] [--dry-run]}
shift
wal=""
dry=false
while [[ $# -gt 0 ]]; do
  case "$1" in
    --wal-dir) wal=$2; shift 2 ;;
    --dry-run) dry=true; shift ;;
    *) echo "rotate: unknown argument $1" >&2; exit 2 ;;
  esac
done
daily=${SUTRA_KEEP_DAILY:-14}
weekly=${SUTRA_KEEP_WEEKLY:-8}
monthly=${SUTRA_KEEP_MONTHLY:-12}
bases=${SUTRA_KEEP_BASE:-2}

del() { if $dry; then echo "delete $1 (dry run)"; else rm -rf -- "${dir:?}/$1"; echo "delete $1"; fi; }

# ---- nightly: GFS on the set date --------------------------------------------------------------
mapfile -t nightly < <(cd "$dir" && ls -1d sutra-nightly-* 2>/dev/null | LC_ALL=C sort -r)
declare -A keep=() seen_w=() seen_m=() seen_d=()
nd=0; nw=0; nm=0
for s in "${nightly[@]}"; do
  d=${s#sutra-nightly-}; d=${d:0:8}
  [[ "$d" =~ ^[0-9]{8}$ ]] || continue
  day="${d:0:4}-${d:4:2}-${d:6:2}"
  if [[ -z "${seen_d[$day]:-}" && $nd -lt $daily ]]; then seen_d[$day]=1; nd=$((nd + 1)); keep[$s]=daily; fi
  w=$(date -u -d "$day" +%G-W%V)
  if [[ -z "${seen_w[$w]:-}" && $nw -lt $weekly ]]; then seen_w[$w]=1; nw=$((nw + 1)); keep[$s]=${keep[$s]:-weekly}; fi
  m=${day:0:7}
  if [[ -z "${seen_m[$m]:-}" && $nm -lt $monthly ]]; then seen_m[$m]=1; nm=$((nm + 1)); keep[$s]=${keep[$s]:-monthly}; fi
done
[[ ${#nightly[@]} -gt 0 ]] && keep[${nightly[0]}]=${keep[${nightly[0]}]:-newest}
for s in "${nightly[@]}"; do
  if [[ -n "${keep[$s]:-}" ]]; then echo "keep $s (${keep[$s]})"; else del "$s"; fi
done

# ---- base backups: newest N --------------------------------------------------------------------
mapfile -t base < <(cd "$dir" && ls -1d sutra-base-* 2>/dev/null | LC_ALL=C sort -r)
oldest_wal=""
i=0
for s in "${base[@]}"; do
  if [[ $i -lt $bases || $i -eq 0 ]]; then
    echo "keep $s (base)"
    w=$(sed -n 's/.*"startWalFile":"\([0-9A-F]\{24\}\)".*/\1/p' "$dir/$s/set.json" 2>/dev/null || true)
    [[ -n "$w" ]] && oldest_wal=$w
  else
    del "$s"
  fi
  i=$((i + 1))
done

# ---- WAL behind the oldest kept base backup ----------------------------------------------------
if [[ -n "$wal" && -d "$wal" ]]; then
  if [[ -z "$oldest_wal" ]]; then
    echo "wal: no base backup with a start segment kept — nothing pruned"
  else
    n=0
    cut=${oldest_wal:8:16}
    while IFS= read -r f; do
      seg=${f%%.*}
      [[ "$seg" =~ ^[0-9A-F]{24}$ ]] || continue          # .history / .partial / .backup kept
      [[ "${seg:8:16}" < "$cut" ]] || continue
      $dry || rm -f -- "${wal:?}/$f"
      n=$((n + 1))
    done < <(cd "$wal" && ls -1)
    # keep SHA256SUMS consistent with what is left
    if ! $dry && [[ -f "$wal/SHA256SUMS" ]]; then
      awk -v d="$wal" '{ f = $2; sub(/^\*/, "", f); if ((getline line < (d "/" f)) >= 0) print; close(d "/" f) }' "$wal/SHA256SUMS" >"$wal/SHA256SUMS.new" \
        && mv "$wal/SHA256SUMS.new" "$wal/SHA256SUMS"
    fi
    echo "wal: $n segments older than $oldest_wal $($dry && echo 'would be ' || true)deleted"
  fi
fi
