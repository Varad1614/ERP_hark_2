//! Identity provisioning ports (Part 2 · Stage 4.3, `docs/readiness/part2/s4-idp.md`).
//!
//! SutraERP owns *who* may sign in and *with which roles*; the identity provider (Keycloak)
//! owns credentials, second factors and sign-in itself. Two ports connect them:
//!
//! * [`IdentityProvisioningPort`] — the IdP's user-administration API (Keycloak Admin REST
//!   adapter `sutra_platform::identity::keycloak_admin`; an in-memory adapter for tests that
//!   production refuses). Every call is idempotent from the caller's point of view: the platform
//!   reconciles a *desired* state (enabled, realm roles, groups, required actions) against what
//!   the IdP reports, so a retry after a failure converges instead of duplicating.
//! * [`IdentitySubjectSource`] — implemented by each module that owns people who sign in
//!   (`sutra_student::identity_source` for students, `sutra_people::identity_source` for staff).
//!   It reads the facts that decide an account (status, institution, e-mail, base role) from the
//!   owning schema, names the subjects an outbox event concerns, and records the IdP subject on
//!   the owning record (`subject_ref`, used by self-service). The platform never runs SQL against
//!   a foreign schema.

use sutra_kernel::auth::Role;
use sutra_kernel::error::KernelError;
use uuid::Uuid;

/// Kind of ERP subject an identity belongs to.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ErpSubjectType {
    /// A student persona (Domain C).
    Student,
    /// A staff member (Domain E).
    Staff,
    /// A platform administrator with no ERP record (created through the admin API).
    Admin,
}

impl ErpSubjectType {
    pub const ALL: [ErpSubjectType; 3] = [Self::Student, Self::Staff, Self::Admin];

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Student => "student",
            Self::Staff => "staff",
            Self::Admin => "admin",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.as_str() == s)
    }
}

/// Account state the ERP wants the identity to be in.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum AccountState {
    /// May sign in.
    Active,
    /// Temporarily blocked (student suspension, staff suspension, administrative hold).
    Suspended,
    /// Permanently blocked (student exit / archival, staff separation / archival / merge).
    Disabled,
}

impl AccountState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Suspended => "suspended",
            Self::Disabled => "disabled",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        [Self::Active, Self::Suspended, Self::Disabled]
            .into_iter()
            .find(|t| t.as_str() == s)
    }

    /// Whether the IdP account is enabled in this state.
    pub fn enabled(&self) -> bool {
        matches!(self, Self::Active)
    }
}

/// Keycloak required action triggered for an account (the user performs it at next sign-in, or
/// through the "execute actions" e-mail).
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "camelCase")]
pub enum RequiredAction {
    /// Prove ownership of the e-mail address (activation).
    VerifyEmail,
    /// Set a new password (activation, password recovery).
    UpdatePassword,
    /// Enrol a TOTP second factor (privileged roles, MFA enrolment).
    ConfigureTotp,
}

impl RequiredAction {
    pub const ALL: [RequiredAction; 3] =
        [Self::VerifyEmail, Self::UpdatePassword, Self::ConfigureTotp];

    /// Keycloak provider id of the action.
    pub fn keycloak(&self) -> &'static str {
        match self {
            Self::VerifyEmail => "VERIFY_EMAIL",
            Self::UpdatePassword => "UPDATE_PASSWORD",
            Self::ConfigureTotp => "CONFIGURE_TOTP",
        }
    }

    pub fn from_keycloak(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|a| a.keycloak() == s)
    }
}

/// What an IdP user looks like to SutraERP.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IdpUser {
    /// IdP user id (Keycloak user id = the OIDC `sub`).
    pub id: String,
    pub username: String,
    pub email: Option<String>,
    pub enabled: bool,
    pub email_verified: bool,
    /// Keycloak provider ids of the pending required actions.
    pub required_actions: Vec<String>,
}

/// A user to create.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewIdpUser {
    pub username: String,
    pub email: Option<String>,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub enabled: bool,
    pub required_actions: Vec<RequiredAction>,
}

/// The tenant / institution group an identity belongs to. Its attributes (`tenant_id`,
/// `institution_id`) are what the realm's user-attribute mappers put into the ID token, so the
/// group *is* the identity's tenant scope.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ScopeGroup {
    pub tenant_id: Uuid,
    pub institution_id: Option<Uuid>,
}

impl ScopeGroup {
    /// Top-level group of a tenant.
    pub fn tenant_group_name(tenant: Uuid) -> String {
        format!("{TENANT_GROUP_PREFIX}{tenant}")
    }

