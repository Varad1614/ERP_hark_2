//! The period guard (contract §3.5): every ledger posting asks [`require_open`] for the
//! financial period of its posting date inside the posting transaction.
//!
//! **Foundation stub, filled by slice 5C.** Until 5C lands there is no `finance.financial_period`
//! table, so every date is open and no period id is recorded. 5C replaces the **body** of
//! [`require_open`] (the signature is frozen): look up the period of `(institution, posting_date)`
//! with `FOR SHARE` (so a concurrent close waits for in-flight postings), then return
//! [`ensure_accepts_postings`] — `409` for a `planned | locked | closed | archived` period, and
//! `409` when an institution that has periods has none covering the date. Institutions with no
//! financial year configured stay open (Batch 5 dev / test data).

use sqlx::PgConnection;
use sutra_contracts::finance::status::FinancialPeriodStatus;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{InstitutionId, TenantId};
use uuid::Uuid;

/// The financial period id the entry belongs to (`None` while no periods are configured), or
/// `409 conflict` when the period does not accept postings.
pub async fn require_open(
    conn: &mut PgConnection,
    tenant: TenantId,
    institution: InstitutionId,
    posting_date: chrono::NaiveDate,
) -> Result<Option<Uuid>, KernelError> {
    // 5C: the period covering the date, share-locked so a concurrent close waits for the
    // in-flight posting (and a posting waits for an in-flight close).
    let covering: Option<(Uuid, String, String)> = sqlx::query_as(
        "SELECT id, status, name FROM finance.financial_period \
         WHERE tenant_id = $1 AND institution_id = $2 AND $3 BETWEEN start_date AND end_date \
         ORDER BY start_date DESC LIMIT 1 FOR SHARE",
    )
    .bind(tenant.as_uuid())
    .bind(institution.as_uuid())
    .bind(posting_date)
    .fetch_optional(&mut *conn)
    .await
    .map_err(sutra_persistence::map_db_error)?;
    match covering {
        Some((id, status, name)) => {
            let status = FinancialPeriodStatus::parse(&status).ok_or_else(|| {
                KernelError::internal_msg(format!("unknown financial period status `{status}`"))
            })?;
            ensure_accepts_postings(Some((id, status, &name)))
        }
        None => {
            let configured: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM finance.financial_period \
                 WHERE tenant_id = $1 AND institution_id = $2)",
            )
            .bind(tenant.as_uuid())
            .bind(institution.as_uuid())
            .fetch_one(&mut *conn)
            .await
            .map_err(sutra_persistence::map_db_error)?;
            if configured {
                Err(KernelError::Conflict(format!(
                    "no financial period covers {posting_date}; establish the financial year first"
                )))
            } else {
                Ok(None)
            }
        }
    }
}

/// Pure rule shared by the stub and 5C's implementation: `period` is the `(id, status, name)`
/// covering the posting date, if any.
pub fn ensure_accepts_postings(
    period: Option<(Uuid, FinancialPeriodStatus, &str)>,
) -> Result<Option<Uuid>, KernelError> {
    match period {
        None => Ok(None),
        Some((id, status, _)) if status.accepts_postings() => Ok(Some(id)),
        Some((_, status, name)) => Err(KernelError::Conflict(format!(
            "financial period {name} is {status}; postings into it are refused"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closed_periods_refuse_postings() {
        let id = Uuid::nil();
        assert_eq!(ensure_accepts_postings(None).ok(), Some(None));
        assert_eq!(
            ensure_accepts_postings(Some((id, FinancialPeriodStatus::Open, "Apr 2026"))).ok(),
            Some(Some(id))
        );
        assert_eq!(
            ensure_accepts_postings(Some((id, FinancialPeriodStatus::Reopened, "Apr 2026"))).ok(),
            Some(Some(id))
        );
        for s in [
            FinancialPeriodStatus::Planned,
            FinancialPeriodStatus::Locked,
            FinancialPeriodStatus::Closed,
            FinancialPeriodStatus::Archived,
        ] {
            assert!(
                matches!(ensure_accepts_postings(Some((id, s, "Mar 2026"))), Err(KernelError::Conflict(m)) if m.contains("refused")),
                "{s}"
            );
        }
    }
}
