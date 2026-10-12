//! Authenticated execution context and the D02 authorization policy (ADR-L5-0024 / ADR-L5-0038).
//!
//! * [`Role`] — coarse application role carried by the authenticated principal.
//! * [`Permission`] — typed, fine-grained action a handler can require.
//! * [`role_permissions`] — the **deny-by-default** role → permission map: a permission that is not
//!   listed for one of the caller's roles is refused. There is no wildcard and no implicit grant.
//! * [`AuthContext`] — what the HTTP layer knows about an authenticated request once its session
//!   has been resolved. Its [`ExecutionScope`] is the *only* scope authority for protected
//!   operations (ADR-L5-0004); client headers never widen or replace it.
//! * [`SessionResolver`] — port the HTTP layer uses to turn an opaque session token into an
//!   [`AuthContext`]. Implemented by the platform module (D01) over its session store.

use crate::error::KernelError;
use crate::scope::ExecutionScope;
use chrono::{DateTime, Utc};
use std::fmt;
use uuid::Uuid;

/// Application role as presented by the web client. Authorization decisions use the
/// [`Permission`] map below, never the role name directly.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "camelCase")]
pub enum Role {
    Administrator,
    Faculty,
    Student,
}

impl Role {
    pub const ALL: [Role; 3] = [Role::Administrator, Role::Faculty, Role::Student];

    pub fn as_str(&self) -> &'static str {
        match self {
            Role::Administrator => "administrator",
            Role::Faculty => "faculty",
            Role::Student => "student",
        }
    }

    /// Strict parse of the stored/wire name. Unknown names yield `None` (and therefore no grants).
    pub fn parse(s: &str) -> Option<Role> {
        Role::ALL.into_iter().find(|r| r.as_str() == s)
    }

    /// Privileged roles require MFA (ADR-L5-0038 §3.8).
    pub fn is_privileged(&self) -> bool {
        matches!(self, Role::Administrator)
    }
}

/// Authentication assurance of a session (ADR-L5-0038 §3.8).
///
/// `Mfa` means the session was established (or stepped up) with a second factor: a TOTP code
/// verified by SutraERP in dev mode, or Keycloak-asserted `amr`/`acr` values in Keycloak mode.
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    serde::Serialize,
    serde::Deserialize,
    utoipa::ToSchema,
)]
#[serde(rename_all = "camelCase")]
pub enum MfaLevel {
    /// Single factor (password / federated login without a second factor).
    #[default]
    None,
    /// A second factor was verified for this session.
    Mfa,
}

impl MfaLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            MfaLevel::None => "none",
            MfaLevel::Mfa => "mfa",
        }
    }

    /// Strict parse of the stored name; unknown values fail closed to `None`.
    pub fn parse(s: &str) -> MfaLevel {
        if s == "mfa" {
            MfaLevel::Mfa
        } else {
            MfaLevel::None
        }
    }
}

/// A protected action. Add a variant per new capability; it is denied to every role until it is
/// explicitly granted in [`role_permissions`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Permission {
    /// Read institutions of the caller's tenant.
    InstitutionRead,
    /// Register a new institution in the caller's tenant.
    InstitutionRegister,
    /// Rename an institution of the caller's tenant.
    InstitutionRename,
    /// Read one's own notifications (inbox, unread count, mark read).
    NotificationRead,
    /// Read the notices addressed to one of the caller's roles.
    NoticeRead,
    /// Create, update and archive institution-wide notices (and see every notice).
    NoticeManage,
    /// Use one's own to-do inbox (create for oneself, complete, dismiss, reopen).
    TodoUse,
    /// Assign a to-do to another person of the tenant.
    TodoAssign,
    /// View and update one's own profile, preferences and sessions.
    ProfileManage,
    // ---- Batch 2 (Domain B). Reads of every Domain B resource use `InstitutionRead`. ----
    // 2A
    /// Configure an institution: profile revision, business timezone, configure + submit for
    /// activation, withdraw a returned request.
    InstitutionManage,
    /// Suspend, reactivate and archive an institution (step-up).
    InstitutionLifecycle,
    /// Decide an institution activation request: approve / reject / return (step-up). The
    /// abstract "Institution Activation Authority" of the L2-B01 amendment.
    InstitutionActivationDecide,
    /// Draft and make effective institutional business policies.
    InstitutionPolicyManage,
    /// Organisation units, type blueprints, governance configuration, restructuring.
    OrganisationManage,
    /// Effective-dated position definitions.
    PositionManage,
    // 2B
    /// Register, activate, modify, suspend and restore campuses.
    CampusManage,
    /// Decommission and archive campuses (step-up).
    CampusDecommission,
    /// Draft academic structure, programmes, curriculum versions and the school model.
    CurriculumManage,
    /// Publish / supersede / retire academic structure, programmes, curricula, school model.
    CurriculumPublish,
    // 2C
    /// Draft calendars, years, terms, key dates, assignments, exceptions and simulations.
    CalendarManage,
    /// Publish calendar versions, open/close academic years, approve exceptions, retire.
    CalendarPublish,
    /// Read the institution activation readiness checklist.
    InstitutionReadinessRead,
}