    /// Sub-group of an institution inside its tenant group.
    pub fn institution_group_name(institution: Uuid) -> String {
        format!("{INSTITUTION_GROUP_PREFIX}{institution}")
    }

    /// Full group path (`/sutra-tenant-<t>` or `/sutra-tenant-<t>/institution-<i>`).
    pub fn path(&self) -> String {
        match self.institution_id {
            None => format!("/{}", Self::tenant_group_name(self.tenant_id)),
            Some(i) => format!(
                "/{}/{}",
                Self::tenant_group_name(self.tenant_id),
                Self::institution_group_name(i)
            ),
        }
    }
}

/// Prefix of the top-level tenant groups SutraERP manages.
pub const TENANT_GROUP_PREFIX: &str = "sutra-tenant-";
/// Prefix of the institution sub-groups SutraERP manages.
pub const INSTITUTION_GROUP_PREFIX: &str = "institution-";

/// A group the user is a member of.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IdpGroup {
    pub id: String,
    pub path: String,
}

/// Additional IdP target an ERP role maps to (per tenant, optionally per institution). The
/// SutraERP realm role of the same name is always mapped; these come on top.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum MappingTarget {
    /// A realm role (never one of SutraERP's own role names).
    RealmRole(String),
    /// A client role of `client_id`.
    ClientRole { client_id: String, role: String },
    /// A group by full path (must not be a SutraERP tenant group).
    Group(String),
}

/// User administration API of the identity provider.
#[async_trait::async_trait]
pub trait IdentityProvisioningPort: Send + Sync + 'static {
    /// `keycloak`, `memory`.
    fn kind(&self) -> &'static str;

    /// Verifies the adapter's own credentials and privileges (production start-up gate):
    /// `Err(Forbidden)` when the admin client is over- or under-privileged, `Err(Unavailable)`
    /// when the IdP cannot be reached.
    async fn check(&self) -> Result<(), KernelError>;

    async fn user(&self, id: &str) -> Result<Option<IdpUser>, KernelError>;

    /// Exact (case-insensitive) username match.
    async fn find_by_username(&self, username: &str) -> Result<Option<IdpUser>, KernelError>;

    /// Exact (case-insensitive) e-mail match.
    async fn find_by_email(&self, email: &str) -> Result<Vec<IdpUser>, KernelError>;

    /// Creates the user and returns its id. `Err(Conflict)` when the username or e-mail is
    /// already taken.
    async fn create_user(&self, user: &NewIdpUser) -> Result<String, KernelError>;

    /// Enables / disables the account.
    async fn set_enabled(&self, id: &str, enabled: bool) -> Result<(), KernelError>;

    /// Adds `actions` to the user's pending required actions (union; actions already completed
    /// are added again — that is the point of a recovery / re-enrolment trigger). With
    /// `send_email` the IdP also e-mails the user an "execute actions" link.
    async fn add_required_actions(
        &self,
        id: &str,
        actions: &[RequiredAction],
        send_email: bool,
    ) -> Result<(), KernelError>;

    /// Ends every IdP session of the user (the IdP then back-channel logs out the clients).
    async fn logout(&self, id: &str) -> Result<(), KernelError>;

    /// Names of the realm roles mapped directly to the user.
    async fn realm_roles(&self, id: &str) -> Result<Vec<String>, KernelError>;

    async fn add_realm_roles(&self, id: &str, roles: &[String]) -> Result<(), KernelError>;

    async fn remove_realm_roles(&self, id: &str, roles: &[String]) -> Result<(), KernelError>;

    /// Client roles of `client_id` mapped directly to the user.
    async fn client_roles(&self, id: &str, client_id: &str) -> Result<Vec<String>, KernelError>;

    async fn add_client_roles(
        &self,
        id: &str,
        client_id: &str,
        roles: &[String],
    ) -> Result<(), KernelError>;

    async fn remove_client_roles(
        &self,
        id: &str,
        client_id: &str,
        roles: &[String],
    ) -> Result<(), KernelError>;

    /// Groups the user is a direct member of.
    async fn groups(&self, id: &str) -> Result<Vec<IdpGroup>, KernelError>;

    /// Creates the tenant / institution group (with its scope attributes) when missing and
    /// returns its id.
    async fn ensure_scope_group(&self, group: &ScopeGroup) -> Result<String, KernelError>;

    /// Id of an existing group by path (`None` when it does not exist).
    async fn group_by_path(&self, path: &str) -> Result<Option<String>, KernelError>;

    async fn join_group(&self, id: &str, group_id: &str) -> Result<(), KernelError>;

    async fn leave_group(&self, id: &str, group_id: &str) -> Result<(), KernelError>;
}

