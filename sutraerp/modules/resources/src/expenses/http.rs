//! HTTP layer of staff expense claims (rules in [`super::rules`], receipt reading in
//! [`super::receipt_text`] and [`super::ocr`]). Operation ids `resources_expense_*`, tag
//! `resources-procurement`.
//!
//! * Staff (`people.self`) capture receipts, record expenses and claim them under
//!   `/api/v1/resources/me/…`; the staff record comes from the session subject.
//! * Finance reads claims with `resources.read`, approves or returns them with
//!   `resources.approve` (never the claimant), and records reimbursement with
//!   `resources.procurement.manage`.
//! * Approval books the claim as payable to the staff member (Dr 5510 / Cr 2310); reimbursement
//!   books the payment (Dr 2310 / Cr bank), both through the procurement posting port, keyed
//!   `expense_claim:approve` / `expense_claim:reimburse` on the claim id (idempotent).
//! * OCR only pre-fills: the reading is stored with the receipt and copied onto the expense the
//!   claimant saves, so an approver can see what the receipt said next to what was claimed.
#![allow(clippy::too_many_arguments)]

use super::receipt_text::fields_from_lines;
use super::rules::{
    self as r, Category, CategoryPolicy, LineFacts, OcrFields, PolicyInput, PAYMENT_METHODS,
};
use crate::shared::decimal::parse_decimal;
use crate::shared::guard::{require_institution, require_institution_writable};
use crate::shared::http::actor;
use crate::shared::numbering::{kind, next};
use crate::shared::scope::{institution_scope, tenant_scope};
use crate::shared::time::business_today;
use crate::shared::ResourcesState;
use crate::suppliers::common::{audit_only, check_version, clean, invalid};
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use base64::Engine;
use chrono::{DateTime, Datelike, NaiveDate, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::PgConnection;
use sutra_contracts::finance::ports::{ProcurementPostingKind, ProcurementPostingRequest};
use sutra_contracts::people::ports::StaffSummary;
use sutra_kernel::auth::{AuthContext, Permission};
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

/// Largest receipt file (decoded bytes); photos are downsized by the screen before upload.
pub const MAX_RECEIPT_BYTES: usize = 1_500_000;
const CONTENT_TYPES: [&str; 4] = ["image/jpeg", "image/png", "image/webp", "application/pdf"];
/// Claims are reimbursed in rupees (the ledger's currency).
const CLAIM_CURRENCY: &str = "INR";

fn bad(field: &str, message: &str) -> KernelError {
    invalid(field, "invalid", message)
}


fn ds(d: NaiveDate) -> String {
    d.format("%Y-%m-%d").to_string()
}

// ------------------------------------------------------------------ settings

/// An expense category with its policy.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExpenseCategory {
    /// Stable key (`travel`, `local_conveyance`…; lowercase letters, digits, `_`).
    pub id: String,
    pub name: String,
    /// Most one expense of this category may claim (₹; none when absent).
    #[schema(value_type = Option<String>, example = "1500.00")]
    pub limit: Option<Decimal>,
    /// A receipt is required above this amount (₹; the institution's threshold when absent).
    #[schema(value_type = Option<String>, example = "0.00")]
    pub receipt_above: Option<Decimal>,
}

/// Expense-claim settings of an institution.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExpenseSettings {
    /// A receipt is required for any expense above this amount (₹; default 250).
    #[schema(value_type = String, example = "250.00")]
    pub receipt_required_above: Decimal,
    /// Expenses older than this many days cannot be claimed (0 = no limit; default 90).
    pub max_age_days: i32,
    pub categories: Vec<ExpenseCategory>,
}

fn cat(id: &str, name: &str, receipt_above: Option<i64>) -> ExpenseCategory {
    ExpenseCategory {
        id: id.into(),
        name: name.into(),
        limit: None,
        receipt_above: receipt_above.map(Decimal::from),
    }
}

impl Default for ExpenseSettings {
    fn default() -> Self {
        Self {
            receipt_required_above: Decimal::from(250),
            max_age_days: 90,
            categories: vec![
                cat("travel", "Travel (TA)", Some(0)),
                cat("local_conveyance", "Local conveyance", None),
                cat("lodging", "Lodging", Some(0)),
                cat("meals", "Meals (DA)", None),
                cat("fuel", "Fuel", None),
                cat("conference", "Conference and registration fees", Some(0)),
                cat("books", "Books and stationery", None),
                cat("lab", "Lab consumables", None),
                cat("printing", "Printing and postage", None),
                cat("other", "Other", None),
            ],
        }
    }
}

impl ExpenseSettings {
    fn rule_categories(&self) -> Vec<Category> {
        self.categories
            .iter()
            .map(|c| Category {
                id: c.id.clone(),
                name: c.name.clone(),
            })
            .collect()
    }
    fn policy(&self, id: Option<&str>) -> (bool, Option<CategoryPolicy>) {
        match id.and_then(|id| self.categories.iter().find(|c| c.id == id)) {
            Some(c) => (
                true,
                Some(CategoryPolicy {
                    limit: c.limit,
                    receipt_above: c.receipt_above,
                }),
            ),
            None => (false, None),
        }
    }
}

async fn settings_of(
    conn: &mut PgConnection,
    t: TenantId,
    inst: Uuid,
) -> Result<ExpenseSettings, KernelError> {
    let row: Option<(Decimal, i32, Value)> = sqlx::query_as(
        "SELECT receipt_required_above, max_age_days, categories FROM resources.expense_settings \
         WHERE tenant_id = $1 AND institution_id = $2",
    )
    .bind(t.as_uuid())
    .bind(inst)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(match row {
        None => ExpenseSettings::default(),
        Some((above, age, cats)) => ExpenseSettings {
            receipt_required_above: above,
            max_age_days: age,
            categories: serde_json::from_value(cats).unwrap_or_default(),
        },
    })
}

/// Expense-claim settings (`resources.read`; defaults until saved).
#[utoipa::path(get, path = "/api/v1/resources/institutions/{id}/expense-settings", operation_id = "resources_expense_settings_get", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = ExpenseSettings), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn settings_get(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<ExpenseSettings>> {
    let ctx = auth.require(Permission::ResourcesRead)?;
    let inst = InstitutionId(id);
    require_institution(&st, &institution_scope(ctx, inst), inst).await?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    Ok(Json(settings_of(&mut conn, ctx.scope.tenant_id, id).await?))
}

/// Save expense-claim settings (`resources.procurement.manage`; audited).
#[utoipa::path(put, path = "/api/v1/resources/institutions/{id}/expense-settings", operation_id = "resources_expense_settings_put", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid)), request_body = ExpenseSettings, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ExpenseSettings), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn settings_put(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Json(b): Json<ExpenseSettings>,
) -> ApiResult<Json<ExpenseSettings>> {
    let ctx = auth.require(Permission::ProcurementManage)?;
    let inst = InstitutionId(id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let b = validate_settings(b)?;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    sqlx::query(
        "INSERT INTO resources.expense_settings (tenant_id, institution_id, \
         receipt_required_above, max_age_days, categories, updated_by) VALUES ($1, $2, $3, $4, \
         $5, $6) ON CONFLICT (tenant_id, institution_id) DO UPDATE SET receipt_required_above = \
         EXCLUDED.receipt_required_above, max_age_days = EXCLUDED.max_age_days, categories = \
         EXCLUDED.categories, updated_by = EXCLUDED.updated_by, version = \
         resources.expense_settings.version + 1, updated_at = now()",
    )
    .bind(ctx.scope.tenant_id.as_uuid())
    .bind(id)
    .bind(b.receipt_required_above)
    .bind(b.max_age_days)
    .bind(serde_json::to_value(&b.categories).unwrap_or_default())
    .bind(a.id.as_deref())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    audit_only(
        &mut tx,
        &scope,
        "expense_settings",
        id,
        "resources.expense_settings.update",
        serde_json::to_value(&b).unwrap_or_default(),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(Json(b))
}

fn validate_settings(mut b: ExpenseSettings) -> Result<ExpenseSettings, KernelError> {
    let money_ok = |d: Decimal| d >= Decimal::ZERO && d.normalize().scale() <= 2;
    if !money_ok(b.receipt_required_above) {
        return Err(bad(
            "receiptRequiredAbove",
            "not negative, at most two decimals",
        ));
    }
    if !(0..=730).contains(&b.max_age_days) {
        return Err(invalid("maxAgeDays", "out_of_range", "between 0 and 730"));
    }
    if b.categories.is_empty() || b.categories.len() > 40 {
        return Err(bad("categories", "one to forty categories"));
    }
    let mut seen = std::collections::HashSet::new();
    for c in &mut b.categories {
        c.id = c.id.trim().to_owned();
        c.name = c.name.trim().to_owned();
        let id_ok = !c.id.is_empty()
            && c.id.len() <= 60
            && c.id
                .chars()
                .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_');
        if !id_ok || !seen.insert(c.id.clone()) {
            return Err(bad(
                "categories",
                "unique ids of lowercase letters, digits and _",
            ));
        }
        if c.name.is_empty() || c.name.chars().count() > 80 {
            return Err(bad(
                "categories",
                "every category needs a name of up to 80 characters",
            ));
        }
        if c.limit.is_some_and(|d| !money_ok(d) || d.is_zero())
            || c.receipt_above.is_some_and(|d| !money_ok(d))
        {
            return Err(bad(
                "categories",
                "limits are positive amounts with two decimals",
            ));
        }
    }
    Ok(b)
}

// ------------------------------------------------------------------ who

async fn me(st: &ResourcesState, ctx: &AuthContext) -> Result<StaffSummary, KernelError> {
    st.workforce
        .staff_by_subject(&tenant_scope(ctx), &ctx.subject)
        .await?
        .ok_or(KernelError::NotFound {
            entity: "staff record",
        })
}

/// A claim or receipt is visible to its owner (`people.self`) and to `resources.read`.
async fn may_see(
    st: &ResourcesState,
    auth: &Authenticated,
    staff_id: Uuid,
) -> Result<(), KernelError> {
    let ctx = &auth.0;
    if ctx.has_permission(Permission::ResourcesRead) {
        return Ok(());
    }
    ctx.require(Permission::PeopleSelfService)?;
    let mine = me(st, ctx).await?;
    if mine.id.as_uuid() == staff_id {
        Ok(())
    } else {
        Err(KernelError::NotFound {
            entity: "expense claim",
        })
    }
}

fn staff_label(s: &StaffSummary) -> String {
    format!("{} {}", s.staff_number, s.display_name)
}

// ------------------------------------------------------------------ receipts

/// Upload a receipt (JPEG, PNG, WebP or PDF; at most 1.5 MB decoded).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReceiptUpload {
    pub file_name: String,
    pub content_type: String,
    /// The file, base64.
    pub data_base64: String,
}

/// What reading a receipt pre-filled (every field is a suggestion).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReceiptReading {
    /// `read`, `failed` (the reader errored; type the fields) or `disabled` (no reader set up).
    pub status: String,
    pub engine: String,
    pub error: Option<String>,
    pub fields: Option<OcrFields>,
    /// The merchant's GSTIN when one with a valid checksum was printed.
    pub gstin: Option<String>,
    pub line_count: usize,
}

