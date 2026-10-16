//! The two provisioning jobs (advisory-locked scheduler, system database context).
//!
//! * [`IdpEventConsumer`] (`idp-events`) — lifecycle events from the outbox → link updates /
//!   creations (exactly once per event via `platform.event_receipt`, consumer `platform.idp`).
//!   A transient failure leaves the event un-receipted so the next tick retries it.
//! * [`IdpSyncWorker`] (`idp-sync`) — reconciles `pending` / due `failed` links with the IdP.
//!   Each link runs in a savepoint; an unreachable IdP marks the link `failed` with exponential
//!   back-off (`attempts`, `next_attempt_at`), a duplicate / foreign account marks it `conflict`
//!   (no automatic retry; an administrator resolves and calls `…/retry`). Every outcome that
//!   changes access is audited.

use super::{
    apply_facts, diff, foreign_tenant_group, FactsOutcome, IdentityRuntime, LinkRow, Mappings,
    LINK_COLUMNS,
};
use sqlx::{PgConnection, Row};
use sutra_contracts::platform::identity::{
    AccountState, ErpSubjectType, IdpUser, MappingTarget, NewIdpUser, RequiredAction, ScopeGroup,
    SubjectLink,
};
use sutra_kernel::audit::{ActorKind, AuditEntry, AuditOutcome};
use sutra_kernel::auth::Role;
use sutra_kernel::error::KernelError;
use sutra_kernel::events::EventEnvelope;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::backoff::Backoff;
use sutra_persistence::jobs::SweepJob;
use uuid::Uuid;

/// Consumer name in `platform.event_receipt`.
pub const CONSUMER: &str = "platform.idp";

fn scope_of(link: &LinkRow) -> ExecutionScope {
    match link.institution_id {
        Some(i) => ExecutionScope::institution(TenantId(link.tenant_id), InstitutionId(i)),
        None => ExecutionScope::tenant(TenantId(link.tenant_id)),
    }
}

/// Audit entry of a provisioning step (system actor `system:idp-sync`).
pub fn audit_entry(
    link: &LinkRow,
    action: &str,
    outcome: AuditOutcome,
    details: serde_json::Value,
) -> AuditEntry {
    AuditEntry::new(scope_of(link), action, outcome)
        .actor(ActorKind::System, Some("system:idp-sync".into()))
        .target("platform.identity_link", link.id.to_string())
        .details(details)
}

// ---------------------------------------------------------------------------------------------
// Event consumer
// ---------------------------------------------------------------------------------------------

/// Job `idp-events`.
pub struct IdpEventConsumer {
    pub runtime: IdentityRuntime,
}

async fn receipt(
    conn: &mut PgConnection,
    event_id: Uuid,
    outcome: &str,
    detail: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO platform.event_receipt (consumer, event_id, outcome, detail) \
         VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING",
    )
    .bind(CONSUMER)
    .bind(event_id)
    .bind(outcome)
    .bind(detail)
    .execute(conn)
    .await?;
    Ok(())
}

