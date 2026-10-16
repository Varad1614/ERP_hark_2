//! Import job engine: persistence of jobs / rows / profiles, mapping, dry run, commit, approval,
//! reconciliation, error report, templates and exports (see the module docs of `dataimport`).

use super::files::{self, FileFormat};
use super::{ImportState, COMMIT_APPROVAL};
use base64::Engine as _;
use rust_decimal::Decimal;
use serde_json::{json, Value};
use sqlx::postgres::PgRow;
use sqlx::{PgPool, Row};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Instant;
use sutra_contracts::platform::dataimport::{
    parse_amount, parse_bool, parse_date, parse_int, ColumnKind, ColumnSpec, EntitySpec,
    ImportBatch, ImportRow, ImportTargetPort, OnExisting, RowAction, RowOutcome,
};
use sutra_contracts::platform::workflow::StartProcess;
use sutra_kernel::audit::{ActorKind, AuditEntry, AuditOutcome};
use sutra_kernel::error::{FieldViolation, KernelError};
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use uuid::Uuid;

/// Upload size limit of one file (after base64 decoding).
pub const MAX_FILE_BYTES: usize = 8 * 1024 * 1024;
/// Default rows per commit transaction.
pub const DEFAULT_BATCH_SIZE: i32 = 500;

// ------------------------------------------------------------------ job model

/// Lifecycle of an import job.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    /// Parsed; mapping can be edited; no dry run yet (or the mapping changed since).
    Uploaded,
    /// Last dry run found no error: ready to commit.
    Validated,
    /// Last dry run found errors (see the error report).
    Invalid,
    /// Commit requested; waiting for a second person's decision (opening balances).
    PendingApproval,
    /// A commit is running.
    Committing,
    Committed,
    /// A commit batch failed and was rolled back (earlier batches stay committed; dry-run again
    /// and commit to resume — committed rows are skipped by natural key).
    Failed,
    /// The commit was rejected by the approver.
    Rejected,
}

impl JobStatus {
    pub const ALL: [JobStatus; 8] = [
        Self::Uploaded,
        Self::Validated,
        Self::Invalid,
        Self::PendingApproval,
        Self::Committing,
        Self::Committed,
        Self::Failed,
        Self::Rejected,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Uploaded => "uploaded",
            Self::Validated => "validated",
            Self::Invalid => "invalid",
            Self::PendingApproval => "pending_approval",
            Self::Committing => "committing",
            Self::Committed => "committed",
            Self::Failed => "failed",
            Self::Rejected => "rejected",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.as_str() == s)
    }
}

/// Predicted (after a dry run) or actual (after a commit) row actions.
#[derive(Clone, Debug, Default, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ActionCounts {
    pub create: i64,
    pub update: i64,
    pub skip: i64,
    pub error: i64,
    /// Rows not checked yet.
    pub pending: i64,
}

/// Reconciliation summary.
#[derive(Clone, Debug, Default, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Reconciliation {
    pub rows_in_file: i32,
    pub created: i32,
    pub updated: i32,
    pub skipped: i32,
    pub errors: i32,
    pub duplicates_in_file: i32,
    /// Rows whose natural key already existed (skipped or updated).
    pub existing_in_database: i32,
    pub batches_committed: i32,
    /// Σ amount column of the file (entities with money).
    pub file_total: Option<Decimal>,
    /// Σ amounts this job posted.
    pub committed_total: Option<Decimal>,
    /// Σ the ledger holds for the job's records.
    pub ledger_total: Option<Decimal>,
    /// `ledgerTotal == fileTotal` and the job committed (entities with money).
    pub reconciled: Option<bool>,
}

/// An import job.
#[derive(Clone, Debug, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ImportJob {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub entity: String,
    pub job_key: String,
    pub file_name: String,
    pub file_format: FileFormat,
    pub file_sha256: String,
    pub file_bytes: i32,
    /// Header cells of the file, in order.
    pub headers: Vec<String>,
    /// Source header → canonical column.
    pub mapping: BTreeMap<String, String>,
    pub mapping_profile_id: Option<Uuid>,
    pub on_existing: OnExisting,
    pub batch_size: i32,
    pub status: JobStatus,
    pub requires_approval: bool,
    pub actions: ActionCounts,
    pub summary: Reconciliation,
    pub failure_reason: Option<String>,
    pub process_instance_id: Option<String>,
    pub created_by: Option<String>,
    pub validated_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Duration of the last dry run.
    pub validated_ms: Option<i32>,
    pub commit_requested_by: Option<String>,
    pub decided_by: Option<String>,
    pub decision_reason: Option<String>,
    pub committed_by: Option<String>,
    pub committed_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Duration of the commit.
    pub committed_ms: Option<i32>,
    pub version: i64,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

const JOB_COLUMNS: &str = "id, institution_id, entity, job_key, file_name, file_format, \
    file_sha256, file_bytes, headers, mapping, mapping_profile_id, on_existing, batch_size, status, \
    row_count, error_count, duplicate_in_file_count, existing_count, created_count, updated_count, \
    skipped_count, batches_committed, file_total, committed_total, ledger_total, reconciled, \
    failure_reason, process_instance_id, created_by, validated_at, validated_ms, \
    commit_requested_by, decided_by, decision_reason, committed_by, committed_at, committed_ms, \
    version, created_at, updated_at";

fn internal(msg: &str) -> KernelError {
    KernelError::internal_msg(msg)
}

fn invalid(field: &str, code: &str, message: impl Into<String>) -> KernelError {
    KernelError::Validation(vec![FieldViolation::new(field, code, message.into())])
}

