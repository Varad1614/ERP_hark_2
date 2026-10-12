//! Domain C — Student Lifecycle contracts (Batch 3, see `docs/batch3-contracts.md`).
//!
//! * [`ids`] — typed UUIDv7 ids of every Batch 3 aggregate (foundation-owned, append-only).
//! * [`status`] — lifecycle state enums; the shared Student / StudentContext machines carry
//!   their transitions here, every other machine's transitions are slice-owned.
//! * [`events`] — released event type names + V1 payloads, one file per slice.
//! * [`ports`] — what other domains may ask Domain C ([`ports::StudentDirectory`]) and the
//!   external-integration ports Domain C calls (ABC, external scholarship ecosystems).
//!
//! Other modules must only ever reach this domain through items declared here.

pub mod events;
pub mod ids;
pub mod ports;
pub mod status;
