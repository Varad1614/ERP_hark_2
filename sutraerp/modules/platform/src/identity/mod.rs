//! Identity provisioning (Part 2 · Stage 4.3, `docs/readiness/part2/s4-idp.md`).
//!
//! SutraERP decides **who** may sign in and **with which roles**; Keycloak keeps credentials,
//! second factors and the sign-in itself. This module keeps the two in step:
//!
//! * [`platform.identity_link`] — one row per ERP subject (student, staff member, platform
//!   administrator). It is the **duplicate guard** (one identity per subject; one subject per IdP
//!   user, across tenants; one username / e-mail per IdP) and the **desired state** (account
//!   state, roles, required actions) that the [`sync::IdpSyncWorker`] reconciles with the IdP —
//!   retried with exponential back-off while the IdP is down, idempotent because every step
//!   compares the IdP's current state with the desired one before changing it.
//! * [`sync::IdpEventConsumer`] — reads the owning modules' lifecycle events from the outbox
//!   (`student.status_changed`, staff / engagement / separation events), asks the owning module
//!   for the subject's current facts ([`IdentitySubjectSource`]) and updates — or, for a newly
//!   active subject, creates — the link. Exactly once per event (`platform.event_receipt`).
//! * [`http`] — admin API: provision, administrative suspension / re-enabling, required actions
//!   (activation, password recovery, MFA enrolment), role requests with maker-checker for
//!   privileged roles, ERP-role → IdP mappings, retry. Permission + step-up + SoD + audit.
//! * [`keycloak_admin`] — Keycloak Admin REST adapter (least-privilege service-account client);
//!   [`memory`] — in-memory adapter for tests (refused in production).
//!
//! [`platform.identity_link`]: ../../../../migrations/20261231004300_platform_identity_link.sql

pub mod http;
pub mod keycloak_admin;
pub mod memory;
pub mod sync;

use chrono::{DateTime, Utc};
use sqlx::{PgConnection, Row};
use std::sync::Arc;
use std::time::Duration;
use sutra_contracts::platform::auth::SessionStore;
use sutra_contracts::platform::identity::{
    generated_username, AccountState, ErpSubjectType, IdentityFacts, IdentityProvisioningPort,
    IdentitySubjectSource, MappingTarget, RequiredAction,
};
use sutra_kernel::auth::Role;
use sutra_kernel::error::KernelError;
use uuid::Uuid;

/// Settings of the provisioning runtime (`SUTRA_IDP_*`).
#[derive(Clone, Debug)]
pub struct IdentitySettings {
    /// Create an identity automatically when a subject becomes active (`SUTRA_IDP_AUTO_PROVISION`,
    /// default `true`); otherwise only existing links follow lifecycle changes.
    pub auto_provision: bool,
    /// Ask the IdP to e-mail "execute actions" links on activation (`SUTRA_IDP_SEND_ACTIONS_EMAIL`;
    /// needs SMTP configured in the realm).
    pub send_actions_email: bool,
    pub poll_interval: Duration,
    pub batch_size: u32,
    /// Ignore outbox events older than this.
    pub lookback: Duration,
    /// Back-off of a failed sync (base, max).
    pub retry_base: Duration,
    pub retry_max: Duration,
}

impl Default for IdentitySettings {
    fn default() -> Self {
        Self {
            auto_provision: true,
            send_actions_email: false,
            poll_interval: Duration::from_secs(5),
            batch_size: 50,
            lookback: Duration::from_secs(7 * 24 * 3600),
            retry_base: Duration::from_secs(10),
            retry_max: Duration::from_secs(3600),
        }
    }
}

/// Everything the jobs and the admin API need.
#[derive(Clone)]
pub struct IdentityRuntime {
    pub port: Arc<dyn IdentityProvisioningPort>,
    pub sources: Vec<Arc<dyn IdentitySubjectSource>>,
    pub sessions: Arc<dyn SessionStore>,
    pub settings: IdentitySettings,
}

