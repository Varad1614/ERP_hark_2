//! Institutional business policies with versioned revisions (L3-B-005, L2-B01 §2.1).
//!
//! A policy (stable `key`) owns numbered revisions. At most one revision is a `draft`; publishing
//! it makes it `effective` from its business-local `effectiveFrom` (institution timezone) and
//! marks the previous effective revision `superseded` — never edited in place. The predecessor
//! still governs until the successor's date: the revision **in force** on a date is the
//! non-draft revision with the latest `effectiveFrom` on or before it. Effective dates are
//! strictly increasing per policy, so two revisions never govern the same day
//! (L4-B-005-BH-002..006). This is institutional business policy, not authorization policy.

use super::common::{business_today, check_version, finish, invalid, validate_text};
use crate::shared::guard::lock_institution_for_change;
use crate::shared::publish::{audit, event};
use crate::shared::Actor;
use chrono::{DateTime, NaiveDate, Utc};
use sqlx::{PgConnection, PgPool, Row};
use sutra_contracts::institution::events::lifecycle::{
    InstitutionPolicyRevisedV1, INSTITUTION_POLICY_REVISED, INSTITUTION_POLICY_REVISED_SCHEMA,
};
use sutra_contracts::institution::ids::InstitutionPolicyId;
use sutra_contracts::institution::status::PolicyStatus;
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::{FieldViolation, KernelError};
use sutra_kernel::scope::{ExecutionScope, InstitutionId};
use sutra_persistence::map_db_error;
use uuid::Uuid;

/// One revision of a policy.
#[derive(Debug, Clone, PartialEq, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PolicyRevision {
    pub id: Uuid,
    pub policy_id: InstitutionPolicyId,
    pub revision: i32,
    pub status: PolicyStatus,
    pub title: String,
    pub body: String,
    /// Business-local date (institution timezone) from which the revision governs.
    pub effective_from: NaiveDate,
    pub published_by: Option<String>,
    pub published_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

/// An institutional policy.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Policy {
    pub id: InstitutionPolicyId,
    pub institution_id: InstitutionId,
    /// Stable key, e.g. `attendance.minimum_percentage`.
    pub key: String,
    pub title: String,
    /// `draft` until the first revision is published, then `effective`.
    pub status: PolicyStatus,
    /// Latest published revision number (0 = none yet).
    pub revision: i32,
    /// Effective date of the latest published revision.
    pub effective_from: Option<NaiveDate>,
    /// Revision in force on the requested date (default: today in the institution timezone).
    pub current_revision: Option<PolicyRevision>,
    /// The open draft revision, if any.
    pub draft_revision: Option<PolicyRevision>,
    pub version: Version,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// `POST …/institutions/{id}/policies`: the policy and its first draft revision.
#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreatePolicy {
    /// Lowercase key: `a-z0-9`, `.`, `_` (3–100 characters).
    #[schema(example = "attendance.minimum_percentage")]
    pub key: String,
    pub title: String,
    pub body: String,
    pub effective_from: NaiveDate,
}

/// `POST …/policies/{id}/revisions`: creates or replaces the open draft revision.
#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DraftPolicyRevision {
    pub title: String,
    pub body: String,
    pub effective_from: NaiveDate,
}

fn validate_key(key: &str, v: &mut Vec<FieldViolation>) {
    let k = key.trim();
    if !(3..=100).contains(&k.len())
        || !k
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'_')
        || k.starts_with('.')
        || k.ends_with('.')
    {
        v.push(FieldViolation::new(
            "key",
            "format",
            "key must be 3-100 characters of a-z, 0-9, '.' or '_'",
        ));
    }
}

fn validate_content(title: &str, body: &str, v: &mut Vec<FieldViolation>) {
    validate_text("title", title, 200, v);
    validate_text("body", body, 20_000, v);
}

impl CreatePolicy {
    pub fn validate(&self) -> Result<(), KernelError> {
        let mut v = vec![];
        validate_key(&self.key, &mut v);
        validate_content(&self.title, &self.body, &mut v);
        finish(v)
    }
}

