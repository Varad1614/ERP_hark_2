//! Audit remediation A3 (M5): start-up safety checks that must fail **closed** in production.
//!
//! The API must serve as a role that PostgreSQL row-level security applies to (Part 1 S3,
//! DEF-X-003). Before A3 a probe that *errored or timed out* only logged a warning and the
//! server started anyway — so a production deployment could run as a superuser / BYPASSRLS role
//! whenever the check itself could not complete. Now production refuses to start unless the
//! probe positively confirms an RLS-subject role; other environments keep warning.

/// Outcome of the runtime-role probe (`sutra_persistence::pool::role_security` with a timeout).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoleProbe {
    /// The login role is subject to row-level security.
    SubjectToRls { role: String },
    /// The login role bypasses row-level security.
    BypassesRls {
        role: String,
        superuser: bool,
        bypass_rls: bool,
    },
    /// The probe query failed (database unreachable, permission error…).
    Failed(String),
    /// The probe did not answer in time.
    TimedOut,
}

/// What start-up does with a probe outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoleVerdict {
    /// Start; nothing to report.
    Ok,
    /// Start, but log this warning (non-production only).
    Warn(String),
    /// Refuse to start with this message.
    Refuse(String),
}

/// Decides start-up for `probe`; `production` is `SUTRA_ENV=production`.
pub fn runtime_role_verdict(production: bool, probe: &RoleProbe) -> RoleVerdict {
    let problem = match probe {
        RoleProbe::SubjectToRls { .. } => return RoleVerdict::Ok,
        RoleProbe::BypassesRls {
            role,
            superuser,
            bypass_rls,
        } => format!(
            "DATABASE_URL role `{role}` is superuser={superuser} bypassrls={bypass_rls}: \
             row-level tenant isolation would not apply; serve as the runtime role (member of \
             sutra_app)"
        ),
        RoleProbe::Failed(e) => format!(
            "could not verify that the DATABASE_URL role is subject to row-level security \
             ({e}); refusing to serve without that check"
        ),
        RoleProbe::TimedOut => "timed out verifying that the DATABASE_URL role is subject to \
                                row-level security; refusing to serve without that check"
            .to_owned(),
    };
    if production {
        RoleVerdict::Refuse(problem)
    } else {
        RoleVerdict::Warn(problem)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_refuses_unless_rls_is_confirmed() {
        let bypass = RoleProbe::BypassesRls {
            role: "postgres".into(),
            superuser: true,
            bypass_rls: false,
        };
        let failed = RoleProbe::Failed("connection refused".into());
        for p in [&bypass, &failed, &RoleProbe::TimedOut] {
            assert!(
                matches!(runtime_role_verdict(true, p), RoleVerdict::Refuse(_)),
                "{p:?}"
            );
            assert!(
                matches!(runtime_role_verdict(false, p), RoleVerdict::Warn(_)),
                "{p:?}"
            );
        }
        let ok = RoleProbe::SubjectToRls {
            role: "sutra_runtime".into(),
        };
        assert_eq!(runtime_role_verdict(true, &ok), RoleVerdict::Ok);
        assert_eq!(runtime_role_verdict(false, &ok), RoleVerdict::Ok);
    }
}