/// A stored receipt.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExpenseReceiptView {
    pub id: Uuid,
    pub file_name: String,
    pub content_type: String,
    pub byte_size: i32,
    /// `true` when this exact file had been uploaded before (the stored copy is returned).
    pub duplicate: bool,
    pub reading: ReceiptReading,
    pub created_at: DateTime<Utc>,
}

fn reading_of(
    status: &str,
    engine: &str,
    error: Option<String>,
    fields: Option<Value>,
    lines: usize,
) -> ReceiptReading {
    let gstin = fields
        .as_ref()
        .and_then(|v| v.get("gstin"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    ReceiptReading {
        status: status.into(),
        engine: engine.into(),
        error,
        fields: fields.and_then(|v| serde_json::from_value(v).ok()),
        gstin,
        line_count: lines,
    }
}

/// Upload and read a receipt (`people.self`). The file is kept with the caller's staff record;
/// when a reader is configured its text pre-fills the expense form.
#[utoipa::path(post, path = "/api/v1/resources/me/expense-receipts", operation_id = "resources_expense_receipt_upload", tag = "resources-procurement",
    request_body = ReceiptUpload, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = ExpenseReceiptView), (status = 200, body = ExpenseReceiptView, description = "This file had been uploaded before"), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn receipt_upload(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Json(b): Json<ReceiptUpload>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PeopleSelfService)?;
    let mine = me(&st, ctx).await?;
    let inst = mine.institution_id;
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let content_type = b.content_type.trim().to_lowercase();
    if !CONTENT_TYPES.contains(&content_type.as_str()) {
        return Err(bad("contentType", "a JPEG, PNG, WebP or PDF file").into());
    }
    let file_name: String = b.file_name.trim().chars().take(200).collect();
    if file_name.is_empty() {
        return Err(bad("fileName", "required").into());
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b.data_base64.trim())
        .map_err(|_| bad("dataBase64", "not valid base64"))?;
    if bytes.is_empty() || bytes.len() > MAX_RECEIPT_BYTES {
        return Err(invalid("dataBase64", "too_large", "a file of at most 1.5 MB").into());
    }
    let digest: Vec<u8> = Sha256::digest(&bytes).to_vec();
    let t = ctx.scope.tenant_id;
    let staff = mine.id.as_uuid();
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    if let Some(v) = receipt_by_hash(&mut conn, t, staff, &digest).await? {
        return Ok((StatusCode::OK, Json(v)).into_response());
    }
    let settings = settings_of(&mut conn, t, inst.as_uuid()).await?;
    let today = ds(business_today(&st, &scope, inst).await?);
    drop(conn);
    // Read outside any transaction: the sidecar may take seconds.
    let (status, engine, error, lines, fields) = match st.ocr.read(&bytes, &content_type).await {
        Ok(text) => {
            let raw = fields_from_lines(&text.lines, &today);
            let mut norm = serde_json::to_value(r::normalize_ocr_result(
                &raw,
                &settings.rule_categories(),
                CLAIM_CURRENCY,
                &today,
            ))
            .unwrap_or_default();
            if let (Some(o), Some(g)) = (norm.as_object_mut(), raw.get("gstin")) {
                o.insert("gstin".into(), g.clone());
            }
            ("read", text.engine, None, text.lines, Some(norm))
        }
        Err(super::ocr::OcrError::Disabled) => (
            "disabled",
            st.ocr.engine().to_owned(),
            None,
            Vec::new(),
            None,
        ),
        Err(e) => (
            "failed",
            st.ocr.engine().to_owned(),
            Some(e.to_string().chars().take(500).collect()),
            Vec::new(),
            None,
        ),
    };
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let row: Option<(Uuid, DateTime<Utc>)> = sqlx::query_as(
        "INSERT INTO resources.expense_receipt (tenant_id, institution_id, staff_id, file_name, \
         content_type, byte_size, sha256, data, ocr_status, ocr_engine, ocr_error, ocr_lines, \
         ocr_fields, created_by) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, \
         $14) ON CONFLICT (tenant_id, staff_id, sha256) DO NOTHING RETURNING id, created_at",
    )
    .bind(t.as_uuid())
    .bind(inst.as_uuid())
    .bind(staff)
    .bind(&file_name)
    .bind(&content_type)
    .bind(i32::try_from(bytes.len()).unwrap_or(i32::MAX))
    .bind(&digest)
    .bind(&bytes)
    .bind(status)
    .bind(&engine)
    .bind(error.as_deref())
    .bind(serde_json::to_value(&lines).unwrap_or_default())
    .bind(fields.clone())
    .bind(a.id.as_deref())
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let Some((id, created_at)) = row else {
        // A concurrent upload of the same file won.
        tx.rollback().await.map_err(map_db_error)?;
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        let v = receipt_by_hash(&mut conn, t, staff, &digest)
            .await?
            .ok_or(KernelError::NotFound { entity: "receipt" })?;
        return Ok((StatusCode::OK, Json(v)).into_response());
    };
    audit_only(
        &mut tx,
        &scope,
        "expense_receipt",
        id,
        "resources.expense_receipt.upload",
        json!({ "fileName": file_name, "bytes": bytes.len(), "ocr": status, "engine": engine }),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let view = ExpenseReceiptView {
        id,
        file_name,
        content_type,
        byte_size: i32::try_from(bytes.len()).unwrap_or(i32::MAX),
        duplicate: false,
        reading: reading_of(status, &engine, error, fields, lines.len()),
        created_at,
    };
    Ok((StatusCode::CREATED, Json(view)).into_response())
}

type ReceiptMeta = (
    Uuid,
    String,
    String,
    i32,
    String,
    String,
    Option<String>,
    Value,
    Option<Value>,
    DateTime<Utc>,
);

const RECEIPT_META: &str = "id, file_name, content_type, byte_size, ocr_status, ocr_engine, \
    ocr_error, ocr_lines, ocr_fields, created_at";

fn receipt_view(m: ReceiptMeta, duplicate: bool) -> ExpenseReceiptView {
    let (id, file_name, content_type, byte_size, status, engine, error, lines, fields, created_at) =
        m;
    let n = lines.as_array().map_or(0, Vec::len);
    ExpenseReceiptView {
        id,
        file_name,
        content_type,
        byte_size,
        duplicate,
        reading: reading_of(&status, &engine, error, fields, n),
        created_at,
    }
}

async fn receipt_by_hash(
    conn: &mut PgConnection,
    t: TenantId,
    staff: Uuid,
    digest: &[u8],
) -> Result<Option<ExpenseReceiptView>, KernelError> {
    let m: Option<ReceiptMeta> = sqlx::query_as(&format!(
        "SELECT {RECEIPT_META} FROM resources.expense_receipt WHERE tenant_id = $1 AND staff_id = \
         $2 AND sha256 = $3"
    ))
    .bind(t.as_uuid())
    .bind(staff)
    .bind(digest)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(m.map(|m| receipt_view(m, true)))
}

/// The receipt file (its owner, or `resources.read`).
#[utoipa::path(get, path = "/api/v1/resources/expense-receipts/{id}/file", operation_id = "resources_expense_receipt_file", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, description = "The receipt file", content_type = "application/octet-stream"), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn receipt_file(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    // Permission before any data access: finance reads all, staff their own.
    if !auth.0.has_permission(Permission::ResourcesRead) {
        auth.0.require(Permission::PeopleSelfService)?;
    }
    let t = auth.0.scope.tenant_id;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let row: Option<(Uuid, Uuid, String, String, Vec<u8>)> = sqlx::query_as(
        "SELECT staff_id, institution_id, file_name, content_type, data FROM \
         resources.expense_receipt WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(t.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    drop(conn);
    let (staff, inst, name, ctype, data) =
        row.ok_or(KernelError::NotFound { entity: "receipt" })?;
    may_see(&st, &auth, staff).await?;
    if auth.0.has_permission(Permission::ResourcesRead) {
        let i = InstitutionId(inst);
        require_institution(&st, &institution_scope(&auth.0, i), i).await?;
    }
    let safe: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, ctype),
            (
                header::CONTENT_DISPOSITION,
                format!("inline; filename=\"{safe}\""),
            ),
            (header::CACHE_CONTROL, "private, max-age=300".to_owned()),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_owned()),
        ],
        data,
    )
        .into_response())
}

