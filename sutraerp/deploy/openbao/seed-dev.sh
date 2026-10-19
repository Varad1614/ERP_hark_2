#!/usr/bin/env bash
# Seed a DEV OpenBao (kind CI) for SutraERP — 9B, docs/batch9-contracts.md §5.
#   * KV v2 mount `kv`, kubernetes auth at `auth/kubernetes`
#   * policy `sutraerp-<env>` (deploy/openbao/policy-sutraerp.hcl)
#   * role `sutraerp` bound to the ServiceAccounts sutraerp-{api,migrate,backup,postgres} in $SUTRA_CI_NS
#   * the secret groups in $SEED_GROUPS with throwaway values (openssl rand -hex); a group that
#     already exists is left untouched, so re-running keeps the passwords the database was created with.
# Inputs: BAO_TOKEN (dev root token, required), OPENBAO_NS (openbao), OPENBAO_POD (openbao-0),
#   SUTRA_CI_NS (sutraerp), SUTRA_ENV (development), SUTRA_RELEASE (sutraerp),
#   SEED_GROUPS ("database app audit flowable"), KUBECONFIG.
# Output: one "ok <check>" line per step; non-zero exit on the first failure; --dry-run prints the plan.
set -euo pipefail
cd "$(dirname "$0")"

NS=${OPENBAO_NS:-openbao}
POD=${OPENBAO_POD:-openbao-0}
APP_NS=${SUTRA_CI_NS:-sutraerp}
ENV_NAME=${SUTRA_ENV:-development}
REL=${SUTRA_RELEASE:-sutraerp}
GROUPS_TO_SEED=${SEED_GROUPS:-database app audit flowable}
PREFIX="sutraerp/${ENV_NAME}"

if [[ "${1:-}" == "--dry-run" ]]; then
  echo "seed-dev: kv-v2 at kv/, kubernetes auth, policy sutraerp-${ENV_NAME}, role sutraerp (ns ${APP_NS}), groups: ${GROUPS_TO_SEED}"
  exit 0
fi
: "${BAO_TOKEN:?BAO_TOKEN (dev root token) is required}"

bao_as() { # token args...
  local tok=$1; shift
  kubectl exec -i -n "$NS" "$POD" -- env BAO_ADDR=http://127.0.0.1:8200 BAO_TOKEN="$tok" bao "$@"
}
bao() { bao_as "$BAO_TOKEN" "$@"; }
hex() { openssl rand -hex "${1:-32}"; }

kubectl wait -n "$NS" --for=condition=Ready "pod/$POD" --timeout=180s >/dev/null
echo "ok openbao-ready"

# INT: "already in use" counts as done — a transient `kubectl exec` hiccup in the list call made the
# re-seed (kind helm-upgrade step) try to enable an existing mount and abort.
enable_once() { # what args...
  local out
  out=$(bao "$@" 2>&1) && return 0
  grep -q "already in use" <<<"$out" && return 0
  echo "$out" >&2; return 1
}
if ! bao secrets list -format=json | grep -q '"kv/"'; then
  enable_once secrets enable -path=kv -version=2 kv
fi
echo "ok kv-v2-mount"

if ! bao auth list -format=json | grep -q '"kubernetes/"'; then
  enable_once auth enable kubernetes
fi
bao write auth/kubernetes/config kubernetes_host=https://kubernetes.default.svc:443 >/dev/null
echo "ok kubernetes-auth"

sed "s/__ENV__/${ENV_NAME}/g" policy-sutraerp.hcl | bao policy write "sutraerp-${ENV_NAME}" - >/dev/null
echo "ok policy sutraerp-${ENV_NAME}"

bao write auth/kubernetes/role/sutraerp \
  bound_service_account_names="${REL}-api,${REL}-migrate,${REL}-backup,${REL}-postgres" \
  bound_service_account_namespaces="${APP_NS}" \
  token_policies="sutraerp-${ENV_NAME}" token_ttl=1h token_max_ttl=4h >/dev/null