fn job_from_row(r: &PgRow, requires_approval: bool) -> Result<ImportJob, KernelError> {
    let headers: Value = r.try_get("headers").map_err(map_db_error)?;
    let mapping: Value = r.try_get("mapping").map_err(map_db_error)?;
    let format: String = r.try_get("file_format").map_err(map_db_error)?;
    let on_existing: String = r.try_get("on_existing").map_err(map_db_error)?;
    let status: String = r.try_get("status").map_err(map_db_error)?;
    Ok(ImportJob {
        id: r.try_get("id").map_err(map_db_error)?,
        institution_id: r.try_get("institution_id").map_err(map_db_error)?,
        entity: r.try_get("entity").map_err(map_db_error)?,
        job_key: r.try_get("job_key").map_err(map_db_error)?,
        file_name: r.try_get("file_name").map_err(map_db_error)?,
        file_format: FileFormat::parse(&format).ok_or_else(|| internal("file format"))?,
        file_sha256: r.try_get("file_sha256").map_err(map_db_error)?,
        file_bytes: r.try_get("file_bytes").map_err(map_db_error)?,
        headers: serde_json::from_value(headers).map_err(KernelError::internal)?,
        mapping: serde_json::from_value(mapping).map_err(KernelError::internal)?,
        mapping_profile_id: r.try_get("mapping_profile_id").map_err(map_db_error)?,
        on_existing: OnExisting::parse(&on_existing).ok_or_else(|| internal("on_existing"))?,
        batch_size: r.try_get("batch_size").map_err(map_db_error)?,
        status: JobStatus::parse(&status).ok_or_else(|| internal("job status"))?,
        requires_approval,
        actions: ActionCounts::default(),
        summary: Reconciliation {
            rows_in_file: r.try_get("row_count").map_err(map_db_error)?,
            created: r.try_get("created_count").map_err(map_db_error)?,
            updated: r.try_get("updated_count").map_err(map_db_error)?,
            skipped: r.try_get("skipped_count").map_err(map_db_error)?,
            errors: r.try_get("error_count").map_err(map_db_error)?,
            duplicates_in_file: r.try_get("duplicate_in_file_count").map_err(map_db_error)?,
            existing_in_database: r.try_get("existing_count").map_err(map_db_error)?,
            batches_committed: r.try_get("batches_committed").map_err(map_db_error)?,
            file_total: r.try_get("file_total").map_err(map_db_error)?,
            committed_total: r.try_get("committed_total").map_err(map_db_error)?,
            ledger_total: r.try_get("ledger_total").map_err(map_db_error)?,
            reconciled: r.try_get("reconciled").map_err(map_db_error)?,
        },
        failure_reason: r.try_get("failure_reason").map_err(map_db_error)?,
        process_instance_id: r.try_get("process_instance_id").map_err(map_db_error)?,
        created_by: r.try_get("created_by").map_err(map_db_error)?,
        validated_at: r.try_get("validated_at").map_err(map_db_error)?,
        validated_ms: r.try_get("validated_ms").map_err(map_db_error)?,
        commit_requested_by: r.try_get("commit_requested_by").map_err(map_db_error)?,
        decided_by: r.try_get("decided_by").map_err(map_db_error)?,
        decision_reason: r.try_get("decision_reason").map_err(map_db_error)?,
        committed_by: r.try_get("committed_by").map_err(map_db_error)?,
        committed_at: r.try_get("committed_at").map_err(map_db_error)?,
        committed_ms: r.try_get("committed_ms").map_err(map_db_error)?,
        version: r.try_get("version").map_err(map_db_error)?,
        created_at: r.try_get("created_at").map_err(map_db_error)?,
        updated_at: r.try_get("updated_at").map_err(map_db_error)?,
    })
}

fn target<'a>(
    st: &'a ImportState,
    entity: &str,
) -> Result<&'a Arc<dyn ImportTargetPort>, KernelError> {
    st.target(entity).ok_or_else(|| {
        invalid(
            "entity",
            "unknown",
            format!("`{entity}` is not an importable entity"),
        )
    })
}

fn needs_approval(st: &ImportState, entity: &str) -> bool {
    st.target(entity)
        .map(|t| t.spec().requires_approval)
        .unwrap_or(false)
}

async fn action_counts(pool: &PgPool, job: Uuid) -> Result<ActionCounts, KernelError> {
    let rows: Vec<(Option<String>, i64)> = sqlx::query_as(
        "SELECT action, count(*) FROM platform.import_job_row WHERE job_id = $1 GROUP BY action",
    )
    .bind(job)
    .fetch_all(pool)
    .await
    .map_err(map_db_error)?;
    let mut c = ActionCounts::default();
    for (a, n) in rows {
        match a.as_deref().and_then(RowAction::parse) {
            Some(RowAction::Create) => c.create = n,
            Some(RowAction::Update) => c.update = n,
            Some(RowAction::Skip) => c.skip = n,
            Some(RowAction::Error) => c.error = n,
            None => c.pending += n,
        }
    }
    Ok(c)
}

/// One job of the tenant (404 when missing / foreign — RLS also hides other tenants' jobs).
pub async fn load_job(
    st: &ImportState,
    tenant: TenantId,
    id: Uuid,
) -> Result<ImportJob, KernelError> {
    let row = sqlx::query(&format!(
        "SELECT {JOB_COLUMNS} FROM platform.import_job WHERE id = $1 AND tenant_id = $2"
    ))
    .bind(id)
    .bind(tenant.as_uuid())
    .fetch_optional(&st.pool)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "import job",
    })?;
    let entity: String = row.try_get("entity").map_err(map_db_error)?;
    let mut job = job_from_row(&row, needs_approval(st, &entity))?;
    job.actions = action_counts(&st.pool, id).await?;
    Ok(job)
}

/// Jobs of an institution, newest first.
pub async fn list_jobs(
    st: &ImportState,
    tenant: TenantId,
    institution: Option<Uuid>,
    entity: Option<&str>,
) -> Result<Vec<ImportJob>, KernelError> {
    let rows = sqlx::query(&format!(
        "SELECT {JOB_COLUMNS} FROM platform.import_job WHERE tenant_id = $1 \
         AND ($2::uuid IS NULL OR institution_id = $2) AND ($3::text IS NULL OR entity = $3) \
         ORDER BY created_at DESC, id DESC LIMIT 200"
    ))
    .bind(tenant.as_uuid())
    .bind(institution)
    .bind(entity)
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    rows.iter()
        .map(|r| {
            let entity: String = r.try_get("entity").map_err(map_db_error)?;
            job_from_row(r, needs_approval(st, &entity))
        })
        .collect()
}

async fn audit(
    pool: &PgPool,
    scope: &ExecutionScope,
    action: &str,
    job: Uuid,
    actor: Option<&str>,
    details: Value,
) -> Result<(), KernelError> {
    let kind = if actor.is_some() {
        ActorKind::User
    } else {
        ActorKind::System
    };
    sutra_persistence::audit::insert(
        pool,
        &AuditEntry::new(*scope, action, AuditOutcome::Succeeded)
            .actor(kind, actor.map(str::to_owned))
            .target("import_job", job.to_string())
            .correlation(sutra_web::request_id::current_correlation_id())
            .details(details),
    )
    .await
    .map_err(map_db_error)
}

fn scope_of(tenant: TenantId, institution: Uuid) -> ExecutionScope {
    ExecutionScope::institution(tenant, InstitutionId(institution))
}

// ------------------------------------------------------------------ mapping

/// Suggested mapping: a header matches a column whose name or label normalises to the same key.
pub fn suggest_mapping(spec: &EntitySpec, headers: &[String]) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let mut used = std::collections::HashSet::new();
    for h in headers {
        let k = files::header_key(h);
        if k.is_empty() {
            continue;
        }
        if let Some(c) = spec.columns.iter().find(|c| {
            !used.contains(c.name)
                && (files::header_key(c.name) == k || files::header_key(c.label) == k)
        }) {
            used.insert(c.name);
            out.insert(h.clone(), c.name.to_owned());
        }
    }
    out
}

/// Checks a mapping against the entity and the file's headers.
pub fn check_mapping(
    spec: &EntitySpec,
    headers: &[String],
    mapping: &BTreeMap<String, String>,
) -> Result<(), KernelError> {
    let mut v = vec![];
    let mut targets = std::collections::HashSet::new();
    for (h, c) in mapping {
        if !headers.iter().any(|x| x == h) {
            v.push(FieldViolation::new(
                format!("mapping.{h}"),
                "unknown_header",
                format!("`{h}` is not a column header of the file"),
            ));
        }
        if spec.column(c).is_none() {
            v.push(FieldViolation::new(
                format!("mapping.{h}"),
                "unknown_column",
                format!("`{c}` is not a column of {}", spec.entity),
            ));
        } else if !targets.insert(c.clone()) {
            v.push(FieldViolation::new(
                format!("mapping.{h}"),
                "duplicate_target",
                format!("two headers are mapped to `{c}`"),
            ));
        }
    }
    for c in spec.columns.iter().filter(|c| c.required) {
        if !targets.contains(c.name) {
            v.push(FieldViolation::new(
                format!("mapping.{}", c.name),
                "required_column_unmapped",
                format!("required column `{}` is not mapped", c.name),
            ));
        }
    }
    if v.is_empty() {
        Ok(())
    } else {
        Err(KernelError::Validation(v))
    }
}

