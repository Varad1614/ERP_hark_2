//! # sutra-kernel
//!
//! The shared technical kernel of the SutraERP modular monolith.
//!
//! It holds only *technical* primitives that every module needs and that carry no business
//! meaning of their own (ADR-L5-0003 §7, ADR-L5-0036 §3):
//!
//! * [`approval`] – approval mode (human decision vs dev auto-approval) + separation of duties
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
//! * [`text`] – server-side text policy: identity names refuse markup / control characters,
//!   Indian mobile, e-mail and date-of-birth rules (audit 2026-10 A2)
//! * [`india`] – Indian statutory identifier formats (GSTIN check character, GST state codes)
//! * [`mail`] – outbound e-mail port (notification e-mail channel)
//! * [`health`] – readiness probe port (ADR-L5-0044)
//! * [`module`] – module descriptors for the composition root
//! * [`entitlement`] – module / package registry, licence state, entitlement access rule (Part 2 · S7)
//! * [`error`] – framework-neutral technical error taxonomy
//!
//! The kernel must never depend on Axum, SQLx or any domain module.

pub mod approval;
pub mod audit;
pub mod audit_chain;
pub mod auth;
pub mod cache;
pub mod concurrency;
pub mod db_context; // Part 1 S3: DEF-X-003 row-level security context
pub mod entitlement; // Part 2 · S7-entitle: module entitlements + licence state
pub mod error;
pub mod events;
pub mod health;
pub mod id;
pub mod idempotency;
pub mod india; // ERP_EXTENSIONS 5D: GSTIN + GST state codes (moved from resources 3D)
pub mod mail;
pub mod messaging;
pub mod module;
pub mod money;
pub mod scope;
pub mod secret;
pub mod signal; // Part 2 S5-monitor: security / integration signals → Prometheus
pub mod text; // Audit 2026-10 A2: identity-text / contact / birth-date policy (SEC-1, VAL-1..3)
pub mod tls; // Part 2 · S5-capacity: SUTRA_TLS_CA_FILE extra trusted roots
pub mod weblink;

pub use error::{KernelError, KernelResult};