echo "ok role sutraerp"

PG_HOST="${REL}-postgres.${APP_NS}.svc"
for g in $GROUPS_TO_SEED; do
  if bao kv get -mount=kv "${PREFIX}/${g}" >/dev/null 2>&1; then
    echo "ok group ${g} (kept)"
    continue
  fi
  case "$g" in
    database)
      pg=$(hex 24); rt=$(hex 24)
      bao kv put -mount=kv "${PREFIX}/database" \
        POSTGRES_PASSWORD="$pg" \
        SUTRA_MIGRATION_DATABASE_URL="postgres://sutra:${pg}@${PG_HOST}:5432/sutra" \
        SUTRA_DB_RUNTIME_USER="sutra_api" \
        SUTRA_DB_RUNTIME_PASSWORD="$rt" \
        DATABASE_URL="postgres://sutra_api:${rt}@${PG_HOST}:5432/sutra" >/dev/null ;;
    app)
      bao kv put -mount=kv "${PREFIX}/app" SUTRA_CSRF_KEY="$(hex 32)" SUTRA_MFA_ENCRYPTION_KEY="$(hex 32)" >/dev/null ;;
    oidc)
      bao kv put -mount=kv "${PREFIX}/oidc" SUTRA_OIDC_CLIENT_SECRET="$(hex 24)" >/dev/null ;;
    smtp)
      bao kv put -mount=kv "${PREFIX}/smtp" SUTRA_SMTP_USERNAME="sutra-ci" SUTRA_SMTP_PASSWORD="$(hex 24)" >/dev/null ;;
    audit)
      bao kv put -mount=kv "${PREFIX}/audit" SUTRA_AUDIT_ANCHOR_KEY="$(hex 32)" SUTRA_AUDIT_ANCHOR_KEY_ID="ci-k1" >/dev/null ;;
    flowable)
      bao kv put -mount=kv "${PREFIX}/flowable" SUTRA_FLOWABLE_PASSWORD="$(hex 24)" FLOWABLE_DB_PASSWORD="$(hex 24)" >/dev/null ;;
    opensearch)
      bao kv put -mount=kv "${PREFIX}/opensearch" SUTRA_OPENSEARCH_USER="admin" SUTRA_OPENSEARCH_PASSWORD="$(hex 24)" >/dev/null ;;
    backup)
      bao kv put -mount=kv "${PREFIX}/backup" SUTRA_BACKUP_S3_ACCESS_KEY="$(hex 10)" SUTRA_BACKUP_S3_SECRET_KEY="$(hex 24)" >/dev/null ;;
    payments)  # INT: SEC-9D-003 — only needed when SUTRA_PAYMENT_GATEWAY=mock in production
      bao kv put -mount=kv "${PREFIX}/payments" SUTRA_PAYMENT_WEBHOOK_SECRET="$(hex 32)" >/dev/null ;;
    *) echo "unknown group $g" >&2; exit 1 ;;
  esac
  echo "ok group ${g}"
done

# The policy really is read-only and scoped: a token for it can read a group but not write one.
tok=$(bao token create -policy="sutraerp-${ENV_NAME}" -ttl=5m -field=token)
first=${GROUPS_TO_SEED%% *}
bao_as "$tok" kv get -mount=kv "${PREFIX}/${first}" >/dev/null
if bao_as "$tok" kv put -mount=kv "${PREFIX}/${first}" X=y >/dev/null 2>&1; then
  echo "policy sutraerp-${ENV_NAME} allows writes" >&2; exit 1
fi
if bao_as "$tok" kv get -mount=kv "sutraerp/other-env/${first}" >/dev/null 2>&1; then
  echo "policy sutraerp-${ENV_NAME} reads another environment" >&2; exit 1
fi
echo "ok policy-read-only-scoped"