// ------------------------------------------------------------------ expenses

/// Record or change an expense (amounts as decimal strings).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExpenseInput {
    pub spent_on: NaiveDate,
    pub merchant: String,
    #[schema(example = "378.00")]
    pub amount: String,
    /// ISO code (default `INR`). Only rupee expenses can be claimed.
    pub currency: Option<String>,
    pub category_id: Option<String>,
    /// `Card`, `Cash`, `Bank transfer`, `Mobile payment`, `Other`.
    pub payment_method: Option<String>,
    /// GST paid (`CGST + SGST` or `IGST`), ₹.
    pub tax: Option<String>,
    pub gstin: Option<String>,
    pub notes: Option<String>,
    #[serde(default)]
    pub items: Vec<r::OcrItem>,
    pub receipt_id: Option<Uuid>,
    /// Required on update (optimistic concurrency).
    pub version: Option<i64>,
}

/// One expense.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExpenseView {
    pub id: Uuid,
    pub claim_id: Option<Uuid>,
    pub spent_on: NaiveDate,
    pub merchant: String,
    #[schema(value_type = String, example = "378.00")]
    pub amount: Decimal,
    pub currency: String,
    pub category_id: Option<String>,
    pub category_name: Option<String>,
    pub payment_method: Option<String>,
    #[schema(value_type = Option<String>)]
    pub tax: Option<Decimal>,
    pub gstin: Option<String>,
    pub notes: Option<String>,
    pub items: Vec<r::OcrItem>,
    pub receipt_id: Option<Uuid>,
    /// What OCR read from the receipt when the expense was created from it.
    pub ocr: Option<Value>,
    /// Policy findings: `over_limit`, `receipt_missing`, `too_old`, `future_date`,
    /// `uncategorised`, `foreign_currency`. `receipt_missing`, `too_old`, `future_date` and
    /// `foreign_currency` stop a submission.
    pub flags: Vec<String>,
    pub version: i64,
}

const EXPENSE_COLUMNS: &str = "id, staff_id, institution_id, claim_id, spent_on, merchant, \
    amount, currency, category_id, payment_method, tax, gstin, notes, items, receipt_id, ocr, \
    version";

#[derive(Debug, Clone, sqlx::FromRow)]
struct ExpenseRow {
    id: Uuid,
    staff_id: Uuid,
    #[allow(dead_code)] // selected for tenant-scoped reads; not echoed back
    institution_id: Uuid,
    claim_id: Option<Uuid>,
    spent_on: NaiveDate,
    merchant: String,
    amount: Decimal,
    currency: String,
    category_id: Option<String>,
    payment_method: Option<String>,
    tax: Option<Decimal>,
    gstin: Option<String>,
    notes: Option<String>,
    items: Value,
    receipt_id: Option<Uuid>,
    ocr: Option<Value>,
    version: i64,
}

fn flags_of(e: &ExpenseRow, s: &ExpenseSettings, today: &str) -> Vec<String> {
    let (known, policy) = s.policy(e.category_id.as_deref());
    let date = ds(e.spent_on);
    let mut out: Vec<String> = r::policy_flags(
        &PolicyInput {
            amount: e.amount,
            date: &date,
            has_receipt: e.receipt_id.is_some(),
            category: policy.as_ref(),
            category_known: known,
        },
        s.receipt_required_above,
        i64::from(s.max_age_days),
        today,
    )
    .into_iter()
    .map(str::to_owned)
    .collect();
    if e.currency != CLAIM_CURRENCY {
        out.push("foreign_currency".into());
    }
    out
}

fn blocks(flag: &str) -> bool {
    r::blocking(flag) || flag == "foreign_currency"
}

fn expense_view(e: &ExpenseRow, s: &ExpenseSettings, today: &str) -> ExpenseView {
    ExpenseView {
        id: e.id,
        claim_id: e.claim_id,
        spent_on: e.spent_on,
        merchant: e.merchant.clone(),
        amount: e.amount,
        currency: e.currency.clone(),
        category_id: e.category_id.clone(),
        category_name: e
            .category_id
            .as_ref()
            .and_then(|id| s.categories.iter().find(|c| &c.id == id))
            .map(|c| c.name.clone()),
        payment_method: e.payment_method.clone(),
        tax: e.tax,
        gstin: e.gstin.clone(),
        notes: e.notes.clone(),
        items: serde_json::from_value(e.items.clone()).unwrap_or_default(),
        receipt_id: e.receipt_id,
        ocr: e.ocr.clone(),
        flags: flags_of(e, s, today),
        version: e.version,
    }
}

async fn expense_row(
    conn: &mut PgConnection,
    t: TenantId,
    id: Uuid,
) -> Result<ExpenseRow, KernelError> {
    sqlx::query_as::<_, ExpenseRow>(&format!(
        "SELECT {EXPENSE_COLUMNS} FROM resources.expense WHERE id = $1 AND tenant_id = $2"
    ))
    .bind(id)
    .bind(t.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound { entity: "expense" })
}

struct CleanExpense {
    spent_on: NaiveDate,
    merchant: String,
    amount: Decimal,
    currency: String,
    category_id: Option<String>,
    payment_method: Option<String>,
    tax: Option<Decimal>,
    gstin: Option<String>,
    notes: Option<String>,
    items: Value,
    receipt_id: Option<Uuid>,
}

async fn clean_expense(
    conn: &mut PgConnection,
    t: TenantId,
    staff: Uuid,
    s: &ExpenseSettings,
    b: ExpenseInput,
) -> Result<CleanExpense, KernelError> {
    let merchant = b.merchant.trim().to_owned();
    if merchant.is_empty() || merchant.chars().count() > 120 {
        return Err(bad("merchant", "required, up to 120 characters"));
    }
    let amount = parse_decimal("amount", &b.amount)?;
    if amount <= Decimal::ZERO || amount > Decimal::from(10_000_000) {
        return Err(bad("amount", "more than 0 and at most 1,00,00,000"));
    }
    let currency =
        clean(b.currency).map_or_else(|| CLAIM_CURRENCY.to_owned(), |c| c.to_uppercase());
    if !r::is_currency(&currency) {
        return Err(bad("currency", "an ISO currency code such as INR"));
    }
    let category_id = clean(b.category_id);
    if let Some(c) = &category_id {
        if !s.categories.iter().any(|k| &k.id == c) {
            return Err(bad("categoryId", "one of the institution's categories"));
        }
    }
    let payment_method = clean(b.payment_method);
    if let Some(m) = &payment_method {
        if !PAYMENT_METHODS.contains(&m.as_str()) {
            return Err(bad(
                "paymentMethod",
                "Card, Cash, Bank transfer, Mobile payment or Other",
            ));
        }
    }
    let tax = match clean(b.tax) {
        None => None,
        Some(v) => {
            let d = parse_decimal("tax", &v)?;
            if d < Decimal::ZERO || d > amount {
                return Err(bad("tax", "between 0 and the amount"));
            }
            Some(d)
        }
    };
    let gstin = clean(b.gstin).map(|g| g.to_uppercase());
    if let Some(g) = &gstin {
        if !super::receipt_text::gstin_valid(g) {
            return Err(bad(
                "gstin",
                "a 15-character GSTIN with a valid check digit",
            ));
        }
    }
    let notes = clean(b.notes);
    if notes.as_ref().is_some_and(|n| n.chars().count() > 2000) {
        return Err(bad("notes", "up to 2000 characters"));
    }
    if b.items.len() > 25 {
        return Err(bad("items", "at most 25 items"));
    }
    if let Some(rid) = b.receipt_id {
        let owner: Option<Uuid> = sqlx::query_scalar(
            "SELECT staff_id FROM resources.expense_receipt WHERE id = $1 AND tenant_id = $2",
        )
        .bind(rid)
        .bind(t.as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?;
        if owner != Some(staff) {
            return Err(bad("receiptId", "one of your uploaded receipts"));
        }
    }
    Ok(CleanExpense {
        spent_on: b.spent_on,
        merchant,
        amount,
        currency,
        category_id,
        payment_method,
        tax,
        gstin,
        notes,
        items: serde_json::to_value(&b.items).unwrap_or_else(|_| json!([])),
        receipt_id: b.receipt_id,
    })
}

async fn ocr_snapshot(
    conn: &mut PgConnection,
    t: TenantId,
    receipt: Option<Uuid>,
) -> Result<Option<Value>, KernelError> {
    let Some(rid) = receipt else { return Ok(None) };
    let v: Option<Option<Value>> = sqlx::query_scalar(
        "SELECT ocr_fields FROM resources.expense_receipt WHERE id = $1 AND tenant_id = $2",
    )
    .bind(rid)
    .bind(t.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(v.flatten())
}

/// Record an expense (`people.self`; Idempotency-Key). Attach the receipt it came from.
#[utoipa::path(post, path = "/api/v1/resources/me/expenses", operation_id = "resources_expense_create", tag = "resources-procurement",
    request_body = ExpenseInput, params(("Idempotency-Key" = String, Header)), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = ExpenseView), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn expense_create(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    headers: HeaderMap,
    Json(b): Json<ExpenseInput>,
) -> ApiResult<(StatusCode, Json<ExpenseView>)> {
    let ctx = auth.require(Permission::PeopleSelfService)?;
    require_idempotency_key(&headers)?;
    let mine = me(&st, ctx).await?;
    let inst = mine.institution_id;
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let today = ds(business_today(&st, &scope, inst).await?);
    let t = ctx.scope.tenant_id;
    let staff = mine.id.as_uuid();
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let s = settings_of(&mut tx, t, inst.as_uuid()).await?;
    let c = clean_expense(&mut tx, t, staff, &s, b).await?;
    let ocr = ocr_snapshot(&mut tx, t, c.receipt_id).await?;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO resources.expense (tenant_id, institution_id, staff_id, spent_on, merchant, \
         amount, currency, category_id, payment_method, tax, gstin, notes, items, receipt_id, \
         ocr, created_by) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, \
         $15, $16) RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(inst.as_uuid())
    .bind(staff)
    .bind(c.spent_on)
    .bind(&c.merchant)
    .bind(c.amount)
    .bind(&c.currency)
    .bind(c.category_id.as_deref())
    .bind(c.payment_method.as_deref())
    .bind(c.tax)
    .bind(c.gstin.as_deref())
    .bind(c.notes.as_deref())
    .bind(&c.items)
    .bind(c.receipt_id)
    .bind(ocr)
    .bind(a.id.as_deref())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    audit_only(
        &mut tx,
        &scope,
        "expense",
        id,
        "resources.expense.create",
        json!({ "amount": c.amount.to_string(), "merchant": c.merchant, "spentOn": c.spent_on }),
        &a,
    )
    .await?;
    let row = expense_row(&mut tx, t, id).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok((StatusCode::CREATED, Json(expense_view(&row, &s, &today))))
}