impl IdentityRuntime {
    pub fn source(&self, t: ErpSubjectType) -> Option<&Arc<dyn IdentitySubjectSource>> {
        self.sources.iter().find(|s| s.subject_type() == t)
    }

    /// Every event type some source listens to (deduplicated).
    pub fn event_types(&self) -> Vec<&'static str> {
        let mut v: Vec<&'static str> = self
            .sources
            .iter()
            .flat_map(|s| s.event_types().iter().copied())
            .collect();
        v.sort_unstable();
        v.dedup();
        v
    }
}

// ---------------------------------------------------------------------------------------------
// Pure rules (unit-tested)
// ---------------------------------------------------------------------------------------------

/// Account state from the record's lifecycle and an administrative hold: a hold suspends an
/// active account, never re-enables a suspended / disabled one.
pub fn desired_state(erp: AccountState, admin_hold: bool) -> AccountState {
    match (erp, admin_hold) {
        (AccountState::Active, true) => AccountState::Suspended,
        (s, _) => s,
    }
}

/// Effective SutraERP roles: base ∪ granted, known names only, in [`Role::ALL`] order.
pub fn effective_roles(base: &[String], granted: &[String]) -> Vec<Role> {
    Role::ALL
        .into_iter()
        .filter(|r| base.iter().any(|b| b == r.as_str()) || granted.iter().any(|g| g == r.as_str()))
        .collect()
}

/// Required actions of a new identity: verify the e-mail (when there is one) and set a password
/// (activation); enrol TOTP when any role is privileged.
pub fn initial_actions(email: Option<&str>, roles: &[Role]) -> Vec<RequiredAction> {
    let mut v = vec![];
    if email.is_some_and(|e| !e.trim().is_empty()) {
        v.push(RequiredAction::VerifyEmail);
    }
    v.push(RequiredAction::UpdatePassword);
    if roles.iter().any(Role::is_privileged) {
        v.push(RequiredAction::ConfigureTotp);
    }
    v
}

/// `add` = desired − current, `remove` = (current ∩ managed) − desired. Targets outside the
/// managed set (Keycloak defaults, roles set by hand for other systems) are never touched.
pub fn diff(
    current: &[String],
    desired: &[String],
    managed: &[String],
) -> (Vec<String>, Vec<String>) {
    let mut add: Vec<String> = desired
        .iter()
        .filter(|d| !current.contains(d))
        .cloned()
        .collect();
    let mut remove: Vec<String> = current
        .iter()
        .filter(|c| managed.contains(c) && !desired.contains(c))
        .cloned()
        .collect();
    add.sort();
    add.dedup();
    remove.sort();
    remove.dedup();
    (add, remove)
}

/// Whether a group path is one of SutraERP's tenant-scope groups of **another** tenant.
pub fn foreign_tenant_group(path: &str, tenant: Uuid) -> bool {
    let own = format!(
        "/{}",
        sutra_contracts::platform::identity::ScopeGroup::tenant_group_name(tenant)
    );
    path.starts_with("/sutra-tenant-") && !(path == own || path.starts_with(&format!("{own}/")))
}

// ---------------------------------------------------------------------------------------------
// identity_link repository
// ---------------------------------------------------------------------------------------------

/// Columns of `platform.identity_link` as read by [`LinkRow::from_row`].
pub const LINK_COLUMNS: &str = "id, tenant_id, institution_id, subject_type, subject_id, idp, \
    idp_user_id, username, email, given_name, family_name, erp_state, admin_hold, hold_reason, \
    desired_state, base_roles, granted_roles, pending_actions, send_actions_email, sync_status, \
    attempts, next_attempt_at, last_error, synced_at, applied_state, applied_roles, created_by, \
    version, created_at, updated_at";