// ------------------------------------------------------------------ profiles

/// A saved column mapping.
#[derive(Clone, Debug, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MappingProfile {
    pub id: Uuid,
    pub entity: String,
    pub name: String,
    pub mapping: BTreeMap<String, String>,
    pub created_by: Option<String>,
    pub version: i64,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

fn profile_from_row(r: &PgRow) -> Result<MappingProfile, KernelError> {
    let m: Value = r.try_get("mapping").map_err(map_db_error)?;
    Ok(MappingProfile {
        id: r.try_get("id").map_err(map_db_error)?,
        entity: r.try_get("entity").map_err(map_db_error)?,
        name: r.try_get("name").map_err(map_db_error)?,
        mapping: serde_json::from_value(m).map_err(KernelError::internal)?,
        created_by: r.try_get("created_by").map_err(map_db_error)?,
        version: r.try_get("version").map_err(map_db_error)?,
        updated_at: r.try_get("updated_at").map_err(map_db_error)?,
    })
}

pub async fn list_profiles(
    st: &ImportState,
    tenant: TenantId,
    entity: Option<&str>,
) -> Result<Vec<MappingProfile>, KernelError> {
    let rows = sqlx::query(
        "SELECT id, entity, name, mapping, created_by, version, updated_at \
         FROM platform.import_mapping_profile WHERE tenant_id = $1 \
         AND ($2::text IS NULL OR entity = $2) ORDER BY entity, name",
    )
    .bind(tenant.as_uuid())
    .bind(entity)
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    rows.iter().map(profile_from_row).collect()
}

/// Creates or replaces (by name) a mapping profile.
pub async fn save_profile(
    st: &ImportState,
    scope: &ExecutionScope,
    entity: &str,
    name: &str,
    mapping: &BTreeMap<String, String>,
    actor: Option<&str>,
) -> Result<MappingProfile, KernelError> {
    let spec = target(st, entity)?.spec();
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 120 {
        return Err(invalid("name", "required", "1 to 120 characters"));
    }
    let mut v = vec![];
    for (h, c) in mapping {
        if spec.column(c).is_none() {
            v.push(FieldViolation::new(
                format!("mapping.{h}"),
                "unknown_column",
                format!("`{c}` is not a column of {entity}"),
            ));
        }
    }
    if !v.is_empty() {
        return Err(KernelError::Validation(v));
    }
    let row = sqlx::query(
        "INSERT INTO platform.import_mapping_profile (tenant_id, entity, name, mapping, created_by) \
         VALUES ($1, $2, $3, $4, $5) ON CONFLICT (tenant_id, entity, name) DO UPDATE \
         SET mapping = EXCLUDED.mapping, version = platform.import_mapping_profile.version + 1, \
         updated_at = now() RETURNING id, entity, name, mapping, created_by, version, updated_at",
    )
    .bind(scope.tenant_id.as_uuid())
    .bind(entity)
    .bind(name)
    .bind(json!(mapping))
    .bind(actor)
    .fetch_one(&st.pool)
    .await
    .map_err(map_db_error)?;
    let p = profile_from_row(&row)?;
    sutra_persistence::audit::insert(
        &st.pool,
        &AuditEntry::new(
            *scope,
            "platform.import.mapping_profile.save",
            AuditOutcome::Succeeded,
        )
        .actor(ActorKind::User, actor.map(str::to_owned))
        .target("import_mapping_profile", p.id.to_string())
        .correlation(sutra_web::request_id::current_correlation_id())
        .details(json!({"entity": entity, "name": name, "columns": mapping.len()})),
    )
    .await
    .map_err(map_db_error)?;
    Ok(p)
}

// ------------------------------------------------------------------ upload

/// An upload.
#[derive(Clone, Debug)]
pub struct Upload {
    pub entity: String,
    pub institution_id: Uuid,
    pub file_name: String,
    pub content_base64: String,
    pub job_key: Option<String>,
    pub mapping_profile_id: Option<Uuid>,
    pub on_existing: OnExisting,
    pub batch_size: Option<i32>,
}

fn sha256_hex(bytes: &[u8]) -> String {
    sutra_kernel::secret::hex(ring::digest::digest(&ring::digest::SHA256, bytes).as_ref())
}

fn append_only(spec: &EntitySpec) -> KernelError {
    invalid(
        "onExisting",
        "unsupported",
        format!(
            "{} records are append-only; re-imports can only skip",
            spec.label
        ),
    )
}