impl Permission {
    pub const ALL: [Permission; 22] = [
        Permission::InstitutionRead,
        Permission::InstitutionRegister,
        Permission::InstitutionRename,
        Permission::NotificationRead,
        Permission::NoticeRead,
        Permission::NoticeManage,
        Permission::TodoUse,
        Permission::TodoAssign,
        Permission::ProfileManage,
        Permission::InstitutionManage,
        Permission::InstitutionLifecycle,
        Permission::InstitutionActivationDecide,
        Permission::InstitutionPolicyManage,
        Permission::OrganisationManage,
        Permission::PositionManage,
        Permission::CampusManage,
        Permission::CampusDecommission,
        Permission::CurriculumManage,
        Permission::CurriculumPublish,
        Permission::CalendarManage,
        Permission::CalendarPublish,
        Permission::InstitutionReadinessRead,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            Permission::InstitutionRead => "institution.read",
            Permission::InstitutionRegister => "institution.register",
            Permission::InstitutionRename => "institution.rename",
            Permission::NotificationRead => "notification.read",
            Permission::NoticeRead => "notice.read",
            Permission::NoticeManage => "notice.manage",
            Permission::TodoUse => "todo.use",
            Permission::TodoAssign => "todo.assign",
            Permission::ProfileManage => "profile.manage",
            Permission::InstitutionManage => "institution.manage",
            Permission::InstitutionLifecycle => "institution.lifecycle",
            Permission::InstitutionActivationDecide => "institution.activation.decide",
            Permission::InstitutionPolicyManage => "institution.policy.manage",
            Permission::OrganisationManage => "organisation.manage",
            Permission::PositionManage => "position.manage",
            Permission::CampusManage => "campus.manage",
            Permission::CampusDecommission => "campus.decommission",
            Permission::CurriculumManage => "curriculum.manage",
            Permission::CurriculumPublish => "curriculum.publish",
            Permission::CalendarManage => "calendar.manage",
            Permission::CalendarPublish => "calendar.publish",
            Permission::InstitutionReadinessRead => "institution.readiness.read",
        }
    }

    /// Admin-sensitive permissions that additionally need an MFA-verified session when step-up
    /// is enforced ([`AuthContext::step_up_enforced`]).
    pub fn requires_step_up(&self) -> bool {
        matches!(
            self,
            Permission::InstitutionRegister
                | Permission::InstitutionLifecycle
                | Permission::InstitutionActivationDecide
                | Permission::CampusDecommission
        )
    }
}

/// The role → permission policy. Exhaustive over [`Role`] so a new role cannot compile without
/// an explicit (possibly empty) grant list; anything not listed is denied.
pub fn role_permissions(role: Role) -> &'static [Permission] {
    use Permission::*;
    match role {
        Role::Administrator => &[
            InstitutionRead,
            InstitutionRegister,
            InstitutionRename,
            NotificationRead,
            NoticeRead,
            NoticeManage,
            TodoUse,
            TodoAssign,
            ProfileManage,
            // Batch 2 (Domain B): administrators only.
            InstitutionManage,
            InstitutionLifecycle,
            InstitutionActivationDecide,
            InstitutionPolicyManage,
            OrganisationManage,
            PositionManage,
            CampusManage,
            CampusDecommission,
            CurriculumManage,
            CurriculumPublish,
            CalendarManage,
            CalendarPublish,
            InstitutionReadinessRead,
        ],
        Role::Faculty => &[
            InstitutionRead,
            NotificationRead,
            NoticeRead,
            TodoUse,
            TodoAssign,
            ProfileManage,
        ],
        Role::Student => &[
            InstitutionRead,
            NotificationRead,
            NoticeRead,
            TodoUse,
            ProfileManage,
        ],
    }
}

/// `true` when any of `roles` is granted `permission`. An empty role set is granted nothing.
pub fn is_granted(roles: &[Role], permission: Permission) -> bool {
    roles
        .iter()
        .any(|r| role_permissions(*r).contains(&permission))
}

