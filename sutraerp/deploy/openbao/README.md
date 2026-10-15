# OpenBao (slice 9B)

Contract: `docs/batch9-contracts.md` §5. Runbook: `docs/runbooks/deploy.md`.

| File | What |
|------|------|
| `values-dev.yaml` | openbao/openbao chart **0.30.3** (OpenBao 2.7.1), dev mode + agent injector — kind CI only |
| `policy-sutraerp.hcl` | policy `sutraerp-<env>`: `read` on `kv/data/sutraerp/<env>/*` only (`__ENV__` placeholder) |
| `seed-dev.sh` | enables kv v2 at `kv/` + kubernetes auth, writes the policy and the role `sutraerp`, and the §5 groups the run needs with throwaway `openssl rand -hex` values (idempotent: existing groups are kept) |

**Annotation prefix.** The chart's injector is the upstream `hashicorp/vault-k8s` 1.7.6 image driving the
OpenBao agent image (`quay.io/openbao/openbao:2.7.1`), so pod annotations are `vault.hashicorp.com/*`
(chart value `secrets.openbao.annotationPrefix`), not `openbao.org/*`. The kind smoke stage asserts
the `vault.hashicorp.com/agent-inject-status: injected` annotation and a successful `vault-agent-init`
container on the api and migrate pods.

**Role.** `auth/kubernetes/role/sutraerp` binds ServiceAccounts `sutraerp-api`, `sutraerp-migrate`,
`sutraerp-backup` (contract) and `sutraerp-postgres` (bundled CI / pilot database only, reads
`POSTGRES_PASSWORD`) in the release namespace.

**Extra key (contract addition).** The `database` group carries `POSTGRES_PASSWORD` for the bundled
PostgreSQL StatefulSet (the superuser it is initialised with); the api's rendered dotenv excludes it
and `SUTRA_MIGRATION_DATABASE_URL` (owner), the migrate Job's includes the owner URL.