async fn my_expense(
    conn: &mut PgConnection,
    t: TenantId,
    staff: Uuid,
    id: Uuid,
) -> Result<ExpenseRow, KernelError> {
    let e = expense_row(conn, t, id).await?;
    if e.staff_id != staff {
        return Err(KernelError::NotFound { entity: "expense" });
    }
    if let Some(cid) = e.claim_id {
        let status: String = sqlx::query_scalar(
            "SELECT status FROM resources.expense_claim WHERE id = $1 FOR UPDATE",
        )
        .bind(cid)
        .fetch_one(&mut *conn)
        .await
        .map_err(map_db_error)?;
        if !r::is_editable(Some(&status)) {
            return Err(KernelError::Conflict(format!(
                "the expense is on a {status} claim; it is locked"
            )));
        }
    }
    Ok(e)
}

/// Change an expense (`people.self`; only while unclaimed or on a draft claim).
#[utoipa::path(put, path = "/api/v1/resources/me/expenses/{id}", operation_id = "resources_expense_update", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid)), request_body = ExpenseInput, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ExpenseView), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn expense_update(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Json(b): Json<ExpenseInput>,
) -> ApiResult<Json<ExpenseView>> {
    let ctx = auth.require(Permission::PeopleSelfService)?;
    let mine = me(&st, ctx).await?;
    let inst = mine.institution_id;
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let today = ds(business_today(&st, &scope, inst).await?);
    let version = b.version.ok_or_else(|| bad("version", "required"))?;
    let t = ctx.scope.tenant_id;
    let staff = mine.id.as_uuid();
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let e = my_expense(&mut tx, t, staff, id).await?;
    check_version(version, e.version)?;
    let s = settings_of(&mut tx, t, inst.as_uuid()).await?;
    let c = clean_expense(&mut tx, t, staff, &s, b).await?;
    let ocr = if c.receipt_id == e.receipt_id {
        e.ocr.clone()
    } else {
        ocr_snapshot(&mut tx, t, c.receipt_id).await?
    };
    sqlx::query(
        "UPDATE resources.expense SET spent_on = $2, merchant = $3, amount = $4, currency = $5, \
         category_id = $6, payment_method = $7, tax = $8, gstin = $9, notes = $10, items = $11, \
         receipt_id = $12, ocr = $13, version = version + 1, updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(c.spent_on)
    .bind(&c.merchant)
    .bind(c.amount)
    .bind(&c.currency)
    .bind(c.category_id.as_deref())
    .bind(c.payment_method.as_deref())
    .bind(c.tax)
    .bind(c.gstin.as_deref())
    .bind(c.notes.as_deref())
    .bind(&c.items)
    .bind(c.receipt_id)
    .bind(ocr)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    audit_only(
        &mut tx,
        &scope,
        "expense",
        id,
        "resources.expense.update",
        json!({ "amount": c.amount.to_string(), "merchant": c.merchant }),
        &a,
    )
    .await?;
    let row = expense_row(&mut tx, t, id).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(Json(expense_view(&row, &s, &today)))
}

/// Delete an expense (`people.self`; only while unclaimed or on a draft claim).
#[utoipa::path(delete, path = "/api/v1/resources/me/expenses/{id}", operation_id = "resources_expense_delete", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 204), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn expense_delete(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let ctx = auth.require(Permission::PeopleSelfService)?;
    let mine = me(&st, ctx).await?;
    let inst = mine.institution_id;
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let e = my_expense(&mut tx, t, mine.id.as_uuid(), id).await?;
    sqlx::query("DELETE FROM resources.expense WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    if let Some(cid) = e.claim_id {
        refresh_total(&mut tx, cid).await?;
    }
    audit_only(
        &mut tx,
        &scope,
        "expense",
        id,
        "resources.expense.delete",
        json!({ "amount": e.amount.to_string(), "merchant": e.merchant }),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(StatusCode::NO_CONTENT)
}

// ------------------------------------------------------------------ claims

/// A claim with its lines.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExpenseClaimView {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub number: String,
    pub title: String,
    pub purpose: Option<String>,
    /// `draft`, `submitted`, `approved`, `reimbursed`.
    pub status: String,
    /// Label of the next step (`Submit claim`, `Approve`, `Mark reimbursed`), none at the end.
    pub next_step: Option<String>,
    pub staff_id: Uuid,
    pub staff_label: String,
    pub currency: String,
    #[schema(value_type = String, example = "1250.00")]
    pub total: Decimal,
    #[schema(value_type = String)]
    pub tax_total: Decimal,
    pub line_count: usize,
    pub with_receipt: usize,
    pub submitted_at: Option<DateTime<Utc>>,
    pub approved_at: Option<DateTime<Utc>>,
    pub reimbursed_at: Option<DateTime<Utc>>,
    pub decided_by: Option<String>,
    pub decision_note: Option<String>,
    /// Why an approver sent the claim back (until it is submitted again).
    pub returned_reason: Option<String>,
    pub approval_entry: Option<String>,
    pub payment_entry: Option<String>,
    pub payment_reference: Option<String>,
    /// Flags on any line; blocking ones must be fixed before submitting.
    pub flags: Vec<String>,
    pub can_submit: bool,
    pub lines: Vec<ExpenseView>,
    pub version: i64,
    pub updated_at: DateTime<Utc>,
}

/// A claim in a list (no lines).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExpenseClaimSummary {
    pub id: Uuid,
    pub number: String,
    pub title: String,
    pub status: String,
    pub staff_id: Uuid,
    pub staff_label: String,
    #[schema(value_type = String)]
    pub total: Decimal,
    pub line_count: i64,
    pub submitted_at: Option<DateTime<Utc>>,
    pub returned_reason: Option<String>,
    pub version: i64,
    pub updated_at: DateTime<Utc>,
}

const CLAIM_COLUMNS: &str = "c.id, c.institution_id, c.staff_id, c.number, c.title, c.purpose, \
    c.status, c.currency, c.total, c.submitted_at, c.approved_at, c.reimbursed_at, c.decided_by, \
    c.decision_note, c.returned_reason, c.approval_entry, c.payment_entry, c.payment_reference, \
    c.created_by, c.version, c.updated_at";

#[derive(Debug, Clone, sqlx::FromRow)]
struct ClaimRow {
    id: Uuid,
    institution_id: Uuid,
    staff_id: Uuid,
    number: String,
    title: String,
    purpose: Option<String>,
    status: String,
    currency: String,
    total: Decimal,
    submitted_at: Option<DateTime<Utc>>,
    approved_at: Option<DateTime<Utc>>,
    reimbursed_at: Option<DateTime<Utc>>,
    decided_by: Option<String>,
    decision_note: Option<String>,
    returned_reason: Option<String>,
    approval_entry: Option<String>,
    payment_entry: Option<String>,
    payment_reference: Option<String>,
    created_by: Option<String>,
    version: i64,
    updated_at: DateTime<Utc>,
}

