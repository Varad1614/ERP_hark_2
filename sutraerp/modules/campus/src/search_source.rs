//! `library-catalogue` OpenSearch projection source (Batch 9, slice 9C —
//! `docs/batch9-contracts.md` §8).
//!
//! Read-only [`SearchSource`] over Domain G's own schema: one document per catalogue record
//! (`id, tenantId, institutionId, title, authors, isbn, subjects, availableCopies`). Loans,
//! returns and reservations change `availableCopies` without touching the record, so the document
//! version is the record version plus the versions of all its copies (each copy status change
//! bumps the copy version), which keeps external versions strictly increasing.

use sqlx::{PgPool, Row};
use sutra_contracts::platform::search::{
    SearchDocument, SearchSource, PROJECTION_LIBRARY_CATALOGUE,
};
use sutra_kernel::error::KernelError;
use sutra_persistence::map_db_error;
use uuid::Uuid;

/// Domain G read port for the `library-catalogue` projection.
#[derive(Clone)]
pub struct PgCatalogueSearchSource {
    pool: PgPool,
}

impl PgCatalogueSearchSource {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

/// Event types that can change a catalogue document.
pub const EVENT_PREFIXES: &[&str] = &[
    "sutraerp.campus.catalogue_record.",
    "sutraerp.campus.library_copy.",
    "sutraerp.campus.library_loan.",
    "sutraerp.campus.library_reservation.",
];

const SELECT: &str = "SELECT r.id, r.tenant_id, r.institution_id, r.title, r.authors, r.isbn, \
       r.subjects, \
       (SELECT count(*) FROM campus.library_copy c \
         WHERE c.record_id = r.id AND c.status = 'available')::bigint AS available, \
       r.version + COALESCE((SELECT sum(c.version) FROM campus.library_copy c \
                             WHERE c.record_id = r.id), 0)::bigint AS doc_version \
     FROM campus.catalogue_record r";

fn doc(r: &sqlx::postgres::PgRow) -> Result<SearchDocument, sqlx::Error> {
    let id: Uuid = r.try_get("id")?;
    let tenant: Uuid = r.try_get("tenant_id")?;
    let institution: Uuid = r.try_get("institution_id")?;
    let title: String = r.try_get("title")?;
    let authors: Vec<String> = r.try_get("authors")?;
    let isbn: Option<String> = r.try_get("isbn")?;
    let subjects: Vec<String> = r.try_get("subjects")?;
    let available: i64 = r.try_get("available")?;
    Ok(SearchDocument {
        id,
        tenant_id: tenant,
        version: r.try_get("doc_version")?,
        body: serde_json::json!({
            "id": id,
            "tenantId": tenant,
            "institutionId": institution,
            "title": title,
            "authors": authors,
            "isbn": isbn,
            "subjects": subjects,
            "availableCopies": available,
        }),
    })
}

/// Candidate ids an event may name (pure).
pub fn candidate_ids(subject: Option<&str>, data: &serde_json::Value) -> Vec<Uuid> {
    let mut v: Vec<Uuid> = [
        "recordId",
        "copyId",
        "loanId",
        "reservationId",
        "aggregateId",
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
impl SearchSource for PgCatalogueSearchSource {
    fn projection(&self) -> &'static str {
        PROJECTION_LIBRARY_CATALOGUE
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
            "SELECT id FROM campus.catalogue_record WHERE tenant_id = $1 AND id = ANY($2) \
             UNION SELECT record_id FROM campus.library_copy WHERE tenant_id = $1 AND id = ANY($2) \
             UNION SELECT record_id FROM campus.library_loan WHERE tenant_id = $1 AND id = ANY($2) \
             UNION SELECT record_id FROM campus.library_reservation \
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
            "{SELECT} WHERE r.tenant_id = $1 AND r.id = ANY($2) ORDER BY r.id"
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
            "{SELECT} WHERE ($1::uuid IS NULL OR r.id > $1) ORDER BY r.id LIMIT $2"
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
            "SELECT count(*) FROM campus.catalogue_record \
             WHERE ($1::uuid IS NULL OR tenant_id = $1)",
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
    fn loans_copies_and_records_feed_the_catalogue_projection() {
        use sutra_contracts::campus::events::library as l;
        let handles = |t: &str| EVENT_PREFIXES.iter().any(|p| t.starts_with(p));
        assert!(handles(l::CATALOGUE_RECORD_PUBLISHED));
        assert!(handles(l::LIBRARY_LOAN_ISSUED));
        assert!(handles(l::LIBRARY_LOAN_RETURNED));
        assert!(!handles(l::LIBRARY_FINE_RAISED));
        let a = Uuid::now_v7();
        assert_eq!(
            candidate_ids(None, &serde_json::json!({"aggregateId": a.to_string()})),
            vec![a]
        );
    }
}