impl IdpEventConsumer {
    /// Applies one event; `Ok(None)` = transient failure (retry next tick).
    async fn handle(
        &self,
        conn: &mut PgConnection,
        env: &EventEnvelope,
    ) -> Result<Option<(&'static str, Option<String>)>, sqlx::Error> {
        let rt = &self.runtime;
        let mut touched = 0usize;
        let mut notes: Vec<String> = vec![];
        for src in &rt.sources {
            if !src.event_types().contains(&env.event_type.as_str()) {
                continue;
            }
            for id in src.subjects_of(&env.event_type, env.subject.as_deref(), &env.data) {
                let facts = match src.facts(env.tenantid, id).await {
                    Ok(Some(f)) => f,
                    Ok(None) => continue,
                    Err(e) => {
                        tracing::warn!(event = %env.id, subject = %id, error = %e, "idp: facts unavailable, retrying later");
                        return Ok(None);
                    }
                };
                let create = rt.settings.auto_provision && facts.state == AccountState::Active;
                match apply_facts(
                    conn,
                    &facts,
                    rt.port.kind(),
                    create,
                    rt.settings.send_actions_email,
                    Some("system:idp-events"),
                )
                .await
                {
                    Ok(FactsOutcome::Unchanged(_)) => {}
                    Ok(FactsOutcome::Created(link)) => {
                        touched += 1;
                        sutra_persistence::audit::insert(
                            &mut *conn,
                            &audit_entry(
                                &link,
                                "platform.identity.link.created",
                                AuditOutcome::Succeeded,
                                serde_json::json!({
                                    "subjectType": link.subject_type,
                                    "subjectId": link.subject_id,
                                    "cause": env.event_type,
                                }),
                            ),
                        )
                        .await?;
                    }
                    Ok(FactsOutcome::Updated(_)) => touched += 1,
                    Err(super::LinkError::Duplicate(m)) => {
                        tracing::warn!(event = %env.id, subject = %id, reason = %m, "idp: duplicate identity refused");
                        notes.push(format!("{id}: {m}"));
                    }
                    Err(super::LinkError::Db(m)) => {
                        tracing::warn!(event = %env.id, error = %m, "idp: link update failed");
                        return Ok(None);
                    }
                }
            }
        }
        let detail = (!notes.is_empty()).then(|| notes.join("; "));
        Ok(Some((
            if touched > 0 || detail.is_some() {
                "processed"
            } else {
                "ignored"
            },
            detail,
        )))
    }
}

#[async_trait::async_trait]
impl SweepJob for IdpEventConsumer {
    fn name(&self) -> &'static str {
        "idp-events"
    }
    fn interval(&self) -> std::time::Duration {
        self.runtime.settings.poll_interval
    }
    async fn run(&self, conn: &mut PgConnection) -> Result<u64, sqlx::Error> {
        let types = self.runtime.event_types();
        if types.is_empty() {
            return Ok(0);
        }
        let rows = sqlx::query(
            "SELECT o.id, o.envelope FROM platform.outbox o \
             WHERE o.event_type = ANY($1) \
               AND o.created_at > now() - make_interval(secs => $2) \
               AND NOT EXISTS (SELECT 1 FROM platform.event_receipt r \
                               WHERE r.consumer = $3 AND r.event_id = o.id) \
               AND ($5::uuid[] IS NULL OR o.tenant_id = ANY($5)) \
             ORDER BY o.created_at, o.id LIMIT $4",
        )
        .bind(&types)
        .bind(self.runtime.settings.lookback.as_secs_f64())
        .bind(CONSUMER)
        .bind(i64::from(self.runtime.settings.batch_size.max(1)))
        .bind(self.runtime.settings.tenants.as_deref())
        .fetch_all(&mut *conn)
        .await?;
        let mut processed = 0;
        for row in rows {
            let id: Uuid = row.try_get("id")?;
            let raw: serde_json::Value = row.try_get("envelope")?;
            let env = match serde_json::from_value::<EventEnvelope>(raw) {
                Ok(e) => e,
                Err(e) => {
                    receipt(&mut *conn, id, "failed", Some(&e.to_string())).await?;
                    continue;
                }
            };
            match self.handle(&mut *conn, &env).await? {
                Some((outcome, detail)) => {
                    receipt(&mut *conn, id, outcome, detail.as_deref()).await?;
                    processed += 1;
                }
                None => continue,
            }
        }
        Ok(processed)
    }
}

// ---------------------------------------------------------------------------------------------
// Sync worker
// ---------------------------------------------------------------------------------------------

/// Job `idp-sync`.
pub struct IdpSyncWorker {
    pub runtime: IdentityRuntime,
}

/// Why one sync did not complete.
#[derive(Debug)]
pub enum SyncFailure {
    /// Transient (IdP down, 5xx, timeouts): retried with back-off.
    Retry(String),
    /// Needs an administrator (duplicate / foreign account, ERP record bound to another login).
    Conflict(String),
}

impl From<KernelError> for SyncFailure {
    fn from(e: KernelError) -> Self {
        match e {
            KernelError::Conflict(m) => SyncFailure::Conflict(m),
            other => SyncFailure::Retry(other.to_string()),
        }
    }
}