fn next_step(status: &str) -> Option<String> {
    match status {
        "draft" => Some("Submit claim".into()),
        "submitted" => Some("Approve".into()),
        "approved" => Some("Mark reimbursed".into()),
        _ => None,
    }
}

async fn claim_row(
    conn: &mut PgConnection,
    t: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<ClaimRow, KernelError> {
    sqlx::query_as::<_, ClaimRow>(&format!(
        "SELECT {CLAIM_COLUMNS} FROM resources.expense_claim c WHERE c.id = $1 AND c.tenant_id = \
         $2{}",
        if lock { " FOR UPDATE" } else { "" }
    ))
    .bind(id)
    .bind(t.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "expense claim",
    })
}

async fn claim_lines(
    conn: &mut PgConnection,
    t: TenantId,
    claim: Uuid,
) -> Result<Vec<ExpenseRow>, KernelError> {
    sqlx::query_as::<_, ExpenseRow>(&format!(
        "SELECT {EXPENSE_COLUMNS} FROM resources.expense WHERE tenant_id = $1 AND claim_id = $2 \
         ORDER BY spent_on, created_at, id"
    ))
    .bind(t.as_uuid())
    .bind(claim)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)
}

async fn refresh_total(conn: &mut PgConnection, claim: Uuid) -> Result<(), KernelError> {
    sqlx::query(
        "UPDATE resources.expense_claim c SET total = COALESCE((SELECT sum(e.amount) FROM \
         resources.expense e WHERE e.claim_id = c.id AND e.currency = c.currency), 0), \
         updated_at = now() WHERE c.id = $1",
    )
    .bind(claim)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(())
}

async fn claim_view(
    st: &ResourcesState,
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    t: TenantId,
    c: ClaimRow,
) -> Result<ExpenseClaimView, KernelError> {
    let s = settings_of(conn, t, c.institution_id).await?;
    let today = ds(business_today(st, scope, InstitutionId(c.institution_id)).await?);
    let rows = claim_lines(conn, t, c.id).await?;
    let lines: Vec<ExpenseView> = rows.iter().map(|e| expense_view(e, &s, &today)).collect();
    let totals = r::claim_totals(
        &rows
            .iter()
            .map(|e| LineFacts {
                amount: e.amount,
                currency: e.currency.clone(),
                has_receipt: e.receipt_id.is_some(),
                reconciled: false,
            })
            .collect::<Vec<_>>(),
    );
    let mut flags: Vec<String> = lines.iter().flat_map(|l| l.flags.clone()).collect();
    flags.sort();
    flags.dedup();
    let tax_total = rows.iter().filter_map(|e| e.tax).sum();
    let staff = st
        .workforce
        .staff(
            scope,
            sutra_contracts::people::ids::StaffMemberId(c.staff_id),
        )
        .await?
        .map_or_else(|| "Unknown staff".to_owned(), |s| staff_label(&s));
    let total = if c.status == "draft" {
        totals
            .by_currency
            .iter()
            .find(|(cur, _)| cur == &c.currency)
            .map_or(Decimal::ZERO, |(_, v)| v.round_dp(2))
    } else {
        c.total
    };
    let can_submit = c.status == "draft" && !lines.is_empty() && !flags.iter().any(|f| blocks(f));
    Ok(ExpenseClaimView {
        id: c.id,
        institution_id: c.institution_id,
        number: c.number,
        title: c.title,
        purpose: c.purpose,
        next_step: next_step(&c.status),
        status: c.status,
        staff_id: c.staff_id,
        staff_label: staff,
        currency: c.currency,
        total,
        tax_total,
        line_count: totals.count,
        with_receipt: totals.with_receipt,
        submitted_at: c.submitted_at,
        approved_at: c.approved_at,
        reimbursed_at: c.reimbursed_at,
        decided_by: c.decided_by,
        decision_note: c.decision_note,
        returned_reason: c.returned_reason,
        approval_entry: c.approval_entry,
        payment_entry: c.payment_entry,
        payment_reference: c.payment_reference,
        flags,
        can_submit,
        lines,
        version: c.version,
        updated_at: c.updated_at,
    })
}

/// Create a claim from unclaimed expenses.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ClaimCreate {
    /// Default "<Month YYYY> expenses".
    pub title: Option<String>,
    pub purpose: Option<String>,
    #[serde(default)]
    pub expense_ids: Vec<Uuid>,
}

/// Replace the lines of a draft claim.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ClaimLines {
    pub title: Option<String>,
    pub purpose: Option<String>,
    pub expense_ids: Vec<Uuid>,
    pub version: i64,
}

/// A step of the claim's lifecycle (optimistic concurrency by `version`).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ClaimStep {
    pub version: i64,
}

/// An approver's decision.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ClaimDecision {
    /// `approve`, or `return` to send the claim back to the claimant (reason required).
    pub decision: String,
    pub reason: Option<String>,
    pub version: i64,
}

/// Recording the reimbursement.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ClaimReimburse {
    /// Payment date (default today).
    pub paid_on: Option<NaiveDate>,
    /// UTR, cheque or payroll reference.
    pub reference: Option<String>,
    pub version: i64,
}

fn month_title(d: NaiveDate) -> String {
    const M: [&str; 12] = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];
    format!("{} {} expenses", M[d.month0() as usize], d.year())
}

/// Puts `ids` (the caller's, unclaimed or already on this claim) on the claim and frees the rest.
async fn set_lines(
    conn: &mut PgConnection,
    t: TenantId,
    staff: Uuid,
    claim: Uuid,
    ids: &[Uuid],
) -> Result<(), KernelError> {
    if ids.len() > 200 {
        return Err(bad("expenseIds", "at most 200 expenses on a claim"));
    }
    let ok: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM resources.expense WHERE tenant_id = $1 AND staff_id = $2 AND id = \
         ANY($3) AND (claim_id IS NULL OR claim_id = $4)",
    )
    .bind(t.as_uuid())
    .bind(staff)
    .bind(ids)
    .bind(claim)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let distinct: std::collections::HashSet<_> = ids.iter().collect();
    if ok != distinct.len() as i64 {
        return Err(bad(
            "expenseIds",
            "your own expenses that are not on another claim",
        ));
    }
    sqlx::query(
        "UPDATE resources.expense SET claim_id = NULL, version = version + 1, updated_at = now() \
         WHERE tenant_id = $1 AND claim_id = $2 AND NOT (id = ANY($3))",
    )
    .bind(t.as_uuid())
    .bind(claim)
    .bind(ids)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    sqlx::query(
        "UPDATE resources.expense SET claim_id = $2, version = version + 1, updated_at = now() \
         WHERE tenant_id = $1 AND id = ANY($3) AND claim_id IS NULL",
    )
    .bind(t.as_uuid())
    .bind(claim)
    .bind(ids)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    refresh_total(conn, claim).await
}

/// Start a claim (`people.self`; Idempotency-Key).
#[utoipa::path(post, path = "/api/v1/resources/me/expense-claims", operation_id = "resources_expense_claim_create", tag = "resources-procurement",
    request_body = ClaimCreate, params(("Idempotency-Key" = String, Header)), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = ExpenseClaimView), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn claim_create(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    headers: HeaderMap,
    Json(b): Json<ClaimCreate>,
) -> ApiResult<(StatusCode, Json<ExpenseClaimView>)> {
    let ctx = auth.require(Permission::PeopleSelfService)?;
    require_idempotency_key(&headers)?;
    let mine = me(&st, ctx).await?;
    let inst = mine.institution_id;
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let today = business_today(&st, &scope, inst).await?;
    let title = clean(b.title).unwrap_or_else(|| month_title(today));
    if title.chars().count() > 200 {
        return Err(bad("title", "up to 200 characters").into());
    }
    let purpose = clean(b.purpose);
    if purpose.as_ref().is_some_and(|p| p.chars().count() > 2000) {
        return Err(bad("purpose", "up to 2000 characters").into());
    }
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let number = next(&mut tx, t, inst, kind::EXPENSE_CLAIM).await?;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO resources.expense_claim (tenant_id, institution_id, staff_id, number, \
         title, purpose, currency, created_by) VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(inst.as_uuid())
    .bind(mine.id.as_uuid())
    .bind(&number)
    .bind(&title)
    .bind(purpose.as_deref())
    .bind(CLAIM_CURRENCY)
    .bind(a.id.as_deref())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    set_lines(&mut tx, t, mine.id.as_uuid(), id, &b.expense_ids).await?;
    audit_only(
        &mut tx,
        &scope,
        "expense_claim",
        id,
        "resources.expense_claim.create",
        json!({ "number": number, "lines": b.expense_ids.len() }),
        &a,
    )
    .await?;
    let c = claim_row(&mut tx, t, id, false).await?;
    let view = claim_view(&st, &mut tx, &scope, t, c).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok((StatusCode::CREATED, Json(view)))
}

