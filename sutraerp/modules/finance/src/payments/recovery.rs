//! Part 2 · Stage 4 (S4-pay) — **gateway payment recovery job** (L3-F-010 safe recovery).
//!
//! A payer may close the browser after paying, a webhook may never arrive, or the gateway may
//! answer late. `payment_gateway_recovery` (a sweeper: one replica at a time under its advisory
//! lock) asks the configured gateway for the outcome of:
//!
//! * open gateway payments (`initiated` / `pending`) older than `min_age`, and
//! * `failed` ones younger than `expire_after` (a late capture after a failed attempt — money
//!   received is a fact, `failed → completed` is a legal transition),
//!
//! with exponential back-off per payment (`gateway_polled_at`, `gateway_poll_count`: next poll
//! after `min_age × 2^min(count, 6)`). Captured → completed (allocation + posting + receipt
//! through the same `apply_outcome` as `confirm` and the webhook); failed → failed; still pending
//! after `expire_after` → expired (`TIMEOUT`). A gateway error leaves the payment untouched (never
//! a guessed outcome). Every poll is audited (`finance.payment.recovery_poll`) in the payment's
//! tenant.

use super::http::{apply_outcome, outcome_label};
use super::service::{close_unpaid, load_payment};
use crate::shared::publish::audit;
use crate::shared::{Actor, FinanceState};
use serde_json::json;
use sqlx::{PgConnection, Postgres, Row, Transaction};
use std::time::Duration;
use sutra_contracts::finance::ports::GatewayPaymentOutcome;
use sutra_contracts::finance::status::PaymentStatus;
use sutra_kernel::db_context::DbContext;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, TenantId};
use sutra_persistence::jobs::SweepJob;
use sutra_persistence::map_db_error;
use uuid::Uuid;

/// Sweeper name (advisory-lock identity, log field).
pub const JOB_NAME: &str = "payment_gateway_recovery";

/// The recovery job.
#[derive(Clone)]
pub struct GatewayRecovery {
    pub state: FinanceState,
    /// Run interval (0 = off).
    pub interval: Duration,
    /// First poll after this age; base of the back-off.
    pub min_age: Duration,
    /// Open payments still pending after this are expired; failed ones are re-checked until then.
    pub expire_after: Duration,
    /// Payments polled per run.
    pub batch_size: u32,
}

/// What one run did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RecoveryReport {
    pub polled: u64,
    pub completed: u64,
    pub failed: u64,
    pub expired: u64,
    pub unchanged: u64,
    pub errors: u64,
}

/// A payment the job will poll.
#[derive(Clone, Copy, Debug)]
struct Candidate {
    id: Uuid,
    tenant: TenantId,
    /// Older than `expire_after`.
    overdue: bool,
}

async fn tenant_tx(
    st: &FinanceState,
    tenant: TenantId,
) -> Result<Transaction<'static, Postgres>, KernelError> {
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    sutra_persistence::pool::apply_context(
        &mut tx,
        Some(DbContext::Scoped(ExecutionScope::tenant(tenant))),
        true,
    )
    .await
    .map_err(map_db_error)?;
    Ok(tx)
}

