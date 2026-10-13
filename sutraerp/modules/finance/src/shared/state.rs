//! Shared handler state of every Domain F router.

use crate::shared::approval::ApprovalMode;
use sqlx::PgPool;
use std::sync::Arc;
use sutra_contracts::finance::ports::{
    ChargeDirectory, PaymentGatewayPort, PostingPort, ReceivableReader,
};
use sutra_contracts::institution::reference::InstitutionReferencePort;
use sutra_contracts::platform::workflow::WorkflowPort;
use sutra_contracts::student::ports::{AidDecisionSource, StudentDirectory};

/// Handler state: the owner's pool plus every port Domain F calls. Cheap to clone.
#[derive(Clone)]
pub struct FinanceState {
    pub pool: PgPool,
    /// Approval steps (ADR-L5-0023; `NoopWorkflowAdapter` in Batch 5).
    pub workflow: Arc<dyn WorkflowPort>,
    pub approvals: ApprovalMode,
    /// Domain B reference data (institution + timezone, academic years and terms).
    pub references: Arc<dyn InstitutionReferencePort>,
    /// Domain C students (charge parties, self-service resolution).
    pub students: Arc<dyn StudentDirectory>,
    /// Domain C (3C) scholarship decisions handed off to Financial Aid — 5A aid.
    pub aid: Arc<dyn AidDecisionSource>,
    /// Payment gateway adapter — 5B (mock in Batch 5).
    pub gateway: Arc<dyn PaymentGatewayPort>,
    /// Charges and party accounts (foundation tables) — 5B / 5C read charges only through this
    /// or the read-only `shared::charge::load_*`.
    pub charges: Arc<dyn ChargeDirectory>,
    /// Party balances (outstanding / overdue).
    pub receivables: Arc<dyn ReceivableReader>,
    /// Ledger posting for code that is **not** inside a finance transaction (inside one, call
    /// `shared::posting::post` on the transaction).
    pub posting: Arc<dyn PostingPort>,
}
