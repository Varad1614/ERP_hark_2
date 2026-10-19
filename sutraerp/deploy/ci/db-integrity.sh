#!/usr/bin/env bash
# Part 1 · DB integrity check of the demo database (after the journey + restart persistence).
# Runs deploy/ci/db-integrity.sql (orphans, balances, duplicates, idempotency, maker-checker,
# guards, negative probes — one rolled-back transaction) and compares the applied migrations with
# the migration files. Exit 1 on any violation. Usage: DATABASE_URL=… bash deploy/ci/db-integrity.sh
set -euo pipefail
cd "$(dirname "$0")/../.."
: "${DATABASE_URL:?DATABASE_URL is required}"
out=$(psql "$DATABASE_URL" -X -v ON_ERROR_STOP=1 -f deploy/ci/db-integrity.sql)
echo "$out"
violations=$(echo "$out" | sed -n 's/^INTEGRITY_VIOLATIONS=//p' | tail -1)

# Applied migrations = the migration files, same versions, same order.
files=$(ls migrations/*.sql | sed -E 's#.*/([0-9]+)_.*#\1#' | sort)
applied=$(psql "$DATABASE_URL" -XqtA -c "SELECT version FROM _sqlx_migrations ORDER BY version")
nf=$(echo "$files" | wc -l); na=$(echo "$applied" | wc -l)
echo "migrations: ${nf} files, ${na} applied"
if [ "$files" != "$applied" ]; then
  echo "MIGRATION MISMATCH (files vs applied):"; diff <(echo "$files") <(echo "$applied") || true
  violations=$(( ${violations:-0} + 1 ))
fi
if [ "$(echo "$files" | uniq -d | wc -l)" != "0" ] && [ -n "$(echo "$files" | uniq -d)" ]; then
  echo "DUPLICATE MIGRATION VERSIONS: $(echo "$files" | uniq -d | tr '\n' ' ')"; violations=$(( ${violations:-0} + 1 ))
fi
echo "INTEGRITY TOTAL VIOLATIONS: ${violations:-unknown}"
[ "${violations:-1}" = "0" ]
