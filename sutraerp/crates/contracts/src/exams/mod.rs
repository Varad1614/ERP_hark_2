//! Domain I — Examinations contracts (Batch 6, see `docs/batch6-contracts.md`).
//!
//! * [`results`] — the Batch 3 [`ResultsPort`] Domain C reads (published term results); real
//!   since the Batch 6 foundation (`sutra_exams::PgResultsPort`).
//! * [`ids`] — typed UUIDv7 ids of every Batch 6 aggregate (foundation-owned, append-only).
//! * [`status`] — lifecycle / classification enums; the shared machines (session, paper,
//!   registration, marks, result) carry their transitions here, every other machine's
//!   transitions are slice-owned.
//! * [`events`] — released event type names + V1 payloads, one file per slice.
//! * [`ports`] — [`ports::ExamDirectory`] ("6A → 6B / 6C") and [`ports::MarksReader`]
//!   ("6B → 6C") over the foundation tables.
//!
//! Domain I reaches other domains only through their contract ports:
//! `institution::reference::InstitutionReferencePort` (institution + timezone, terms, curriculum),
//! `student::ports::{StudentDirectory, RegistrationReader}` (candidates, term registrations),
//! `academic::ports::{OfferingDirectory, AcademicExamFacts}` (offerings, rosters, faculty,
//! finalised internal assessment, attendance standing), `finance::{FinanceClearancePort,
//! ports::ChargeDirectory, ports::ExamFeePort}` (dues for hall tickets, exam fee charges for
//! revaluation). Marks and grade points are `rust_decimal` end to end — never floats.

pub mod events;
pub mod ids;
pub mod ports;
pub mod results;
pub mod status;

pub use results::*;