/// The authenticated caller of a request, resolved from its server-side session.
#[derive(Clone, PartialEq, Eq)]
pub struct AuthContext {
    /// Technical id of the server-side session (safe to log; never the token).
    pub session_id: Uuid,
    /// Stable subject reference (Keycloak `sub` in production).
    pub subject: String,
    pub login_id: String,
    pub display_name: String,
    pub roles: Vec<Role>,
    /// Session-bound execution scope — the scope authority for this request.
    pub scope: ExecutionScope,
    /// Synchronizer CSRF token bound to this session.
    pub csrf_token: String,
    /// Absolute session expiry.
    pub expires_at: DateTime<Utc>,
    /// Expiry if the session stays idle from now on.
    pub idle_expires_at: DateTime<Utc>,
    /// Assurance level of the session (second factor verified or not).
    pub mfa_level: MfaLevel,
    /// Authentication methods references (RFC 8176 style: `pwd`, `otp`, `hwk`, …).
    pub amr: Vec<String>,
    /// Whether step-up permissions ([`Permission::requires_step_up`]) need `mfa_level = Mfa`.
    /// Set by the session resolver from configuration.
    pub step_up_enforced: bool,
}

impl fmt::Debug for AuthContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AuthContext")
            .field("session_id", &self.session_id)
            .field("subject", &self.subject)
            .field("roles", &self.roles)
            .field("scope", &self.scope)
            .field("csrf_token", &"<redacted>")
            .field("expires_at", &self.expires_at)
            .field("mfa_level", &self.mfa_level)
            .finish_non_exhaustive()
    }
}

impl AuthContext {
    pub fn has_permission(&self, permission: Permission) -> bool {
        is_granted(&self.roles, permission)
    }

    /// Deny-by-default permission check: `Forbidden` unless one of the caller's roles grants it;
    /// `StepUpRequired` when the permission is admin-sensitive, step-up is enforced and the
    /// session has not verified a second factor.
    pub fn require(&self, permission: Permission) -> Result<(), KernelError> {
        if !self.has_permission(permission) {
            return Err(KernelError::Forbidden(format!(
                "missing permission `{}`",
                permission.as_str()
            )));
        }
        if self.step_up_enforced && permission.requires_step_up() && self.mfa_level < MfaLevel::Mfa
        {
            return Err(KernelError::StepUpRequired(permission.as_str()));
        }
        Ok(())
    }

    /// Constant-time check of a presented synchronizer token.
    pub fn csrf_matches(&self, presented: &str) -> bool {
        !self.csrf_token.is_empty()
            && crate::secret::ct_eq(self.csrf_token.as_bytes(), presented.as_bytes())
    }
}

