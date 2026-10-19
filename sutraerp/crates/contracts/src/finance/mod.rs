//! Domain F — Finance contracts (Batch 5, see `docs/batch5-contracts.md`).
//!
//! * [`clearance`] — the Batch 3 [`FinanceClearancePort`] Domain C reads (admission financial
//!   clearance, registration holds, exit clearance); real since the Batch 5 foundation.
//! * [`ids`] — typed UUIDv7 ids of every Batch 5 aggregate (foundation-owned, append-only).
//! * [`status`] — lifecycle / classification enums; the shared [`status::ChargeStatus`]
//!   machine carries its transitions here, every other machine's transitions are slice-owned.
//! * [`events`] — released event type names + V1 payloads, one file per slice.
//! * [`ports`] — [`ports::ChargeDirectory`], [`ports::ReceivableReader`], [`ports::PostingPort`]
//!   and [`ports::PaymentGatewayPort`].
//!
//! Domain F reaches other domains only through their contract ports:
//! `institution::reference::InstitutionReferencePort` (institution + timezone, terms),
//! `student::ports::{StudentDirectory, AidDecisionSource}` (students; approved scholarship
//! decisions handed off to Financial Aid). Other modules must only ever reach this domain
//! through items declared here. Money is `rust_decimal` end to end (NUMERIC(18,2), INR).

pub mod clearance;
pub mod events;
pub mod ids;
pub mod ports;
pub mod status;

pub use clearance::*;
