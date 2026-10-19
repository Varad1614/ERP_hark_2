//! HTTP API of the data migration jobs (see the module docs of `dataimport`).

use super::engine::{self, ImportJob, ImportJobRow, MappingProfile, Upload};
use super::files::FileFormat;
use super::ImportState;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use std::collections::BTreeMap;
use sutra_contracts::platform::dataimport::{ColumnKind, EntitySpec, OnExisting};
use sutra_kernel::auth::{AuthContext, Permission};
use sutra_kernel::entitlement;
use sutra_kernel::error::{FieldViolation, KernelError};
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

/// Transport limit of the upload body: the engine's largest file (8 MiB) as base64 in the JSON
/// body, plus room for the other members (Part 2 · S4-ui-import). The composition root enforces
/// it on `POST /api/v1/platform/imports` only (`storage::BodyLimits`); every other route keeps
/// `SUTRA_MAX_BODY_BYTES`.
pub const UPLOAD_BODY_BYTES: usize = engine::MAX_FILE_BYTES.div_ceil(3) * 4 + 64 * 1024;

/// Mounted by the composition root next to the platform router. The extractor limit is lifted
/// to [`UPLOAD_BODY_BYTES`] here; the composition root's per-route body limit is the control.
pub fn router(state: ImportState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(entity_list))
        .routes(routes!(entity_template))
        .routes(routes!(entity_export))
        .routes(routes!(profile_list, profile_save))
        .routes(routes!(job_list, job_upload))
        .routes(routes!(job_get))
        .routes(routes!(job_rows))
        .routes(routes!(job_error_report))
        .routes(routes!(job_mapping))
        .routes(routes!(job_dry_run))
        .routes(routes!(job_commit))
        .routes(routes!(job_decision))
        .with_state(state)
        .layer(axum::extract::DefaultBodyLimit::max(UPLOAD_BODY_BYTES))
}

// ------------------------------------------------------------------ wire types

/// One column of an importable entity.
#[derive(Clone, Debug, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ImportColumnView {
    pub name: String,
    pub label: String,
    pub required: bool,
    pub kind: ColumnKind,
    pub choices: Vec<String>,
    pub example: String,
    pub help: String,
}

/// An importable / exportable entity.
#[derive(Clone, Debug, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ImportEntityView {
    pub entity: String,
    pub label: String,
    pub owner: String,
    pub columns: Vec<ImportColumnView>,
    pub natural_key: Vec<String>,
    pub amount_column: Option<String>,
    pub group_by: Option<String>,
    pub supports_update: bool,
    /// A commit needs a second person's approval.
    pub requires_approval: bool,
    /// The owning domain's manage permission (needed in addition to `platform.import.manage`).
    pub permission: String,
    /// `true` when the caller holds that permission.
    pub allowed: bool,
}

#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
pub struct ImportEntityList {
    pub items: Vec<ImportEntityView>,
}

#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
pub struct ImportJobList {
    pub items: Vec<ImportJob>,
}

#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
pub struct ImportJobRowList {
    pub items: Vec<ImportJobRow>,
}

#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
pub struct ImportMappingProfileList {
    pub items: Vec<MappingProfile>,
}

/// Upload of one CSV / XLSX file.
#[derive(Clone, Debug, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImportUpload {
    #[schema(example = "students")]
    pub entity: String,
    pub institution_id: Uuid,
    /// `.csv` or `.xlsx`.
    #[schema(example = "students-2026.csv")]
    pub file_name: String,
    /// The file, base64 encoded (≤ 8 MiB decoded; the request body limit applies).
    pub content_base64: String,
    /// Idempotency of the upload (default `<entity>:<institution>:<sha256 of the file>`).
    pub job_key: Option<String>,
    /// Saved mapping to apply (default: headers matched to column names / labels).
    pub mapping_profile_id: Option<Uuid>,
    /// What to do with rows whose natural key exists (default `skip`).
    pub on_existing: Option<OnExisting>,
    /// Rows per commit transaction (1–5000, default 500).
    pub batch_size: Option<i32>,
}

