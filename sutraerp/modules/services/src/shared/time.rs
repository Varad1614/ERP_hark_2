//! Business-local dates (ADR-L5-0036 §3.4): every Domain H business date is a `date` in the institution's IANA
//! timezone, obtained from Domain B through the reference port.

use crate::shared::ServicesState;
use chrono::NaiveDate;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId};
use sutra_persistence::map_db_error;

/// Today's date in the institution's business timezone (404 when the institution is missing).
pub async fn business_today(
    state: &ServicesState,
    scope: &ExecutionScope,
    institution_id: InstitutionId,
) -> Result<NaiveDate, KernelError> {
    let inst = state
        .references
        .institution(scope, institution_id)
        .await?
        .ok_or(KernelError::NotFound {
            entity: "institution",
        })?;
    sqlx::query_scalar("SELECT (now() AT TIME ZONE $1)::date")
        .bind(&inst.timezone)
        .fetch_one(&state.pool)
        .await
        .map_err(map_db_error)
}