impl DraftPolicyRevision {
    pub fn validate(&self) -> Result<(), KernelError> {
        let mut v = vec![];
        validate_content(&self.title, &self.body, &mut v);
        finish(v)
    }
}

/// Publication check (pure): not retroactive, strictly after the latest published revision.
pub fn check_publishable(
    effective_from: NaiveDate,
    today: NaiveDate,
    latest_published: Option<NaiveDate>,
) -> Result<(), KernelError> {
    if effective_from < today {
        return Err(invalid(
            "effectiveFrom",
            "retroactive",
            format!("effectiveFrom must not be before today ({today}) in the institution timezone"),
        ));
    }
    if let Some(prev) = latest_published {
        if effective_from <= prev {
            return Err(invalid(
                "effectiveFrom",
                "overlap",
                format!(
                    "effectiveFrom must be after the current revision's effective date ({prev})"
                ),
            ));
        }
    }
    Ok(())
}

const REV_COLUMNS: &str =
    "id, policy_id, revision, status, title, body, effective_from, published_by, published_at, created_at";

fn rev_from_row(r: &sqlx::postgres::PgRow) -> Result<PolicyRevision, KernelError> {
    let status: String = r.try_get("status").map_err(map_db_error)?;
    Ok(PolicyRevision {
        id: r.try_get("id").map_err(map_db_error)?,
        policy_id: InstitutionPolicyId(r.try_get("policy_id").map_err(map_db_error)?),
        revision: r.try_get("revision").map_err(map_db_error)?,
        status: PolicyStatus::parse(&status).ok_or_else(|| {
            KernelError::internal_msg(format!("unknown policy status `{status}`"))
        })?,
        title: r.try_get("title").map_err(map_db_error)?,
        body: r.try_get("body").map_err(map_db_error)?,
        effective_from: r.try_get("effective_from").map_err(map_db_error)?,
        published_by: r.try_get("published_by").map_err(map_db_error)?,
        published_at: r.try_get("published_at").map_err(map_db_error)?,
        created_at: r.try_get("created_at").map_err(map_db_error)?,
    })
}

struct PolicyRow {
    id: InstitutionPolicyId,
    institution_id: InstitutionId,
    key: String,
    title: String,
    status: PolicyStatus,
    revision: i32,
    effective_from: Option<NaiveDate>,
    version: i64,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

const POLICY_COLUMNS: &str =
    "id, institution_id, key, title, status, revision, effective_from, version, created_at, updated_at";

fn policy_from_row(r: &sqlx::postgres::PgRow) -> Result<PolicyRow, KernelError> {
    let status: String = r.try_get("status").map_err(map_db_error)?;
    Ok(PolicyRow {
        id: InstitutionPolicyId(r.try_get("id").map_err(map_db_error)?),
        institution_id: InstitutionId(r.try_get("institution_id").map_err(map_db_error)?),
        key: r.try_get("key").map_err(map_db_error)?,
        title: r.try_get("title").map_err(map_db_error)?,
        status: PolicyStatus::parse(&status).ok_or_else(|| {
            KernelError::internal_msg(format!("unknown policy status `{status}`"))
        })?,
        revision: r.try_get("revision").map_err(map_db_error)?,
        effective_from: r.try_get("effective_from").map_err(map_db_error)?,
        version: r.try_get("version").map_err(map_db_error)?,
        created_at: r.try_get("created_at").map_err(map_db_error)?,
        updated_at: r.try_get("updated_at").map_err(map_db_error)?,
    })
}

async fn load_row(
    conn: &mut PgConnection,
    tenant: &ExecutionScope,
    id: InstitutionPolicyId,
    for_update: bool,
) -> Result<PolicyRow, KernelError> {
    let lock = if for_update { " FOR UPDATE" } else { "" };
    let row = sqlx::query(&format!(
        "SELECT {POLICY_COLUMNS} FROM institution.institution_policy WHERE id = $1 AND tenant_id = $2{lock}"
    ))
    .bind(id.as_uuid())
    .bind(tenant.tenant_id.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "institution_policy",
    })?;
    policy_from_row(&row)
}

