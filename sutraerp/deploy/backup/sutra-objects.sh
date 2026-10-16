#!/usr/bin/env bash
# SutraERP document objects — backup, verification and (selective) restore (Part 2 · Stage 5).
#
#   sutra-objects.sh manifest <out.tsv> [--tenant T]        document rows from PostgreSQL
#   sutra-objects.sh mirror <dir> [--tenant T]                bucket → <dir>/objects (aws s3 sync)
#   sutra-objects.sh verify <dir> <manifest.tsv> [--tenant T] every live row's object present in
#                                                             <dir>/objects with the recorded sha256
#   sutra-objects.sh restore <dir> [--tenant T]               <dir>/objects[/T] → bucket[/T]
#   sutra-objects.sh verify-live <manifest.tsv> [--tenant T]  download the bucket (prefix) and verify
#   sutra-objects.sh listing [--tenant T]                     "key etag size" of the bucket (prefix)
#   sutra-objects.sh purge-tenant T                           delete every object under T/ (drills)
#
# Manifest (TSV, sorted by key): storage_key, sha256, size_bytes, tenant_id, state where state =
# live | deleted (row deleted, object not purged yet: must still exist) | quarantined (integrity
# already failed in production: reported, not counted as a backup failure). Purged rows are not
# listed (their object is gone on purpose). Keys are `<tenant>/<institution>/<owner>/<id>/<doc>`,
# so a tenant's documents are exactly the prefix `<tenant>/`.
#
# Env: SUTRA_BACKUP_DATABASE_URL (superuser / BYPASSRLS; RLS hides rows otherwise),
#      SUTRA_S3_ENDPOINT, SUTRA_S3_BUCKET, SUTRA_S3_ACCESS_KEY_ID, SUTRA_S3_SECRET_ACCESS_KEY,
#      SUTRA_S3_REGION (us-east-1), SUTRA_S3_PATH_STYLE (true). Tools: psql, aws (AWS CLI v2),
#      sha256sum. A read-only access key is enough for manifest / mirror / verify-live.
set -euo pipefail
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=lib.sh
. "$here/lib.sh"

cmd=${1:-}
[[ -n "$cmd" ]] || die "usage: sutra-objects.sh manifest|mirror|verify|restore|verify-live|listing|purge-tenant …"
shift
tenant=""
args=()
while [[ $# -gt 0 ]]; do
  case "$1" in
    --tenant) tenant=$2; shift 2 ;;
    *) args+=("$1"); shift ;;
  esac
done
if [[ -n "$tenant" ]]; then
  [[ "$tenant" =~ ^[0-9a-f-]{36}$ ]] || die "--tenant must be a tenant uuid"
fi

aws_setup() {
  need aws
  : "${SUTRA_S3_ENDPOINT:?SUTRA_S3_ENDPOINT}" "${SUTRA_S3_BUCKET:?SUTRA_S3_BUCKET}"
  export AWS_ACCESS_KEY_ID=${SUTRA_S3_ACCESS_KEY_ID:?SUTRA_S3_ACCESS_KEY_ID}
  export AWS_SECRET_ACCESS_KEY=${SUTRA_S3_SECRET_ACCESS_KEY:?SUTRA_S3_SECRET_ACCESS_KEY}
  export AWS_DEFAULT_REGION=${SUTRA_S3_REGION:-us-east-1}
  AWS_CONFIG_FILE=$(mktemp)
  export AWS_CONFIG_FILE
  local style=path
  [[ "${SUTRA_S3_PATH_STYLE:-true}" == false ]] && style=virtual
  printf '[default]\nregion = %s\ns3 =\n  addressing_style = %s\n  max_concurrent_requests = 16\n' "$AWS_DEFAULT_REGION" "$style" >"$AWS_CONFIG_FILE"
  S3=(aws --endpoint-url "$SUTRA_S3_ENDPOINT")
  BUCKET="s3://$SUTRA_S3_BUCKET"
  PREFIX=""
  [[ -n "$tenant" ]] && PREFIX="/$tenant"
  return 0
}