/// One `platform.identity_link` row.
#[derive(Clone, Debug, PartialEq)]
pub struct LinkRow {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub institution_id: Option<Uuid>,
    pub subject_type: String,
    pub subject_id: Uuid,
    pub idp: String,
    pub idp_user_id: Option<String>,
    pub username: String,
    pub email: Option<String>,
    pub given_name: String,
    pub family_name: Option<String>,
    pub erp_state: String,
    pub admin_hold: bool,
    pub hold_reason: Option<String>,
    pub desired_state: String,
    pub base_roles: Vec<String>,
    pub granted_roles: Vec<String>,
    pub pending_actions: Vec<String>,
    pub send_actions_email: bool,
    pub sync_status: String,
    pub attempts: i32,
    pub next_attempt_at: DateTime<Utc>,
    pub last_error: Option<String>,
    pub synced_at: Option<DateTime<Utc>>,
    pub applied_state: Option<String>,
    pub applied_roles: Option<Vec<String>>,
    pub created_by: Option<String>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl LinkRow {
    pub fn from_row(r: &sqlx::postgres::PgRow) -> Result<Self, sqlx::Error> {
        Ok(Self {
            id: r.try_get("id")?,
            tenant_id: r.try_get("tenant_id")?,
            institution_id: r.try_get("institution_id")?,
            subject_type: r.try_get("subject_type")?,
            subject_id: r.try_get("subject_id")?,
            idp: r.try_get("idp")?,
            idp_user_id: r.try_get("idp_user_id")?,
            username: r.try_get("username")?,
            email: r.try_get("email")?,
            given_name: r.try_get("given_name")?,
            family_name: r.try_get("family_name")?,
            erp_state: r.try_get("erp_state")?,
            admin_hold: r.try_get("admin_hold")?,
            hold_reason: r.try_get("hold_reason")?,
            desired_state: r.try_get("desired_state")?,
            base_roles: r.try_get("base_roles")?,
            granted_roles: r.try_get("granted_roles")?,
            pending_actions: r.try_get("pending_actions")?,
            send_actions_email: r.try_get("send_actions_email")?,
            sync_status: r.try_get("sync_status")?,
            attempts: r.try_get("attempts")?,
            next_attempt_at: r.try_get("next_attempt_at")?,
            last_error: r.try_get("last_error")?,
            synced_at: r.try_get("synced_at")?,
            applied_state: r.try_get("applied_state")?,
            applied_roles: r.try_get("applied_roles")?,
            created_by: r.try_get("created_by")?,
            version: r.try_get("version")?,
            created_at: r.try_get("created_at")?,
            updated_at: r.try_get("updated_at")?,
        })
    }

    pub fn subject_type(&self) -> Option<ErpSubjectType> {
        ErpSubjectType::parse(&self.subject_type)
    }

    pub fn desired(&self) -> AccountState {
        AccountState::parse(&self.desired_state).unwrap_or(AccountState::Disabled)
    }

    pub fn roles(&self) -> Vec<Role> {
        effective_roles(&self.base_roles, &self.granted_roles)
    }

    pub fn pending(&self) -> Vec<RequiredAction> {
        self.pending_actions
            .iter()
            .filter_map(|a| RequiredAction::from_keycloak(a))
            .collect()
    }
}

/// Loads one link by id (`FOR UPDATE` when `lock`).
pub async fn load(
    conn: &mut PgConnection,
    tenant: Uuid,
    id: Uuid,
    lock: bool,
) -> Result<Option<LinkRow>, sqlx::Error> {
    let sql = format!(
        "SELECT {LINK_COLUMNS} FROM platform.identity_link WHERE tenant_id = $1 AND id = $2{}",
        if lock { " FOR UPDATE" } else { "" }
    );
    sqlx::query(&sql)
        .bind(tenant)
        .bind(id)
        .fetch_optional(&mut *conn)
        .await?
        .as_ref()
        .map(LinkRow::from_row)
        .transpose()
}

/// The link of one ERP subject (`FOR UPDATE`).
pub async fn load_for_subject(
    conn: &mut PgConnection,
    tenant: Uuid,
    subject_type: ErpSubjectType,
    subject_id: Uuid,
) -> Result<Option<LinkRow>, sqlx::Error> {
    sqlx::query(&format!(
        "SELECT {LINK_COLUMNS} FROM platform.identity_link \
         WHERE tenant_id = $1 AND subject_type = $2 AND subject_id = $3 FOR UPDATE"
    ))
    .bind(tenant)
    .bind(subject_type.as_str())
    .bind(subject_id)
    .fetch_optional(&mut *conn)
    .await?
    .as_ref()
    .map(LinkRow::from_row)
    .transpose()
}

/// What [`apply_facts`] did.
#[derive(Clone, Debug, PartialEq)]
pub enum FactsOutcome {
    /// A new link was created (queued for its first sync).
    Created(LinkRow),
    /// The existing link changed and was re-queued.
    Updated(LinkRow),
    /// Nothing to do (no change, or no link and not to be created).
    Unchanged(Option<LinkRow>),
}

/// Why a link could not be created.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkError {
    /// Another identity of this IdP already uses the username / e-mail (duplicate prevented).
    Duplicate(String),
    Db(String),
}