/// One claim with its lines (its claimant, or `resources.read`).
#[utoipa::path(get, path = "/api/v1/resources/expense-claims/{id}", operation_id = "resources_expense_claim_get", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = ExpenseClaimView), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn claim_get(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<ExpenseClaimView>> {
    // Permission before any data access: finance reads all, staff their own.
    if !auth.0.has_permission(Permission::ResourcesRead) {
        auth.0.require(Permission::PeopleSelfService)?;
    }
    let t = auth.0.scope.tenant_id;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let c = claim_row(&mut conn, t, id, false).await?;
    may_see(&st, &auth, c.staff_id).await?;
    let inst = InstitutionId(c.institution_id);
    let scope = institution_scope(&auth.0, inst);
    require_institution(&st, &scope, inst).await?;
    Ok(Json(claim_view(&st, &mut conn, &scope, t, c).await?))
}

/// Shared prologue of the claimant's claim commands: the claim (locked), mine, at `version`.
async fn my_claim(
    st: &ResourcesState,
    ctx: &AuthContext,
    conn: &mut PgConnection,
    id: Uuid,
    version: i64,
) -> Result<(ClaimRow, StaffSummary, ExecutionScope), KernelError> {
    let mine = me(st, ctx).await?;
    let c = claim_row(conn, ctx.scope.tenant_id, id, true).await?;
    if c.staff_id != mine.id.as_uuid() {
        return Err(KernelError::NotFound {
            entity: "expense claim",
        });
    }
    let inst = InstitutionId(c.institution_id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(st, &scope, inst).await?;
    check_version(version, c.version)?;
    Ok((c, mine, scope))
}

fn illegal(c: &ClaimRow, to: &str) -> KernelError {
    KernelError::Conflict(format!(
        "expense claim {} is {}; it cannot become {to}",
        c.number, c.status
    ))
}

async fn bump(conn: &mut PgConnection, id: Uuid, set: &str) -> Result<(), KernelError> {
    sqlx::query(&format!(
        "UPDATE resources.expense_claim SET {set}, version = version + 1, updated_at = now() \
         WHERE id = $1"
    ))
    .bind(id)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(())
}

/// Change a draft claim's title, purpose and lines (`people.self`).
#[utoipa::path(put, path = "/api/v1/resources/me/expense-claims/{id}", operation_id = "resources_expense_claim_update", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid)), request_body = ClaimLines, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ExpenseClaimView), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn claim_update(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Json(b): Json<ClaimLines>,
) -> ApiResult<Json<ExpenseClaimView>> {
    let ctx = auth.require(Permission::PeopleSelfService)?;
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let (c, mine, scope) = my_claim(&st, ctx, &mut tx, id, b.version).await?;
    if !r::is_editable(Some(&c.status)) {
        return Err(KernelError::Conflict(format!(
            "expense claim {} is {}; only a draft changes",
            c.number, c.status
        ))
        .into());
    }
    let title = clean(b.title).unwrap_or_else(|| c.title.clone());
    if title.chars().count() > 200 {
        return Err(bad("title", "up to 200 characters").into());
    }
    let purpose = clean(b.purpose);
    if purpose.as_ref().is_some_and(|p| p.chars().count() > 2000) {
        return Err(bad("purpose", "up to 2000 characters").into());
    }
    set_lines(&mut tx, t, mine.id.as_uuid(), id, &b.expense_ids).await?;
    sqlx::query(
        "UPDATE resources.expense_claim SET title = $2, purpose = $3, version = version + 1, \
         updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(&title)
    .bind(purpose.as_deref())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    audit_only(
        &mut tx,
        &scope,
        "expense_claim",
        id,
        "resources.expense_claim.update",
        json!({ "lines": b.expense_ids.len() }),
        &a,
    )
    .await?;
    let c = claim_row(&mut tx, t, id, false).await?;
    let view = claim_view(&st, &mut tx, &scope, t, c).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(Json(view))
}

/// Delete a draft claim (`people.self`); its expenses become unclaimed again.
#[utoipa::path(delete, path = "/api/v1/resources/me/expense-claims/{id}", operation_id = "resources_expense_claim_delete", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid), ClaimVersion), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 204), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn claim_delete(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<ClaimVersion>,
) -> ApiResult<StatusCode> {
    let ctx = auth.require(Permission::PeopleSelfService)?;
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let (c, _, scope) = my_claim(&st, ctx, &mut tx, id, q.version).await?;
    if c.status != "draft" {
        return Err(illegal(&c, "deleted").into());
    }
    sqlx::query(
        "UPDATE resources.expense SET claim_id = NULL, version = version + 1, updated_at = now() \
         WHERE tenant_id = $1 AND claim_id = $2",
    )
    .bind(t.as_uuid())
    .bind(id)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    sqlx::query("DELETE FROM resources.expense_claim WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    audit_only(
        &mut tx,
        &scope,
        "expense_claim",
        id,
        "resources.expense_claim.delete",
        json!({ "number": c.number }),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(StatusCode::NO_CONTENT)
}

/// `?version=` of a delete.
#[derive(Debug, Clone, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ClaimVersion {
    pub version: i64,
}

/// Submit a draft claim (`people.self`). Every line must be in rupees, carry a receipt where
/// the policy needs one and be recent enough; the total is frozen.
#[utoipa::path(post, path = "/api/v1/resources/me/expense-claims/{id}/submit", operation_id = "resources_expense_claim_submit", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid)), request_body = ClaimStep, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ExpenseClaimView), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn claim_submit(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Json(b): Json<ClaimStep>,
) -> ApiResult<Json<ExpenseClaimView>> {
    let ctx = auth.require(Permission::PeopleSelfService)?;
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let (c, _, scope) = my_claim(&st, ctx, &mut tx, id, b.version).await?;
    if !r::can_transition(&c.status, "submitted") {
        return Err(illegal(&c, "submitted").into());
    }
    let view = claim_view(&st, &mut tx, &scope, t, c.clone()).await?;
    if view.lines.is_empty() {
        return Err(
            KernelError::Conflict("add at least one expense before submitting".into()).into(),
        );
    }
    let blocking: Vec<String> = view
        .lines
        .iter()
        .flat_map(|l| {
            l.flags
                .iter()
                .filter(|f| blocks(f))
                .map(move |f| format!("{} ({})", l.merchant, f))
        })
        .collect();
    if !blocking.is_empty() {
        return Err(KernelError::Conflict(format!(
            "fix these lines before submitting: {}",
            blocking.join(", ")
        ))
        .into());
    }
    refresh_total(&mut tx, id).await?;
    bump(
        &mut tx,
        id,
        "status = 'submitted', submitted_at = now(), returned_reason = NULL",
    )
    .await?;
    audit_only(
        &mut tx,
        &scope,
        "expense_claim",
        id,
        "resources.expense_claim.submit",
        json!({ "number": c.number, "total": view.total.to_string() }),
        &a,
    )
    .await?;
    let c = claim_row(&mut tx, t, id, false).await?;
    let view = claim_view(&st, &mut tx, &scope, t, c).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(Json(view))
}

/// Withdraw a submitted claim back to draft (`people.self`).
#[utoipa::path(post, path = "/api/v1/resources/me/expense-claims/{id}/withdraw", operation_id = "resources_expense_claim_withdraw", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid)), request_body = ClaimStep, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ExpenseClaimView), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn claim_withdraw(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Json(b): Json<ClaimStep>,
) -> ApiResult<Json<ExpenseClaimView>> {
    let ctx = auth.require(Permission::PeopleSelfService)?;
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let (c, _, scope) = my_claim(&st, ctx, &mut tx, id, b.version).await?;
    if !(c.status == "submitted" && r::can_transition(&c.status, "draft")) {
        return Err(illegal(&c, "draft").into());
    }
    bump(&mut tx, id, "status = 'draft', submitted_at = NULL").await?;
    audit_only(
        &mut tx,
        &scope,
        "expense_claim",
        id,
        "resources.expense_claim.withdraw",
        json!({ "number": c.number }),
        &a,
    )
    .await?;
    let c = claim_row(&mut tx, t, id, false).await?;
    let view = claim_view(&st, &mut tx, &scope, t, c).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(Json(view))
}

