//! # sutra-infra
//!
//! Infrastructure adapters behind kernel ports. Only the composition root (`apps/api`) depends on
//! this crate; domain modules see the ports in `sutra-kernel` and never a product SDK.
//!
//! * [`nats`] – NATS JetStream [`MessagePublisher`](sutra_kernel::messaging::MessagePublisher)
//!   for the outbox relay (ADR-L5-0022): lazy connect, stream ensured from configuration,
//!   `Nats-Msg-Id` de-duplication, publish acks.
//! * [`valkey`] – Valkey [`Cache`](sutra_kernel::cache::Cache) adapter (ADR-L5-0037): lazy
//!   connection manager, short timeouts, connect cool-down, rustls for `rediss://`.
//! * [`smtp`] – SMTP [`MailSender`](sutra_kernel::mail::MailSender) for the notification
//!   e-mail channel (lettre, rustls; Mailpit in development).
//! * [`s3`] – S3-compatible object storage client (SigV4, MinIO / AWS S3) behind the document
//!   storage port (Part 2 · S4-docs).

pub mod nats;
pub mod opensearch; // 9C
pub mod s3; // Part 2 · S4-docs
pub mod smtp;
pub mod valkey;