/// Parses and stores an upload. Idempotent on the job key: the same key returns the existing
/// job (`false`); the same key for a different file is a 409.
pub async fn upload(
    st: &ImportState,
    tenant: TenantId,
    u: Upload,
    actor: Option<&str>,
) -> Result<(ImportJob, bool), KernelError> {
    let spec = target(st, &u.entity)?.spec();
    let file_name = u.file_name.trim().to_owned();
    let format = FileFormat::from_file_name(&file_name).ok_or_else(|| {
        invalid(
            "fileName",
            "format",
            "the file must be a .csv or .xlsx file",
        )
    })?;
    if file_name.chars().count() > 255 {
        return Err(invalid("fileName", "length", "at most 255 characters"));
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(u.content_base64.trim())
        .map_err(|_| {
            invalid(
                "contentBase64",
                "base64",
                "the file content must be base64 encoded",
            )
        })?;
    if bytes.is_empty() || bytes.len() > MAX_FILE_BYTES {
        return Err(invalid(
            "contentBase64",
            "size",
            format!("the file must hold 1 byte to {MAX_FILE_BYTES} bytes"),
        ));
    }
    let batch_size = u.batch_size.unwrap_or(DEFAULT_BATCH_SIZE);
    if !(1..=5000).contains(&batch_size) {
        return Err(invalid("batchSize", "range", "1 to 5000 rows per batch"));
    }
    if u.on_existing == OnExisting::Update && !spec.supports_update {
        return Err(append_only(&spec));
    }
    let sha = sha256_hex(&bytes);
    let job_key = match u
        .job_key
        .as_deref()
        .map(str::trim)
        .filter(|k| !k.is_empty())
    {
        Some(k) if k.chars().count() <= 200 => k.to_owned(),
        Some(_) => return Err(invalid("jobKey", "length", "at most 200 characters")),
        None => format!("{}:{}:{sha}", u.entity, u.institution_id),
    };
    // Idempotent upload: the same key answers with the job it created.
    let existing: Option<(Uuid, String)> = sqlx::query_as(
        "SELECT id, file_sha256 FROM platform.import_job WHERE tenant_id = $1 AND job_key = $2",
    )
    .bind(tenant.as_uuid())
    .bind(&job_key)
    .fetch_optional(&st.pool)
    .await
    .map_err(map_db_error)?;
    if let Some((id, existing_sha)) = existing {
        if existing_sha != sha {
            return Err(KernelError::Conflict(
                "the job key was already used for a different file".into(),
            ));
        }
        return Ok((load_job(st, tenant, id).await?, false));
    }
    let table = files::read(format, &bytes)?;
    let mapping: BTreeMap<String, String> = match u.mapping_profile_id {
        Some(pid) => {
            let p = list_profiles(st, tenant, Some(&u.entity))
                .await?
                .into_iter()
                .find(|p| p.id == pid)
                .ok_or_else(|| {
                    invalid(
                        "mappingProfileId",
                        "unknown",
                        "no mapping profile with this id for the entity",
                    )
                })?;
            p.mapping
                .into_iter()
                .filter(|(h, _)| table.headers.iter().any(|x| x == h))
                .collect()
        }
        None => suggest_mapping(&spec, &table.headers),
    };
    let scope = scope_of(tenant, u.institution_id);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO platform.import_job (tenant_id, institution_id, entity, job_key, file_name, \
         file_format, file_sha256, file_bytes, headers, mapping, mapping_profile_id, on_existing, \
         batch_size, row_count, created_by) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, \
         $12, $13, $14, $15) RETURNING id",
    )
    .bind(tenant.as_uuid())
    .bind(u.institution_id)
    .bind(&u.entity)
    .bind(&job_key)
    .bind(&file_name)
    .bind(format.as_str())
    .bind(&sha)
    .bind(i32::try_from(bytes.len()).unwrap_or(i32::MAX))
    .bind(json!(table.headers))
    .bind(json!(mapping))
    .bind(u.mapping_profile_id)
    .bind(u.on_existing.as_str())
    .bind(batch_size)
    .bind(i32::try_from(table.rows.len()).unwrap_or(i32::MAX))
    .bind(actor)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    for chunk in table.rows.chunks(1000) {
        let recs: Vec<Value> = chunk
            .iter()
            .map(|(line, cells)| {
                let mut raw = serde_json::Map::new();
                for (i, h) in table.headers.iter().enumerate() {
                    if h.is_empty() {
                        continue;
                    }
                    if let Some(v) = cells.get(i).filter(|v| !v.trim().is_empty()) {
                        raw.insert(h.clone(), Value::String(v.clone()));
                    }
                }
                json!({"line": line, "raw": raw})
            })
            .collect();
        sqlx::query(
            "INSERT INTO platform.import_job_row (job_id, tenant_id, institution_id, line, raw) \
             SELECT $1, $2, $3, x.line, x.raw \
             FROM jsonb_to_recordset($4) AS x(line integer, raw jsonb)",
        )
        .bind(id)
        .bind(tenant.as_uuid())
        .bind(u.institution_id)
        .bind(Value::Array(recs))
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    }
    sutra_persistence::audit::insert(
        &mut *tx,
        &AuditEntry::new(scope, "platform.import.upload", AuditOutcome::Succeeded)
            .actor(ActorKind::User, actor.map(str::to_owned))
            .target("import_job", id.to_string())
            .correlation(sutra_web::request_id::current_correlation_id())
            .details(json!({
                "entity": u.entity, "fileName": file_name, "format": format.as_str(),
                "sha256": sha, "rows": table.rows.len(), "jobKey": job_key,
            })),
    )
    .await
    .map_err(map_db_error)?;
    tx.commit().await.map_err(map_db_error)?;
    Ok((load_job(st, tenant, id).await?, true))
}

/// Replaces the mapping / on-existing policy (back to `uploaded`: dry-run again).
pub async fn set_mapping(
    st: &ImportState,
    tenant: TenantId,
    id: Uuid,
    mapping: BTreeMap<String, String>,
    on_existing: Option<OnExisting>,
    actor: Option<&str>,
) -> Result<ImportJob, KernelError> {
    let job = load_job(st, tenant, id).await?;
    let spec = target(st, &job.entity)?.spec();
    if !matches!(
        job.status,
        JobStatus::Uploaded | JobStatus::Validated | JobStatus::Invalid
    ) {
        return Err(KernelError::Conflict(format!(
            "the import is {}; its mapping can no longer change",
            job.status.as_str()
        )));
    }
    check_mapping(&spec, &job.headers, &mapping)?;
    let on_existing = on_existing.unwrap_or(job.on_existing);
    if on_existing == OnExisting::Update && !spec.supports_update {
        return Err(append_only(&spec));
    }
    let done = sqlx::query(
        "UPDATE platform.import_job SET mapping = $3, on_existing = $4, status = 'uploaded', \
         version = version + 1, updated_at = now() \
         WHERE id = $1 AND tenant_id = $2 AND version = $5",
    )
    .bind(id)
    .bind(tenant.as_uuid())
    .bind(json!(mapping))
    .bind(on_existing.as_str())
    .bind(job.version)
    .execute(&st.pool)
    .await
    .map_err(map_db_error)?;
    if done.rows_affected() == 0 {
        return Err(KernelError::Conflict(
            "the import changed concurrently".into(),
        ));
    }
    sqlx::query(
        "UPDATE platform.import_job_row SET action = NULL, errors = '[]', natural_key = NULL, \
         record_id = NULL, amount = NULL WHERE job_id = $1 AND NOT committed",
    )
    .bind(id)
    .execute(&st.pool)
    .await
    .map_err(map_db_error)?;
    audit(
        &st.pool,
        &scope_of(tenant, job.institution_id),
        "platform.import.mapping",
        id,
        actor,
        json!({"mapping": mapping, "onExisting": on_existing.as_str()}),
    )
    .await?;
    load_job(st, tenant, id).await
}

// ------------------------------------------------------------------ preparation

/// A mapped row and its pre-validation result.
struct Prepared {
    row: ImportRow,
    key: Option<String>,
    errors: Vec<FieldViolation>,
    duplicate: bool,
}

fn check_cell(c: &ColumnSpec, v: &str) -> Option<FieldViolation> {
    let r = match c.kind {
        ColumnKind::Date => parse_date(c.name, v).err(),
        ColumnKind::Decimal => parse_amount(c.name, v).err(),
        ColumnKind::Integer => parse_int(c.name, v).err(),
        ColumnKind::Boolean => parse_bool(c.name, v).err(),
        ColumnKind::Email => (!v.contains('@') || v.contains(char::is_whitespace))
            .then(|| FieldViolation::new(c.name, "email", "must be an e-mail address")),
        ColumnKind::Choice => (!c.choices.is_empty() && !c.choices.contains(&v)).then(|| {
            FieldViolation::new(
                c.name,
                "choice",
                format!("must be one of: {}", c.choices.join(", ")),
            )
        }),
        ColumnKind::Text | ColumnKind::Code | ColumnKind::Phone => None,
    };
    if r.is_some() {
        return r;
    }
    (v.chars().count() > 2000)
        .then(|| FieldViolation::new(c.name, "length", "at most 2000 characters"))
}