/// New column mapping of a job.
#[derive(Clone, Debug, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImportMappingUpdate {
    /// Source header → canonical column.
    pub mapping: BTreeMap<String, String>,
    pub on_existing: Option<OnExisting>,
    /// Also save the mapping as a reusable profile with this name.
    pub save_as_profile: Option<String>,
}

/// Saves (creates or replaces by name) a mapping profile.
#[derive(Clone, Debug, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImportMappingProfileSave {
    pub entity: String,
    pub name: String,
    pub mapping: BTreeMap<String, String>,
}

/// Decision on a commit awaiting approval.
#[derive(Clone, Debug, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImportDecision {
    pub approve: bool,
    /// Required for a rejection.
    pub reason: Option<String>,
}

#[derive(Debug, Default, serde::Deserialize, utoipa::IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct FormatQuery {
    /// `csv` (default) or `xlsx`.
    pub format: Option<String>,
}

#[derive(Debug, serde::Deserialize, utoipa::IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct ExportQuery {
    pub institution_id: Uuid,
    /// `csv` (default) or `xlsx`.
    pub format: Option<String>,
}

#[derive(Debug, Default, serde::Deserialize, utoipa::IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct JobListQuery {
    pub institution_id: Option<Uuid>,
    pub entity: Option<String>,
}

#[derive(Debug, Default, serde::Deserialize, utoipa::IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct RowsQuery {
    /// Only rows with errors.
    pub errors_only: Option<bool>,
    /// 1–5000 (default 500).
    pub limit: Option<i64>,
}

#[derive(Debug, Default, serde::Deserialize, utoipa::IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct ProfileQuery {
    pub entity: Option<String>,
}

// ------------------------------------------------------------------ helpers

fn view(spec: &EntitySpec, ctx: &AuthContext) -> ImportEntityView {
    ImportEntityView {
        entity: spec.entity.to_owned(),
        label: spec.label.to_owned(),
        owner: spec.owner.to_owned(),
        columns: spec
            .columns
            .iter()
            .map(|c| ImportColumnView {
                name: c.name.to_owned(),
                label: c.label.to_owned(),
                required: c.required,
                kind: c.kind,
                choices: c.choices.iter().map(|s| (*s).to_owned()).collect(),
                example: c.example.to_owned(),
                help: c.help.to_owned(),
            })
            .collect(),
        natural_key: spec.natural_key.iter().map(|s| (*s).to_owned()).collect(),
        amount_column: spec.amount_column.map(str::to_owned),
        group_by: spec.group_by.map(str::to_owned),
        supports_update: spec.supports_update,
        requires_approval: spec.requires_approval,
        permission: spec.permission.as_str().to_owned(),
        allowed: ctx.has_permission(spec.permission),
    }
}

fn spec_of(st: &ImportState, entity: &str) -> Result<EntitySpec, KernelError> {
    st.target(entity).map(|t| t.spec()).ok_or_else(|| {
        KernelError::Validation(vec![FieldViolation::new(
            "entity",
            "unknown",
            format!("`{entity}` is not an importable entity"),
        )])
    })
}

/// `platform.import.manage` plus the entity's own manage permission.
/// Who reads jobs: a `platform.import.manage` holder reads every job of the tenant; a holder of
/// `platform.import.approve` only (the finance checker) reads the jobs that need a second
/// approver — the ones it decides (Part 2 · S4-ui-import; deciding still needs step-up).
fn reader(auth: &Authenticated) -> ApiResult<(&AuthContext, bool)> {
    if !auth.0.has_permission(Permission::DataImport)
        && auth.0.has_permission(Permission::DataImportApprove)
    {
        return Ok((&auth.0, true));
    }
    Ok((auth.require(Permission::DataImport)?, false))
}

