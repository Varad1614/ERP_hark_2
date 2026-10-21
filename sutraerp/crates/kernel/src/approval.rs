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

tokio::task_local! {
    /// Subject of the authenticated user serving the current request (set by the web
    /// authentication middleware; absent in background jobs and consumers).
    static REQUESTER: Option<String>;
}

/// Runs `f` with `subject` as the request's authenticated user (Part 1 · S3-appr). The web
/// authentication middleware wraps every authenticated request in it, so a module's
/// `request_approval` can record **who asked** for a governed decision
/// (`platform.approval_request`) without threading the actor through every call site.
pub async fn with_requester<F: std::future::Future>(subject: Option<String>, f: F) -> F::Output {
    REQUESTER.scope(subject, f).await
}

/// The authenticated user of the request being served, if any (see [`with_requester`]).
pub fn current_requester() -> Option<String> {
    REQUESTER
        .try_with(Clone::clone)
        .ok()
        .flatten()
        .filter(|s| !s.trim().is_empty())
}

// Audit C2 (finding M1): approval-ledger rows a request's `request_approval` queued for its
// owning transaction (the persistence crate writes them with the transaction's event; see
// `sutra_persistence::approval::with_request_ledger`). The queue lives here so the web
// middleware (which only depends on the kernel) can scope it to the request.

/// One approval-ledger row queued for the owning transaction.
pub trait QueuedLedgerRow: std::any::Any + Send {
    /// Writes the row on its own (the request succeeded but its transaction emitted no event).
    fn write_alone(
        self: Box<Self>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>;
    /// For the writer to recover its concrete row type.
    fn into_any(self: Box<Self>) -> Box<dyn std::any::Any + Send>;
}

tokio::task_local! {
    static LEDGER: std::sync::Mutex<Vec<Box<dyn QueuedLedgerRow>>>;
}

/// Runs `f` with a request-scoped ledger queue; returns its output and the rows no owning
/// transaction took.
pub async fn with_request_ledger<F: std::future::Future>(
    f: F,
) -> (F::Output, Vec<Box<dyn QueuedLedgerRow>>) {
    LEDGER
        .scope(std::sync::Mutex::new(Vec::new()), async {
            let out = f.await;
            (out, take_ledger_rows())
        })
        .await
}

/// Queues `row` for the owning transaction; gives it back outside [`with_request_ledger`].
#[allow(clippy::result_large_err)]
pub fn queue_ledger_row(row: Box<dyn QueuedLedgerRow>) -> Result<(), Box<dyn QueuedLedgerRow>> {
    let mut slot = Some(row);
    let _ = LEDGER.try_with(|q| {
        if let Some(r) = slot.take() {
            q.lock().unwrap_or_else(|e| e.into_inner()).push(r);
        }
    });
    match slot {
        None => Ok(()),
        Some(r) => Err(r),
    }
}

/// Drains the queued rows of the current request (empty outside [`with_request_ledger`]).
pub fn take_ledger_rows() -> Vec<Box<dyn QueuedLedgerRow>> {
    LEDGER
        .try_with(|q| std::mem::take(&mut *q.lock().unwrap_or_else(|e| e.into_inner())))
        .unwrap_or_default()
}

/// Actor ids that are never a human decider (automatic steps).
pub fn is_system_actor(id: &str) -> bool {
    id.starts_with("system:") || id == "system"
}

/// Separation of duties on the shared decision path: refuses a decision when the decider is
/// unidentified / a system actor, or is one of the request's makers (`requesters`: creator,
/// requester, submitter — whoever moved the aggregate into its pending state) — `403` — and
/// fails closed with `409` when no maker is known at all. `what` names the governed kind in the
/// problem detail (e.g. `"refund"`).
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
    // Audit C1 (finding C2): fail closed. A decision whose maker was never recorded (a row
    // written by a background job, an import or a seed without a recorded request, or a lookup
    // that found no row) cannot prove two different people were involved, so it is refused
    // (`409`) instead of letting any identified human decide it. There is no break-glass path:
    // record the request through the approval workflow (re-submit it) and decide that.
    let makers: Vec<&str> = requesters
        .into_iter()
        .map(str::trim)
        .filter(|r| !r.is_empty())
        .collect();
    if makers.is_empty() {
        return Err(KernelError::Conflict(format!(
            "separation of duties: no recorded requester for this {what}, so an independent \
             decision cannot be verified; re-submit it for approval before deciding"
        )));
    }
    if makers.contains(&decider) {
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
        assert_eq!(
            ApprovalMode::parse("human"),
            Some(ApprovalMode::AwaitDecision)
        );
        assert_eq!(
            ApprovalMode::parse(" AUTO "),
            Some(ApprovalMode::AutoApprove)
        );
        assert_eq!(ApprovalMode::parse("yes"), None);
        assert_eq!(ApprovalMode::AwaitDecision.as_str(), "human");
    }

    #[tokio::test]
    async fn the_requester_is_only_visible_inside_its_request() {
        assert_eq!(current_requester(), None);
        let inside = with_requester(Some("dev|a".into()), async { current_requester() }).await;
        assert_eq!(inside.as_deref(), Some("dev|a"));
        let blank = with_requester(Some("  ".into()), async { current_requester() }).await;
        assert_eq!(blank, None);
        assert_eq!(current_requester(), None);
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

    #[test]
    fn a_decision_without_a_recorded_maker_fails_closed() {
        for req in [vec![], vec!["", "  "]] {
            assert!(matches!(
                ensure_independent_decider(Some("dev|b"), req, "refund"),
                Err(KernelError::Conflict(_))
            ));
        }
        // the decider checks still come first (403, not 409)
        assert!(matches!(
            ensure_independent_decider(None, Vec::<&str>::new(), "refund"),
            Err(KernelError::Forbidden(_))
        ));
    }

    struct Row(u8);
    impl QueuedLedgerRow for Row {
        fn write_alone(
            self: Box<Self>,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
            Box::pin(async {})
        }
        fn into_any(self: Box<Self>) -> Box<dyn std::any::Any + Send> {
            self
        }
    }

    #[tokio::test]
    async fn ledger_rows_queue_only_inside_a_request() {
        assert!(queue_ledger_row(Box::new(Row(0))).is_err());
        let ((), rest) = with_request_ledger(async {
            assert!(queue_ledger_row(Box::new(Row(1))).is_ok());
            assert_eq!(
                take_ledger_rows().len(),
                1,
                "taken by the owning transaction"
            );
            assert!(queue_ledger_row(Box::new(Row(2))).is_ok());
        })
        .await;
        let left: Vec<u8> = rest
            .into_iter()
            .filter_map(|r| r.into_any().downcast::<Row>().ok().map(|b| b.0))
            .collect();
        assert_eq!(left, vec![2]);
        assert!(take_ledger_rows().is_empty());
    }
}