async fn prepare(
    st: &ImportState,
    t: &Arc<dyn ImportTargetPort>,
    spec: &EntitySpec,
    job: &ImportJob,
) -> Result<Vec<Prepared>, KernelError> {
    let raws: Vec<(i32, Value)> = sqlx::query_as(
        "SELECT line, raw FROM platform.import_job_row WHERE job_id = $1 ORDER BY line",
    )
    .bind(job.id)
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    let mut seen: HashMap<String, u32> = HashMap::new();
    let mut out = Vec::with_capacity(raws.len());
    for (line, raw) in raws {
        let line = u32::try_from(line).unwrap_or(0);
        let mut row = ImportRow {
            line,
            values: BTreeMap::new(),
        };
        if let Value::Object(cells) = raw {
            for (h, v) in cells {
                if let (Some(col), Value::String(s)) = (job.mapping.get(&h), v) {
                    let s = s.trim().to_owned();
                    if !s.is_empty() {
                        row.values.insert(col.clone(), s);
                    }
                }
            }
        }
        let mut errors = vec![];
        for c in &spec.columns {
            match row.get(c.name) {
                None if c.required => errors.push(FieldViolation::new(
                    c.name,
                    "required",
                    format!("{} is required", c.label),
                )),
                Some(v) => {
                    if let Some(e) = check_cell(c, v) {
                        errors.push(e);
                    }
                }
                None => {}
            }
        }
        let key = t.natural_key(&row);
        let mut duplicate = false;
        if let Some(k) = &key {
            match seen.get(k) {
                Some(first) => {
                    duplicate = true;
                    errors.push(FieldViolation::new(
                        spec.natural_key.join("+"),
                        "duplicate_in_file",
                        format!("the same {} as line {first}", spec.natural_key.join(" + ")),
                    ));
                }
                None => {
                    seen.insert(k.clone(), line);
                }
            }
        }
        out.push(Prepared {
            row,
            key,
            errors,
            duplicate,
        });
    }
    Ok(out)
}

/// Batches of valid rows (`group_by` rows stay together).
fn batches(spec: &EntitySpec, rows: Vec<ImportRow>, size: usize) -> Vec<Vec<ImportRow>> {
    let size = size.max(1);
    let mut out: Vec<Vec<ImportRow>> = vec![];
    match spec.group_by {
        None => {
            let mut cur = vec![];
            for r in rows {
                cur.push(r);
                if cur.len() >= size {
                    out.push(std::mem::take(&mut cur));
                }
            }
            if !cur.is_empty() {
                out.push(cur);
            }
        }
        Some(col) => {
            let mut groups: Vec<(String, Vec<ImportRow>)> = vec![];
            for r in rows {
                let g = r.get(col).unwrap_or("").to_lowercase();
                match groups.iter_mut().find(|(k, _)| *k == g) {
                    Some((_, v)) => v.push(r),
                    None => groups.push((g, vec![r])),
                }
            }
            let mut cur: Vec<ImportRow> = vec![];
            for (_, g) in groups {
                if !cur.is_empty() && cur.len() + g.len() > size {
                    out.push(std::mem::take(&mut cur));
                }
                cur.extend(g);
            }
            if !cur.is_empty() {
                out.push(cur);
            }
        }
    }
    out
}

fn file_total(spec: &EntitySpec, prepared: &[Prepared]) -> Option<Decimal> {
    let col = spec.amount_column?;
    Some(
        prepared
            .iter()
            .filter_map(|p| p.row.get(col))
            .filter_map(|v| parse_amount(col, v).ok())
            .sum(),
    )
}

type Outcome = (u32, Option<String>, RowOutcome);

/// Saves outcomes of rows (by line).
async fn save_outcomes(
    pool: &PgPool,
    job: Uuid,
    outcomes: &[Outcome],
    committed: bool,
) -> Result<(), KernelError> {
    for chunk in outcomes.chunks(1000) {
        let recs: Vec<Value> = chunk
            .iter()
            .map(|(line, key, o)| {
                json!({
                    "line": line, "natural_key": key, "action": o.action.as_str(),
                    "errors": o.errors, "record_id": o.record_id, "amount": o.amount,
                })
            })
            .collect();
        sqlx::query(
            "UPDATE platform.import_job_row r SET natural_key = x.natural_key, action = x.action, \
             errors = x.errors, record_id = x.record_id, amount = x.amount, \
             committed = r.committed OR $3 \
             FROM jsonb_to_recordset($2) AS x(line integer, natural_key text, action text, \
                  errors jsonb, record_id uuid, amount numeric) \
             WHERE r.job_id = $1 AND r.line = x.line",
        )
        .bind(job)
        .bind(Value::Array(recs))
        .bind(committed)
        .execute(pool)
        .await
        .map_err(map_db_error)?;
    }
    Ok(())
}

// ------------------------------------------------------------------ dry run