/// The job, unless an approver-only reader asks for one that needs no approval (404).
async fn readable_job(
    st: &ImportState,
    ctx: &AuthContext,
    approver_only: bool,
    id: Uuid,
) -> Result<ImportJob, KernelError> {
    let job = engine::load_job(st, ctx.scope.tenant_id, id).await?;
    if approver_only && !job.requires_approval {
        return Err(KernelError::NotFound {
            entity: "import job",
        });
    }
    Ok(job)
}

fn require_entity<'a>(
    auth: &'a Authenticated,
    st: &ImportState,
    entity: &str,
) -> Result<(&'a AuthContext, EntitySpec), sutra_web::ApiError> {
    let ctx = auth.require(Permission::DataImport)?;
    let spec = spec_of(st, entity)?;
    auth.require(spec.permission)?;
    Ok((ctx, spec))
}

/// Part 2 · S7-entitle: the module `entity` writes into must be active for the caller's tenant
/// (`403 entitlement.module_inactive`); institution set-up entities are core.
///
/// Audit A3 (M7): fails **closed** — a state built without an entitlement policy refuses every
/// module entity (`403 entitlement.not_configured`) instead of allowing it.
async fn require_entitled(
    st: &ImportState,
    ctx: &AuthContext,
    entity: &str,
) -> Result<(), KernelError> {
    let reg = entitlement::registry().map_err(|e| KernelError::Unavailable(e.to_owned()))?;
    let Some(module) = reg.import_module(entity) else {
        return Ok(()); // core institution set-up
    };
    let Some(policy) = &st.entitlement else {
        tracing::error!(
            entity,
            module,
            "data import without an entitlement policy; refusing module entity"
        );
        return Err(KernelError::Entitlement {
            code: ENTITLEMENT_NOT_CONFIGURED,
            detail: format!("module `{module}` entitlement is not configured for data import"),
        });
    };
    policy.require_module(ctx.scope.tenant_id, module).await
}

/// Audit A3 (M7): error code of a module import refused because no policy is wired.
pub const ENTITLEMENT_NOT_CONFIGURED: &str = "entitlement.not_configured";

fn format_of(raw: Option<&str>) -> Result<FileFormat, KernelError> {
    match raw {
        None => Ok(FileFormat::Csv),
        Some(f) => FileFormat::parse(f).ok_or_else(|| {
            KernelError::Validation(vec![FieldViolation::new(
                "format",
                "unknown",
                "format must be csv or xlsx",
            )])
        }),
    }
}

fn file_response(name: &str, format: FileFormat, bytes: Vec<u8>) -> Response {
    let safe: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        .collect();
    (
        [
            (header::CONTENT_TYPE, format.content_type().to_owned()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{safe}\""),
            ),
            (header::CACHE_CONTROL, "no-store".to_owned()),
        ],
        bytes,
    )
        .into_response()
}

fn actor(ctx: &AuthContext) -> Option<&str> {
    Some(ctx.subject.as_str())
}

// ------------------------------------------------------------------ handlers

