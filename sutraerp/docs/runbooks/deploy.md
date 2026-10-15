# Runbook — deploy SutraERP (Helm · OpenBao · signed images)

Owner: platform (slice 9B). Contract: `docs/batch9-contracts.md` §5 (OpenBao paths), §10 (chart
values, images, workflows, kind stages). Proved end-to-end in GitHub Actions by
`.github/workflows/sutraerp-deploy.yml` (`Kind (install · migrate · smoke · telemetry · DR)`) on a
throwaway kind cluster; there is no long-lived cluster.

## 1. What gets deployed

| Object | Name (release `sutraerp`) | Notes |
|--------|---------------------------|-------|
| Deployment + Service | `sutraerp-api` :8080 | probes `/health/live` (startup, liveness), `/health/ready` (readiness); `envFrom` ConfigMap `sutraerp-env`; credentials only from `/vault/secrets/sutra.env` (`SUTRA_ENV_FILE`) |
| Service | `sutraerp-api-metrics` :9464 | Prometheus only (rendered by the observability templates, 9A); never behind the ingress |
| Deployment + Service | `sutraerp-web` :8080 | nginx-unprivileged SPA, `/api/` proxied to `sutraerp-api` (ConfigMap `sutraerp-web-nginx`) |
| Job | `sutraerp-migrate` (hook) / `sutraerp-migrate-r<rev>` | `sutra-api migrate` as the schema owner; provisions the runtime role |
| StatefulSets | `sutraerp-postgres`, `sutraerp-nats`, `sutraerp-valkey` | only with `<dep>.mode=bundled` (CI, small pilots) |
| NetworkPolicies | `sutraerp-default-deny` + per component | default deny, explicit allows (see `templates/core/networkpolicy.yaml`) |
| PDB / HPA / Ingress | `sutraerp-api`, `sutraerp-web` / `sutraerp-api` / `sutraerp` | HPA + ingress off by default, on in `values-pilot.yaml` |
| ServiceAccounts | `sutraerp-api`, `-migrate`, `-backup` (`-postgres` when bundled) | bound to the OpenBao kubernetes-auth role `sutraerp` |

The migrate Job is a `pre-install,pre-upgrade` hook when PostgreSQL is external (the normal case):
an upgrade never rolls new pods before the schema is migrated, and a failed migration fails the
`helm upgrade` with the old pods still serving. With `postgres.mode=bundled` the Job is a plain
resource per revision (a hook would run before the bundled database exists) that waits for the
database; install with `--wait --wait-for-jobs`.

## 2. Before every deploy: verify the images

Images are built and pushed only by `sutraerp-ci.yml` on `erp_merge` (`edge`, `sha-<12>`) and `v*`
tags (`vX.Y.Z`, `X.Y`, `sha-<12>`), signed keyless with the workflow's GitHub OIDC identity, with a
syft SPDX SBOM attestation and BuildKit max provenance. Deploy **by digest** and verify first:

```bash
IMG=ghcr.io/varad1614/sutraerp-api@sha256:<digest>          # from the job summary of the CI run
ID='^https://github.com/Varad1614/ERP_hark_2/.github/workflows/sutraerp-ci.yml@refs/(heads/erp_merge|tags/v.*)$'
cosign verify --certificate-identity-regexp "$ID" \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com "$IMG"
cosign verify-attestation --type spdxjson --certificate-identity-regexp "$ID" \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com "$IMG" | jq -r .payload | base64 -d | jq '.predicate.packages | length'
docker buildx imagetools inspect "$IMG" --format '{{ json .Provenance }}' | jq '.SLSA.invocation.configSource'
```

Repeat for `sutraerp-web`. A failed verify = do not deploy. In-cluster enforcement: set
`global.verifyImages.enabled=true` and install a Kyverno `verifyImages` rule or the sigstore
policy-controller with the same identity regexp and issuer (operator task; the chart only carries
the values).

## 3. OpenBao bootstrap (once per environment)

Production OpenBao is operator-run (HA raft, TLS, auto-unseal); the kind job uses dev mode
(`deploy/openbao/values-dev.yaml`). On the operator's OpenBao, as an admin:

```bash
ENV=production NS=sutraerp
bao secrets enable -path=kv -version=2 kv
bao auth enable kubernetes
bao write auth/kubernetes/config kubernetes_host=https://<api-server>:443   # in-cluster OpenBao: https://kubernetes.default.svc:443
sed "s/__ENV__/$ENV/g" deploy/openbao/policy-sutraerp.hcl | bao policy write sutraerp-$ENV -
bao write auth/kubernetes/role/sutraerp \
  bound_service_account_names=sutraerp-api,sutraerp-migrate,sutraerp-backup \
  bound_service_account_namespaces=$NS token_policies=sutraerp-$ENV token_ttl=1h
# The §5 groups (real values; never in git, never Helm values):
bao kv put -mount=kv sutraerp/$ENV/database DATABASE_URL='postgres://sutra_api:…@pg:5432/sutra?sslmode=require' \
  SUTRA_MIGRATION_DATABASE_URL='postgres://sutra:…@pg:5432/sutra?sslmode=require' \
  SUTRA_DB_RUNTIME_USER=sutra_api SUTRA_DB_RUNTIME_PASSWORD='…'
bao kv put -mount=kv sutraerp/$ENV/app SUTRA_CSRF_KEY=$(openssl rand -hex 32) SUTRA_MFA_ENCRYPTION_KEY=$(openssl rand -hex 32)
bao kv put -mount=kv sutraerp/$ENV/oidc SUTRA_OIDC_CLIENT_SECRET='…'
bao kv put -mount=kv sutraerp/$ENV/smtp SUTRA_SMTP_USERNAME='…' SUTRA_SMTP_PASSWORD='…'
bao kv put -mount=kv sutraerp/$ENV/audit SUTRA_AUDIT_ANCHOR_KEY=$(openssl rand -hex 32) SUTRA_AUDIT_ANCHOR_KEY_ID=k1
# + flowable / opensearch / backup when those are enabled.
```

