//! [`TeachingSpacePort`] adapters for Domain D (Academic timetable, slice 4B).
//!
//! * [`PgTeachingSpaces`] — **real** since the Batch 8 foundation: `space` nodes of
//!   `campus.infrastructure_node` whose space type is a teaching type (classroom, lab, seminar
//!   room, auditorium). Unknown / foreign / non-teaching ids answer `None`, so 4B keeps its
//!   free-text room label fallback for slots without a space.
//! * [`NoTeachingSpaces`] — the Batch 4 stub (knows no space), kept for tests.

use crate::shared::rows::{node_from_row, InfrastructureNode, NODE_COLUMNS};
use sqlx::PgPool;
use sutra_contracts::campus::status::{InfrastructureNodeType, InfrastructureStatus};
use sutra_contracts::campus::{TeachingSpacePort, TeachingSpaceRef};
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::ExecutionScope;
use sutra_persistence::map_db_error;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Default)]
pub struct NoTeachingSpaces;

#[async_trait::async_trait]
impl TeachingSpacePort for NoTeachingSpaces {
    async fn space(
        &self,
        _scope: &ExecutionScope,
        _id: Uuid,
    ) -> Result<Option<TeachingSpaceRef>, KernelError> {
        Ok(None)
    }

    async fn spaces_of_campus(
        &self,
        _scope: &ExecutionScope,
        _campus_id: Uuid,
    ) -> Result<Vec<TeachingSpaceRef>, KernelError> {
        Ok(Vec::new())
    }
}

/// Teaching spaces over the Batch 8 infrastructure hierarchy.
#[derive(Clone)]
pub struct PgTeachingSpaces {
    pool: PgPool,
}

impl PgTeachingSpaces {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

/// The teaching view of a node, when it is a teaching space.
pub fn teaching_space(n: &InfrastructureNode) -> Option<TeachingSpaceRef> {
    let st = n.space_type?;
    if n.node_type != InfrastructureNodeType::Space || !st.is_teaching() {
        return None;
    }
    Some(TeachingSpaceRef {
        id: n.id.as_uuid(),
        campus_id: n.campus_id,
        code: n.code.clone(),
        name: n.name.clone(),
        kind: st.as_str().to_owned(),
        capacity: n.capacity,
        active: n.status == InfrastructureStatus::Active,
    })
}

#[async_trait::async_trait]
impl TeachingSpacePort for PgTeachingSpaces {
    async fn space(
        &self,
        scope: &ExecutionScope,
        id: Uuid,
    ) -> Result<Option<TeachingSpaceRef>, KernelError> {
        let sql = format!(
            "SELECT {NODE_COLUMNS} FROM campus.infrastructure_node WHERE id = $1 AND tenant_id = $2"
        );
        let row = sqlx::query(&sql)
            .bind(id)
            .bind(scope.tenant_id.as_uuid())
            .fetch_optional(&self.pool)
            .await
            .map_err(map_db_error)?;
        Ok(match row {
            Some(r) => teaching_space(&node_from_row(&r).map_err(map_db_error)?),
            None => None,
        })
    }

    async fn spaces_of_campus(
        &self,
        scope: &ExecutionScope,
        campus_id: Uuid,
    ) -> Result<Vec<TeachingSpaceRef>, KernelError> {
        let sql = format!(
            "SELECT {NODE_COLUMNS} FROM campus.infrastructure_node WHERE tenant_id = $1 \
             AND campus_id = $2 AND node_type = 'space' \
             AND status IN ('active', 'suspended') ORDER BY code"
        );
        let rows = sqlx::query(&sql)
            .bind(scope.tenant_id.as_uuid())
            .bind(campus_id)
            .fetch_all(&self.pool)
            .await
            .map_err(map_db_error)?;
        let mut out = Vec::new();
        for r in &rows {
            if let Some(s) = teaching_space(&node_from_row(r).map_err(map_db_error)?) {
                out.push(s);
            }
        }
        Ok(out)
    }
}