/// Finance's prologue: the claim (locked) at `version`, its institution writable.
async fn office_claim(
    st: &ResourcesState,
    ctx: &AuthContext,
    conn: &mut PgConnection,
    id: Uuid,
    version: i64,
) -> Result<(ClaimRow, ExecutionScope), KernelError> {
    let c = claim_row(conn, ctx.scope.tenant_id, id, true).await?;
    let inst = InstitutionId(c.institution_id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(st, &scope, inst).await?;
    check_version(version, c.version)?;
    Ok((c, scope))
}

async fn claimant(
    st: &ResourcesState,
    scope: &ExecutionScope,
    c: &ClaimRow,
) -> Result<StaffSummary, KernelError> {
    st.workforce
        .staff(
            scope,
            sutra_contracts::people::ids::StaffMemberId(c.staff_id),
        )
        .await?
        .ok_or(KernelError::NotFound {
            entity: "staff record",
        })
}

/// Approve a submitted claim or send it back (`resources.approve`). The claimant never decides
/// their own claim. Approval books the claim payable to the staff member.
#[utoipa::path(post, path = "/api/v1/resources/expense-claims/{id}/decision", operation_id = "resources_expense_claim_decide", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid)), request_body = ClaimDecision, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ExpenseClaimView), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn claim_decide(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Json(b): Json<ClaimDecision>,
) -> ApiResult<Json<ExpenseClaimView>> {
    let ctx = auth.require(Permission::ResourcesApprove)?;
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let reason = clean(b.reason);
    if reason.as_ref().is_some_and(|r| r.chars().count() > 2000) {
        return Err(bad("reason", "up to 2000 characters").into());
    }
    let approve = match b.decision.as_str() {
        "approve" => true,
        "return" | "reject" => false,
        _ => return Err(bad("decision", "approve or return").into()),
    };
    if !approve && reason.is_none() {
        return Err(invalid("reason", "required", "say why the claim goes back").into());
    }
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let (c, scope) = office_claim(&st, ctx, &mut tx, id, b.version).await?;
    let to = if approve { "approved" } else { "draft" };
    if c.status != "submitted" || !r::can_transition(&c.status, to) {
        return Err(illegal(&c, to).into());
    }
    let person = claimant(&st, &scope, &c).await?;
    // Separation of duties: neither the claimant nor whoever created the claim decides it.
    sutra_kernel::approval::ensure_independent_decider(
        a.id.as_deref(),
        [c.created_by.as_deref(), person.subject_ref.as_deref()]
            .into_iter()
            .flatten(),
        "expense claim",
    )?;
    if approve {
        let today = business_today(&st, &scope, InstitutionId(c.institution_id)).await?;
        let receipt = st
            .postings
            .post_procurement(
                &scope,
                ProcurementPostingRequest {
                    institution_id: InstitutionId(c.institution_id),
                    kind: ProcurementPostingKind::VendorInvoice,
                    source_id: id,
                    posting_key: "expense_claim:approve".into(),
                    posting_date: today,
                    description: format!("Staff expense claim {} {}", c.number, c.title),
                    supplier_label: staff_label(&person),
                    inventory_amount: Decimal::ZERO,
                    expense_amount: c.total,
                    payment_amount: Decimal::ZERO,
                    actor: a.id.clone(),
                },
            )
            .await?;
        sqlx::query(
            "UPDATE resources.expense_claim SET status = 'approved', approved_at = now(), \
             decided_by = $2, decision_note = $3, approval_entry = $4, version = version + 1, \
             updated_at = now() WHERE id = $1",
        )
        .bind(id)
        .bind(a.id.as_deref())
        .bind(reason.as_deref())
        .bind(&receipt.entry_number)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    } else {
        sqlx::query(
            "UPDATE resources.expense_claim SET status = 'draft', submitted_at = NULL, \
             returned_reason = $2, decided_by = $3, version = version + 1, updated_at = now() \
             WHERE id = $1",
        )
        .bind(id)
        .bind(reason.as_deref())
        .bind(a.id.as_deref())
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    }
    audit_only(
        &mut tx,
        &scope,
        "expense_claim",
        id,
        if approve {
            "resources.expense_claim.approve"
        } else {
            "resources.expense_claim.return"
        },
        json!({ "number": c.number, "total": c.total.to_string(), "reason": reason }),
        &a,
    )
    .await?;
    let c = claim_row(&mut tx, t, id, false).await?;
    let view = claim_view(&st, &mut tx, &scope, t, c).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(Json(view))
}

/// Record that an approved claim was paid to the staff member (`resources.procurement.manage`;
/// books the payment).
#[utoipa::path(post, path = "/api/v1/resources/expense-claims/{id}/reimburse", operation_id = "resources_expense_claim_reimburse", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid)), request_body = ClaimReimburse, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ExpenseClaimView), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn claim_reimburse(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Json(b): Json<ClaimReimburse>,
) -> ApiResult<Json<ExpenseClaimView>> {
    let ctx = auth.require(Permission::ProcurementManage)?;
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let reference = clean(b.reference);
    if reference.as_ref().is_some_and(|r| r.chars().count() > 120) {
        return Err(bad("reference", "up to 120 characters").into());
    }
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let (c, scope) = office_claim(&st, ctx, &mut tx, id, b.version).await?;
    if !r::can_transition(&c.status, "reimbursed") {
        return Err(illegal(&c, "reimbursed").into());
    }
    let today = business_today(&st, &scope, InstitutionId(c.institution_id)).await?;
    let paid_on = b.paid_on.unwrap_or(today);
    if paid_on > today {
        return Err(bad("paidOn", "today or earlier").into());
    }
    let person = claimant(&st, &scope, &c).await?;
    let receipt = st
        .postings
        .post_procurement(
            &scope,
            ProcurementPostingRequest {
                institution_id: InstitutionId(c.institution_id),
                kind: ProcurementPostingKind::VendorPayment,
                source_id: id,
                posting_key: "expense_claim:reimburse".into(),
                posting_date: paid_on,
                description: format!(
                    "Reimbursement of expense claim {}{}",
                    c.number,
                    reference
                        .as_ref()
                        .map(|r| format!(" ({r})"))
                        .unwrap_or_default()
                ),
                supplier_label: staff_label(&person),
                inventory_amount: Decimal::ZERO,
                expense_amount: Decimal::ZERO,
                payment_amount: c.total,
                actor: a.id.clone(),
            },
        )
        .await?;
    sqlx::query(
        "UPDATE resources.expense_claim SET status = 'reimbursed', reimbursed_at = now(), \
         payment_entry = $2, payment_reference = $3, version = version + 1, updated_at = now() \
         WHERE id = $1",
    )
    .bind(id)
    .bind(&receipt.entry_number)
    .bind(reference.as_deref())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    audit_only(
        &mut tx,
        &scope,
        "expense_claim",
        id,
        "resources.expense_claim.reimburse",
        json!({ "number": c.number, "total": c.total.to_string(), "paidOn": paid_on, "reference": reference }),
        &a,
    )
    .await?;
    let c = claim_row(&mut tx, t, id, false).await?;
    let view = claim_view(&st, &mut tx, &scope, t, c).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(Json(view))
}

// ------------------------------------------------------------------ lists

/// My expenses and claims, with the policy I claim under.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MyExpenses {
    pub staff_label: String,
    pub institution_id: Uuid,
    pub settings: ExpenseSettings,
    /// Whether uploaded receipts are read (a reader is configured).
    pub ocr_enabled: bool,
    pub ocr_engine: String,
    /// Expenses on no claim yet.
    pub unclaimed: Vec<ExpenseView>,
    #[schema(value_type = String)]
    pub unclaimed_total: Decimal,
    pub claims: Vec<ExpenseClaimSummary>,
}

/// Row of the claims list query: id, number, title, status, staff, total, lines, submitted,
/// returned reason, version, updated.
type ClaimSummaryRow = (
    Uuid,
    String,
    String,
    String,
    Uuid,
    Decimal,
    i64,
    Option<DateTime<Utc>>,
    Option<String>,
    i64,
    DateTime<Utc>,
);
async fn summaries(
    st: &ResourcesState,
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    t: TenantId,
    filter: &str,
    bind: Uuid,
    status: Option<&str>,
) -> Result<Vec<ExpenseClaimSummary>, KernelError> {
    let rows: Vec<ClaimSummaryRow> = sqlx::query_as(&format!(
        "SELECT c.id, c.number, c.title, c.status, c.staff_id, CASE WHEN c.status = 'draft' \
             THEN COALESCE((SELECT sum(e.amount) FROM resources.expense e WHERE e.claim_id = c.id \
             AND e.currency = c.currency), 0) ELSE c.total END, (SELECT count(*) FROM \
             resources.expense e WHERE e.claim_id = c.id), c.submitted_at, c.returned_reason, \
             c.version, c.updated_at FROM resources.expense_claim c WHERE c.tenant_id = $1 AND \
             c.{filter} = $2 AND ($3::text IS NULL OR c.status = $3) ORDER BY c.updated_at DESC, \
             c.id LIMIT 500"
    ))
    .bind(t.as_uuid())
    .bind(bind)
    .bind(status)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let mut labels: std::collections::HashMap<Uuid, String> = std::collections::HashMap::new();
    let mut out = Vec::with_capacity(rows.len());
    for (
        id,
        number,
        title,
        status,
        staff_id,
        total,
        n,
        submitted_at,
        returned_reason,
        version,
        updated_at,
    ) in rows
    {
        if let std::collections::hash_map::Entry::Vacant(e) = labels.entry(staff_id) {
            let l = st
                .workforce
                .staff(scope, sutra_contracts::people::ids::StaffMemberId(staff_id))
                .await?
                .map_or_else(|| "Unknown staff".to_owned(), |s| staff_label(&s));
            e.insert(l);
        }
        out.push(ExpenseClaimSummary {
            id,
            number,
            title,
            status,
            staff_id,
            staff_label: labels.get(&staff_id).cloned().unwrap_or_default(),
            total,
            line_count: n,
            submitted_at,
            returned_reason,
            version,
            updated_at,
        });
    }
    Ok(out)
}