impl GatewayRecovery {
    async fn candidates(&self, conn: &mut PgConnection) -> Result<Vec<Candidate>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT id, tenant_id, created_at < now() - make_interval(secs => $3) AS overdue \
             FROM finance.payment \
             WHERE channel = 'gateway' AND gateway = $1 AND gateway_order_ref IS NOT NULL \
               AND (status IN ('initiated', 'pending') \
                    OR (status = 'failed' AND created_at > now() - make_interval(secs => $3))) \
               AND created_at < now() - make_interval(secs => $2) \
               AND (gateway_polled_at IS NULL OR gateway_polled_at < now() - make_interval( \
                    secs => $2 * power(2.0::float8, LEAST(gateway_poll_count, 6)::float8))) \
             ORDER BY gateway_polled_at NULLS FIRST, created_at, id LIMIT $4",
        )
        .bind(self.state.gateway.name())
        .bind(self.min_age.as_secs_f64())
        .bind(self.expire_after.as_secs_f64())
        .bind(i64::from(self.batch_size.max(1)))
        .fetch_all(&mut *conn)
        .await?;
        rows.iter()
            .map(|r| {
                Ok(Candidate {
                    id: r.try_get("id")?,
                    tenant: TenantId(r.try_get("tenant_id")?),
                    overdue: r.try_get("overdue")?,
                })
            })
            .collect()
    }

    /// Polls one payment; returns `completed | failed | expired | unchanged`.
    async fn recover_one(&self, c: Candidate) -> Result<&'static str, KernelError> {
        let st = &self.state;
        let mut tx = tenant_tx(st, c.tenant).await?;
        let before = load_payment(&mut tx, c.tenant, c.id, false).await?;
        tx.commit().await.map_err(map_db_error)?;
        let Some(order) = before.gateway_order_ref.clone() else {
            return Ok("unchanged");
        };
        let scope = ExecutionScope::institution(before.tenant_id, before.institution_id);
        // Network call outside any transaction.
        let fetched = st.gateway.fetch_outcome(&scope, &order).await;
        let a = Actor::system(Some(format!("recovery:{}", c.id)));
        let mut tx = tenant_tx(st, c.tenant).await?;
        let p = load_payment(&mut tx, c.tenant, c.id, true).await?;
        let attempt: i32 = sqlx::query_scalar(
            "UPDATE finance.payment SET gateway_polled_at = now(), \
             gateway_poll_count = gateway_poll_count + 1 WHERE tenant_id = $1 AND id = $2 \
             RETURNING gateway_poll_count",
        )
        .bind(c.tenant.as_uuid())
        .bind(c.id)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?;
        let details = |outcome: &str, result: &str, error: Option<String>| {
            json!({"paymentNumber": p.payment_number, "gateway": st.gateway.name(),
                   "orderRef": order, "outcome": outcome, "from": p.status.as_str(),
                   "result": result, "attempt": attempt, "error": error})
        };
        let outcome = match fetched {
            Ok(o) => o,
            Err(e) => {
                audit(
                    &mut tx,
                    &scope,
                    "finance.payment.recovery_poll",
                    "payment",
                    c.id.to_string(),
                    details("error", "unchanged", Some(e.to_string())),
                    &a,
                )
                .await?;
                tx.commit().await.map_err(map_db_error)?;
                return Err(e);
            }
        };
        let open = matches!(p.status, PaymentStatus::Initiated | PaymentStatus::Pending);
        let applied: Result<&'static str, KernelError> = match &outcome {
            _ if !open && p.status != PaymentStatus::Failed => Ok("unchanged"),
            GatewayPaymentOutcome::Captured { .. } => apply_outcome(st, &mut tx, &p, &outcome, &a)
                .await
                .map(|_| "completed"),
            GatewayPaymentOutcome::Failed { .. } if open => {
                apply_outcome(st, &mut tx, &p, &outcome, &a)
                    .await
                    .map(|_| "failed")
            }
            GatewayPaymentOutcome::Pending if open && c.overdue => close_unpaid(
                &mut tx,
                &scope,
                &p,
                PaymentStatus::Expired,
                "no gateway outcome before the recovery deadline",
                Some("TIMEOUT"),
                &a,
            )
            .await
            .map(|_| "expired"),
            _ => Ok("unchanged"),
        };
        match applied {
            Ok(result) => {
                audit(
                    &mut tx,
                    &scope,
                    "finance.payment.recovery_poll",
                    "payment",
                    c.id.to_string(),
                    details(outcome_label(&outcome), result, None),
                    &a,
                )
                .await?;
                tx.commit().await.map_err(map_db_error)?;
                Ok(result)
            }
            Err(e) => {
                // Keep the back-off and the audit even when applying the outcome failed
                // (e.g. the gateway captured a different amount → 409 for a human).
                tx.rollback().await.map_err(map_db_error)?;
                let mut tx = tenant_tx(st, c.tenant).await?;
                sqlx::query(
                    "UPDATE finance.payment SET gateway_polled_at = now(), \
                     gateway_poll_count = gateway_poll_count + 1 WHERE tenant_id = $1 AND id = $2",
                )
                .bind(c.tenant.as_uuid())
                .bind(c.id)
                .execute(&mut *tx)
                .await
                .map_err(map_db_error)?;
                audit(
                    &mut tx,
                    &scope,
                    "finance.payment.recovery_poll",
                    "payment",
                    c.id.to_string(),
                    details(outcome_label(&outcome), "error", Some(e.to_string())),
                    &a,
                )
                .await?;
                tx.commit().await.map_err(map_db_error)?;
                Err(e)
            }
        }
    }

    async fn process(&self, candidates: Vec<Candidate>) -> RecoveryReport {
        let mut report = RecoveryReport::default();
        for c in candidates {
            report.polled += 1;
            match self.recover_one(c).await {
                Ok("completed") => report.completed += 1,
                Ok("failed") => report.failed += 1,
                Ok("expired") => report.expired += 1,
                Ok(_) => report.unchanged += 1,
                Err(e) => {
                    report.errors += 1;
                    tracing::warn!(payment = %c.id, error = %e, "payment recovery poll failed");
                }
            }
        }
        report
    }

    /// One run outside the scheduler (tests, operators). Must run in the system database
    /// context (`sutra_kernel::db_context::system`), like every sweeper.
    pub async fn run_now(&self) -> Result<RecoveryReport, KernelError> {
        let mut conn = self.state.pool.acquire().await.map_err(map_db_error)?;
        let candidates = self.candidates(&mut conn).await.map_err(map_db_error)?;
        drop(conn);
        Ok(self.process(candidates).await)
    }
}

#[async_trait::async_trait]
impl SweepJob for GatewayRecovery {
    fn name(&self) -> &'static str {
        JOB_NAME
    }

    fn interval(&self) -> Duration {
        self.interval
    }

    async fn run(&self, conn: &mut PgConnection) -> Result<u64, sqlx::Error> {
        let candidates = self.candidates(conn).await?;
        let report = self.process(candidates).await;
        tracing::info!(
            polled = report.polled,
            completed = report.completed,
            failed = report.failed,
            expired = report.expired,
            errors = report.errors,
            "payment gateway recovery"
        );
        Ok(report.polled)
    }
}
