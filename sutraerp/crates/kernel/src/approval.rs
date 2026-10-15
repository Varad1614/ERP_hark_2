//! Approval mode and separation of duties for governed decisions (Part 1 · DEF-X-001 / DEF-X-002).
//!
//! * [`ApprovalMode`] — the **one** config-driven switch every module state carries
//!   (`SUTRA_APPROVAL_MODE`, composed by the API's composition root). The default is
//!   [`ApprovalMode::AwaitDecision`]: a governed request stays pending until an authorised human
//!   decides it through the slice's decision endpoint. [`ApprovalMode::AutoApprove`] is an explicit
//!   development / test opt-in (`SUTRA_APPROVAL_MODE=auto`); production refuses it at start-up.
//! * [`ensure_independent_decider`] — the maker-checker rule of the shared decision path: the
//!   decider must be an identified human and must not be one of the request's makers (the
//!   creator / requester / submitter). Violations are `403 forbidden`.

use crate::error::KernelError;

/// How approval steps resolve. Shared by every domain module (`shared::approval` re-exports it).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ApprovalMode {
    /// Development / test opt-in only: the process is started and approved at once by the
    /// system actor. Refused when `SUTRA_ENV=production`.
    AutoApprove,
    /// Default: the aggregate stays pending until a decision endpoint is called by a user who
    /// holds the domain's approve permission and is not the requester.
    #[default]
    AwaitDecision,
}

impl ApprovalMode {
    /// Strict parse of `SUTRA_APPROVAL_MODE`: `human` (also `await`, `manual`) or `auto`.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "human" | "await" | "await_decision" | "manual" => Some(Self::AwaitDecision),
            "auto" | "auto_approve" | "dev_auto_approve" => Some(Self::AutoApprove),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::AutoApprove => "auto",
            Self::AwaitDecision => "human",
        }
    }
}

/// Actor ids that are never a human decider (automatic steps).
pub fn is_system_actor(id: &str) -> bool {
    id.starts_with("system:") || id == "system"
}

/// Separation of duties on the shared decision path: refuses a decision when the decider is
/// unidentified / a system actor, or is one of the request's makers (`requesters`: creator,
/// requester, submitter — whoever moved the aggregate into its pending state). `what` names the
/// governed kind in the problem detail (e.g. `"refund"`).
pub fn ensure_independent_decider<'a, I>(
    decider: Option<&str>,
    requesters: I,
    what: &str,
) -> Result<(), KernelError>
where
    I: IntoIterator<Item = &'a str>,
{
    let Some(decider) = decider.map(str::trim).filter(|d| !d.is_empty()) else {
        return Err(KernelError::Forbidden(format!(
            "a {what} decision needs an identified approver"
        )));
    };
    if is_system_actor(decider) {
        return Err(KernelError::Forbidden(format!(
            "a {what} decision needs a human approver"
        )));
    }
    if requesters.into_iter().any(|r| r.trim() == decider) {
        return Err(KernelError::Forbidden(format!(
            "separation of duties: the requester of this {what} cannot decide it; \
             another user with the approve permission must decide"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_human_and_parse_is_strict() {
        assert_eq!(ApprovalMode::default(), ApprovalMode::AwaitDecision);
        assert_eq!(ApprovalMode::parse("human"), Some(ApprovalMode::AwaitDecision));
        assert_eq!(ApprovalMode::parse(" AUTO "), Some(ApprovalMode::AutoApprove));
        assert_eq!(ApprovalMode::parse("yes"), None);
        assert_eq!(ApprovalMode::AwaitDecision.as_str(), "human");
    }

    #[test]
    fn self_approval_and_anonymous_deciders_are_refused() {
        let ok = ensure_independent_decider(Some("dev|b"), ["dev|a"], "refund");
        assert!(ok.is_ok());
        for (d, req) in [
            (Some("dev|a"), vec!["dev|a"]),
            (None, vec!["dev|a"]),
            (Some(""), vec![]),
            (Some("system:auto-approval"), vec![]),
        ] {
            assert!(matches!(
                ensure_independent_decider(d, req, "refund"),
                Err(KernelError::Forbidden(_))
            ));
        }
    }
}
