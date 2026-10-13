//! The real [`FinanceClearancePort`] (contract §6.2): Domain C's admission financial clearance
//! (3A), finance-sourced registration holds (3B) and exit clearance (3C) read the outstanding
//! amounts of the foundation `finance.charge` table. Replaces the Batch 3 stub.
//!
//! Evidence rule ([`evidence`]):
//! * no charge was ever raised for the subject (cancelled-only counts as none) →
//!   `NotApplicable` (so Batch 3 behaviour is unchanged until Finance raises a charge);
//! * an unpaid `published | active` charge → `Outstanding` (amount + count, safe for staff);
//! * otherwise, unpaid `disputed` charges only → `Pending` (Finance cannot state the fact yet);
//! * otherwise → `Cleared` with reference `finance:student_account:<id>`.

use crate::shared::money::display_inr;
use rust_decimal::Decimal;
use sqlx::{PgPool, Row};
use sutra_contracts::finance::{
    AdmissionFinanceQuery, FinanceClearancePort, FinanceEvidence, StudentFinanceQuery,
};
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::ExecutionScope;
use sutra_persistence::map_db_error;
use uuid::Uuid;

/// Aggregated dues of one subject.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Dues {
    /// Charges raised (published or later, cancelled excluded).
    pub raised: i64,
    /// Σ outstanding of `published | active` charges.
    pub outstanding: Decimal,
    /// Number of `published | active` charges with an outstanding amount.
    pub open: i64,
    /// Σ outstanding of `disputed` charges.
    pub disputed: Decimal,
    /// The party account (first one found), for the clearance reference.
    pub account: Option<Uuid>,
}

/// Pure evidence rule (see the module docs).
pub fn evidence(d: &Dues) -> FinanceEvidence {
    if d.raised == 0 {
        return FinanceEvidence::NotApplicable;
    }
    if d.outstanding > Decimal::ZERO {
        let n = d.open.max(1);
        return FinanceEvidence::Outstanding {
            detail: format!(
                "{} outstanding on {n} charge{}",
                display_inr(d.outstanding),
                if n == 1 { "" } else { "s" }
            ),
        };
    }
    if d.disputed > Decimal::ZERO {
        return FinanceEvidence::Pending {
            detail: format!("{} under dispute", display_inr(d.disputed)),
        };
    }
    FinanceEvidence::Cleared {
        reference: format!(
            "finance:student_account:{}",
            d.account.map(|a| a.to_string()).unwrap_or_default()
        ),
    }
}

/// [`FinanceClearancePort`] over `finance.charge`.
#[derive(Clone)]
pub struct PgFinanceClearance {
    pool: PgPool,
}

impl PgFinanceClearance {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    async fn dues(
        &self,
        scope: &ExecutionScope,
        institution: Uuid,
        student_id: Option<Uuid>,
        admission_id: Option<Uuid>,
    ) -> Result<Dues, KernelError> {
        let r = sqlx::query(
            "SELECT \
               count(*) FILTER (WHERE status NOT IN ('draft', 'pending_validation', 'validated', 'cancelled')) AS raised, \
               COALESCE(sum(outstanding_amount) FILTER (WHERE status IN ('published', 'active')), 0) AS outstanding, \
               count(*) FILTER (WHERE status IN ('published', 'active') AND outstanding_amount > 0) AS open, \
               COALESCE(sum(outstanding_amount) FILTER (WHERE status = 'disputed'), 0) AS disputed, \
               (array_agg(student_account_id ORDER BY created_at))[1] AS account \
             FROM finance.charge WHERE tenant_id = $1 AND institution_id = $2 \
               AND ((($3::uuid IS NOT NULL) AND student_id = $3) OR (($4::uuid IS NOT NULL) AND admission_id = $4))",
        )
        .bind(scope.tenant_id.as_uuid())
        .bind(institution)
        .bind(student_id)
        .bind(admission_id)
        .fetch_one(&self.pool)
        .await
        .map_err(map_db_error)?;
        Ok(Dues {
            raised: r.try_get("raised").map_err(map_db_error)?,
            outstanding: r.try_get("outstanding").map_err(map_db_error)?,
            open: r.try_get("open").map_err(map_db_error)?,
            disputed: r.try_get("disputed").map_err(map_db_error)?,
            account: r.try_get("account").map_err(map_db_error)?,
        })
    }
}

#[async_trait::async_trait]
impl FinanceClearancePort for PgFinanceClearance {
    async fn admission_clearance(
        &self,
        scope: &ExecutionScope,
        query: AdmissionFinanceQuery,
    ) -> Result<FinanceEvidence, KernelError> {
        let d = self
            .dues(
                scope,
                query.institution_id.as_uuid(),
                query.student_id,
                Some(query.admission_id),
            )
            .await?;
        Ok(evidence(&d))
    }

    async fn student_dues(
        &self,
        scope: &ExecutionScope,
        query: StudentFinanceQuery,
    ) -> Result<FinanceEvidence, KernelError> {
        let d = self
            .dues(
                scope,
                query.institution_id.as_uuid(),
                Some(query.student_id),
                None,
            )
            .await?;
        Ok(evidence(&d))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn d(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap_or_default()
    }

    #[test]
    fn evidence_rule() {
        assert_eq!(evidence(&Dues::default()), FinanceEvidence::NotApplicable);
        let owing = Dues {
            raised: 2,
            outstanding: d("125000"),
            open: 2,
            ..Dues::default()
        };
        assert_eq!(
            evidence(&owing),
            FinanceEvidence::Outstanding {
                detail: "₹1,25,000.00 outstanding on 2 charges".into()
            }
        );
        let disputed = Dues {
            raised: 1,
            disputed: d("500"),
            ..Dues::default()
        };
        assert!(matches!(
            evidence(&disputed),
            FinanceEvidence::Pending { .. }
        ));
        let paid = Dues {
            raised: 1,
            account: Some(Uuid::nil()),
            ..Dues::default()
        };
        assert!(evidence(&paid).permits_clearance());
        assert!(!evidence(&owing).permits_clearance());
    }
}
