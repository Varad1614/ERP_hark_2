//! Shared handler state of every Domain D router.

use crate::shared::approval::ApprovalMode;
use sqlx::PgPool;
use std::sync::Arc;
use sutra_contracts::academic::ports::OfferingDirectory;
use sutra_contracts::campus::TeachingSpacePort;
use sutra_contracts::institution::reference::InstitutionReferencePort;
use sutra_contracts::people::StaffDirectory;
use sutra_contracts::platform::workflow::WorkflowPort;
use sutra_contracts::student::ports::{CreditPostingPort, RegistrationReader, StudentDirectory};

/// Handler state: the owner's pool plus every port Domain D calls. Cheap to clone.
#[derive(Clone)]
pub struct AcademicState {
    pub pool: PgPool,
    /// Approval steps (ADR-L5-0023; `NoopWorkflowAdapter` in Batch 4).
    pub workflow: Arc<dyn WorkflowPort>,
    pub approvals: ApprovalMode,
    /// Domain B reference data (institution + timezone, campus, org unit, curriculum version and
    /// components, academic year and term, key dates).
    pub references: Arc<dyn InstitutionReferencePort>,
    /// Domain C students and their live StudentContext (rosters, self-service resolution).
    pub students: Arc<dyn StudentDirectory>,
    /// Domain C (3B) registrations: gates 4A offering enrolment.
    pub registrations: Arc<dyn RegistrationReader>,
    /// Domain C (3B) credit ledger: 4C posts recognised credits.
    pub credits: Arc<dyn CreditPostingPort>,
    /// The offering read port (4A's shared tables) — 4B and 4C read offerings/sections/rosters
    /// only through this.
    pub offerings: Arc<dyn OfferingDirectory>,
    /// Campus teaching spaces (stub until Batch 8).
    pub spaces: Arc<dyn TeachingSpacePort>,
    /// People staff records behind faculty login subjects (stub until Batch 7).
    pub staff: Arc<dyn StaffDirectory>,
}