/// My expenses and claims (`people.self`).
#[utoipa::path(get, path = "/api/v1/resources/me/expenses", operation_id = "resources_expense_me", tag = "resources-procurement",
    security(("session_cookie" = [])),
    responses((status = 200, body = MyExpenses), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn me_expenses(
    State(st): State<ResourcesState>,
    auth: Authenticated,
) -> ApiResult<Json<MyExpenses>> {
    let ctx = auth.require(Permission::PeopleSelfService)?;
    let mine = me(&st, ctx).await?;
    let inst = mine.institution_id;
    let scope = institution_scope(ctx, inst);
    require_institution(&st, &scope, inst).await?;
    let t = ctx.scope.tenant_id;
    let today = ds(business_today(&st, &scope, inst).await?);
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let settings = settings_of(&mut conn, t, inst.as_uuid()).await?;
    let rows = sqlx::query_as::<_, ExpenseRow>(&format!(
        "SELECT {EXPENSE_COLUMNS} FROM resources.expense WHERE tenant_id = $1 AND staff_id = $2 \
         AND claim_id IS NULL ORDER BY spent_on DESC, created_at DESC LIMIT 500"
    ))
    .bind(t.as_uuid())
    .bind(mine.id.as_uuid())
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let unclaimed: Vec<ExpenseView> = rows
        .iter()
        .map(|e| expense_view(e, &settings, &today))
        .collect();
    let unclaimed_total = rows
        .iter()
        .filter(|e| e.currency == CLAIM_CURRENCY)
        .map(|e| e.amount)
        .sum();
    let claims = summaries(
        &st,
        &mut conn,
        &scope,
        t,
        "staff_id",
        mine.id.as_uuid(),
        None,
    )
    .await?;
    let engine = st.ocr.engine();
    Ok(Json(MyExpenses {
        staff_label: staff_label(&mine),
        institution_id: inst.as_uuid(),
        settings,
        ocr_enabled: engine != "disabled",
        ocr_engine: engine.to_owned(),
        unclaimed,
        unclaimed_total,
        claims,
    }))
}

/// `?status=` of the claims list.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ClaimFilter {
    /// `draft`, `submitted`, `approved`, `reimbursed`.
    pub status: Option<String>,
}

/// Claims of an institution (`resources.read`), newest first.
#[utoipa::path(get, path = "/api/v1/resources/institutions/{id}/expense-claims", operation_id = "resources_expense_claim_list", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid), ClaimFilter), security(("session_cookie" = [])),
    responses((status = 200, body = Vec<ExpenseClaimSummary>), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn claim_list(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<ClaimFilter>,
) -> ApiResult<Json<Vec<ExpenseClaimSummary>>> {
    let ctx = auth.require(Permission::ResourcesRead)?;
    let inst = InstitutionId(id);
    let scope = institution_scope(ctx, inst);
    require_institution(&st, &scope, inst).await?;
    let status = clean(q.status);
    if let Some(s) = &status {
        if !r::CLAIM_STATUSES.contains(&s.as_str()) {
            return Err(bad("status", "draft, submitted, approved or reimbursed").into());
        }
    }
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let list = summaries(
        &st,
        &mut conn,
        &scope,
        ctx.scope.tenant_id,
        "institution_id",
        id,
        status.as_deref(),
    )
    .await?;
    Ok(Json(list))
}

/// Spend in one category.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CategorySpend {
    pub category_id: Option<String>,
    pub name: String,
    #[schema(value_type = String)]
    pub amount: Decimal,
}

/// Approved spend of one month.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MonthSpend {
    /// `YYYY-MM`.
    pub month: String,
    #[schema(value_type = String)]
    pub amount: Decimal,
}

/// The expense dashboard of an institution.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExpenseDashboard {
    pub awaiting_approval: i64,
    #[schema(value_type = String)]
    pub awaiting_approval_amount: Decimal,
    pub awaiting_reimbursement: i64,
    #[schema(value_type = String)]
    pub awaiting_reimbursement_amount: Decimal,
    /// Approved or reimbursed this financial year (April–March), by category.
    pub by_category: Vec<CategorySpend>,
    /// Approved spend of the last six months, by approval month.
    pub trend: Vec<MonthSpend>,
    /// Lines of claims awaiting approval whose amount differs from what OCR read off the receipt.
    pub ocr_mismatches: i64,
}

/// Expense dashboard (`resources.read`).
#[utoipa::path(get, path = "/api/v1/resources/institutions/{id}/expense-dashboard", operation_id = "resources_expense_dashboard", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = ExpenseDashboard), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn dashboard(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<ExpenseDashboard>> {
    let ctx = auth.require(Permission::ResourcesRead)?;
    let inst = InstitutionId(id);
    let scope = institution_scope(ctx, inst);
    require_institution(&st, &scope, inst).await?;
    let t = ctx.scope.tenant_id;
    let today = business_today(&st, &scope, inst).await?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let settings = settings_of(&mut conn, t, id).await?;
    let counts: Vec<(String, i64, Decimal)> = sqlx::query_as(
        "SELECT status, count(*), COALESCE(sum(total), 0) FROM resources.expense_claim WHERE \
         tenant_id = $1 AND institution_id = $2 AND status IN ('submitted', 'approved') GROUP BY \
         status",
    )
    .bind(t.as_uuid())
    .bind(id)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let get = |s: &str| {
        counts
            .iter()
            .find(|(k, _, _)| k == s)
            .map_or((0, Decimal::ZERO), |(_, n, a)| (*n, *a))
    };
    let fy_start = if today.month() >= 4 {
        NaiveDate::from_ymd_opt(today.year(), 4, 1)
    } else {
        NaiveDate::from_ymd_opt(today.year() - 1, 4, 1)
    }
    .unwrap_or(today);
    let cats: Vec<(Option<String>, Decimal)> = sqlx::query_as(
        "SELECT e.category_id, sum(e.amount) FROM resources.expense e JOIN \
         resources.expense_claim c ON c.id = e.claim_id WHERE c.tenant_id = $1 AND \
         c.institution_id = $2 AND c.status IN ('approved', 'reimbursed') AND c.approved_at >= \
         $3::date AND e.currency = c.currency GROUP BY e.category_id ORDER BY 2 DESC",
    )
    .bind(t.as_uuid())
    .bind(id)
    .bind(fy_start)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let by_category = cats
        .into_iter()
        .map(|(cid, amount)| CategorySpend {
            name: cid
                .as_ref()
                .and_then(|c| settings.categories.iter().find(|k| &k.id == c))
                .map_or_else(|| "Uncategorised".to_owned(), |k| k.name.clone()),
            category_id: cid,
            amount,
        })
        .collect();
    let first = NaiveDate::from_ymd_opt(today.year(), today.month(), 1).unwrap_or(today);
    let start = first
        .checked_sub_months(chrono::Months::new(5))
        .unwrap_or(first);
    let months: Vec<(String, Decimal)> = sqlx::query_as(
        "SELECT to_char(approved_at, 'YYYY-MM'), sum(total) FROM resources.expense_claim WHERE \
         tenant_id = $1 AND institution_id = $2 AND status IN ('approved', 'reimbursed') AND \
         approved_at >= $3::date GROUP BY 1",
    )
    .bind(t.as_uuid())
    .bind(id)
    .bind(start)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let trend = (0..6)
        .filter_map(|i| start.checked_add_months(chrono::Months::new(i)))
        .map(|m| {
            let key = m.format("%Y-%m").to_string();
            MonthSpend {
                amount: months
                    .iter()
                    .find(|(k, _)| k == &key)
                    .map_or(Decimal::ZERO, |(_, a)| *a),
                month: key,
            }
        })
        .collect();
    let ocr_mismatches: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM resources.expense e JOIN resources.expense_claim c ON c.id = \
         e.claim_id WHERE c.tenant_id = $1 AND c.institution_id = $2 AND c.status = 'submitted' \
         AND e.ocr ? 'total' AND jsonb_typeof(e.ocr->'total') = 'number' AND \
         (e.ocr->>'total')::numeric <> e.amount",
    )
    .bind(t.as_uuid())
    .bind(id)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let (n1, a1) = get("submitted");
    let (n2, a2) = get("approved");
    Ok(Json(ExpenseDashboard {
        awaiting_approval: n1,
        awaiting_approval_amount: a1,
        awaiting_reimbursement: n2,
        awaiting_reimbursement_amount: a2,
        by_category,
        trend,
        ocr_mismatches,
    }))
}

/// Routes of expense claims.
pub fn router(st: ResourcesState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(settings_get, settings_put))
        .routes(routes!(receipt_upload))
        .routes(routes!(receipt_file))
        .routes(routes!(me_expenses, expense_create))
        .routes(routes!(expense_update, expense_delete))
        .routes(routes!(claim_create))
        .routes(routes!(claim_update, claim_delete))
        .routes(routes!(claim_submit))
        .routes(routes!(claim_withdraw))
        .routes(routes!(claim_get))
        .routes(routes!(claim_decide))
        .routes(routes!(claim_reimburse))
        .routes(routes!(claim_list))
        .routes(routes!(dashboard))
        .with_state(st)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_settings_validate() {
        let s = validate_settings(ExpenseSettings::default()).expect("defaults are valid");
        assert_eq!(s.categories.len(), 10);
        let mut dup = ExpenseSettings::default();
        dup.categories[1].id = "travel".into();
        assert!(validate_settings(dup).is_err());
        let d = NaiveDate::from_ymd_opt(2026, 10, 17).expect("date");
        assert_eq!(month_title(d), "October 2026 expenses");
        assert_eq!(next_step("approved").as_deref(), Some("Mark reimbursed"));
        assert!(blocks("foreign_currency") && !blocks("over_limit"));
    }
}