/// Importable entities with their columns, natural keys and the caller's access.
#[utoipa::path(get, path = "/api/v1/platform/imports/entities", operation_id = "platform_import_entity_list", tag = "platform-import",
    security(("session_cookie" = [])),
    responses((status = 200, body = ImportEntityList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn entity_list(
    State(st): State<ImportState>,
    auth: Authenticated,
) -> ApiResult<Json<ImportEntityList>> {
    let ctx = auth.require(Permission::DataImport)?;
    let mut items: Vec<ImportEntityView> =
        st.targets.values().map(|t| view(&t.spec(), ctx)).collect();
    // Part 2 · S7-entitle: only entities of active modules (and core set-up) are offered.
    // Audit A3 (M7): without a policy only the core set-up entities are offered (fail closed).
    let reg = entitlement::registry().map_err(|e| KernelError::Unavailable(e.to_owned()))?;
    let ent = match &st.entitlement {
        Some(policy) => Some(policy.effective(ctx.scope.tenant_id).await?),
        None => None,
    };
    items.retain(|v| match reg.import_module(&v.entity) {
        Some(m) => ent.as_ref().is_some_and(|e| e.has_module(m)),
        None => true,
    });
    let order = |e: &str| {
        sutra_contracts::platform::dataimport::entity::ALL
            .iter()
            .position(|x| *x == e)
            .unwrap_or(usize::MAX)
    };
    items.sort_by_key(|v| order(&v.entity));
    Ok(Json(ImportEntityList { items }))
}

/// Blank import template (canonical headers + one example row).
#[utoipa::path(get, path = "/api/v1/platform/imports/entities/{entity}/template", operation_id = "platform_import_template", tag = "platform-import",
    params(("entity" = String, Path), FormatQuery), security(("session_cookie" = [])),
    responses((status = 200, description = "CSV or XLSX file", body = String, content_type = "application/octet-stream"), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn entity_template(
    State(st): State<ImportState>,
    auth: Authenticated,
    Path(entity): Path<String>,
    Query(q): Query<FormatQuery>,
) -> ApiResult<Response> {
    auth.require(Permission::DataImport)?;
    let spec = spec_of(&st, &entity)?;
    let format = format_of(q.format.as_deref())?;
    let bytes = engine::template(&spec, format)?;
    Ok(file_response(
        &format!("{entity}-template.{}", format.as_str()),
        format,
        bytes,
    ))
}

/// Export of an institution's records in the import columns (formula-injection safe; audited).
#[utoipa::path(get, path = "/api/v1/platform/imports/entities/{entity}/export", operation_id = "platform_import_export", tag = "platform-import",
    params(("entity" = String, Path), ExportQuery), security(("session_cookie" = [])),
    responses((status = 200, description = "CSV or XLSX file", body = String, content_type = "application/octet-stream"), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn entity_export(
    State(st): State<ImportState>,
    auth: Authenticated,
    Path(entity): Path<String>,
    Query(q): Query<ExportQuery>,
) -> ApiResult<Response> {
    let (ctx, _) = require_entity(&auth, &st, &entity)?;
    let format = format_of(q.format.as_deref())?;
    let bytes = engine::export(
        &st,
        ctx.scope.tenant_id,
        &entity,
        q.institution_id,
        format,
        actor(ctx),
    )
    .await?;
    Ok(file_response(
        &format!("{entity}-export.{}", format.as_str()),
        format,
        bytes,
    ))
}

/// Saved mapping profiles.
#[utoipa::path(get, path = "/api/v1/platform/imports/mapping-profiles", operation_id = "platform_import_profile_list", tag = "platform-import",
    params(ProfileQuery), security(("session_cookie" = [])),
    responses((status = 200, body = ImportMappingProfileList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn profile_list(
    State(st): State<ImportState>,
    auth: Authenticated,
    Query(q): Query<ProfileQuery>,
) -> ApiResult<Json<ImportMappingProfileList>> {
    let ctx = auth.require(Permission::DataImport)?;
    let items = engine::list_profiles(&st, ctx.scope.tenant_id, q.entity.as_deref()).await?;
    Ok(Json(ImportMappingProfileList { items }))
}

/// Creates or replaces (by name) a mapping profile.
#[utoipa::path(post, path = "/api/v1/platform/imports/mapping-profiles", operation_id = "platform_import_profile_save", tag = "platform-import",
    request_body = ImportMappingProfileSave, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = MappingProfile), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn profile_save(
    State(st): State<ImportState>,
    auth: Authenticated,
    Json(b): Json<ImportMappingProfileSave>,
) -> ApiResult<Json<MappingProfile>> {
    let (ctx, _) = require_entity(&auth, &st, &b.entity)?;
    require_entitled(&st, ctx, &b.entity).await?;
    let p =
        engine::save_profile(&st, &ctx.scope, &b.entity, &b.name, &b.mapping, actor(ctx)).await?;
    Ok(Json(p))
}

/// Import jobs of the tenant (optionally of one institution / entity), newest first.
#[utoipa::path(get, path = "/api/v1/platform/imports", operation_id = "platform_import_job_list", tag = "platform-import",
    params(JobListQuery), security(("session_cookie" = [])),
    responses((status = 200, body = ImportJobList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn job_list(
    State(st): State<ImportState>,
    auth: Authenticated,
    Query(q): Query<JobListQuery>,
) -> ApiResult<Json<ImportJobList>> {
    let (ctx, approver_only) = reader(&auth)?;
    let mut items = engine::list_jobs(
        &st,
        ctx.scope.tenant_id,
        q.institution_id,
        q.entity.as_deref(),
    )
    .await?;
    if approver_only {
        items.retain(|j| j.requires_approval);
    }
    Ok(Json(ImportJobList { items }))
}

/// Uploads a CSV / XLSX file: parsed rows are stored and a mapping is suggested. Idempotent on
/// the job key (201 new job, 200 the job the key already created; 409 same key, other file).
#[utoipa::path(post, path = "/api/v1/platform/imports", operation_id = "platform_import_job_upload", tag = "platform-import",
    params(("Idempotency-Key" = String, Header)), request_body = ImportUpload, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = ImportJob), (status = 200, description = "Existing job of the job key", body = ImportJob), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn job_upload(
    State(st): State<ImportState>,
    auth: Authenticated,
    headers: HeaderMap,
    Json(b): Json<ImportUpload>,
) -> ApiResult<Response> {
    let (ctx, _) = require_entity(&auth, &st, &b.entity)?;
    require_entitled(&st, ctx, &b.entity).await?;
    require_idempotency_key(&headers)?;
    let (job, created) = engine::upload(
        &st,
        ctx.scope.tenant_id,
        Upload {
            entity: b.entity,
            institution_id: b.institution_id,
            file_name: b.file_name,
            content_base64: b.content_base64,
            job_key: b.job_key,
            mapping_profile_id: b.mapping_profile_id,
            on_existing: b.on_existing.unwrap_or_default(),
            batch_size: b.batch_size,
        },
        actor(ctx),
    )
    .await?;
    let status = if created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, Json(job)).into_response())
}

/// One import job with its counters and reconciliation summary.
#[utoipa::path(get, path = "/api/v1/platform/imports/{id}", operation_id = "platform_import_job_get", tag = "platform-import",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = ImportJob), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn job_get(
    State(st): State<ImportState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<ImportJob>> {
    let (ctx, approver_only) = reader(&auth)?;
    Ok(Json(readable_job(&st, ctx, approver_only, id).await?))
}

/// Rows of a job with their dry-run / commit outcome.
#[utoipa::path(get, path = "/api/v1/platform/imports/{id}/rows", operation_id = "platform_import_job_rows", tag = "platform-import",
    params(("id" = String, Path, format = Uuid), RowsQuery), security(("session_cookie" = [])),
    responses((status = 200, body = ImportJobRowList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn job_rows(
    State(st): State<ImportState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<RowsQuery>,
) -> ApiResult<Json<ImportJobRowList>> {
    let (ctx, approver_only) = reader(&auth)?;
    readable_job(&st, ctx, approver_only, id).await?;
    let limit = q.limit.unwrap_or(500).clamp(1, 5000);
    let items = engine::rows(
        &st,
        ctx.scope.tenant_id,
        id,
        q.errors_only.unwrap_or(false),
        limit,
    )
    .await?;
    Ok(Json(ImportJobRowList { items }))
}

/// Row-level error report (CSV: line, column, code, message + the row's source cells).
#[utoipa::path(get, path = "/api/v1/platform/imports/{id}/errors.csv", operation_id = "platform_import_job_error_report", tag = "platform-import",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, description = "CSV file", body = String, content_type = "text/csv"), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn job_error_report(
    State(st): State<ImportState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::DataImport)?;
    let (name, bytes) = engine::error_report(&st, ctx.scope.tenant_id, id).await?;
    Ok(file_response(&name, FileFormat::Csv, bytes))
}

/// Replaces the column mapping (and on-existing policy); the job returns to `uploaded`.
#[utoipa::path(put, path = "/api/v1/platform/imports/{id}/mapping", operation_id = "platform_import_job_mapping", tag = "platform-import",
    params(("id" = String, Path, format = Uuid)), request_body = ImportMappingUpdate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ImportJob), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn job_mapping(
    State(st): State<ImportState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Json(b): Json<ImportMappingUpdate>,
) -> ApiResult<Json<ImportJob>> {
    let ctx = auth.require(Permission::DataImport)?;
    let job = engine::load_job(&st, ctx.scope.tenant_id, id).await?;
    let (ctx, _) = require_entity(&auth, &st, &job.entity)?;
    require_entitled(&st, ctx, &job.entity).await?;
    let updated = engine::set_mapping(
        &st,
        ctx.scope.tenant_id,
        id,
        b.mapping.clone(),
        b.on_existing,
        actor(ctx),
    )
    .await?;
    if let Some(name) = b
        .save_as_profile
        .as_deref()
        .filter(|n| !n.trim().is_empty())
    {
        engine::save_profile(&st, &ctx.scope, &job.entity, name, &b.mapping, actor(ctx)).await?;
    }
    Ok(Json(updated))
}

/// Dry run: validates every row with the owning module's domain rules (nothing is written).
#[utoipa::path(post, path = "/api/v1/platform/imports/{id}/dry-run", operation_id = "platform_import_job_dry_run", tag = "platform-import",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ImportJob), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn job_dry_run(
    State(st): State<ImportState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<ImportJob>> {
    let ctx = auth.require(Permission::DataImport)?;
    let job = engine::load_job(&st, ctx.scope.tenant_id, id).await?;
    let (ctx, _) = require_entity(&auth, &st, &job.entity)?;
    require_entitled(&st, ctx, &job.entity).await?;
    Ok(Json(
        engine::dry_run(&st, ctx.scope.tenant_id, id, actor(ctx)).await?,
    ))
}

/// Commits a validated job (one transaction per batch), or — for opening balances — requests
/// the second person's approval (`pending_approval`).
#[utoipa::path(post, path = "/api/v1/platform/imports/{id}/commit", operation_id = "platform_import_job_commit", tag = "platform-import",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ImportJob), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn job_commit(
    State(st): State<ImportState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<ImportJob>> {
    let ctx = auth.require(Permission::DataImport)?;
    let job = engine::load_job(&st, ctx.scope.tenant_id, id).await?;
    let (ctx, _) = require_entity(&auth, &st, &job.entity)?;
    require_entitled(&st, ctx, &job.entity).await?;
    Ok(Json(
        engine::request_commit(&st, ctx.scope.tenant_id, id, actor(ctx)).await?,
    ))
}

/// Decides a commit awaiting approval (`platform.import.approve`). The job's maker cannot
/// decide it (separation of duties → 403); approval runs the commit, rejection needs a reason.
#[utoipa::path(post, path = "/api/v1/platform/imports/{id}/decision", operation_id = "platform_import_job_decision", tag = "platform-import",
    params(("id" = String, Path, format = Uuid)), request_body = ImportDecision, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ImportJob), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn job_decision(
    State(st): State<ImportState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Json(b): Json<ImportDecision>,
) -> ApiResult<Json<ImportJob>> {
    let ctx = auth.require(Permission::DataImportApprove)?;
    let tenant = ctx.scope.tenant_id;
    // Separation of duties (DEF-X-002): never the job's maker; checked again by `engine::decide`
    // with the job's own maker columns.
    sutra_persistence::approval::guard_decision(&st.pool, ctx, id, "data import").await?;
    // Part 2 · S7-entitle: approving runs the commit, so the entity's module must be active.
    let job = engine::load_job(&st, tenant, id).await?;
    require_entitled(&st, ctx, &job.entity).await?;
    Ok(Json(
        engine::decide(
            &st,
            tenant,
            id,
            b.approve,
            b.reason.as_deref(),
            &ctx.subject,
        )
        .await?,
    ))
}
