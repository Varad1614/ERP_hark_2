//! # sutra-contracts
//!
//! Public, framework-neutral contracts between SutraERP modules (ADR-L5-0003 §7.4).
//!
//! **Boundary rule (enforced by `apps/api/tests/architecture.rs`):** a domain module crate may
//! depend on `sutra-kernel`, `sutra-contracts`, `sutra-web` and `sutra-persistence` only — never on
//! another domain module. Collaboration happens exclusively through the ports (traits) and DTOs
//! declared here; the composition root (`apps/api`) wires implementations to ports.
//!
//! One submodule per domain (letters follow the architecture's domain map):

pub mod academic; // D
pub mod campus; // G
pub mod exams; // I
pub mod finance; // F
pub mod institution; // B
pub mod people; // E
pub mod platform; // A
pub mod resources; // J
pub mod services; // H
pub mod student; // C
