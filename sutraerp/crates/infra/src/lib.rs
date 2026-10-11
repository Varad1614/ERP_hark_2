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

pub mod nats;
pub mod smtp;
pub mod valkey;
