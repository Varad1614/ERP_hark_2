# Runbook — security operations (Batch 9 · slice 9D)

Companion to [`docs/security/threat-model.md`](../security/threat-model.md) and the findings register
[`docs/security/findings.md`](../security/findings.md). Secrets layout: contract §5 (OpenBao KV v2
`kv/sutraerp/<env>/…`); deploy procedures: `docs/runbooks/deploy.md` (9B).

## 1. Where security signals come from

| Signal | Source | Who looks |
|--------|--------|-----------|
| RustSec advisories, licences, sources | `Security (…)` job: cargo-deny + cargo-audit (every push; run it weekly by `workflow_dispatch` on `erp_merge` once live) | on-call engineer |
| npm advisories (production deps) | same job: `pnpm audit --prod --audit-level high` | on-call engineer |
| Secrets in git | same job: gitleaks over the full history | whoever pushed; security lead |
| Image CVEs | `Image scan + ZAP baseline (…)`: Trivy + Grype on api + web | on-call engineer |
| Web / API passive DAST | same job: ZAP baseline (fails on High; Medium listed in the job summary) | security lead |
| Performance regressions | `Performance gate (k6)` / `Capacity (k6)` | API owner |
| Tampering | audit chain verify (`POST /api/v1/platform/audit/chain/verify`, 9A alerts) | security lead |

## 2. Vulnerability triage SLA

The clock starts when the finding is first visible (CI failure, advisory publication, report).

| Severity | Examples | Triage | Fix or mitigate in production |
|----------|----------|--------|-------------------------------|
| Critical | RCE in a reachable dependency, auth bypass, cross-tenant read, leaked production secret | 4 h | 24 h (hotfix tag; rotate secrets per §4 first) |
| High | reachable High CVE with a fix, ZAP High, privilege escalation inside a tenant, payment integrity (SEC-9D-003) | 1 business day | 7 days |
| Medium | defence-in-depth gaps (SEC-9D-004/005), Medium ZAP alerts | 5 business days | 30 days or next release |
| Low / Info | hardening, hygiene | next planning | 90 days or accept |

For every finding:

1. **Confirm reachability** (is the vulnerable code compiled and called? cargo-deny's
   feature-resolved graph answers "compiled" for Rust; for images, is the package used at runtime?).
2. **Record** a row in `docs/security/findings.md` (`SEC-<batch>-<nnn>`, severity, component, owner).
3. **Fix** by upgrading (patches outside the owner's files are marked `// <slice>: security` with the
   finding id), or **accept** with a reason and an expiry ≤ 6 months: add the ignore to `deny.toml`,
   the `cargo-audit --ignore` list, `.trivyignore` (`CVE-… exp:YYYY-MM-DD`) or `.zap/rules.tsv`
   **and** the register row in the same commit.
4. **Expired acceptance** = re-justify or remove the ignore; never extend silently.
5. Lockfiles change only through CI (`cargo-lock` artifact of `SutraERP lockfile`); never resolve
   `Cargo.lock` by hand.
6. Base images: both scanners must pass; when one scanner reports a fixed CVE the other misses, the
   finding is real until disproved (SEC-9D-015 was Grype-only).

## 3. Incident response (summary)

