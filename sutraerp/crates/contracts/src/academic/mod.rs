//! Domain D — Academic delivery contracts (Batch 4, see `docs/batch4-contracts.md`).
//!
//! * [`ids`] — typed UUIDv7 ids of every Batch 4 aggregate (foundation-owned, append-only).
//! * [`status`] — lifecycle state enums; the shared Course Offering / Section / Faculty
//!   Assignment / Section Enrolment machines carry their transitions here, every other
//!   machine's transitions are slice-owned.
//! * [`events`] — released event type names + V1 payloads, one file per slice.
//! * [`ports`] — what other slices and domains may ask Domain D ([`ports::OfferingDirectory`]).
//!
//! Domain D reaches other domains only through their contract ports:
//! `institution::reference::InstitutionReferencePort` (terms, curriculum components, campuses),
//! `student::ports::{StudentDirectory, RegistrationReader, CreditPostingPort}`,
//! `campus::TeachingSpacePort` (rooms, stub until Batch 8) and `people::StaffDirectory`
//! (faculty, stub until Batch 7). Other modules must only ever reach this domain through items
//! declared here.

pub mod events;
pub mod ids;
pub mod ports;
pub mod status;