impl From<LinkError> for KernelError {
    fn from(e: LinkError) -> Self {
        match e {
            LinkError::Duplicate(m) => KernelError::Conflict(m),
            LinkError::Db(m) => KernelError::internal_msg(m),
        }
    }
}

fn db(e: sqlx::Error) -> LinkError {
    match &e {
        sqlx::Error::Database(d) if d.is_unique_violation() => {
            let which = match d.constraint() {
                Some("identity_link_username_unique") => "username",
                Some("identity_link_email_unique") => "e-mail address",
                Some("identity_link_idp_user_unique") => "identity-provider account",
                _ => "subject",
            };
            LinkError::Duplicate(format!(
                "another identity already uses this {which} (duplicate identity prevented)"
            ))
        }
        _ => LinkError::Db(e.to_string()),
    }
}

/// Brings the link of `facts`' subject in line with the facts: new lifecycle state, institution,
/// base roles, e-mail / names (before the first sync). Creates the link when `create` is set and
/// none exists. Changes re-queue the link (`pending`, attempt counter reset). Runs inside a
/// savepoint so a duplicate refusal leaves the caller's transaction usable.
pub async fn apply_facts(
    conn: &mut PgConnection,
    facts: &IdentityFacts,
    idp: &str,
    create: bool,
    send_actions_email: bool,
    actor: Option<&str>,
) -> Result<FactsOutcome, LinkError> {
    sqlx::query("SAVEPOINT identity_facts")
        .execute(&mut *conn)
        .await
        .map_err(db)?;
    let res = apply_facts_inner(conn, facts, idp, create, send_actions_email, actor).await;
    let end = if res.is_ok() {
        "RELEASE SAVEPOINT identity_facts"
    } else {
        "ROLLBACK TO SAVEPOINT identity_facts"
    };
    sqlx::query(end).execute(&mut *conn).await.map_err(db)?;
    res
}

