//! `staff` OpenSearch projection source (Batch 9, slice 9C — `docs/batch9-contracts.md` §8).
//!
//! Read-only [`SearchSource`] over Domain E's own schema: one document per staff record
//! (`id, tenantId, institutionId, number, displayName, department, status`), where `department`
//! is the org unit of the staff member's current engagement (active first, else the latest).
//! The document version is the staff record version plus the versions of all its engagements,
//! so every record or engagement change produces a strictly higher external version.

use sqlx::{PgPool, Row};
use sutra_contracts::platform::search::{SearchDocument, SearchSource, PROJECTION_STAFF};
use sutra_kernel::error::KernelError;
use sutra_persistence::map_db_error;
use uuid::Uuid;

/// Domain E read port for the `staff` projection.
#[derive(Clone)]
pub struct PgStaffSearchSource {
    pool: PgPool,
}

impl PgStaffSearchSource {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

/// Event types that can change a staff document.
pub const EVENT_PREFIXES: &[&str] = &[
    "sutraerp.people.staff_member.",
    "sutraerp.people.staff_engagement.",
];

const SELECT: &str = "SELECT s.id, s.tenant_id, s.institution_id, s.staff_number, s.status, \
       s.display_name, e.org_unit_id, \
       s.version + COALESCE((SELECT sum(x.version) FROM people.staff_engagement x \
                             WHERE x.staff_id = s.id), 0)::bigint AS doc_version \
     FROM people.staff_member s \
     LEFT JOIN LATERAL (SELECT se.org_unit_id FROM people.staff_engagement se \
                        WHERE se.staff_id = s.id \
                        ORDER BY (se.status = 'active') DESC, se.created_at DESC, se.id DESC \
                        LIMIT 1) e \
       ON true";

fn doc(r: &sqlx::postgres::PgRow) -> Result<SearchDocument, sqlx::Error> {
    let id: Uuid = r.try_get("id")?;
    let tenant: Uuid = r.try_get("tenant_id")?;
    let institution: Uuid = r.try_get("institution_id")?;
    let department: Option<Uuid> = r.try_get("org_unit_id")?;
    let number: String = r.try_get("staff_number")?;
    let name: String = r.try_get("display_name")?;
    let status: String = r.try_get("status")?;
    Ok(SearchDocument {
        id,
        tenant_id: tenant,
        version: r.try_get("doc_version")?,
        body: serde_json::json!({
            "id": id,
            "tenantId": tenant,
            "institutionId": institution,
            "number": number,
            "displayName": name,
            "department": department.map(|d| d.to_string()),
            "status": status,
        }),
    })
}

/// Candidate ids an event may name (pure).
pub fn candidate_ids(subject: Option<&str>, data: &serde_json::Value) -> Vec<Uuid> {
    let mut v: Vec<Uuid> = [
        "staffId",
        "staffMemberId",
        "engagementId",
        "aggregateId",
        "canonicalStaffId",
    ]
    .iter()
    .filter_map(|k| data.get(*k).and_then(|x| x.as_str()))
    .chain(subject)
    .filter_map(|s| Uuid::parse_str(s.trim()).ok())
    .collect();
    v.sort();
    v.dedup();
    v
}

#[async_trait::async_trait]
impl SearchSource for PgStaffSearchSource {
    fn projection(&self) -> &'static str {
        PROJECTION_STAFF
    }

    fn event_prefixes(&self) -> &'static [&'static str] {
        EVENT_PREFIXES
    }

    async fn affected(
        &self,
        tenant: Uuid,
        _event_type: &str,
        subject: Option<&str>,
        data: &serde_json::Value,
    ) -> Result<Vec<Uuid>, KernelError> {
        let ids = candidate_ids(subject, data);
        if ids.is_empty() {
            return Ok(vec![]);
        }
        sqlx::query_scalar::<_, Uuid>(
            "SELECT id FROM people.staff_member WHERE tenant_id = $1 AND id = ANY($2) \
             UNION SELECT staff_id FROM people.staff_engagement \
             WHERE tenant_id = $1 AND id = ANY($2)",
        )
        .bind(tenant)
        .bind(&ids)
        .fetch_all(&self.pool)
        .await
        .map_err(map_db_error)
    }

    async fn fetch(&self, tenant: Uuid, ids: &[Uuid]) -> Result<Vec<SearchDocument>, KernelError> {
        let rows = sqlx::query(&format!(
            "{SELECT} WHERE s.tenant_id = $1 AND s.id = ANY($2) ORDER BY s.id"
        ))
        .bind(tenant)
        .bind(ids)
        .fetch_all(&self.pool)
        .await
        .map_err(map_db_error)?;
        rows.iter()
            .map(doc)
            .collect::<Result<_, _>>()
            .map_err(map_db_error)
    }

    async fn scan(
        &self,
        after: Option<Uuid>,
        limit: u32,
    ) -> Result<Vec<SearchDocument>, KernelError> {
        let rows = sqlx::query(&format!(
            "{SELECT} WHERE ($1::uuid IS NULL OR s.id > $1) ORDER BY s.id LIMIT $2"
        ))
        .bind(after)
        .bind(i64::from(limit.max(1)))
        .fetch_all(&self.pool)
        .await
        .map_err(map_db_error)?;
        rows.iter()
            .map(doc)
            .collect::<Result<_, _>>()
            .map_err(map_db_error)
    }

    async fn count(&self, tenant: Option<Uuid>) -> Result<u64, KernelError> {
        let n: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM people.staff_member WHERE ($1::uuid IS NULL OR tenant_id = $1)",
        )
        .bind(tenant)
        .fetch_one(&self.pool)
        .await
        .map_err(map_db_error)?;
        Ok(u64::try_from(n).unwrap_or(0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn staff_and_engagement_events_feed_the_staff_projection() {
        use sutra_contracts::people::events::workforce as w;
        let handles = |t: &str| EVENT_PREFIXES.iter().any(|p| t.starts_with(p));
        assert!(handles(w::STAFF_CREATED));
        assert!(handles(w::ENGAGEMENT_ACTIVATED));
        assert!(!handles(w::REQUISITION_DECIDED));
        let a = Uuid::now_v7();
        assert_eq!(
            candidate_ids(
                Some(&a.to_string()),
                &serde_json::json!({"staffId": a.to_string()})
            ),
            vec![a]
        );
    }
}
