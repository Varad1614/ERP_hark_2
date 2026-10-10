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
}

impl Permission {
    pub const ALL: [Permission; 3] = [
        Permission::InstitutionRead,
        Permission::InstitutionRegister,
        Permission::InstitutionRename,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            Permission::InstitutionRead => "institution.read",
            Permission::InstitutionRegister => "institution.register",
            Permission::InstitutionRename => "institution.rename",
        }
    }
}

/// The role → permission policy. Exhaustive over [`Role`] so a new role cannot compile without
/// an explicit (possibly empty) grant list; anything not listed is denied.
pub fn role_permissions(role: Role) -> &'static [Permission] {
    use Permission::*;
    match role {
        Role::Administrator => &[InstitutionRead, InstitutionRegister, InstitutionRename],
        Role::Faculty => &[InstitutionRead],
        Role::Student => &[InstitutionRead],
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
            .finish_non_exhaustive()
    }
}

impl AuthContext {
    pub fn has_permission(&self, permission: Permission) -> bool {
        is_granted(&self.roles, permission)
    }

    /// Deny-by-default permission check: `Forbidden` unless one of the caller's roles grants it.
    pub fn require(&self, permission: Permission) -> Result<(), KernelError> {
        if self.has_permission(permission) {
            Ok(())
        } else {
            Err(KernelError::Forbidden(format!(
                "missing permission `{}`",
                permission.as_str()
            )))
        }
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
        }
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
            ] {
                assert!(
                    matches!(c.require(p), Err(KernelError::Forbidden(m)) if m.contains(p.as_str())),
                    "{r:?} must not hold {p:?}"
                );
            }
        }
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
    fn administrator_is_privileged() {
        assert!(Role::Administrator.is_privileged());
        assert!(!Role::Student.is_privileged());
    }
}
