//! # sutra-api — composition root of the SutraERP modular monolith
//!
//! Wires infrastructure (config, telemetry, PostgreSQL pool, stores) to the module crates and
//! exposes one Axum application:
//!
//! * `/health/live`, `/health/ready`
//! * `/api/v1/...` module routes (OpenAPI 3.1 generated from code via utoipa)
//! * `/api/openapi.json`
//!
//! Only this crate knows about every module; modules know only contracts (ADR-L5-0003).

pub mod app;
pub mod config;
pub mod config_b9;
pub mod config_http; // Part 1 · Stage 3: transport settings + production start-up gate
pub mod config_idp; // Part 2 · S4-idp: identity provisioning settings
pub mod idp_wiring; // Part 2 · S4-idp: identity provisioning composition
pub mod metrics; // 9A
pub mod metrics_bridge; // INT: 9C `metrics` facade → 9A registry
pub mod openapi;
pub mod payment_gateway; // Part 2 S4-pay: Razorpay adapter of the finance gateway port
pub mod shutdown;
pub mod telemetry;
