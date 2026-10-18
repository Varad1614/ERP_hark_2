//! # sutra-web
//!
//! Shared HTTP platform layer for every module router (Axum/Tower, ADR-L5-0019):
//!
//! * [`problem`] – RFC 9457 `application/problem+json` errors with SutraERP extensions
//!   (`errorCode`, `correlationId`, `validationErrors`) — ADR-L5-0034 §3.4
//! * [`request_id`] – request/correlation id propagation (`X-Request-Id`)
//! * [`auth`] – session authentication middleware, CSRF synchronizer-token enforcement and the
//!   [`Authenticated`] principal extractor with deny-by-default permission checks, the
//!   pre-session (login) double-submit CSRF token and the cross-site `Origin` guard —
//!   ADR-L5-0038/0024
//! * [`scope`] – session-bound Tenant/Institution execution-scope extractor — ADR-L5-0004
//! * [`idempotency`] – `Idempotency-Key` middleware — ADR-L5-0034 §3.6
//! * [`health`] – `/health/live` and `/health/ready` — ADR-L5-0044
//! * [`security_headers`], [`cors`], [`rate_limit`] – response hardening headers, CORS allow-list and
//!   public-endpoint rate limiting (Part 1 · Stage 3, `docs/security/config.md`)
//! * [`entitlement`] – server-side module entitlement and licence gate (Part 2 · S7-entitle)
//!
//! Framework types defined here never leak into canonical contracts (`sutra-contracts`).

pub mod auth;
pub mod cors;
pub mod entitlement; // Part 2 · S7-entitle: module entitlement / licence gate
pub mod health;
pub mod idempotency;
pub mod problem;
pub mod rate_limit;
pub mod request_id;
pub mod scope;
pub mod security_headers;

pub use auth::{Authenticated, OriginPolicy, PreSessionCsrf, SessionAuth};
pub use problem::{ApiError, ApiResult, Problem};
pub use scope::Scope;
