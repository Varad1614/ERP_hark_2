//! Domain H — Institutional services & intelligence contracts (Batch 8, see
//! `docs/batch8-contracts.md`).
//!
//! Batch 8 implements H07 enterprise dashboards and H08 regulatory reporting (slice 8C) as
//! **read-only projections** over the other domains. Every figure comes through
//! [`ports::OperationalMetricsPort`], which each owning domain implements over its own schema
//! (foundation adapters `PgStudentMetrics`, `PgPeopleMetrics`, `PgFinanceMetrics`,
//! `PgCampusMetrics`, `PgResourceMetrics`). Other modules must only ever reach this domain
//! through items declared here.

pub mod events;
pub mod ids;
pub mod ports;
