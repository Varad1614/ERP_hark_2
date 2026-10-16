#!/usr/bin/env bash
# SutraERP data fingerprint (Part 2 · Stage 5 backup) — a deterministic text summary of every
# piece of state a restore must bring back, so two databases (before the disaster / after the
# restore) are compared with a plain `diff`:
#
#   fingerprint.sh <sutra-db-url> [<keycloak-db-url>] > fp.txt
#   fingerprint.sh --compare a.txt b.txt          # exit 1 + unified diff when they differ
#
# Sections (one "key value…" line each, sorted):
#   rows <schema.table> <exact count>   every base table of the application schemas + _sqlx_migrations
#   tb <tenant> <institution> <ledger code> <debit> <credit>   trial balance per ledger account
#   tb-total <debit> <credit>           journal_line totals (double entry: equal)
#   party <count> <md5>                 every student account's receivable balance (debit − credit)
#   audit-head <tenant> <seq> <hash>    audit chain head per tenant
#   audit-log <count> <max id>          rows of platform.audit_log
#   docs <count> <bytes> <md5>          platform.document_object (id, key, sha256, size, state)
#   kc-* …                              Keycloak realm `sutra` users / credentials / clients / roles /
#                                       groups (when the Keycloak database URL is given)
# Run it as a superuser (or the schema owner with BYPASSRLS): tenant RLS would hide rows otherwise.
# Needs psql only. The application part is read in ONE repeatable-read transaction.
set -euo pipefail
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=lib.sh
. "$here/lib.sh"

if [[ "${1:-}" == "--compare" ]]; then
  [[ $# -eq 3 ]] || die "usage: fingerprint.sh --compare a.txt b.txt"
  if diff -u "$2" "$3" >&2; then
    ok "fingerprint equal ($(grep -c '^rows ' "$2") tables, $(grep -c '^tb ' "$2") ledger accounts, $(grep '^docs ' "$2" | cut -d' ' -f2) documents, $(grep -c '^audit-head ' "$2") audit chains)"
    exit 0
  fi
  die "fingerprints differ ($2 vs $3)"
fi

[[ $# -ge 1 ]] || die "usage: fingerprint.sh <sutra-db-url> [<keycloak-db-url>]"
url=$1
kc=${2:-}
need "$PSQL"

list=""
for s in $SUTRA_SCHEMAS; do list+="'$s',"; done
list=${list%,}

{
  q "$url" <<SQL
BEGIN ISOLATION LEVEL REPEATABLE READ, READ ONLY;
SELECT 'rows ' || c.table_schema || '.' || c.table_name || ' ' ||
       (xpath('/row/n/text()', query_to_xml(
          format('SELECT count(*) AS n FROM %I.%I', c.table_schema, c.table_name), false, true, '')))[1]::text
  FROM information_schema.tables c
 WHERE c.table_type = 'BASE TABLE'
   AND (c.table_schema IN ($list) OR (c.table_schema = 'public' AND c.table_name = '_sqlx_migrations'))
 ORDER BY 1;
SELECT 'tb ' || la.tenant_id || ' ' || la.institution_id || ' ' || la.code || ' ' ||
       coalesce(sum(jl.debit), 0) || ' ' || coalesce(sum(jl.credit), 0)
  FROM finance.ledger_account la
  LEFT JOIN finance.journal_line jl ON jl.ledger_account_id = la.id
 GROUP BY la.tenant_id, la.institution_id, la.code
 ORDER BY 1;
SELECT 'tb-total ' || coalesce(sum(debit), 0) || ' ' || coalesce(sum(credit), 0) FROM finance.journal_line;
SELECT 'party ' || count(*) || ' ' || coalesce(md5(string_agg(b, ',' ORDER BY b)), '-') FROM (
  SELECT jl.student_account_id || ':' || (sum(jl.debit) - sum(jl.credit)) AS b
    FROM finance.journal_line jl WHERE jl.student_account_id IS NOT NULL
   GROUP BY jl.student_account_id) t;
SELECT 'audit-head ' || tenant_id || ' ' || seq || ' ' || encode(head_hash, 'hex')
  FROM platform.audit_chain_head ORDER BY tenant_id;
SELECT 'audit-log ' || count(*) || ' ' || coalesce(max(id::text), '-') FROM platform.audit_log;
SELECT 'docs ' || count(*) || ' ' || coalesce(sum(size_bytes), 0) || ' ' ||
       coalesce(md5(string_agg(id || '|' || storage_key || '|' || sha256 || '|' || size_bytes || '|' ||
                               (deleted_at IS NULL) || '|' || (quarantined_at IS NULL), ',' ORDER BY id)), '-')
  FROM platform.document_object;
COMMIT;
SQL
  if [[ -n "$kc" ]]; then
    q "$kc" <<'SQL'
SELECT 'kc-users ' || count(*) || ' ' || coalesce(md5(string_agg(u.username || '|' || coalesce(u.email, '') || '|' || u.enabled, ',' ORDER BY u.username)), '-')
  FROM user_entity u JOIN realm r ON r.id = u.realm_id WHERE r.name = 'sutra';
SELECT 'kc-credentials ' || count(*) || ' ' || coalesce(md5(string_agg(u.username || '|' || c.type || '|' || c.secret_data, ',' ORDER BY u.username, c.type, c.id)), '-')
  FROM credential c JOIN user_entity u ON u.id = c.user_id JOIN realm r ON r.id = u.realm_id WHERE r.name = 'sutra';
SELECT 'kc-clients ' || count(*) || ' ' || coalesce(md5(string_agg(c.client_id || '|' || c.enabled, ',' ORDER BY c.client_id)), '-')
  FROM client c JOIN realm r ON r.id = c.realm_id WHERE r.name = 'sutra';
SELECT 'kc-roles ' || count(*) FROM keycloak_role k JOIN realm r ON r.id = k.realm_id WHERE r.name = 'sutra';
SELECT 'kc-groups ' || count(*) FROM keycloak_group g JOIN realm r ON r.id = g.realm_id WHERE r.name = 'sutra';
SQL
  fi
} | grep -v '^\(BEGIN\|COMMIT\)$' | LC_ALL=C sort