async fn apply_facts_inner(
    conn: &mut PgConnection,
    facts: &IdentityFacts,
    idp: &str,
    create: bool,
    send_actions_email: bool,
    actor: Option<&str>,
) -> Result<FactsOutcome, LinkError> {
    let base: Vec<String> = facts
        .base_roles
        .iter()
        .map(|r| r.as_str().to_owned())
        .collect();
    let existing = load_for_subject(conn, facts.tenant_id, facts.subject_type, facts.subject_id)
        .await
        .map_err(db)?;
    match existing {
        Some(link) => {
            let desired = desired_state(facts.state, link.admin_hold);
            let mut sorted_base = base.clone();
            sorted_base.sort();
            let mut old_base = link.base_roles.clone();
            old_base.sort();
            let unsynced = link.idp_user_id.is_none();
            let changed = link.erp_state != facts.state.as_str()
                || link.desired_state != desired.as_str()
                || link.institution_id != facts.institution_id
                || old_base != sorted_base
                || (unsynced && link.email != facts.email);
            if !changed {
                return Ok(FactsOutcome::Unchanged(Some(link)));
            }
            let row = sqlx::query(&format!(
                "UPDATE platform.identity_link SET erp_state = $2, desired_state = $3, \
                   institution_id = $4, base_roles = $5, \
                   email = CASE WHEN idp_user_id IS NULL THEN $6 ELSE email END, \
                   given_name = $7, family_name = $8, \
                   sync_status = 'pending', attempts = 0, next_attempt_at = now(), \
                   last_error = NULL, version = version + 1, updated_at = now() \
                 WHERE id = $1 RETURNING {LINK_COLUMNS}"
            ))
            .bind(link.id)
            .bind(facts.state.as_str())
            .bind(desired.as_str())
            .bind(facts.institution_id)
            .bind(&base)
            .bind(&facts.email)
            .bind(&facts.given_name)
            .bind(&facts.family_name)
            .fetch_one(&mut *conn)
            .await
            .map_err(db)?;
            Ok(FactsOutcome::Updated(LinkRow::from_row(&row).map_err(db)?))
        }
        None if !create => Ok(FactsOutcome::Unchanged(None)),
        None => {
            let username =
                generated_username(&facts.number, facts.email.as_deref(), facts.tenant_id);
            let actions: Vec<String> = initial_actions(facts.email.as_deref(), &facts.base_roles)
                .iter()
                .map(|a| a.keycloak().to_owned())
                .collect();
            let row = sqlx::query(&format!(
                "INSERT INTO platform.identity_link \
                   (tenant_id, institution_id, subject_type, subject_id, idp, username, email, \
                    given_name, family_name, erp_state, desired_state, base_roles, \
                    pending_actions, send_actions_email, created_by) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $10, $11, $12, $13, $14) \
                 RETURNING {LINK_COLUMNS}"
            ))
            .bind(facts.tenant_id)
            .bind(facts.institution_id)
            .bind(facts.subject_type.as_str())
            .bind(facts.subject_id)
            .bind(idp)
            .bind(&username)
            .bind(&facts.email)
            .bind(&facts.given_name)
            .bind(&facts.family_name)
            .bind(facts.state.as_str())
            .bind(&base)
            .bind(&actions)
            .bind(send_actions_email && facts.email.is_some())
            .bind(actor)
            .fetch_one(&mut *conn)
            .await
            .map_err(db)?;
            Ok(FactsOutcome::Created(LinkRow::from_row(&row).map_err(db)?))
        }
    }
}

/// Additional IdP targets of the tenant's role mappings.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Mappings {
    /// Active mappings: (institution, ERP role, target).
    pub rows: Vec<(Option<Uuid>, Role, MappingTarget)>,
    /// Targets of retired mappings (still managed: removed from identities that hold them).
    pub retired: Vec<MappingTarget>,
}

