//! Domain E — People & Workforce contracts (Batch 7, see `docs/batch7-contracts.md`).
//!
//! * [`staff`] — the Batch 4 [`StaffDirectory`] Domain D reads (staff behind a login subject);
//!   real since the Batch 7 foundation (`sutra_people::PgStaffDirectory`).
//! * [`ids`] — typed UUIDv7 ids of every Batch 7 aggregate (foundation-owned, append-only).
//! * [`status`] — lifecycle / classification enums; the shared machines (staff record,
//!   engagement, leave ledger kinds, attendance days) carry their rules here, every other
//!   machine's transitions are slice-owned.
//! * [`events`] — released event type names + V1 payloads, one file per slice.
//! * [`ports`] — [`ports::WorkforceDirectory`] ("7A → 7B / 7C") and [`ports::LeaveReader`]
//!   ("7B → 7C") over the foundation tables.
//!
//! Domain E reaches other domains only through their contract ports:
//! `institution::reference::InstitutionReferencePort` (institution + timezone, org units,
//! campuses), `academic::ports::FacultyWorkloadPort` (teaching load of a faculty member),
//! `finance::ports::PayrollPostingPort` (payroll liability postings). Money and leave days are
//! `rust_decimal` end to end — never floats. Other modules must only ever reach this domain
//! through items declared here.

pub mod events;
pub mod ids;
pub mod ports;
pub mod staff;
pub mod status;

pub use staff::*;