/// Resolves an opaque session token (from the session cookie) to the caller's [`AuthContext`].
///
/// Must return `Ok(None)` for unknown, revoked, idle-expired or absolutely-expired sessions.
#[async_trait::async_trait]
pub trait SessionResolver: Send + Sync + 'static {
    async fn resolve(&self, token: &str) -> Result<Option<AuthContext>, KernelError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scope::TenantId;

    fn ctx(roles: Vec<Role>) -> AuthContext {
        AuthContext {
            session_id: Uuid::now_v7(),
            subject: "s".into(),
            login_id: "l".into(),
            display_name: "d".into(),
            roles,
            scope: ExecutionScope::tenant(TenantId::new()),
            csrf_token: "a".repeat(64),
            expires_at: Utc::now(),
            idle_expires_at: Utc::now(),
            mfa_level: MfaLevel::None,
            amr: vec!["pwd".into()],
            step_up_enforced: false,
        }
    }

    #[test]
    fn step_up_is_required_only_when_enforced() {
        let mut a = ctx(vec![Role::Administrator]);
        assert!(a.require(Permission::InstitutionRegister).is_ok());
        a.step_up_enforced = true;
        assert!(matches!(
            a.require(Permission::InstitutionRegister),
            Err(KernelError::StepUpRequired("institution.register"))
        ));
        assert!(
            a.require(Permission::InstitutionRead).is_ok(),
            "not sensitive"
        );
        a.mfa_level = MfaLevel::Mfa;
        assert!(a.require(Permission::InstitutionRegister).is_ok());
        // Step-up never grants a permission the role does not hold.
        let mut s = ctx(vec![Role::Student]);
        s.step_up_enforced = true;
        s.mfa_level = MfaLevel::Mfa;
        assert!(matches!(
            s.require(Permission::InstitutionRegister),
            Err(KernelError::Forbidden(_))
        ));
        assert_eq!(MfaLevel::parse("mfa"), MfaLevel::Mfa);
        assert_eq!(MfaLevel::parse("bogus"), MfaLevel::None);
    }

    #[test]
    fn administrator_holds_institution_admin_permissions() {
        let a = ctx(vec![Role::Administrator]);
        for p in Permission::ALL {
            assert!(a.require(p).is_ok(), "{p:?}");
        }
    }

    #[test]
    fn non_admin_roles_may_only_read() {
        for r in [Role::Faculty, Role::Student] {
            let c = ctx(vec![r]);
            assert!(c.require(Permission::InstitutionRead).is_ok());
            for p in [
                Permission::InstitutionRegister,
                Permission::InstitutionRename,
                Permission::NoticeManage,
            ] {
                assert!(
                    matches!(c.require(p), Err(KernelError::Forbidden(m)) if m.contains(p.as_str())),
                    "{r:?} must not hold {p:?}"
                );
            }
        }
    }

    #[test]
    fn batch2_domain_b_writes_are_administrator_only() {
        let writes = [
            Permission::InstitutionManage,
            Permission::InstitutionLifecycle,
            Permission::InstitutionActivationDecide,
            Permission::InstitutionPolicyManage,
            Permission::OrganisationManage,
            Permission::PositionManage,
            Permission::CampusManage,
            Permission::CampusDecommission,
            Permission::CurriculumManage,
            Permission::CurriculumPublish,
            Permission::CalendarManage,
            Permission::CalendarPublish,
            Permission::InstitutionReadinessRead,
        ];
        for p in writes {
            assert!(is_granted(&[Role::Administrator], p), "{p:?}");
            assert!(!is_granted(&[Role::Faculty, Role::Student], p), "{p:?}");
        }
        for p in [
            Permission::InstitutionLifecycle,
            Permission::InstitutionActivationDecide,
            Permission::CampusDecommission,
        ] {
            assert!(p.requires_step_up(), "{p:?}");
        }
        assert!(!Permission::CampusManage.requires_step_up());
    }

    #[test]
    fn deny_by_default_without_roles() {
        let none = ctx(vec![]);
        for p in Permission::ALL {
            assert!(!none.has_permission(p));
            assert!(none.require(p).is_err());
        }
    }

    #[test]
    fn grants_are_the_union_of_roles() {
        assert!(is_granted(
            &[Role::Student, Role::Administrator],
            Permission::InstitutionRename
        ));
        assert!(!is_granted(
            &[Role::Student, Role::Faculty],
            Permission::InstitutionRename
        ));
    }

    #[test]
    fn role_names_roundtrip_and_unknown_is_rejected() {
        for r in Role::ALL {
            assert_eq!(Role::parse(r.as_str()), Some(r));
        }
        assert_eq!(Role::parse("superuser"), None);
        assert_eq!(Role::parse("Administrator"), None);
    }

    #[test]
    fn csrf_compare_and_redacted_debug() {
        let c = ctx(vec![Role::Student]);
        assert!(c.csrf_matches(&"a".repeat(64)));
        assert!(!c.csrf_matches(&"b".repeat(64)));
        assert!(!c.csrf_matches(""));
        assert!(!format!("{c:?}").contains(&"a".repeat(64)));
        let mut empty = c;
        empty.csrf_token.clear();
        assert!(!empty.csrf_matches(""), "an empty token never matches");
    }

    #[test]
    fn self_service_permissions_for_every_role_and_assignment_for_staff() {
        for r in Role::ALL {
            let c = ctx(vec![r]);
            for p in [
                Permission::NotificationRead,
                Permission::NoticeRead,
                Permission::TodoUse,
                Permission::ProfileManage,
            ] {
                assert!(c.require(p).is_ok(), "{r:?} needs {p:?}");
            }
        }
        assert!(ctx(vec![Role::Faculty]).has_permission(Permission::TodoAssign));
        assert!(!ctx(vec![Role::Student]).has_permission(Permission::TodoAssign));
        assert!(!ctx(vec![Role::Faculty]).has_permission(Permission::NoticeManage));
        let names: std::collections::HashSet<_> =
            Permission::ALL.iter().map(Permission::as_str).collect();
        assert_eq!(
            names.len(),
            Permission::ALL.len(),
            "permission names are unique"
        );
    }

    #[test]
    fn administrator_is_privileged() {
        assert!(Role::Administrator.is_privileged());
        assert!(!Role::Student.is_privileged());
    }
}