impl From<sqlx::Error> for SyncFailure {
    fn from(e: sqlx::Error) -> Self {
        match &e {
            sqlx::Error::Database(d) if d.is_unique_violation() => SyncFailure::Conflict(
                "the identity-provider account is already linked to another ERP subject \
                 (duplicate identity prevented)"
                    .into(),
            ),
            _ => SyncFailure::Retry(format!("database: {e}")),
        }
    }
}

/// What a successful sync did (audit details).
#[derive(Debug, Default)]
pub struct Applied {
    pub idp_user_id: String,
    pub created: bool,
    pub adopted: bool,
    pub enabled_changed: Option<bool>,
    pub roles_added: Vec<String>,
    pub roles_removed: Vec<String>,
    pub actions: Vec<String>,
    pub sessions_revoked: u64,
}

impl IdpSyncWorker {
    /// Finds or creates the IdP user of `link`; never adopts an account of another tenant or
    /// another person.
    async fn resolve(
        &self,
        link: &LinkRow,
        subject_ref: Option<&str>,
    ) -> Result<(IdpUser, bool, bool), SyncFailure> {
        let port = &self.runtime.port;
        if let Some(id) = &link.idp_user_id {
            if let Some(u) = port.user(id).await? {
                return Ok((u, false, false));
            }
            tracing::warn!(link = %link.id, "idp: linked account vanished at the identity provider; re-provisioning");
        }
        let mut candidate: Option<IdpUser> = None;
        if let Some(sref) = subject_ref.filter(|s| !s.contains('|')) {
            candidate = port.user(sref).await?;
        }
        if candidate.is_none() {
            candidate = port.find_by_username(&link.username).await?;
            if let (Some(u), Some(e)) = (&candidate, &link.email) {
                if u.email
                    .as_deref()
                    .is_some_and(|ue| !ue.eq_ignore_ascii_case(e))
                {
                    return Err(SyncFailure::Conflict(format!(
                        "username {} is taken at the identity provider by an account with a \
                         different e-mail address",
                        link.username
                    )));
                }
            }
        }
        if candidate.is_none() {
            if let Some(e) = &link.email {
                let found = port.find_by_email(e).await?;
                if found.len() > 1 {
                    return Err(SyncFailure::Conflict(
                        "several identity-provider accounts share this e-mail address".into(),
                    ));
                }
                candidate = found.into_iter().next();
            }
        }
        if let Some(u) = candidate {
            let foreign = port
                .groups(&u.id)
                .await?
                .iter()
                .any(|g| foreign_tenant_group(&g.path, link.tenant_id));
            if foreign {
                return Err(SyncFailure::Conflict(
                    "the matching identity-provider account belongs to another tenant".into(),
                ));
            }
            return Ok((u, false, true));
        }
        let actions = link.pending();
        let id = port
            .create_user(&NewIdpUser {
                username: link.username.clone(),
                email: link.email.clone(),
                first_name: Some(link.given_name.clone()),
                last_name: link.family_name.clone(),
                enabled: link.desired().enabled(),
                required_actions: actions.clone(),
            })
            .await?;
        let u = port.user(&id).await?.ok_or_else(|| {
            SyncFailure::Retry("created identity-provider account not readable yet".into())
        })?;
        Ok((u, true, false))
    }