`secrets.openbao.groups` must list exactly the groups that exist: the agent init container waits
for every listed path. The injector is the upstream vault-k8s shipped by the openbao chart, so the
pod annotations are `vault.hashicorp.com/*` (`secrets.openbao.annotationPrefix`). The agent runs as
an init container only (`agent-pre-populate-only`); rotating a value = `bao kv put …` then
`kubectl rollout restart deploy/sutraerp-api` (and the next migrate Job picks it up).

Clusters without OpenBao: `secrets.mode=kubernetes` + `secrets.existingSecret=<Secret with the §5
keys>` (created out of band, e.g. External Secrets / sealed-secrets). Not used in CI.

## 4. Install

```bash
helm upgrade --install sutraerp deploy/helm/sutraerp -n sutraerp --create-namespace \
  -f deploy/helm/sutraerp/values-pilot.yaml \
  --set image.api.digest=sha256:<api> --set image.web.digest=sha256:<web> \
  --wait --wait-for-jobs --timeout 15m
kubectl -n sutraerp get pods
kubectl -n sutraerp logs job/sutraerp-migrate        # hook Job (kept until the next upgrade)
SUTRA_CI_MODE=kind SUTRA_CI_API_URL=https://erp.<host> SUTRA_CI_WEB_URL=https://erp.<host> \
  SUTRA_CI_LOGIN=<an admin> bash deploy/ci/smoke.sh   # dev-auth sign-in only works with authMode=dev
```

Values for a real cluster (`values-pilot.yaml` is the template; every `CHANGE-ME`):
`global.environment=production`; `api.authMode=keycloak` + `api.extraEnv.SUTRA_OIDC_*`;
`api.publicUrl`; `ingress.host` + TLS secret (cert-manager); `postgres|nats|valkey.mode=external`
with their URLs; `networkPolicy.externalCidrs` = the managed-services subnet and
`networkPolicy.extraEgress` for the IdP / SMTP; `networkPolicy.ingressNamespaceSelector` = the
ingress controller's namespace; `observability.otlpEndpoint` = the cluster collector.

## 5. Upgrade

1. Verify the new digests (§2).
2. `helm diff upgrade …` (plugin) or `helm template … | kubectl diff -f -`.
3. `helm upgrade sutraerp deploy/helm/sutraerp -n sutraerp -f values-pilot.yaml --set image.api.digest=… --set image.web.digest=… --wait --wait-for-jobs --timeout 15m`
   — the pre-upgrade migrate hook runs first; the api rolls with `maxUnavailable: 0`.
4. Smoke (§4) and check the dashboards (`docs/runbooks/observability.md`).

Migrations are forward-only; take a backup before an upgrade that carries migrations
(`docs/runbooks/backup-dr.md`).

## 6. Rollback

- Application only (no new migrations in the release): `helm rollback sutraerp <rev> -n sutraerp --wait`.
  A rollback does not run the pre-upgrade migrate hook (there is no pre-rollback hook), so the schema
  stays at the newer version; that is safe when the release only added columns / tables (additive
  migrations, the project rule).
- Release carried migrations that the old code cannot read: restore the pre-upgrade backup
  (`docs/runbooks/backup-dr.md`), then `helm rollback`.
- `helm history sutraerp -n sutraerp` lists revisions with their image digests (`--set` values).

## 7. CI proof (what the workflows show)

| Job | Proves |
|-----|--------|
| `Helm (lint · template)` | `helm lint --strict` + `kubeconform -strict` (k8s 1.31.0) for default / kind / pilot values; no Secret or credential rendered; digest wins over tag; kubernetes-secret fallback; values schema; hook vs plain migrate Job; shellcheck of the deploy scripts |
| `Kind (install · migrate · smoke · telemetry · DR)` | api (docker profile) + web images; kind v0.31.0 / k8s 1.31.14 (kindnet enforces NetworkPolicy); OpenBao 2.7.1 dev + injector, `seed-dev.sh` (policy read-only + scoped); `helm install --wait --wait-for-jobs`, then `helm upgrade` (new migrate revision); stages `smoke.sh` → `check-telemetry.sh` (9A) → `seed-pilot.sh` (9E) → `dr-drill.sh` (9C); resource headroom in the job summary; `kind-diagnostics` artifact on failure |
| `Compose boot (docker compose --profile app)` | the compose `app` profile builds and boots (migrate → api → web), `smoke.sh` in compose mode, `demo-seed` runs twice (idempotent) |
| `Container images (build · push · sign)` (`sutraerp-ci.yml`) | release build of api + web with SBOM + max provenance; on branches: push to a local `registry:2`, sign / attest SPDX / verify with a throwaway key and a wrong-key rejection; on `erp_merge` / `v*`: GHCR push + keyless sign + attest + verify against the workflow identity |