/// Validates every row (column checks, in-file duplicates, then the owning module's domain
/// validation inside rolled-back transactions). `validated` when no row fails.
pub async fn dry_run(
    st: &ImportState,
    tenant: TenantId,
    id: Uuid,
    actor: Option<&str>,
) -> Result<ImportJob, KernelError> {
    let started = Instant::now();
    let job = load_job(st, tenant, id).await?;
    if !matches!(
        job.status,
        JobStatus::Uploaded
            | JobStatus::Validated
            | JobStatus::Invalid
            | JobStatus::Failed
            | JobStatus::Rejected
    ) {
        return Err(KernelError::Conflict(format!(
            "the import is {}; it cannot be dry-run now",
            job.status.as_str()
        )));
    }
    let t = target(st, &job.entity)?.clone();
    let spec = t.spec();
    check_mapping(&spec, &job.headers, &job.mapping)?;
    let scope = scope_of(tenant, job.institution_id);
    let prepared = prepare(st, &t, &spec, &job).await?;
    let mut outcomes: Vec<Outcome> = Vec::with_capacity(prepared.len());
    let mut valid = vec![];
    let mut keys: HashMap<u32, Option<String>> = HashMap::new();
    for p in &prepared {
        keys.insert(p.row.line, p.key.clone());
        if p.errors.is_empty() {
            valid.push(p.row.clone());
        } else {
            outcomes.push((
                p.row.line,
                p.key.clone(),
                RowOutcome::error(p.row.line, p.errors.clone()),
            ));
        }
    }
    let size = usize::try_from(job.batch_size).unwrap_or(500);
    for b in batches(&spec, valid, size) {
        let r = t
            .apply(
                &scope,
                ImportBatch {
                    job_id: id,
                    institution_id: InstitutionId(job.institution_id),
                    on_existing: job.on_existing,
                    dry_run: true,
                    actor: actor.map(str::to_owned),
                    rows: b,
                },
            )
            .await?;
        for o in r.rows {
            let key = keys.get(&o.line).cloned().flatten();
            outcomes.push((o.line, key, o));
        }
    }
    outcomes.sort_by_key(|(l, _, _)| *l);
    save_outcomes(&st.pool, id, &outcomes, false).await?;
    let errors = outcomes
        .iter()
        .filter(|(_, _, o)| o.action == RowAction::Error)
        .count();
    let existing = outcomes
        .iter()
        .filter(|(_, _, o)| matches!(o.action, RowAction::Skip | RowAction::Update))
        .count();
    let dups = prepared.iter().filter(|p| p.duplicate).count();
    let status = if errors == 0 {
        JobStatus::Validated
    } else {
        JobStatus::Invalid
    };
    let ms = i32::try_from(started.elapsed().as_millis()).unwrap_or(i32::MAX);
    let n = |v: usize| i32::try_from(v).unwrap_or(i32::MAX);
    sqlx::query(
        "UPDATE platform.import_job SET status = $3, error_count = $4, valid_count = $5, \
         existing_count = $6, duplicate_in_file_count = $7, file_total = $8, validated_at = now(), \
         validated_ms = $9, failure_reason = NULL, version = version + 1, updated_at = now() \
         WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(tenant.as_uuid())
    .bind(status.as_str())
    .bind(n(errors))
    .bind(n(prepared.len().saturating_sub(errors)))
    .bind(n(existing))
    .bind(n(dups))
    .bind(file_total(&spec, &prepared))
    .bind(ms)
    .execute(&st.pool)
    .await
    .map_err(map_db_error)?;
    audit(
        &st.pool,
        &scope,
        "platform.import.dry_run",
        id,
        actor,
        json!({"rows": prepared.len(), "errors": errors, "duplicatesInFile": dups,
               "existing": existing, "status": status.as_str(), "ms": ms}),
    )
    .await?;
    load_job(st, tenant, id).await
}

// ------------------------------------------------------------------ commit

/// Moves the job into `committing` when it is in one of `from` (or a stale `committing` claim
/// older than ten minutes); 409 otherwise.
async fn claim(
    pool: &PgPool,
    tenant: TenantId,
    id: Uuid,
    from: &[JobStatus],
) -> Result<(), KernelError> {
    let from: Vec<String> = from.iter().map(|s| s.as_str().to_owned()).collect();
    let n = sqlx::query(
        "UPDATE platform.import_job SET status = 'committing', version = version + 1, \
         updated_at = now() WHERE id = $1 AND tenant_id = $2 AND (status = ANY($3) \
         OR (status = 'committing' AND updated_at < now() - interval '10 minutes'))",
    )
    .bind(id)
    .bind(tenant.as_uuid())
    .bind(&from)
    .execute(pool)
    .await
    .map_err(map_db_error)?
    .rows_affected();
    if n == 0 {
        return Err(KernelError::Conflict(
            "the import is not ready to commit (run a clean dry run first) or is being committed"
                .into(),
        ));
    }
    Ok(())
}

/// Requests the commit: runs it now, or — for entities that need a second person — starts the
/// approval and leaves the job `pending_approval`.
pub async fn request_commit(
    st: &ImportState,
    tenant: TenantId,
    id: Uuid,
    actor: Option<&str>,
) -> Result<ImportJob, KernelError> {
    let job = load_job(st, tenant, id).await?;
    let spec = target(st, &job.entity)?.spec();
    if job.status != JobStatus::Validated {
        return Err(KernelError::Conflict(format!(
            "the import is {}; only a validated import (clean dry run) can be committed",
            job.status.as_str()
        )));
    }
    let scope = scope_of(tenant, job.institution_id);
    if spec.requires_approval {
        let business_key = format!("import_job:{id}:v{}", job.version);
        let mut vars = BTreeMap::new();
        vars.insert("entity".to_owned(), json!(job.entity));
        vars.insert("rows".to_owned(), json!(job.summary.rows_in_file));
        vars.insert("fileTotal".to_owned(), json!(job.summary.file_total));
        let p = st
            .workflow
            .start_process(
                &scope,
                StartProcess {
                    process_definition_key: COMMIT_APPROVAL.to_owned(),
                    business_key,
                    variables: vars,
                },
            )
            .await?;
        sutra_persistence::approval::record_request(
            &st.pool,
            tenant.as_uuid(),
            &p.process_definition_key,
            &p.business_key,
            &p.process_instance_id,
            actor,
        )
        .await?;
        let n = sqlx::query(
            "UPDATE platform.import_job SET status = 'pending_approval', process_instance_id = $3, \
             commit_requested_by = $4, commit_requested_at = now(), decided_by = NULL, \
             decided_at = NULL, decision_reason = NULL, version = version + 1, updated_at = now() \
             WHERE id = $1 AND tenant_id = $2 AND status = 'validated' AND version = $5",
        )
        .bind(id)
        .bind(tenant.as_uuid())
        .bind(&p.process_instance_id)
        .bind(actor)
        .bind(job.version)
        .execute(&st.pool)
        .await
        .map_err(map_db_error)?
        .rows_affected();
        if n == 0 {
            return Err(KernelError::Conflict(
                "the import changed concurrently".into(),
            ));
        }
        audit(
            &st.pool,
            &scope,
            "platform.import.commit_request",
            id,
            actor,
            json!({"processInstanceId": p.process_instance_id, "fileTotal": job.summary.file_total}),
        )
        .await?;
        return load_job(st, tenant, id).await;
    }
    sqlx::query(
        "UPDATE platform.import_job SET commit_requested_by = $3, commit_requested_at = now() \
         WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(tenant.as_uuid())
    .bind(actor)
    .execute(&st.pool)
    .await
    .map_err(map_db_error)?;
    claim(&st.pool, tenant, id, &[JobStatus::Validated]).await?;
    run_commit(st, tenant, id, actor).await
}

/// Decision on a pending commit (`platform.import.approve`). Separation of duties: the job's
/// maker (uploader / commit requester) and system actors are refused (403).
pub async fn decide(
    st: &ImportState,
    tenant: TenantId,
    id: Uuid,
    approve: bool,
    reason: Option<&str>,
    decider: &str,
) -> Result<ImportJob, KernelError> {
    let job = load_job(st, tenant, id).await?;
    sutra_persistence::approval::guard_decision_by(
        &st.pool,
        tenant.as_uuid(),
        Some(decider),
        id,
        &[
            job.created_by.as_deref(),
            job.commit_requested_by.as_deref(),
        ],
        "data import",
    )
    .await?;
    if job.status != JobStatus::PendingApproval {
        return Err(KernelError::Conflict(
            "no import commit is awaiting a decision".into(),
        ));
    }
    let scope = scope_of(tenant, job.institution_id);
    let reason = reason.map(str::trim).filter(|r| !r.is_empty());
    if !approve {
        let r =
            reason.ok_or_else(|| invalid("reason", "required", "a rejection needs a reason"))?;
        let n = sqlx::query(
            "UPDATE platform.import_job SET status = 'rejected', decided_by = $3, \
             decided_at = now(), decision_reason = $4, version = version + 1, updated_at = now() \
             WHERE id = $1 AND tenant_id = $2 AND status = 'pending_approval'",
        )
        .bind(id)
        .bind(tenant.as_uuid())
        .bind(decider)
        .bind(r)
        .execute(&st.pool)
        .await
        .map_err(map_db_error)?
        .rows_affected();
        if n == 0 {
            return Err(KernelError::Conflict(
                "the import changed concurrently".into(),
            ));
        }
        audit(
            &st.pool,
            &scope,
            "platform.import.reject",
            id,
            Some(decider),
            json!({"reason": r}),
        )
        .await?;
        return load_job(st, tenant, id).await;
    }
    sqlx::query(
        "UPDATE platform.import_job SET decided_by = $3, decided_at = now(), decision_reason = $4 \
         WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(tenant.as_uuid())
    .bind(decider)
    .bind(reason)
    .execute(&st.pool)
    .await
    .map_err(map_db_error)?;
    audit(
        &st.pool,
        &scope,
        "platform.import.approve",
        id,
        Some(decider),
        json!({"reason": reason, "processInstanceId": job.process_instance_id}),
    )
    .await?;
    claim(&st.pool, tenant, id, &[JobStatus::PendingApproval]).await?;
    run_commit(st, tenant, id, Some(decider)).await
}

/// Runs a claimed (`committing`) job: one transaction per batch; stops at the first failed batch.
async fn run_commit(
    st: &ImportState,
    tenant: TenantId,
    id: Uuid,
    actor: Option<&str>,
) -> Result<ImportJob, KernelError> {
    let started = Instant::now();
    let job = load_job(st, tenant, id).await?;
    let t = target(st, &job.entity)?.clone();
    let spec = t.spec();
    let scope = scope_of(tenant, job.institution_id);
    let result = commit_batches(st, &t, &spec, &job, &scope, actor).await;
    let ms = i32::try_from(started.elapsed().as_millis()).unwrap_or(i32::MAX);
    let (status, failure, batches_done) = match &result {
        Ok((None, done)) => (JobStatus::Committed, None, *done),
        Ok((Some(f), done)) => (JobStatus::Failed, Some(f.clone()), *done),
        Err(e) => (JobStatus::Failed, Some(format!("commit aborted: {e}")), 0),
    };
    // Counters from the row ledger (committed rows only).
    let counts: Vec<(String, i64, Option<Decimal>)> = sqlx::query_as(
        "SELECT action, count(*), sum(amount) FROM platform.import_job_row \
         WHERE job_id = $1 AND committed AND action IS NOT NULL GROUP BY action",
    )
    .bind(id)
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    let get = |a: &str| {
        counts
            .iter()
            .find(|(x, _, _)| x == a)
            .map_or(0, |(_, n, _)| i32::try_from(*n).unwrap_or(i32::MAX))
    };
    let committed_total: Option<Decimal> = spec.amount_column.map(|_| {
        counts
            .iter()
            .filter(|(a, _, _)| a == "create" || a == "update")
            .filter_map(|(_, _, s)| *s)
            .sum()
    });
    let ledger_total = if spec.amount_column.is_some() {
        let ids: Vec<Uuid> = sqlx::query_scalar(
            "SELECT record_id FROM platform.import_job_row WHERE job_id = $1 \
             AND action IN ('create', 'update', 'skip') AND record_id IS NOT NULL",
        )
        .bind(id)
        .fetch_all(&st.pool)
        .await
        .map_err(map_db_error)?;
        t.ledger_total(&scope, InstitutionId(job.institution_id), &ids)
            .await?
    } else {
        None
    };
    let reconciled = match (ledger_total, job.summary.file_total) {
        (Some(l), Some(f)) => Some(l == f && status == JobStatus::Committed),
        _ => None,
    };
    sqlx::query(
        "UPDATE platform.import_job SET status = $3, failure_reason = $4, created_count = $5, \
         updated_count = $6, skipped_count = $7, batches_committed = batches_committed + $8, \
         committed_total = $9, ledger_total = $10, reconciled = $11, \
         committed_by = CASE WHEN $3 = 'committed' THEN $12 ELSE committed_by END, \
         committed_at = CASE WHEN $3 = 'committed' THEN now() ELSE committed_at END, \
         committed_ms = $13, version = version + 1, updated_at = now() \
         WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(tenant.as_uuid())
    .bind(status.as_str())
    .bind(&failure)
    .bind(get("create"))
    .bind(get("update"))
    .bind(get("skip"))
    .bind(batches_done)
    .bind(committed_total)
    .bind(ledger_total)
    .bind(reconciled)
    .bind(actor)
    .bind(ms)
    .execute(&st.pool)
    .await
    .map_err(map_db_error)?;
    audit(
        &st.pool,
        &scope,
        "platform.import.commit",
        id,
        actor,
        json!({
            "status": status.as_str(), "failure": failure, "batches": batches_done,
            "created": get("create"), "updated": get("update"), "skipped": get("skip"),
            "committedTotal": committed_total, "ledgerTotal": ledger_total,
            "fileTotal": job.summary.file_total, "reconciled": reconciled, "ms": ms,
        }),
    )
    .await?;
    result?;
    load_job(st, tenant, id).await
}

/// `(failure, batches committed)`.
async fn commit_batches(
    st: &ImportState,
    t: &Arc<dyn ImportTargetPort>,
    spec: &EntitySpec,
    job: &ImportJob,
    scope: &ExecutionScope,
    actor: Option<&str>,
) -> Result<(Option<String>, i32), KernelError> {
    let prepared = prepare(st, t, spec, job).await?;
    if let Some(p) = prepared.iter().find(|p| !p.errors.is_empty()) {
        return Ok((
            Some(format!(
                "line {} no longer passes the column checks; dry-run again",
                p.row.line
            )),
            0,
        ));
    }
    let keys: HashMap<u32, Option<String>> = prepared
        .iter()
        .map(|p| (p.row.line, p.key.clone()))
        .collect();
    let rows: Vec<ImportRow> = prepared.into_iter().map(|p| p.row).collect();
    let size = usize::try_from(job.batch_size).unwrap_or(500);
    let mut done = 0;
    for (i, b) in batches(spec, rows, size).into_iter().enumerate() {
        let r = t
            .apply(
                scope,
                ImportBatch {
                    job_id: job.id,
                    institution_id: InstitutionId(job.institution_id),
                    on_existing: job.on_existing,
                    dry_run: false,
                    actor: actor.map(str::to_owned),
                    rows: b,
                },
            )
            .await?;
        let outcomes: Vec<Outcome> = r
            .rows
            .into_iter()
            .map(|o| (o.line, keys.get(&o.line).cloned().flatten(), o))
            .collect();
        if r.committed {
            save_outcomes(&st.pool, job.id, &outcomes, true).await?;
            done += 1;
            continue;
        }
        // Rolled back: record why (rows of the failed batch stay uncommitted).
        let first = outcomes
            .iter()
            .find(|(_, _, o)| o.action == RowAction::Error)
            .map(|(l, _, o)| {
                let msg = o
                    .errors
                    .first()
                    .map(|e| e.message.clone())
                    .unwrap_or_default();
                format!("line {l}: {msg}")
            })
            .unwrap_or_default();
        let failed: Vec<Outcome> = outcomes
            .into_iter()
            .map(|(l, k, mut o)| {
                if o.action != RowAction::Error {
                    o.record_id = None;
                }
                (l, k, o)
            })
            .collect();
        save_outcomes(&st.pool, job.id, &failed, false).await?;
        return Ok((
            Some(format!(
                "batch {} was rolled back ({first}); {done} earlier batch(es) stay committed — \
                 fix the file or the data, dry-run again and commit to resume (committed rows are \
                 skipped by natural key)",
                i + 1
            )),
            done,
        ));
    }
    Ok((None, done))
}

// ------------------------------------------------------------------ rows / reports

/// One row of a job.
#[derive(Clone, Debug, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ImportJobRow {
    pub line: i32,
    pub natural_key: Option<String>,
    pub action: Option<RowAction>,
    pub errors: Vec<FieldViolation>,
    pub record_id: Option<Uuid>,
    pub amount: Option<Decimal>,
    pub committed: bool,
    /// Source cells by header.
    pub cells: BTreeMap<String, String>,
}

pub async fn rows(
    st: &ImportState,
    tenant: TenantId,
    id: Uuid,
    only_errors: bool,
    limit: i64,
) -> Result<Vec<ImportJobRow>, KernelError> {
    load_job(st, tenant, id).await?;
    let rs = sqlx::query(
        "SELECT line, natural_key, action, errors, record_id, amount, committed, raw \
         FROM platform.import_job_row WHERE job_id = $1 AND tenant_id = $2 \
         AND (NOT $3 OR action = 'error') ORDER BY line LIMIT $4",
    )
    .bind(id)
    .bind(tenant.as_uuid())
    .bind(only_errors)
    .bind(limit)
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    rs.iter()
        .map(|r| {
            let action: Option<String> = r.try_get("action").map_err(map_db_error)?;
            let errors: Value = r.try_get("errors").map_err(map_db_error)?;
            let raw: Value = r.try_get("raw").map_err(map_db_error)?;
            Ok(ImportJobRow {
                line: r.try_get("line").map_err(map_db_error)?,
                natural_key: r.try_get("natural_key").map_err(map_db_error)?,
                action: action.as_deref().and_then(RowAction::parse),
                errors: serde_json::from_value(errors).map_err(KernelError::internal)?,
                record_id: r.try_get("record_id").map_err(map_db_error)?,
                amount: r.try_get("amount").map_err(map_db_error)?,
                committed: r.try_get("committed").map_err(map_db_error)?,
                cells: serde_json::from_value(raw).map_err(KernelError::internal)?,
            })
        })
        .collect()
}

/// Row-level error report: `line, column, code, message` + the row's source cells.
pub async fn error_report(
    st: &ImportState,
    tenant: TenantId,
    id: Uuid,
) -> Result<(String, Vec<u8>), KernelError> {
    let job = load_job(st, tenant, id).await?;
    let error_rows = rows(st, tenant, id, true, 100_000).await?;
    let source: Vec<&String> = job.headers.iter().filter(|h| !h.is_empty()).collect();
    let mut headers: Vec<String> = ["line", "column", "code", "message"]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    headers.extend(source.iter().map(|h| (*h).clone()));
    let mut out = vec![];
    for r in &error_rows {
        for e in &r.errors {
            let mut cells = vec![
                r.line.to_string(),
                e.field.clone(),
                e.code.clone(),
                e.message.clone(),
            ];
            for h in &source {
                cells.push(r.cells.get(*h).cloned().unwrap_or_default());
            }
            out.push(cells);
        }
    }
    let base = job
        .file_name
        .rsplit_once('.')
        .map_or(job.file_name.as_str(), |(b, _)| b)
        .to_owned();
    Ok((
        format!("{base}-errors.csv"),
        files::write_csv(&headers, &out),
    ))
}

/// Template of an entity: canonical headers and one example row.
pub fn template(spec: &EntitySpec, format: FileFormat) -> Result<Vec<u8>, KernelError> {
    let headers: Vec<String> = spec.columns.iter().map(|c| c.name.to_owned()).collect();
    let example: Vec<String> = spec.columns.iter().map(|c| c.example.to_owned()).collect();
    match format {
        FileFormat::Csv => Ok(files::write_csv(&headers, &[example])),
        FileFormat::Xlsx => files::write_xlsx(spec.entity, &headers, &[example]),
    }
}

/// Export of an institution's records in the import columns (audited).
pub async fn export(
    st: &ImportState,
    tenant: TenantId,
    entity: &str,
    institution: Uuid,
    format: FileFormat,
    actor: Option<&str>,
) -> Result<Vec<u8>, KernelError> {
    let t = target(st, entity)?.clone();
    let spec = t.spec();
    let scope = scope_of(tenant, institution);
    let records = t.export(&scope, InstitutionId(institution)).await?;
    let headers: Vec<String> = spec.columns.iter().map(|c| c.name.to_owned()).collect();
    let rows: Vec<Vec<String>> = records
        .iter()
        .map(|r| {
            headers
                .iter()
                .map(|h| r.get(h).cloned().unwrap_or_default())
                .collect()
        })
        .collect();
    sutra_persistence::audit::insert(
        &st.pool,
        &AuditEntry::new(scope, "platform.import.export", AuditOutcome::Succeeded)
            .actor(ActorKind::User, actor.map(str::to_owned))
            .target("import_entity", entity.to_owned())
            .correlation(sutra_web::request_id::current_correlation_id())
            .details(json!({"entity": entity, "rows": rows.len(), "format": format.as_str()})),
    )
    .await
    .map_err(map_db_error)?;
    match format {
        FileFormat::Csv => Ok(files::write_csv(&headers, &rows)),
        FileFormat::Xlsx => files::write_xlsx(entity, &headers, &rows),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sutra_kernel::auth::Permission;

    fn spec(group: Option<&'static str>) -> EntitySpec {
        EntitySpec {
            entity: "things",
            label: "Things",
            owner: "test",
            columns: vec![
                ColumnSpec::new("code", "Code", ColumnKind::Code).required(),
                ColumnSpec::new("group", "Group", ColumnKind::Text),
                ColumnSpec::new("amount", "Opening amount", ColumnKind::Decimal),
            ],
            natural_key: &["code"],
            amount_column: Some("amount"),
            group_by: group,
            supports_update: true,
            requires_approval: false,
            permission: Permission::DataImport,
        }
    }

    fn row(line: u32, code: &str, group: &str) -> ImportRow {
        let mut r = ImportRow {
            line,
            values: BTreeMap::new(),
        };
        r.values.insert("code".into(), code.into());
        r.values.insert("group".into(), group.into());
        r
    }

    #[test]
    fn mapping_is_suggested_from_names_and_labels() {
        let s = spec(None);
        let m = suggest_mapping(
            &s,
            &["CODE".into(), "Opening Amount".into(), "Other".into()],
        );
        assert_eq!(m.get("CODE").map(String::as_str), Some("code"));
        assert_eq!(m.get("Opening Amount").map(String::as_str), Some("amount"));
        assert!(!m.contains_key("Other"));
        assert!(check_mapping(&s, &["CODE".into()], &m).is_err()); // unknown header
        let ok: BTreeMap<String, String> = [("CODE".to_owned(), "code".to_owned())].into();
        assert!(check_mapping(&s, &["CODE".into()], &ok).is_ok());
        assert!(check_mapping(&s, &["CODE".into()], &BTreeMap::new()).is_err());
        // required
    }

    #[test]
    fn batches_respect_size_and_groups() {
        let rows: Vec<ImportRow> = (0..5).map(|i| row(i + 2, &format!("c{i}"), "")).collect();
        assert_eq!(
            batches(&spec(None), rows, 2)
                .iter()
                .map(Vec::len)
                .collect::<Vec<_>>(),
            vec![2, 2, 1]
        );
        let rows = vec![
            row(2, "a", "g1"),
            row(3, "b", "g2"),
            row(4, "c", "g1"),
            row(5, "d", "g2"),
            row(6, "e", "g3"),
        ];
        let b = batches(&spec(Some("group")), rows, 3);
        // Groups are never split: [g1 g1], [g2 g2 g3].
        assert_eq!(b.len(), 2);
        assert!(b[0].iter().all(|r| r.get("group") == Some("g1")));
        assert_eq!(b[1].len(), 3);
    }

    #[test]
    fn cells_are_checked_by_kind() {
        let c = ColumnSpec::new("d", "D", ColumnKind::Date);
        assert!(check_cell(&c, "2026-01-01").is_none());
        assert!(check_cell(&c, "1/1/26").is_some());
        let c = ColumnSpec::new("k", "K", ColumnKind::Choice).choices(&["a", "b"]);
        assert!(check_cell(&c, "a").is_none());
        assert!(check_cell(&c, "z").is_some());
        let c = ColumnSpec::new("e", "E", ColumnKind::Email);
        assert!(check_cell(&c, "x@y").is_none());
        assert!(check_cell(&c, "nope").is_some());
        assert_eq!(
            JobStatus::parse("pending_approval"),
            Some(JobStatus::PendingApproval)
        );
    }
}
