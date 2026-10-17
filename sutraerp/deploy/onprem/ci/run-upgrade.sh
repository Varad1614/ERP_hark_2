#!/usr/bin/env bash
# Staging drill B (Part 2 · S5-deploy): install the PREVIOUS release (erp_merge Part 1 images) from
# its bundle, put real data in, upgrade to the current bundle with the data kept, then roll back:
# first app-only (diagnostic: old images on the migrated schema), then the supported rollback
# (restore of the pre-upgrade backup, migrations being forward-only), then upgrade again.
#
#   OUT=<results dir> run-upgrade.sh <previous bundle .tar> <current bundle .tar>
set -euo pipefail
# shellcheck source=common.sh
. "$(dirname "${BASH_SOURCE[0]}")/common.sh"
prev_tar=${1:?run-upgrade.sh <previous tar> <current tar>}
cur_tar=${2:?}
KEY_FIELDS=(institutions students admissions charges payments payment_total receipts journal_entries)

say "host facts"
bash "$CI_DIR/host-facts.sh" "$OUT" staging-runner-upgrade

say "unpack both bundles, go offline"
prev=$(unpack_bundle "$prev_tar" /tmp/onprem-prev)
cur=$(unpack_bundle "$cur_tar" /tmp/onprem-cur)
go_offline
staging_dns

say "install the previous release $(cat "$prev/VERSION")"
t0=$(now)
sudo -E "$prev/bin/sutra-onprem" install --domain "$ERP_HOST" --sso-domain "$SSO_HOST" --local-ca
record install_previous_wall "$(since "$t0")"
bootstrap_people
smoke seed
smoke verify
c1=$(counts)
result counts-previous "$c1"

say "upgrade $(cat "$prev/VERSION") → $(cat "$cur/VERSION")"
t0=$(now)
$SO upgrade "$cur"
record upgrade_wall "$(since "$t0")"
c2=$(counts)
result counts-after-upgrade "$c2"
same_fields "$c1" "$c2" "upgrade (business tables of the previous release)" "${KEY_FIELDS[@]}"
SMOKE_SUFFIX=_after_upgrade smoke verify
# New capability of the upgraded release on the migrated data: a document on the old student.
smoke document
c3=$(counts)

say "rollback A — app only (old images on the migrated schema; diagnostic, not supported)"
if $SO rollback --app-only; then
  if ONPREM_SKIP_DOCUMENT=1 SMOKE_SUFFIX=_app_only_rollback smoke verify; then
    result rollback-app-only "previous images started on the migrated schema and served the smoke reads (unsupported: forward-only migrations, new tables ignored)"
  else
    result rollback-app-only "previous images started but the smoke reads FAILED on the migrated schema"
  fi
else
  result rollback-app-only "previous images did NOT become healthy on the migrated schema"
fi

say "rollback B — supported: previous images + restore of the pre-upgrade backup"
t0=$(now)
$SO rollback
record rollback_wall "$(since "$t0")"
c4=$(counts)
result counts-after-rollback "$c4"
same_fields "$c1" "$c4" "rollback = pre-upgrade data" "${KEY_FIELDS[@]}"
ONPREM_SKIP_DOCUMENT=1 SMOKE_SUFFIX=_after_rollback smoke verify
result rollback-data-loss "writes after the upgrade are discarded by design (document rows before rollback: $(sed -n 's/.*documents=\([^ ]*\).*/\1/p' <<<"$c3"), after: $(sed -n 's/.*documents=\([^ ]*\).*/\1/p' <<<"$c4"))"

say "upgrade again after the rollback (roll forward)"
t0=$(now)
$SO upgrade "$cur"
record reupgrade_wall "$(since "$t0")"
c5=$(counts)
same_fields "$c1" "$c5" "re-upgrade" "${KEY_FIELDS[@]}"
SMOKE_SUFFIX=_after_reupgrade smoke document
SMOKE_SUFFIX=_after_reupgrade smoke verify
resources after-reupgrade

say "done"
collect_logs
column -t -s $'\t' "$SUTRA_TIMINGS_FILE"
