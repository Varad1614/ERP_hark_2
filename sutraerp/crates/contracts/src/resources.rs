//! Domain J — Enterprise resources contracts (Batch 8, see `docs/batch8-contracts.md`).
//!
//! * [`ids`] — typed UUIDv7 ids (foundation-owned, append-only).
//! * [`status`] — item / store / stock ledger enums (the stock ledger sign rule lives here).
//! * [`events`] — released event type names + V1 payloads (slice 8D).
//!
//! Domain J reaches Finance only through `finance::ports::ProcurementPostingPort` (vendor
//! invoices and payments, Batch 5 deferrals F-044 / 045 / 081 / 082) — never cross-schema SQL.
//! Other modules must only ever reach this domain through items declared here.

pub mod events;
pub mod ids;
pub mod status;
