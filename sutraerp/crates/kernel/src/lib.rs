//! # sutra-kernel
//!
//! The shared technical kernel of the SutraERP modular monolith.
//!
//! It holds only *technical* primitives that every module needs and that carry no business
//! meaning of their own (ADR-L5-0003 §7, ADR-L5-0036 §3):
//!
//! * [`auth`] – authenticated context, deny-by-default role→permission policy, session resolver port
//! * [`secret`] – CSPRNG tokens, SHA-256/HMAC, constant-time compare (ADR-L5-0038)
//! * [`id`] – UUIDv7 technical identifiers (ADR-L5-0036 §3.3)
//! * [`scope`] – Tenant-rooted execution scope (ADR-L5-0004)
//! * [`money`] – `rust_decimal` monetary scalar (ADR-L5-0032)
//! * [`concurrency`] – optimistic-concurrency version tokens (ADR-L5-0006)
//! * [`events`] – CloudEvents-compatible envelope + publisher port (ADR-L5-0007 / 0035)
//! * [`messaging`] – broker-neutral `MessagePublisher` port + subject derivation (ADR-L5-0022)
//! * [`cache`] – advisory, scope-namespaced `Cache` port (ADR-L5-0037)
//! * [`audit`] – audit entry + sink port
//! * [`idempotency`] – idempotency store port (ADR-L5-0034 §3.6)
//! * [`health`] – readiness probe port (ADR-L5-0044)
//! * [`module`] – module descriptors for the composition root
//! * [`error`] – framework-neutral technical error taxonomy
//!
//! The kernel must never depend on Axum, SQLx or any domain module.

pub mod audit;
pub mod auth;
pub mod cache;
pub mod concurrency;
pub mod error;
pub mod events;
pub mod health;
pub mod id;
pub mod idempotency;
pub mod messaging;
pub mod module;
pub mod money;
pub mod scope;
pub mod secret;

pub use error::{KernelError, KernelResult};