async fn assemble(
    conn: &mut PgConnection,
    tenant: &ExecutionScope,
    p: PolicyRow,
    as_of: Option<NaiveDate>,
) -> Result<Policy, KernelError> {
    let as_of = match as_of {
        Some(d) => d,
        None => business_today(&mut *conn, tenant, p.institution_id).await?,
    };
    let current = sqlx::query(&format!(
        "SELECT {REV_COLUMNS} FROM institution.institution_policy_revision \
         WHERE policy_id = $1 AND status <> 'draft' AND effective_from <= $2 \
         ORDER BY effective_from DESC LIMIT 1"
    ))
    .bind(p.id.as_uuid())
    .bind(as_of)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .map(|r| rev_from_row(&r))
    .transpose()?;
    let draft = sqlx::query(&format!(
        "SELECT {REV_COLUMNS} FROM institution.institution_policy_revision \
         WHERE policy_id = $1 AND status = 'draft'"
    ))
    .bind(p.id.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .map(|r| rev_from_row(&r))
    .transpose()?;
    Ok(Policy {
        id: p.id,
        institution_id: p.institution_id,
        key: p.key,
        title: p.title,
        status: p.status,
        revision: p.revision,
        effective_from: p.effective_from,
        current_revision: current,
        draft_revision: draft,
        version: Version(p.version),
        created_at: p.created_at,
        updated_at: p.updated_at,
    })
}

fn inst_scope(tenant: &ExecutionScope, id: InstitutionId) -> ExecutionScope {
    ExecutionScope::institution(tenant.tenant_id, id)
}

/// Creates a policy (status `draft`) with draft revision 1.
pub async fn create(
    pool: &PgPool,
    scope: &ExecutionScope,
    institution_id: InstitutionId,
    cmd: &CreatePolicy,
    actor: &Actor,
) -> Result<Policy, KernelError> {
    cmd.validate()?;
    let mut tx = pool.begin().await.map_err(map_db_error)?;
    lock_institution_for_change(&mut tx, scope, institution_id).await?;
    let id = InstitutionPolicyId::new();
    sqlx::query(
        "INSERT INTO institution.institution_policy (id, tenant_id, institution_id, key, title) \
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(institution_id.as_uuid())
    .bind(cmd.key.trim())
    .bind(cmd.title.trim())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    sqlx::query(
        "INSERT INTO institution.institution_policy_revision \
           (tenant_id, institution_id, policy_id, revision, status, title, body, effective_from, drafted_by) \
         VALUES ($1, $2, $3, 1, 'draft', $4, $5, $6, $7)",
    )
    .bind(scope.tenant_id.as_uuid())
    .bind(institution_id.as_uuid())
    .bind(id.as_uuid())
    .bind(cmd.title.trim())
    .bind(cmd.body.trim())
    .bind(cmd.effective_from)
    .bind(actor.id.as_deref())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    audit(
        &mut tx,
        &inst_scope(scope, institution_id),
        "institution.institution_policy.create",
        "institution_policy",
        id.to_string(),
        serde_json::json!({ "key": cmd.key.trim() }),
        actor,
    )
    .await?;
    let row = load_row(&mut tx, scope, id, false).await?;
    let p = assemble(&mut tx, scope, row, None).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(p)
}

/// Policies of an institution, by key.
pub async fn list(
    pool: &PgPool,
    scope: &ExecutionScope,
    institution_id: InstitutionId,
    status: Option<PolicyStatus>,
) -> Result<Vec<Policy>, KernelError> {
    let mut conn = pool.acquire().await.map_err(map_db_error)?;
    crate::shared::guard::institution_status(&mut *conn, scope, institution_id).await?;
    let rows = sqlx::query(&format!(
        "SELECT {POLICY_COLUMNS} FROM institution.institution_policy \
         WHERE institution_id = $1 AND tenant_id = $2 AND ($3::text IS NULL OR status = $3) ORDER BY key"
    ))
    .bind(institution_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(status.map(|s| s.as_str()))
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let mut out = Vec::with_capacity(rows.len());
    for r in &rows {
        let p = policy_from_row(r)?;
        out.push(assemble(&mut conn, scope, p, None).await?);
    }
    Ok(out)
}

/// One policy; `as_of` selects the revision in force (default today, institution timezone).
pub async fn get(
    pool: &PgPool,
    scope: &ExecutionScope,
    id: InstitutionPolicyId,
    as_of: Option<NaiveDate>,
) -> Result<Policy, KernelError> {
    let mut conn = pool.acquire().await.map_err(map_db_error)?;
    let row = load_row(&mut conn, scope, id, false).await?;
    assemble(&mut conn, scope, row, as_of).await
}

/// Every revision, newest first.
pub async fn revisions(
    pool: &PgPool,
    scope: &ExecutionScope,
    id: InstitutionPolicyId,
) -> Result<Vec<PolicyRevision>, KernelError> {
    let mut conn = pool.acquire().await.map_err(map_db_error)?;
    load_row(&mut conn, scope, id, false).await?;
    let rows = sqlx::query(&format!(
        "SELECT {REV_COLUMNS} FROM institution.institution_policy_revision WHERE policy_id = $1 ORDER BY revision DESC"
    ))
    .bind(id.as_uuid())
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    rows.iter().map(rev_from_row).collect()
}

/// Creates the successor draft revision, or replaces the open draft.
pub async fn draft_revision(
    pool: &PgPool,
    scope: &ExecutionScope,
    id: InstitutionPolicyId,
    expected: Version,
    cmd: &DraftPolicyRevision,
    actor: &Actor,
) -> Result<Policy, KernelError> {
    cmd.validate()?;
    let mut tx = pool.begin().await.map_err(map_db_error)?;
    let p = load_row(&mut tx, scope, id, true).await?;
    lock_institution_for_change(&mut tx, scope, p.institution_id).await?;
    check_version(expected, p.version)?;
    let updated = sqlx::query(
        "UPDATE institution.institution_policy_revision SET title = $1, body = $2, effective_from = $3, \
         drafted_by = $4, updated_at = now() WHERE policy_id = $5 AND status = 'draft'",
    )
    .bind(cmd.title.trim())
    .bind(cmd.body.trim())
    .bind(cmd.effective_from)
    .bind(actor.id.as_deref())
    .bind(id.as_uuid())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?
    .rows_affected();
    if updated == 0 {
        sqlx::query(
            "INSERT INTO institution.institution_policy_revision \
               (tenant_id, institution_id, policy_id, revision, status, title, body, effective_from, drafted_by) \
             SELECT $1, $2, $3, COALESCE(MAX(revision), 0) + 1, 'draft', $4, $5, $6, $7 \
             FROM institution.institution_policy_revision WHERE policy_id = $3",
        )
        .bind(scope.tenant_id.as_uuid())
        .bind(p.institution_id.as_uuid())
        .bind(id.as_uuid())
        .bind(cmd.title.trim())
        .bind(cmd.body.trim())
        .bind(cmd.effective_from)
        .bind(actor.id.as_deref())
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    }
    sqlx::query("UPDATE institution.institution_policy SET version = version + 1, updated_at = now() WHERE id = $1")
        .bind(id.as_uuid())
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    audit(
        &mut tx,
        &inst_scope(scope, p.institution_id),
        "institution.institution_policy.draft_revision",
        "institution_policy",
        id.to_string(),
        serde_json::json!({ "effectiveFrom": cmd.effective_from, "replacedDraft": updated > 0 }),
        actor,
    )
    .await?;
    let row = load_row(&mut tx, scope, id, false).await?;
    let out = assemble(&mut tx, scope, row, None).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(out)
}

/// Publishes the open draft: `draft → effective`, predecessor `effective → superseded`, event
/// `institution_policy.revised`.
pub async fn publish(
    pool: &PgPool,
    scope: &ExecutionScope,
    id: InstitutionPolicyId,
    expected: Version,
    actor: &Actor,
) -> Result<Policy, KernelError> {
    let mut tx = pool.begin().await.map_err(map_db_error)?;
    let p = load_row(&mut tx, scope, id, true).await?;
    lock_institution_for_change(&mut tx, scope, p.institution_id).await?;
    check_version(expected, p.version)?;
    let draft = sqlx::query(&format!(
        "SELECT {REV_COLUMNS} FROM institution.institution_policy_revision WHERE policy_id = $1 AND status = 'draft'"
    ))
    .bind(id.as_uuid())
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_db_error)?
    .map(|r| rev_from_row(&r))
    .transpose()?
    .ok_or_else(|| KernelError::Conflict("the policy has no draft revision to publish".into()))?;
    let today = business_today(&mut *tx, scope, p.institution_id).await?;
    check_publishable(draft.effective_from, today, p.effective_from)?;
    sqlx::query(
        "UPDATE institution.institution_policy_revision SET status = 'superseded', updated_at = now() \
         WHERE policy_id = $1 AND status = 'effective'",
    )
    .bind(id.as_uuid())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    sqlx::query(
        "UPDATE institution.institution_policy_revision SET status = 'effective', published_by = $1, \
         published_at = now(), updated_at = now() WHERE id = $2",
    )
    .bind(actor.id.as_deref())
    .bind(draft.id)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    sqlx::query(
        "UPDATE institution.institution_policy SET status = 'effective', revision = $1, title = $2, \
         effective_from = $3, version = version + 1, updated_at = now() WHERE id = $4",
    )
    .bind(draft.revision)
    .bind(&draft.title)
    .bind(draft.effective_from)
    .bind(id.as_uuid())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let es = inst_scope(scope, p.institution_id);
    event(
        &mut tx,
        &es,
        INSTITUTION_POLICY_REVISED,
        INSTITUTION_POLICY_REVISED_SCHEMA,
        id.to_string(),
        &InstitutionPolicyRevisedV1 {
            institution_id: p.institution_id.as_uuid(),
            policy_id: id.as_uuid(),
            policy_key: p.key.clone(),
            revision: draft.revision,
            effective_from: draft.effective_from,
        },
        actor,
    )
    .await?;
    audit(
        &mut tx,
        &es,
        "institution.institution_policy.publish",
        "institution_policy",
        id.to_string(),
        serde_json::json!({ "revision": draft.revision, "effectiveFrom": draft.effective_from, "supersedesRevision": (p.revision > 0).then_some(p.revision) }),
        actor,
    )
    .await?;
    let row = load_row(&mut tx, scope, id, false).await?;
    let out = assemble(&mut tx, scope, row, None).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        s.parse().unwrap_or_default()
    }

    #[test]
    fn publication_dates_are_forward_only_and_non_overlapping() {
        assert!(check_publishable(d("2026-11-01"), d("2026-10-12"), None).is_ok());
        assert!(check_publishable(d("2026-10-12"), d("2026-10-12"), Some(d("2026-09-01"))).is_ok());
        assert!(check_publishable(d("2026-10-11"), d("2026-10-12"), None).is_err());
        assert!(
            check_publishable(d("2026-11-01"), d("2026-10-12"), Some(d("2026-11-01"))).is_err()
        );
    }

    #[test]
    fn validates_keys_and_content() {
        let ok = CreatePolicy {
            key: "attendance.minimum_percentage".into(),
            title: "Attendance".into(),
            body: "75%".into(),
            effective_from: d("2026-11-01"),
        };
        assert!(ok.validate().is_ok());
        let bad = CreatePolicy {
            key: "Bad Key".into(),
            title: String::new(),
            ..ok
        };
        let Err(KernelError::Validation(v)) = bad.validate() else {
            panic!("violations expected")
        };
        assert_eq!(v.len(), 2);
    }
}