    /// Groups, roles, enabled flag, sessions and required actions of an existing IdP user.
    async fn apply(
        &self,
        link: &LinkRow,
        user: &IdpUser,
        created: bool,
        mappings: &Mappings,
    ) -> Result<Applied, SyncFailure> {
        let port = &self.runtime.port;
        let id = user.id.as_str();
        let mut out = Applied {
            idp_user_id: user.id.clone(),
            created,
            ..Applied::default()
        };
        let roles = link.roles();
        let desired_targets = mappings.desired(&roles, link.institution_id);
        let managed_targets = mappings.managed();
        // 1. Tenant / institution scope group (+ mapped groups).
        let scope = ScopeGroup {
            tenant_id: link.tenant_id,
            institution_id: link.institution_id,
        };
        let scope_gid = port.ensure_scope_group(&scope).await?;
        let groups = port.groups(id).await?;
        let mut want_groups: Vec<String> = vec![scope_gid.clone()];
        for t in &desired_targets {
            if let MappingTarget::Group(path) = t {
                let gid = port.group_by_path(path).await?.ok_or_else(|| {
                    SyncFailure::Conflict(format!("mapped group {path} does not exist"))
                })?;
                want_groups.push(gid);
            }
        }
        let managed_paths: Vec<&str> = managed_targets
            .iter()
            .filter_map(|t| match t {
                MappingTarget::Group(p) => Some(p.as_str()),
                _ => None,
            })
            .collect();
        for g in &groups {
            let managed =
                g.path.starts_with("/sutra-tenant-") || managed_paths.contains(&g.path.as_str());
            if managed && !want_groups.contains(&g.id) {
                port.leave_group(id, &g.id).await?;
            }
        }
        for gid in &want_groups {
            if !groups.iter().any(|g| &g.id == gid) {
                port.join_group(id, gid).await?;
            }
        }
        // 2. Realm roles: SutraERP roles 1:1 + mapped realm roles.
        let mut desired_realm: Vec<String> = roles.iter().map(|r| r.as_str().to_owned()).collect();
        let mut managed_realm: Vec<String> =
            Role::ALL.iter().map(|r| r.as_str().to_owned()).collect();
        for t in &desired_targets {
            if let MappingTarget::RealmRole(r) = t {
                desired_realm.push(r.clone());
            }
        }
        for t in &managed_targets {
            if let MappingTarget::RealmRole(r) = t {
                managed_realm.push(r.clone());
            }
        }
        let current = port.realm_roles(id).await?;
        let (add, remove) = diff(&current, &desired_realm, &managed_realm);
        port.add_realm_roles(id, &add).await?;
        port.remove_realm_roles(id, &remove).await?;
        out.roles_added.extend(add);
        out.roles_removed.extend(remove);
        // 3. Mapped client roles, per client.
        let mut clients: Vec<&str> = managed_targets
            .iter()
            .filter_map(|t| match t {
                MappingTarget::ClientRole { client_id, .. } => Some(client_id.as_str()),
                _ => None,
            })
            .collect();
        clients.sort_unstable();
        clients.dedup();
        for client in clients {
            let pick = |ts: &[MappingTarget]| -> Vec<String> {
                ts.iter()
                    .filter_map(|t| match t {
                        MappingTarget::ClientRole { client_id, role } if client_id == client => {
                            Some(role.clone())
                        }
                        _ => None,
                    })
                    .collect()
            };
            let current = port.client_roles(id, client).await?;
            let (add, remove) = diff(&current, &pick(&desired_targets), &pick(&managed_targets));
            port.add_client_roles(id, client, &add).await?;
            port.remove_client_roles(id, client, &remove).await?;
            out.roles_added
                .extend(add.iter().map(|r| format!("{client}:{r}")));
            out.roles_removed
                .extend(remove.iter().map(|r| format!("{client}:{r}")));
        }
        // 4. Enabled flag; a disabled account or a removed role ends every session now.
        let enabled = link.desired().enabled();
        if user.enabled != enabled {
            port.set_enabled(id, enabled).await?;
            out.enabled_changed = Some(enabled);
        }
        if !enabled || !out.roles_removed.is_empty() {
            port.logout(id).await?;
            out.sessions_revoked = self
                .runtime
                .sessions
                .revoke_others(
                    &ExecutionScope::tenant(TenantId(link.tenant_id)),
                    id,
                    Uuid::nil(),
                )
                .await
                .map_err(SyncFailure::from)?;
        }
        // 5. Required actions (activation / recovery / MFA enrolment triggers).
        let actions: Vec<RequiredAction> = link.pending();
        if !actions.is_empty() && (!created || link.send_actions_email) {
            port.add_required_actions(id, &actions, link.send_actions_email && enabled)
                .await?;
        }
        out.actions = actions.iter().map(|a| a.keycloak().to_owned()).collect();
        Ok(out)
    }