impl Mappings {
    pub async fn load(conn: &mut PgConnection, tenant: Uuid) -> Result<Self, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT institution_id, erp_role, target_kind, target, client_id, \
                    retired_at IS NOT NULL AS retired \
             FROM platform.identity_role_mapping WHERE tenant_id = $1 ORDER BY created_at, id",
        )
        .bind(tenant)
        .fetch_all(&mut *conn)
        .await?;
        let mut out = vec![];
        let mut retired = vec![];
        for r in rows {
            let role: String = r.try_get("erp_role")?;
            let kind: String = r.try_get("target_kind")?;
            let target: String = r.try_get("target")?;
            let client: Option<String> = r.try_get("client_id")?;
            let Some(role) = Role::parse(&role) else {
                continue;
            };
            let t = match (kind.as_str(), client) {
                ("realm_role", _) => MappingTarget::RealmRole(target),
                ("client_role", Some(c)) => MappingTarget::ClientRole {
                    client_id: c,
                    role: target,
                },
                ("group", _) => MappingTarget::Group(target),
                _ => continue,
            };
            if r.try_get::<bool, _>("retired")? {
                retired.push(t);
            } else {
                out.push((r.try_get("institution_id")?, role, t));
            }
        }
        Ok(Self { rows: out, retired })
    }

    /// Targets that apply to an identity with `roles` in `institution`.
    pub fn desired(&self, roles: &[Role], institution: Option<Uuid>) -> Vec<MappingTarget> {
        let mut v: Vec<MappingTarget> = self
            .rows
            .iter()
            .filter(|(inst, role, _)| {
                roles.contains(role) && (inst.is_none() || *inst == institution)
            })
            .map(|(_, _, t)| t.clone())
            .collect();
        v.sort();
        v.dedup();
        v
    }

    /// Every target SutraERP manages for the tenant (removed when no longer desired).
    pub fn managed(&self) -> Vec<MappingTarget> {
        let mut v: Vec<MappingTarget> = self
            .rows
            .iter()
            .map(|(_, _, t)| t.clone())
            .chain(self.retired.iter().cloned())
            .collect();
        v.sort();
        v.dedup();
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| (*x).to_owned()).collect()
    }

    #[test]
    fn hold_suspends_but_never_reenables() {
        assert_eq!(
            desired_state(AccountState::Active, false),
            AccountState::Active
        );
        assert_eq!(
            desired_state(AccountState::Active, true),
            AccountState::Suspended
        );
        assert_eq!(
            desired_state(AccountState::Disabled, false),
            AccountState::Disabled
        );
        assert_eq!(
            desired_state(AccountState::Suspended, true),
            AccountState::Suspended
        );
    }

    #[test]
    fn effective_roles_ignore_unknown_names() {
        assert_eq!(
            effective_roles(&s(&["student", "bogus"]), &s(&["registrar", "student"])),
            vec![Role::Student, Role::Registrar]
        );
    }

    #[test]
    fn privileged_identities_must_enrol_totp() {
        assert_eq!(
            initial_actions(Some("a@b.c"), &[Role::Student]),
            vec![RequiredAction::VerifyEmail, RequiredAction::UpdatePassword]
        );
        assert_eq!(
            initial_actions(None, &[Role::Administrator]),
            vec![
                RequiredAction::UpdatePassword,
                RequiredAction::ConfigureTotp
            ]
        );
    }

    #[test]
    fn diff_touches_only_managed_targets() {
        let (add, remove) = diff(
            &s(&["default-roles-sutra", "administrator", "student"]),
            &s(&["student", "faculty"]),
            &s(&["administrator", "faculty", "student"]),
        );
        assert_eq!(add, s(&["faculty"]));
        assert_eq!(remove, s(&["administrator"]));
    }

    #[test]
    fn foreign_tenant_groups_are_detected() {
        let t = Uuid::now_v7();
        let other = Uuid::now_v7();
        assert!(!foreign_tenant_group(&format!("/sutra-tenant-{t}"), t));
        assert!(!foreign_tenant_group(
            &format!("/sutra-tenant-{t}/institution-x"),
            t
        ));
        assert!(foreign_tenant_group(&format!("/sutra-tenant-{other}"), t));
        assert!(!foreign_tenant_group("/staff-wifi", t));
    }

    #[test]
    fn mappings_apply_per_institution() {
        let i1 = Uuid::now_v7();
        let i2 = Uuid::now_v7();
        let m = Mappings {
            rows: vec![
                (
                    None,
                    Role::Faculty,
                    MappingTarget::Group("/lms/teachers".into()),
                ),
                (
                    Some(i1),
                    Role::Faculty,
                    MappingTarget::RealmRole("lab-access".into()),
                ),
                (
                    Some(i2),
                    Role::Student,
                    MappingTarget::RealmRole("wifi".into()),
                ),
            ],
            retired: vec![MappingTarget::Group("/old".into())],
        };
        assert_eq!(m.desired(&[Role::Faculty], Some(i1)).len(), 2);
        assert_eq!(m.desired(&[Role::Faculty], Some(i2)).len(), 1);
        assert!(m.desired(&[Role::Student], Some(i1)).is_empty());
        assert_eq!(m.managed().len(), 4);
    }
}