# verify_tree <objects-dir> <manifest.tsv> <label>: every non-quarantined row present with its sha.
verify_tree() {
  local root=$1 man=$2 label=$3 sums bad summary
  [[ -f "$man" ]] || die "manifest $man missing"
  sums=$(mktemp)
  echo "0  __sentinel__" >"$sums"
  if [[ -d "$root" ]]; then
    (cd "$root" && find . -type f -print0 | xargs -0 -r -n 200 -P 4 sha256sum) | sed 's# \./# #' | LC_ALL=C sort -k2 >>"$sums"
  fi
  # report: missing / mismatch lines, then "SUMMARY rows ok missing mismatch quarantined extra"
  bad=$(awk -F'\t' -v t="$tenant" '
    FNR == NR { split($0, a, " "); key = substr($0, index($0, " ") + 1); sub(/^ +/, "", key); got[key] = a[1]; next }
    t != "" && $4 != t { next }
    { rows++
      if ($5 == "quarantined") { quar++; next }
      if (!($1 in got)) { missing++; if (shown++ < 20) print "MISSING " $1; next }
      if (got[$1] != $2) { mism++; if (shown++ < 20) print "SHA256 MISMATCH " $1 " expected " $2 " got " got[$1]; next }
      okc++; seen[$1] = 1 }
    END { for (k in got) if (k != "__sentinel__" && !(k in seen)) { if (t == "" || index(k, t "/") == 1) extra++ }
          printf "SUMMARY %d %d %d %d %d %d\n", rows, okc, missing, mism, quar, extra }
  ' "$sums" "$man")
  rm -f "$sums"
  summary=$(grep '^SUMMARY' <<<"$bad")
  grep -v '^SUMMARY' <<<"$bad" >&2 || true
  read -r _ rows okc missing mism quar extra <<<"$summary"
  if [[ "$missing" != 0 || "$mism" != 0 ]]; then
    die "$label: $missing missing, $mism sha256 mismatches out of $rows documents${tenant:+ (tenant $tenant)}"
  fi
  [[ "$rows" -gt 0 ]] || log "$label: the manifest lists no documents${tenant:+ for tenant $tenant}"
  ok "$label: $okc/$rows documents verified by sha256${tenant:+ (tenant $tenant)} ($quar quarantined skipped, $extra objects without a row)"
}

case "$cmd" in
  manifest)
    [[ ${#args[@]} -eq 1 ]] || die "usage: sutra-objects.sh manifest <out.tsv> [--tenant T]"
    url=$(sutra_url)
    q "$url" -F $'\t' -v t="$tenant" >"${args[0]}.tmp" <<'SQL'
SELECT storage_key, sha256, size_bytes, tenant_id,
       CASE WHEN quarantined_at IS NOT NULL THEN 'quarantined' WHEN deleted_at IS NOT NULL THEN 'deleted' ELSE 'live' END
  FROM platform.document_object
 WHERE purged_at IS NULL AND (:'t' = '' OR tenant_id::text = :'t')
 ORDER BY storage_key;
SQL
    mv "${args[0]}.tmp" "${args[0]}"
    ok "manifest $(wc -l <"${args[0]}") document rows${tenant:+ (tenant $tenant)} → ${args[0]}"
    ;;
  mirror)
    [[ ${#args[@]} -eq 1 ]] || die "usage: sutra-objects.sh mirror <dir>"
    aws_setup
    mkdir -p "${args[0]}/objects$PREFIX"
    "${S3[@]}" s3 sync --only-show-errors "$BUCKET$PREFIX" "${args[0]}/objects$PREFIX"
    ok "mirror $BUCKET$PREFIX → ${args[0]}/objects ($(find "${args[0]}/objects" -type f | wc -l) objects, $(du -sb "${args[0]}/objects" | cut -f1) bytes)"
    ;;
  verify)
    [[ ${#args[@]} -eq 2 ]] || die "usage: sutra-objects.sh verify <dir> <manifest.tsv> [--tenant T]"
    verify_tree "${args[0]}/objects" "${args[1]}" "objects in ${args[0]}"
    ;;
  restore)
    [[ ${#args[@]} -eq 1 ]] || die "usage: sutra-objects.sh restore <dir> [--tenant T]"
    aws_setup
    src="${args[0]}/objects$PREFIX"
    [[ -d "$src" ]] || die "$src missing (nothing backed up for this tenant?)"
    "${S3[@]}" s3api head-bucket --bucket "$SUTRA_S3_BUCKET" >/dev/null 2>&1 \
      || "${S3[@]}" s3 mb "$BUCKET" >/dev/null
    "${S3[@]}" s3 sync --only-show-errors "$src" "$BUCKET$PREFIX"
    ok "restore $(find "$src" -type f | wc -l) objects → $BUCKET$PREFIX"
    ;;
  verify-live)
    [[ ${#args[@]} -eq 1 ]] || die "usage: sutra-objects.sh verify-live <manifest.tsv> [--tenant T]"
    aws_setup
    tmp=$(mktemp -d)
    trap 'rm -rf "$tmp"' EXIT
    mkdir -p "$tmp/objects$PREFIX"
    "${S3[@]}" s3 sync --only-show-errors "$BUCKET$PREFIX" "$tmp/objects$PREFIX"
    verify_tree "$tmp/objects" "${args[0]}" "objects in $BUCKET (downloaded back)"
    ;;
  listing)
    aws_setup
    "${S3[@]}" s3api list-objects-v2 --bucket "$SUTRA_S3_BUCKET" ${tenant:+--prefix "$tenant/"} \
      --query 'Contents[].[Key, ETag, Size]' --output text | LC_ALL=C sort
    ;;
  purge-tenant)
    [[ ${#args[@]} -eq 1 && "${args[0]}" =~ ^[0-9a-f-]{36}$ ]] || die "usage: sutra-objects.sh purge-tenant <tenant uuid>"
    tenant=${args[0]}
    aws_setup
    "${S3[@]}" s3 rm --only-show-errors --recursive "$BUCKET/$tenant/"
    ok "purged every object under $BUCKET/$tenant/"
    ;;
  *) die "unknown command: $cmd" ;;
esac