    /// Re-reads the owning record and refreshes the link's lifecycle facts before syncing (a
    /// missed event can never leave an account enabled).
    async fn refresh(
        &self,
        conn: &mut PgConnection,
        link: LinkRow,
    ) -> Result<(LinkRow, Option<String>), SyncFailure> {
        let Some(t) = link.subject_type().filter(|t| *t != ErpSubjectType::Admin) else {
            return Ok((link, None));
        };
        let Some(src) = self.runtime.source(t) else {
            return Ok((link, None));
        };
        match src.facts(link.tenant_id, link.subject_id).await? {
            Some(f) => {
                let sref = f.subject_ref.clone();
                let link = match apply_facts(conn, &f, &link.idp, false, false, None).await {
                    Ok(FactsOutcome::Updated(l)) | Ok(FactsOutcome::Created(l)) => l,
                    Ok(FactsOutcome::Unchanged(l)) => l.unwrap_or(link),
                    Err(super::LinkError::Duplicate(m)) => return Err(SyncFailure::Conflict(m)),
                    Err(super::LinkError::Db(m)) => return Err(SyncFailure::Retry(m)),
                };
                Ok((link, sref))
            }
            None => {
                // The record is gone: the account must not stay usable.
                let row = sqlx::query(&format!(
                    "UPDATE platform.identity_link SET erp_state = 'disabled', \
                       desired_state = 'disabled', version = version + 1, updated_at = now() \
                     WHERE id = $1 RETURNING {LINK_COLUMNS}"
                ))
                .bind(link.id)
                .fetch_one(&mut *conn)
                .await?;
                Ok((LinkRow::from_row(&row)?, None))
            }
        }
    }

    async fn sync_one(
        &self,
        conn: &mut PgConnection,
        link: LinkRow,
    ) -> Result<(LinkRow, Applied), SyncFailure> {
        let (link, subject_ref) = self.refresh(conn, link).await?;
        let (user, created, adopted) = self.resolve(&link, subject_ref.as_deref()).await?;
        // Claim the IdP account first: the unique index refuses an account already linked to
        // another subject (any tenant) before anything changes at the IdP.
        if link.idp_user_id.as_deref() != Some(user.id.as_str()) {
            sqlx::query(
                "UPDATE platform.identity_link SET idp_user_id = $2, updated_at = now() WHERE id = $1",
            )
            .bind(link.id)
            .bind(&user.id)
            .execute(&mut *conn)
            .await?;
        }
        if let Some(src) = link.subject_type().and_then(|t| self.runtime.source(t)) {
            match src
                .link_subject(link.tenant_id, link.subject_id, &user.id)
                .await?
            {
                SubjectLink::Conflict(m) => return Err(SyncFailure::Conflict(m)),
                SubjectLink::Linked | SubjectLink::AlreadyLinked | SubjectLink::Missing => {}
            }
        }
        let mappings = Mappings::load(conn, link.tenant_id).await?;
        let mut applied = self.apply(&link, &user, created, &mappings).await?;
        applied.adopted = adopted;
        Ok((link, applied))
    }
}