1. Contain: revoke affected sessions (`POST /api/v1/auth/sessions/revoke-others` per account, or
   delete the tenant's rows from the PostgreSQL session store for a tenant-wide reset), scale the API
   to zero if data is leaking, block at the ingress.
2. Preserve evidence: export `platform.audit_log` + `audit_chain_*` + `audit_anchor` for the window,
   run chain verify and store the output, keep pod logs (Loki) and CI logs.
3. Eradicate + rotate (§4); redeploy by digest from a signed image.
4. Notify: DPDP Act 2023 breach intimation to the Data Protection Board and affected data principals
   when personal data is involved; institution administrators per the pilot agreement.
5. Post-incident review within 5 business days; add the root cause to the findings register.

## 4. Secret rotation in OpenBao

All application secrets live under `kv/sutraerp/<env>/` (contract §5). The OpenBao Agent renders
them into `/vault/secrets/sutra.env`, which the API reads at start-up (`SUTRA_ENV_FILE`), so a
rotation is **write a new version → rolling restart**.

```bash
export BAO_ADDR=https://openbao.<cluster> NS=sutraerp ENV=pilot
bao login -method=oidc                                  # operator identity, never a shared token
bao kv metadata get kv/sutraerp/$ENV/app                # current version (for rollback)
bao kv patch kv/sutraerp/$ENV/app SUTRA_CSRF_KEY="$(openssl rand -hex 32)"
kubectl -n $NS rollout restart deploy/sutraerp-api && kubectl -n $NS rollout status deploy/sutraerp-api
# rollback: bao kv rollback -version=<n> kv/sutraerp/$ENV/app ; destroy old versions after 7 days
```

| Secret (path#key) | Rotation | Side effects |
|-------------------|----------|--------------|
| `database#SUTRA_DB_RUNTIME_PASSWORD` (+ `DATABASE_URL`) | 90 days or on suspicion | Run the migrate Job with the new password first (`migrate` creates / updates the runtime role), then restart the API; old connections drain |
| `database#SUTRA_MIGRATION_DATABASE_URL` (owner) | 90 days | `ALTER ROLE` by the DBA, update the path; the next migrate Job uses it |
| `app#SUTRA_CSRF_KEY` | 180 days or on suspicion | Outstanding sign-in CSRF tokens fail once (users reload the sign-in page); sessions survive |
| `app#SUTRA_MFA_ENCRYPTION_KEY` | only on compromise | Encrypts stored TOTP secrets: rotating without re-encryption invalidates every enrolled authenticator — plan a forced re-enrolment, never rotate casually |
| `oidc#SUTRA_OIDC_CLIENT_SECRET` | 180 days | Regenerate in Keycloak (client `sutra-web` → Credentials), patch, restart; in-flight logins retry |
| `smtp#…` | provider policy | — |
| `flowable#…`, `opensearch#…` (9C) | 180 days | Update the service user first, then the path, then restart |
| `backup#…` (9C) | 180 days | Old backups stay decryptable only with the old age identity — keep it offline until retention ends |
| `audit#SUTRA_AUDIT_ANCHOR_KEY` | yearly, or on compromise (§5) | Always with a **new** `SUTRA_AUDIT_ANCHOR_KEY_ID`; old anchors verify only while the old key is available to the verifier |

Leaked secret in git: rotate first, then purge history only if the repository is public; gitleaks
keeps failing until the commit is gone or the revoked value is allow-listed with a findings row.

## 5. Compromise of the audit anchor key

With the anchor key, an attacker who can also write the database could rewrite `audit_log`,
recompute the chain and forge matching anchors.

1. **Freeze the evidence**: export `platform.audit_anchor` (all rows with `key_id`) and every
   tenant's `audit_chain_head` to write-once storage outside the cluster *before* any change; record
   the export hash in the incident ticket.
2. **Verify** every tenant's chain with the current key (`POST /api/v1/platform/audit/chain/verify`
   without a range). The last anchor per tenant that verifies **and** predates the suspected
   compromise is the last trusted point.
3. **Rotate**: `bao kv put kv/sutraerp/$ENV/audit SUTRA_AUDIT_ANCHOR_KEY="$(openssl rand -hex 32)" SUTRA_AUDIT_ANCHOR_KEY_ID=k2`,
   rolling-restart the API; the anchor job writes fresh anchors under `k2` at its next tick
   (`SUTRA_AUDIT_ANCHOR_SECS`).
4. **Re-establish trust** for the window between the last trusted anchor and the rotation: compare
   the chain entries with an independent copy (logical backups, contract §11; Loki audit logs);
   differences are tampering candidates — record them.
5. **Destroy** the compromised key version in OpenBao when the incident closes (keep a sealed
   offline copy only if counsel requires old anchors to stay verifiable).
6. Root cause: who could read `kv/sutraerp/<env>/audit`? Review the OpenBao audit device and the
   Kubernetes auth role bindings (only `sutraerp-api` needs it).

## 6. Routine checks (monthly)

- All CI security jobs green on `erp_merge`; no acceptance past its expiry in `findings.md`.
- `bao kv metadata get` on every path: versions older than the rotation table → rotate.
- Audit chain verify for every tenant; anchors advancing.
- Review Medium ZAP alerts in the latest `Image scan + ZAP baseline` summary.