/// Facts the owning module reports about one ERP subject.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IdentityFacts {
    pub subject_type: ErpSubjectType,
    pub subject_id: Uuid,
    pub tenant_id: Uuid,
    /// Institution the identity is scoped to (the student's live context / the staff record).
    pub institution_id: Option<Uuid>,
    /// Business number (`S0001`, `EMP-000001`) — the basis of a generated username.
    pub number: String,
    pub email: Option<String>,
    pub given_name: String,
    pub family_name: Option<String>,
    /// Account state the record's lifecycle calls for.
    pub state: AccountState,
    /// Roles the record itself implies (student → `student`, teaching staff → `faculty`).
    pub base_roles: Vec<Role>,
    /// Login subject already recorded on the ERP record, if any.
    pub subject_ref: Option<String>,
}

/// Result of recording the IdP subject on the owning ERP record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SubjectLink {
    /// Recorded now.
    Linked,
    /// The record already carried this subject.
    AlreadyLinked,
    /// The record (or another live record of the tenant) is bound to a different login.
    Conflict(String),
    /// The record does not exist (any more) in the tenant.
    Missing,
}

/// Read / link port of a module that owns people who sign in.
#[async_trait::async_trait]
pub trait IdentitySubjectSource: Send + Sync + 'static {
    fn subject_type(&self) -> ErpSubjectType;

    /// CloudEvents `type`s whose events can change an identity of this source.
    fn event_types(&self) -> &'static [&'static str];

    /// ERP subject ids an outbox event concerns (pure).
    fn subjects_of(
        &self,
        event_type: &str,
        subject: Option<&str>,
        data: &serde_json::Value,
    ) -> Vec<Uuid>;

    /// Current facts of one subject of `tenant` (`None` when it does not exist).
    async fn facts(&self, tenant: Uuid, id: Uuid) -> Result<Option<IdentityFacts>, KernelError>;

    /// Records `subject_ref` (the IdP user id) on the subject's record when it carries none.
    async fn link_subject(
        &self,
        tenant: Uuid,
        id: Uuid,
        subject_ref: &str,
    ) -> Result<SubjectLink, KernelError>;
}

/// Username of a generated identity: the lower-cased e-mail when there is one, otherwise
/// `<number>.<last 12 hex digits of the tenant>` (Keycloak usernames are realm-wide, so the
/// tenant suffix keeps two tenants' `S0001` apart).
pub fn generated_username(number: &str, email: Option<&str>, tenant: Uuid) -> String {
    match email.map(str::trim).filter(|e| !e.is_empty()) {
        Some(e) => e.to_ascii_lowercase(),
        None => {
            let t = tenant.simple().to_string();
            let suffix = &t[t.len().saturating_sub(12)..];
            let n: String = number
                .trim()
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                        c.to_ascii_lowercase()
                    } else {
                        '-'
                    }
                })
                .collect();
            format!("{n}.{suffix}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enums_round_trip() {
        for t in ErpSubjectType::ALL {
            assert_eq!(ErpSubjectType::parse(t.as_str()), Some(t));
        }
        for a in RequiredAction::ALL {
            assert_eq!(RequiredAction::from_keycloak(a.keycloak()), Some(a));
        }
        assert_eq!(
            AccountState::parse("suspended"),
            Some(AccountState::Suspended)
        );
        assert!(AccountState::Active.enabled());
        assert!(!AccountState::Suspended.enabled() && !AccountState::Disabled.enabled());
    }

    #[test]
    fn usernames_are_tenant_unique_and_safe() {
        let t = Uuid::parse_str("0192f5a0-7c3e-7000-8000-0000000a1b2c").unwrap_or_default();
        assert_eq!(
            generated_username("S/0001 A", None, t),
            "s-0001-a.0000000a1b2c"
        );
        assert_eq!(
            generated_username("x", Some(" Asha@Example.EDU "), t),
            "asha@example.edu"
        );
    }

    #[test]
    fn scope_group_paths() {
        let t = Uuid::nil();
        let g = ScopeGroup {
            tenant_id: t,
            institution_id: None,
        };
        assert_eq!(g.path(), format!("/sutra-tenant-{t}"));
        let g = ScopeGroup {
            tenant_id: t,
            institution_id: Some(t),
        };
        assert!(g.path().ends_with(&format!("/institution-{t}")));
    }
}
