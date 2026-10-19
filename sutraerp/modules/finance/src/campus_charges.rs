//! [`CampusChargePort`] — campus service charges raised by Domain G (Batch 8 foundation, contract
//! change to Domain F recorded in `docs/batch8-contracts.md` §6.2): library fines (8C), hostel
//! fees and damage charges, transport pass fees (8B).
//!
//! Same flow as [`crate::exam_fees`]: one owner-local transaction in Finance reusing 5A's charge
//! flow — the fee head (`fee_code`, the request's category, income `OtherIncome`) is created on
//! first use, the student's party account is opened / reused, the charge (`source_type =
//! campus_charge`, `source_id` = the Domain G aggregate) is validated and **published** (Dr
//! receivables / Cr other income, `charge.published`). Idempotent on `source_id`.

use crate::charges::repo::{create_in_tx, line, publish_locked, validate_locked, ChargeInput};
use crate::shared::coa::resolve;
use crate::shared::money::require_positive;
use crate::shared::time::business_today;
use crate::shared::{Actor, FinanceState};
use sqlx::Row;
use sutra_contracts::finance::ids::ChargeId;
use sutra_contracts::finance::ports::{
    AccountRef, CampusCharge, CampusChargePort, CampusChargeRequest, SystemAccount,
};
use sutra_contracts::finance::status::ChargeSourceType;
use sutra_contracts::student::ids::StudentId;
use sutra_kernel::error::{FieldViolation, KernelError};
use sutra_kernel::scope::ExecutionScope;
use sutra_persistence::map_db_error;
use sutra_web::request_id::current_correlation_id;
use uuid::Uuid;

#[derive(Clone)]
pub struct PgCampusCharges {
    state: FinanceState,
}

impl PgCampusCharges {
    pub fn new(state: FinanceState) -> Self {
        Self { state }
    }
}

fn valid_fee_code(code: &str) -> bool {
    let b = code.as_bytes();
    (2..=32).contains(&b.len())
        && (b[0].is_ascii_uppercase() || b[0].is_ascii_digit())
        && b.iter()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || *c == b'_' || *c == b'-')
}

#[async_trait::async_trait]
impl CampusChargePort for PgCampusCharges {
    async fn raise_campus_charge(
        &self,
        scope: &ExecutionScope,
        req: CampusChargeRequest,
    ) -> Result<CampusCharge, KernelError> {
        let st = &self.state;
        let tenant = scope.tenant_id;
        if let Some(r) = sqlx::query(
            "SELECT id, charge_number, total_amount FROM finance.charge \
             WHERE tenant_id = $1 AND source_type = 'campus_charge' AND source_id = $2",
        )
        .bind(tenant.as_uuid())
        .bind(req.source_id)
        .fetch_optional(&st.pool)
        .await
        .map_err(map_db_error)?
        {
            return Ok(CampusCharge {
                charge_id: ChargeId(r.try_get("id").map_err(map_db_error)?),
                charge_number: r.try_get("charge_number").map_err(map_db_error)?,
                amount: r.try_get("total_amount").map_err(map_db_error)?,
                already_raised: true,
            });
        }
        if !valid_fee_code(&req.fee_code) {
            return Err(KernelError::Validation(vec![FieldViolation::new(
                "feeCode",
                "invalid_code",
                "must match ^[A-Z0-9][A-Z0-9_-]{1,31}$",
            )]));
        }
        let amount = require_positive("amount", req.amount)?;
        let institution = req.institution_id;
        let scope = ExecutionScope::institution(tenant, institution);
        let posting_date = business_today(st, &scope, institution).await?;
        let actor = Actor {
            id: req.actor.clone(),
            correlation_id: current_correlation_id(),
        };
        let mut tx = st.pool.begin().await.map_err(map_db_error)?;
        let existing: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM finance.fee_head WHERE tenant_id = $1 AND institution_id = $2 AND code = $3",
        )
        .bind(tenant.as_uuid())
        .bind(institution.as_uuid())
        .bind(&req.fee_code)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_db_error)?;
        let fee_head = match existing {
            Some(id) => id,
            None => {
                let income = resolve(
                    &mut tx,
                    tenant,
                    institution,
                    AccountRef::System(SystemAccount::OtherIncome),
                )
                .await?;
                sqlx::query_scalar(
                    "INSERT INTO finance.fee_head (tenant_id, institution_id, code, name, category, \
                     income_account_id) VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
                )
                .bind(tenant.as_uuid())
                .bind(institution.as_uuid())
                .bind(&req.fee_code)
                .bind(format!("Campus charge ({})", req.fee_code))
                .bind(req.category.as_str())
                .bind(income.id.as_uuid())
                .fetch_one(&mut *tx)
                .await
                .map_err(map_db_error)?
            }
        };
        let draft = create_in_tx(
            st,
            &mut tx,
            &scope,
            institution,
            &ChargeInput {
                student_id: StudentId(req.student_id),
                description: req.description.clone(),
                due_date: req.due_date,
                academic_year_id: None,
                academic_term_id: None,
                source_type: ChargeSourceType::CampusCharge,
                source_id: Some(req.source_id),
                lines: vec![line(fee_head, req.description.clone(), amount)],
            },
            &actor,
        )
        .await?;
        let validated = validate_locked(&mut tx, &draft, &actor).await?;
        let (published, _) =
            publish_locked(&mut tx, &validated, posting_date, None, &actor).await?;
        tx.commit().await.map_err(map_db_error)?;
        Ok(CampusCharge {
            charge_id: published.id,
            charge_number: published.charge_number,
            amount: published.total_amount,
            already_raised: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fee_codes_follow_the_fee_head_pattern() {
        assert!(valid_fee_code("LIB-FINE") && valid_fee_code("HOSTEL-FEE"));
        assert!(!valid_fee_code("exam") && !valid_fee_code("E"));
    }
}
