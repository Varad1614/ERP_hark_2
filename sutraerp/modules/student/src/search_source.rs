//! `students` OpenSearch projection source (Batch 9, slice 9C — `docs/batch9-contracts.md` §8).
//!
//! Read-only [`SearchSource`] over Domain C's own schema: one document per persona
//! (`id, tenantId, institutionId, number, displayName, programme, status`), where
//! `institutionId` / `programme` come from the persona's latest StudentContext. The document
//! version is the persona version plus the versions of all its contexts, so every persona or
//! context change produces a strictly higher external version.

use sqlx::{PgPool, Row};
use sutra_contracts::platform::search::{SearchDocument, SearchSource, PROJECTION_STUDENTS};
use sutra_kernel::error::KernelError;
use sutra_persistence::map_db_error;
use uuid::Uuid;

/// Domain C read port for the `students` projection.
#[derive(Clone)]
pub struct PgStudentSearchSource {
    pool: PgPool,
}

impl PgStudentSearchSource {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

/// Event types that can change a student document.
pub const EVENT_PREFIXES: &[&str] = &["sutraerp.student.student."];

const SELECT: &str = "SELECT s.id, s.tenant_id, s.student_number, s.status, \
       concat_ws(' ', s.given_name, s.middle_name, s.family_name) AS display_name, \
       c.institution_id, c.programme_id, \
       s.version + COALESCE((SELECT sum(x.version) FROM student.student_context x \
                             WHERE x.student_id = s.id), 0)::bigint AS doc_version \
     FROM student.student s \
     LEFT JOIN LATERAL (SELECT sc.institution_id, sc.programme_id FROM student.student_context sc \
                        WHERE sc.student_id = s.id ORDER BY sc.created_at DESC, sc.id DESC LIMIT 1) c \
       ON true";

fn doc(r: &sqlx::postgres::PgRow) -> Result<SearchDocument, sqlx::Error> {
    let id: Uuid = r.try_get("id")?;
    let tenant: Uuid = r.try_get("tenant_id")?;
    let institution: Option<Uuid> = r.try_get("institution_id")?;
    let programme: Option<Uuid> = r.try_get("programme_id")?;
    let number: String = r.try_get("student_number")?;
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
            "programme": programme.map(|p| p.to_string()),
            "status": status,
        }),
    })
}

/// Candidate ids an event may name (pure).
pub fn candidate_ids(subject: Option<&str>, data: &serde_json::Value) -> Vec<Uuid> {
    let mut v: Vec<Uuid> = [
        "studentId",
        "studentContextId",
        "aggregateId",
        "survivorId",
        "duplicateId",
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
impl SearchSource for PgStudentSearchSource {
    fn projection(&self) -> &'static str {
        PROJECTION_STUDENTS
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
            "SELECT id FROM student.student WHERE tenant_id = $1 AND id = ANY($2) \
             UNION SELECT student_id FROM student.student_context \
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
            "SELECT count(*) FROM student.student WHERE ($1::uuid IS NULL OR tenant_id = $1)",
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
    fn candidates_come_from_ids_in_the_payload_and_the_subject() {
        let a = Uuid::now_v7();
        let b = Uuid::now_v7();
        let ids = candidate_ids(
            Some(&a.to_string()),
            &serde_json::json!({"studentId": b.to_string(), "aggregateId": a.to_string(), "x": "nope"}),
        );
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&a) && ids.contains(&b));
        assert!(candidate_ids(Some("not-a-uuid"), &serde_json::json!({})).is_empty());
        let handles = |t: &str| EVENT_PREFIXES.iter().any(|p| t.starts_with(p));
        assert!(handles(
            sutra_contracts::student::events::STUDENT_STATUS_CHANGED
        ));
        assert!(handles(
            sutra_contracts::student::events::record::STUDENT_CREATED
        ));
    }
}
