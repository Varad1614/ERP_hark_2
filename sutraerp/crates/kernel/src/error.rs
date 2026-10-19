//! Framework-neutral technical error taxonomy.
//!
//! The HTTP layer (`sutra-web`) maps every variant onto an RFC 9457 Problem Details response
//! (ADR-L5-0034 §3.4). Business rejections that are part of a canonical business contract must
//! be modelled as explicit outcomes by the owning module, not squeezed into these variants
//! (ADR-L5-0034 §3.5).

use std::fmt;

/// A single field-level validation failure (`validationErrors[]` in problem+json).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct FieldViolation {
    /// JSON pointer or dotted path of the offending member.
    pub field: String,
    /// Stable machine-readable reason code.
    pub code: String,
    /// Human readable explanation.
    pub message: String,
}

impl FieldViolation {
    pub fn new(
        field: impl Into<String>,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            field: field.into(),
            code: code.into(),
            message: message.into(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum KernelError {
    #[error("validation failed")]
    Validation(Vec<FieldViolation>),
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("authentication required")]
    Unauthenticated,
    #[error("forbidden: {0}")]
    Forbidden(String),
    /// The permission is granted but needs a session that completed multi-factor authentication
    /// (step-up, ADR-L5-0038 §3.8). Carries the permission name.
    #[error("multi-factor step-up required for `{0}`")]
    StepUpRequired(&'static str),
    /// Missing, ambiguous, mismatched or widened Tenant/Institution scope (ADR-L5-0004: fail closed).
    #[error("scope violation: {0}")]
    Scope(String),
    #[error("{entity} not found")]
    NotFound { entity: &'static str },
    /// Optimistic-concurrency precondition failed (ADR-L5-0006).
    #[error("version conflict: expected {expected}, actual {actual}")]
    VersionConflict { expected: i64, actual: i64 },
    #[error("conflict: {0}")]
    Conflict(String),
    /// Same Idempotency-Key reused with a materially different request (ADR-L5-0034 §3.6).
    #[error("idempotency key reused with a different request")]
    IdempotencyMismatch,
    #[error("a request with this idempotency key is still in progress")]
    IdempotencyInFlight,
    #[error("not implemented: {0}")]
    NotImplemented(&'static str),
    #[error("dependency unavailable: {0}")]
    Unavailable(String),
    #[error("internal error")]
    Internal(#[source] Box<dyn std::error::Error + Send + Sync>),
    /// Part 2 · S7-entitle: the tenant's licence does not cover the operation (`403`). `code` is
    /// one of the stable `entitlement.*` codes of [`crate::entitlement`] (e.g.
    /// `entitlement.module_inactive` when an import job targets a module that is not active).
    #[error("not entitled: {detail}")]
    Entitlement { code: &'static str, detail: String },
}

impl KernelError {
    /// Stable machine-readable error code (`errorCode` extension member).
    pub fn code(&self) -> &'static str {
        match self {
            Self::Validation(_) => "validation-failed",
            Self::BadRequest(_) => "bad-request",
            Self::Unauthenticated => "unauthenticated",
            Self::Forbidden(_) => "forbidden",
            Self::StepUpRequired(_) => "mfa-step-up-required",
            Self::Scope(_) => "scope-violation",
            Self::NotFound { .. } => "not-found",
            Self::VersionConflict { .. } => "version-conflict",
            Self::Conflict(_) => "conflict",
            Self::IdempotencyMismatch => "idempotency-key-reused",
            Self::IdempotencyInFlight => "idempotency-in-flight",
            Self::NotImplemented(_) => "not-implemented",
            Self::Unavailable(_) => "dependency-unavailable",
            Self::Internal(_) => "internal-error",
            Self::Entitlement { code, .. } => code,
        }
    }

    pub fn internal<E>(err: E) -> Self
    where
        E: std::error::Error + Send + Sync + 'static,
    {
        Self::Internal(Box::new(err))
    }

    pub fn internal_msg(msg: impl fmt::Display) -> Self {
        Self::Internal(msg.to_string().into())
    }
}

pub type KernelResult<T> = Result<T, KernelError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_stable_kebab_case() {
        let all = [
            KernelError::Validation(vec![]),
            KernelError::Unauthenticated,
            KernelError::StepUpRequired("x"),
            KernelError::Scope("x".into()),
            KernelError::NotFound {
                entity: "institution",
            },
            KernelError::VersionConflict {
                expected: 1,
                actual: 2,
            },
            KernelError::IdempotencyMismatch,
            KernelError::internal_msg("boom"),
        ];
        for e in all {
            let c = e.code();
            assert!(
                c.chars().all(|ch| ch.is_ascii_lowercase() || ch == '-'),
                "{c}"
            );
        }
    }
}
