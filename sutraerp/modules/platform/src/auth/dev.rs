//! **Development-only** credential authenticator (Batch 0).
//!
//! Recognises the seeded personas (three product personas + Part 1 checker personas) that mirror the product prototype and a single shared dev
//! password. The composition root refuses to wire it when `SUTRA_ENV=production`. It verifies
//! the primary factor only; the platform HTTP layer applies [`MfaPolicy`] and real TOTP.

use sutra_contracts::platform::auth::{
    CredentialAuthenticator, Credentials, MfaPolicy, Principal, Role,
};
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::ExecutionScope;

pub struct DevPersona {
    pub login_id: &'static str,
    pub display_name: &'static str,
    pub role: Role,
}

pub const DEV_PERSONAS: [DevPersona; 9] = [
    DevPersona {
        login_id: "admin@sutra.dev",
        display_name: "Dhruv Naik",
        role: Role::Administrator,
    },
    DevPersona {
        login_id: "meera.shah@sutra.dev",
        display_name: "Dr. Meera Shah",
        role: Role::Faculty,
    },
    DevPersona {
        login_id: "aarav.desai@sutra.dev",
        display_name: "Aarav Desai",
        role: Role::Student,
    },
    // Part 1 (DEF-X-002): checker personas — a second, independent user for each governed
    // decision kind (docs/readiness/APPROVAL_CONTROL_MATRIX.md).
    DevPersona {
        login_id: "finance.approver@sutra.dev",
        display_name: "Kavita Rao",
        role: Role::FinanceApprover,
    },
    DevPersona {
        login_id: "exam.controller@sutra.dev",
        display_name: "Dr. Anil Menon",
        role: Role::ExamController,
    },
    DevPersona {
        login_id: "registrar@sutra.dev",
        display_name: "Sunita Kulkarni",
        role: Role::Registrar,
    },
    DevPersona {
        login_id: "payroll.approver@sutra.dev",
        display_name: "Rahul Iyer",
        role: Role::PayrollApprover,
    },
    DevPersona {
        login_id: "procurement.approver@sutra.dev",
        display_name: "Farah Siddiqui",
        role: Role::ProcurementApprover,
    },
    // Part 1 · S3-appr (DEF-X-002): a second, independent administrator — the checker for
    // administrator-only decisions (registration, faculty assignment, enrolment override, HR …)
    // that the first administrator requested.
    DevPersona {
        login_id: "dean@sutra.dev",
        display_name: "Prof. Kiran Bhat",
        role: Role::Administrator,
    },
];

pub struct DevAuthenticator {
    password: String,
    scope: ExecutionScope,
}

impl DevAuthenticator {
    pub fn new(password: String, scope: ExecutionScope) -> Self {
        Self { password, scope }
    }

    fn principal(&self, p: &DevPersona) -> Principal {
        Principal {
            subject: format!("dev|{}", p.login_id),
            display_name: p.display_name.into(),
            login_id: p.login_id.into(),
            roles: vec![p.role],
            scope: self.scope,
        }
    }
}

/// Constant-time-ish comparison so the dev stub does not model a timing oracle.
fn eq_ct(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let mut diff = a.len() ^ b.len();
    for i in 0..a.len().max(b.len()) {
        diff |= usize::from(a.get(i).copied().unwrap_or(0) ^ b.get(i).copied().unwrap_or(0));
    }
    diff == 0
}

#[async_trait::async_trait]
impl CredentialAuthenticator for DevAuthenticator {
    async fn authenticate(&self, c: Credentials) -> Result<Principal, KernelError> {
        let login = c.login_id.trim().to_lowercase();
        let persona = DEV_PERSONAS.iter().find(|p| p.login_id == login);
        let password_ok = eq_ct(&c.password, &self.password);
        // Same generic failure for unknown user and wrong password (no enumeration).
        let Some(persona) = persona.filter(|_| password_ok) else {
            return Err(KernelError::Unauthenticated);
        };
        Ok(self.principal(persona))
    }
}

/// MFA policy for dev: privileged roles need MFA only when `enforce` is on.
pub struct PrivilegedRoleMfaPolicy {
    pub enforce: bool,
}

impl MfaPolicy for PrivilegedRoleMfaPolicy {
    fn requires_mfa(&self, p: &Principal) -> bool {
        self.enforce && p.roles.iter().any(Role::is_privileged)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sutra_kernel::scope::TenantId;

    fn auth() -> DevAuthenticator {
        DevAuthenticator::new("sutra".into(), ExecutionScope::tenant(TenantId::new()))
    }

    fn creds(l: &str, p: &str) -> Credentials {
        Credentials {
            login_id: l.into(),
            password: p.into(),
        }
    }

    #[tokio::test]
    async fn accepts_known_persona() {
        let r = auth()
            .authenticate(creds("Meera.Shah@sutra.dev", "sutra"))
            .await;
        assert!(matches!(r, Ok(p) if p.roles == vec![Role::Faculty]));
    }

    #[tokio::test]
    async fn generic_failure_for_unknown_or_wrong_password() {
        let a = auth();
        assert!(matches!(
            a.authenticate(creds("nobody@sutra.dev", "sutra")).await,
            Err(KernelError::Unauthenticated)
        ));
        assert!(matches!(
            a.authenticate(creds("admin@sutra.dev", "wrong")).await,
            Err(KernelError::Unauthenticated)
        ));
    }

    #[tokio::test]
    async fn privileged_roles_need_mfa_only_when_enforced() {
        let admin = auth()
            .authenticate(creds("admin@sutra.dev", "sutra"))
            .await
            .unwrap();
        let student = auth()
            .authenticate(creds("aarav.desai@sutra.dev", "sutra"))
            .await
            .unwrap();
        let on = PrivilegedRoleMfaPolicy { enforce: true };
        let off = PrivilegedRoleMfaPolicy { enforce: false };
        assert!(on.requires_mfa(&admin));
        assert!(!on.requires_mfa(&student));
        assert!(!off.requires_mfa(&admin));
    }
}