#[async_trait::async_trait]
impl SweepJob for IdpSyncWorker {
    fn name(&self) -> &'static str {
        "idp-sync"
    }
    fn interval(&self) -> std::time::Duration {
        self.runtime.settings.poll_interval
    }
    async fn run(&self, conn: &mut PgConnection) -> Result<u64, sqlx::Error> {
        let s = &self.runtime.settings;
        let rows = sqlx::query(&format!(
            "SELECT {LINK_COLUMNS} FROM platform.identity_link \
             WHERE sync_status IN ('pending', 'failed') AND next_attempt_at <= now() \
               AND ($2::uuid[] IS NULL OR tenant_id = ANY($2)) \
             ORDER BY next_attempt_at, id LIMIT $1 FOR UPDATE SKIP LOCKED"
        ))
        .bind(i64::from(s.batch_size.max(1)))
        .bind(s.tenants.as_deref())
        .fetch_all(&mut *conn)
        .await?;
        let backoff = Backoff::new(s.retry_base, s.retry_max);
        let mut done = 0;
        for row in rows {
            let link = LinkRow::from_row(&row)?;
            sqlx::query("SAVEPOINT idp_link")
                .execute(&mut *conn)
                .await?;
            let result = self.sync_one(&mut *conn, link.clone()).await;
            match result {
                Ok((link, applied)) => {
                    sqlx::query("RELEASE SAVEPOINT idp_link")
                        .execute(&mut *conn)
                        .await?;
                    let roles: Vec<String> =
                        link.roles().iter().map(|r| r.as_str().to_owned()).collect();
                    sqlx::query(
                        "UPDATE platform.identity_link SET sync_status = 'synced', attempts = 0, \
                           last_error = NULL, synced_at = now(), applied_state = desired_state, \
                           applied_roles = $2, pending_actions = '{}', send_actions_email = false, \
                           version = version + 1, updated_at = now() \
                         WHERE id = $1",
                    )
                    .bind(link.id)
                    .bind(&roles)
                    .execute(&mut *conn)
                    .await?;
                    for (action, show) in [
                        ("platform.identity.provisioned", applied.created),
                        ("platform.identity.adopted", applied.adopted),
                        (
                            "platform.identity.enabled",
                            applied.enabled_changed == Some(true),
                        ),
                        (
                            "platform.identity.disabled",
                            applied.enabled_changed == Some(false),
                        ),
                        (
                            "platform.identity.roles.synced",
                            !applied.roles_added.is_empty() || !applied.roles_removed.is_empty(),
                        ),
                        (
                            "platform.identity.required_actions",
                            !applied.actions.is_empty(),
                        ),
                    ] {
                        if show {
                            sutra_persistence::audit::insert(
                                &mut *conn,
                                &audit_entry(
                                    &link,
                                    action,
                                    AuditOutcome::Succeeded,
                                    serde_json::json!({
                                        "idpUserId": applied.idp_user_id,
                                        "desiredState": link.desired_state,
                                        "rolesAdded": applied.roles_added,
                                        "rolesRemoved": applied.roles_removed,
                                        "requiredActions": applied.actions,
                                        "sessionsRevoked": applied.sessions_revoked,
                                    }),
                                ),
                            )
                            .await?;
                        }
                    }
                    done += 1;
                }
                Err(failure) => {
                    sqlx::query("ROLLBACK TO SAVEPOINT idp_link")
                        .execute(&mut *conn)
                        .await?;
                    let (status, why, retry_in) = match failure {
                        SyncFailure::Retry(m) => {
                            let attempt = u32::try_from(link.attempts.max(0)).unwrap_or(0) + 1;
                            ("failed", m, Some(backoff.delay(attempt)))
                        }
                        SyncFailure::Conflict(m) => ("conflict", m, None),
                    };
                    tracing::warn!(link = %link.id, status, reason = %why, "idp: sync did not complete");
                    sqlx::query(
                        "UPDATE platform.identity_link SET sync_status = $2, attempts = attempts + 1, \
                           last_error = $3, \
                           next_attempt_at = now() + make_interval(secs => $4), updated_at = now() \
                         WHERE id = $1",
                    )
                    .bind(link.id)
                    .bind(status)
                    .bind(why.chars().take(1000).collect::<String>())
                    .bind(retry_in.map_or(0.0, |d| d.as_secs_f64()))
                    .execute(&mut *conn)
                    .await?;
                    // Audit the first failure of a streak and every conflict (not each retry).
                    if status == "conflict" || link.attempts == 0 {
                        sutra_persistence::audit::insert(
                            &mut *conn,
                            &audit_entry(
                                &link,
                                if status == "conflict" {
                                    "platform.identity.sync.conflict"
                                } else {
                                    "platform.identity.sync.failed"
                                },
                                if status == "conflict" {
                                    AuditOutcome::Rejected
                                } else {
                                    AuditOutcome::Failed
                                },
                                serde_json::json!({ "reason": why }),
                            ),
                        )
                        .await?;
                    }
                }
            }
        }
        Ok(done)
    }
}
