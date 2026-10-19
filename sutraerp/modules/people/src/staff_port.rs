//! [`StaffDirectory`] for Domain D (Batch 7 foundation, contract doc §6.2): the real staff record
//! behind a login subject. `active` = the current engagement `is_working()` (an `active`
//! engagement; suspended / long leave / closed staff must not get new teaching assignments).
//!
//! **Fallback:** when no live staff record is linked to the subject, the Batch 4 subject-only
//! answer is returned (`staff_id = None`, `active = true`), exactly like [`crate::SubjectOnlyStaff`],
//! so Batch 4 / 6 data (faculty known only by login) keep working until HR records them.

use crate::shared::rows::{current_engagement, staff_from_row, STAFF_COLUMNS};
use sqlx::PgPool;
use sutra_contracts::people::{StaffDirectory, StaffRef};
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::ExecutionScope;
use sutra_persistence::map_db_error;

#[derive(Clone)]
pub struct PgStaffDirectory {
    pool: PgPool,
}

impl PgStaffDirectory {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait::async_trait]
impl StaffDirectory for PgStaffDirectory {
    async fn staff_by_subject(
        &self,
        scope: &ExecutionScope,
        subject_ref: &str,
    ) -> Result<Option<StaffRef>, KernelError> {
        let s = subject_ref.trim();
        if s.is_empty() {
            return Ok(None);
        }
        let mut conn = self.pool.acquire().await.map_err(map_db_error)?;
        let sql = format!(
            "SELECT {STAFF_COLUMNS} FROM people.staff_member WHERE tenant_id = $1 \
             AND subject_ref = $2 AND status NOT IN ('merged', 'archived')"
        );
        let staff = sqlx::query(&sql)
            .bind(scope.tenant_id.as_uuid())
            .bind(s)
            .fetch_optional(&mut *conn)
            .await
            .map_err(map_db_error)?
            .as_ref()
            .map(staff_from_row)
            .transpose()
            .map_err(map_db_error)?;
        let Some(staff) = staff else {
            return Ok(Some(StaffRef {
                subject_ref: s.to_owned(),
                staff_id: None,
                display_name: None,
                org_unit_id: None,
                active: true,
            }));
        };
        let engagement = current_engagement(&mut conn, scope.tenant_id, staff.id).await?;
        Ok(Some(StaffRef {
            subject_ref: s.to_owned(),
            staff_id: Some(staff.id.as_uuid()),
            display_name: Some(staff.display_name.clone()),
            org_unit_id: engagement.as_ref().and_then(|e| e.org_unit_id),
            active: staff.status == sutra_contracts::people::status::StaffStatus::Active
                && engagement.is_some_and(|e| e.status.is_working()),
        }))
    }
}
