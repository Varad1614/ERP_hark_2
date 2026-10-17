//! **ERP_EXTENSIONS 2A** — admissions funnel HTTP: enquiries (prospective applicants with a
//! stage history), the funnel report, the joining forecast with seat targets, possible
//! duplicates, and per-institution funnel settings. The maths lives in [`super::funnel`].
//!
//! Paths: `/api/v1/student/institutions/{id}/admission-enquiries`,
//! `/api/v1/student/admission-enquiries/{id}[/link-application]`,
//! `/api/v1/student/institutions/{id}/admission-{funnel,forecast,duplicates,funnel-settings}`.
//! operation_id prefix `admission_funnel_`. Tag: `admissions`.
//!
//! Enquiries linked to an application follow it: an offer moves the enquiry to the configured
//! offered stage, a created student admits it, and a rejected / withdrawn application or a
//! closed admission loses it. That catch-up runs before every funnel read.

use super::funnel::{
    self, avg_closed_value, avg_cycle_days, close_reason_breakdown, contact_duplicates,
    forecast_by_month, funnel_report, pipeline_coverage, probability_of, resolve_stages,
    stage_summary, stage_velocity, win_rate, win_rate_by, ClosedValues, Contact, Enquiry,
    FunnelReport, GroupRate, ReasonBreakdown, Stage, StageEntry, StageSummary, StageVelocity,
    WinRate, ADMITTED, LOST,
};
use crate::record::refs;
use crate::record::support::{
    check_version, expect_version, finish, invalid, next_number, norm, validate_email,
    validate_opt_text, validate_phone, validate_text, CommandErrors, CreateErrors, ReadErrors,
};
use crate::shared::guard::require_institution_writable;
use crate::shared::http::{actor, with_etag};
use crate::shared::scope::{institution_scope, tenant};
use crate::shared::time::business_today;
use crate::shared::{publish, StudentState};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::{DateTime, Datelike, NaiveDate, Utc};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
use std::collections::HashMap;
use sutra_kernel::auth::Permission;
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::{FieldViolation, KernelError};
use sutra_kernel::scope::{InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated};
use uuid::Uuid;

/// Where an enquiry came from.
pub const SOURCES: &[&str] = &[
    "walk_in",
    "website",
    "phone",
    "referral",
    "campaign",
    "education_fair",
    "social",
    "agent",
    "school_visit",
    "other",
];

const COLUMNS: &str = "id, institution_id, enquiry_number, programme_id, intake, given_name, \
    family_name, email, phone, guardian_name, guardian_phone, city, source, campaign, stage, \
    stage_history, stage_entered_at, expected_join, expected_fee, confidence, close_reason, \
    closed_at, owner_ref, next_follow_up, notes, application_id, version, created_at, updated_at";

// ---------------------------------------------------------------- settings

/// Funnel settings of an institution as stored.
#[derive(Debug, Clone)]
struct Settings {
    custom: Vec<serde_json::Value>,
    stale_days: i64,
    applied_stage: Option<String>,
    offered_stage: Option<String>,
    version: i64,
}

impl Settings {
    fn stages(&self) -> Vec<Stage> {
        resolve_stages(&self.custom, &funnel::default_open_stages())
    }
    /// The stage a linked application moves an enquiry to (`applied` by default), when the
    /// funnel has it.
    fn applied(&self, stages: &[Stage]) -> Option<String> {
        pick_stage(self.applied_stage.as_deref().unwrap_or("applied"), stages)
    }
    fn offered(&self, stages: &[Stage]) -> Option<String> {
        pick_stage(self.offered_stage.as_deref().unwrap_or("offered"), stages)
    }
}

fn pick_stage(id: &str, stages: &[Stage]) -> Option<String> {
    stages
        .iter()
        .any(|s| s.id == id && !funnel::is_terminal(id))
        .then(|| id.to_owned())
}

async fn load_settings(
    conn: &mut PgConnection,
    tenant: TenantId,
    inst: InstitutionId,
) -> Result<Settings, KernelError> {
    let row = sqlx::query(
        "SELECT stages, stale_days, applied_stage, offered_stage, version \
         FROM student.admission_funnel_settings WHERE tenant_id = $1 AND institution_id = $2",
    )
    .bind(tenant.as_uuid())
    .bind(inst.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(match row {
        None => Settings {
            custom: Vec::new(),
            stale_days: funnel::DEFAULT_STALE_DAYS,
            applied_stage: None,
            offered_stage: None,
            version: 0,
        },
        Some(r) => {
            let stages: serde_json::Value = r.try_get("stages").map_err(map_db_error)?;
            Settings {
                custom: stages.as_array().cloned().unwrap_or_default(),
                stale_days: i64::from(r.try_get::<i32, _>("stale_days").map_err(map_db_error)?),
                applied_stage: r.try_get("applied_stage").map_err(map_db_error)?,
                offered_stage: r.try_get("offered_stage").map_err(map_db_error)?,
                version: i64::from(r.try_get::<i32, _>("version").map_err(map_db_error)?),
            }
        }
    })
}

// ---------------------------------------------------------------- enquiry rows

/// One recorded stage move (wire form).
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct StageMove {
    pub stage: String,
    pub at: DateTime<Utc>,
}

/// An admission enquiry (wire form).
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AdmissionEnquiry {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub enquiry_number: String,
    pub programme_id: Option<Uuid>,
    /// `YYYY` or `YYYY-MM` intake the enquirer wants.
    pub intake: Option<String>,
    pub given_name: String,
    pub family_name: Option<String>,
    pub display_name: String,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub guardian_name: Option<String>,
    pub guardian_phone: Option<String>,
    pub city: Option<String>,
    pub source: String,
    pub campaign: Option<String>,
    pub stage: String,
    pub stage_label: String,
    pub stage_history: Vec<StageMove>,
    pub stage_entered_at: DateTime<Utc>,
    pub days_in_stage: i64,
    /// Open and unmoved for the institution's stale threshold.
    pub stalled: bool,
    pub expected_join: Option<NaiveDate>,
    #[schema(value_type = String)]
    pub expected_fee: Decimal,
    /// Own win probability (0..1); `null` uses the stage's.
    pub confidence: Option<f64>,
    /// Effective win probability used by the forecast.
    pub probability: f64,
    pub close_reason: Option<String>,
    pub closed_at: Option<DateTime<Utc>>,
    pub owner_ref: Option<String>,
    pub next_follow_up: Option<NaiveDate>,
    pub notes: Option<String>,
    pub application_id: Option<Uuid>,
    #[schema(value_type = i64)]
    pub version: Version,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A raw row plus what the maths needs.
struct Row_ {
    wire: AdmissionEnquiry,
    history: Vec<StageEntry>,
}

fn ms(t: DateTime<Utc>) -> f64 {
    t.timestamp_millis() as f64
}

fn from_ms(v: f64) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(v as i64).unwrap_or_default()
}

fn parse_history(v: &serde_json::Value) -> Vec<StageEntry> {
    v.as_array()
        .map(|a| {
            a.iter()
                .map(|e| StageEntry {
                    stage: e.get("stage").and_then(|s| s.as_str()).map(str::to_owned),
                    at: e.get("at").and_then(serde_json::Value::as_f64),
                })
                .collect()
        })
        .unwrap_or_default()
}

fn history_json(h: &[StageEntry]) -> serde_json::Value {
    serde_json::Value::Array(
        h.iter()
            .map(|e| serde_json::json!({ "stage": e.stage, "at": e.at.map(|a| a as i64) }))
            .collect(),
    )
}

fn from_row(r: &PgRow) -> Result<Row_, sqlx::Error> {
    let given: String = r.try_get("given_name")?;
    let family: Option<String> = r.try_get("family_name")?;
    let hist_raw: serde_json::Value = r.try_get("stage_history")?;
    let history = parse_history(&hist_raw);
    let confidence: Option<Decimal> = r.try_get("confidence")?;
    let wire = AdmissionEnquiry {
        id: r.try_get("id")?,
        institution_id: r.try_get("institution_id")?,
        enquiry_number: r.try_get("enquiry_number")?,
        programme_id: r.try_get("programme_id")?,
        intake: r.try_get("intake")?,
        display_name: match family.as_deref() {
            Some(f) => format!("{given} {f}"),
            None => given.clone(),
        },
        given_name: given,
        family_name: family,
        email: r.try_get("email")?,
        phone: r.try_get("phone")?,
        guardian_name: r.try_get("guardian_name")?,
        guardian_phone: r.try_get("guardian_phone")?,
        city: r.try_get("city")?,
        source: r.try_get("source")?,
        campaign: r.try_get("campaign")?,
        stage: r.try_get("stage")?,
        stage_label: String::new(),
        stage_history: history
            .iter()
            .filter_map(|e| {
                Some(StageMove {
                    stage: e.stage.clone()?,
                    at: from_ms(e.at?),
                })
            })
            .collect(),
        stage_entered_at: r.try_get("stage_entered_at")?,
        days_in_stage: 0,
        stalled: false,
        expected_join: r.try_get("expected_join")?,
        expected_fee: r.try_get("expected_fee")?,
        confidence: confidence.and_then(|c| c.to_f64()),
        probability: 0.0,
        close_reason: r.try_get("close_reason")?,
        closed_at: r.try_get("closed_at")?,
        owner_ref: r.try_get("owner_ref")?,
        next_follow_up: r.try_get("next_follow_up")?,
        notes: r.try_get("notes")?,
        application_id: r.try_get("application_id")?,
        version: Version(i64::from(r.try_get::<i32, _>("version")?)),
        created_at: r.try_get("created_at")?,
        updated_at: r.try_get("updated_at")?,
    };
    Ok(Row_ { wire, history })
}

impl Row_ {
    fn enquiry(&self) -> Enquiry {
        let w = &self.wire;
        Enquiry {
            id: w.id.to_string(),
            stage: w.stage.clone(),
            stage_history: self.history.clone(),
            value: w.expected_fee.to_f64().unwrap_or(0.0),
            confidence: w.confidence,
            expected_close: w.expected_join.map(|d| d.to_string()),
            closed_at: w.closed_at.map(ms),
            created_at: Some(ms(w.created_at)),
            updated_at: Some(ms(w.updated_at)),
            close_reason: w.close_reason.clone(),
            source: Some(w.source.clone()),
        }
    }
    /// Fills the derived fields.
    fn finish(mut self, stages: &[Stage], settings: &Settings, now_ms: f64) -> AdmissionEnquiry {
        let e = self.enquiry();
        self.wire.stage_label = stages
            .iter()
            .find(|s| s.id == self.wire.stage)
            .map_or_else(|| self.wire.stage.clone(), |s| s.label.clone());
        self.wire.days_in_stage = funnel::days_in_stage(&e, now_ms);
        self.wire.stalled = funnel::is_stalled(&e, settings.stale_days, now_ms);
        self.wire.probability = probability_of(&e, &HashMap::new(), stages);
        self.wire
    }
}

async fn load(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<Row_, KernelError> {
    let sql = format!(
        "SELECT {COLUMNS} FROM student.admission_enquiry WHERE id = $1 AND tenant_id = $2{}",
        if lock { " FOR UPDATE" } else { "" }
    );
    let row = sqlx::query(&sql)
        .bind(id)
        .bind(tenant.as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound {
            entity: "admission enquiry",
        })?;
    from_row(&row).map_err(map_db_error)
}

async fn load_all(
    conn: &mut PgConnection,
    tenant: TenantId,
    inst: InstitutionId,
    programme: Option<Uuid>,
) -> Result<Vec<Row_>, KernelError> {
    let sql = format!(
        "SELECT {COLUMNS} FROM student.admission_enquiry WHERE tenant_id = $1 \
         AND institution_id = $2 AND ($3::uuid IS NULL OR programme_id = $3) \
         ORDER BY created_at DESC, id DESC LIMIT 5000"
    );
    let rows = sqlx::query(&sql)
        .bind(tenant.as_uuid())
        .bind(inst.as_uuid())
        .bind(programme)
        .fetch_all(&mut *conn)
        .await
        .map_err(map_db_error)?;
    rows.iter()
        .map(from_row)
        .collect::<Result<_, _>>()
        .map_err(map_db_error)
}

fn now_ms() -> f64 {
    ms(Utc::now())
}

fn respond(status: StatusCode, e: &AdmissionEnquiry) -> Response {
    with_etag(status, e, e.version)
}

/// Appends `stage` to `history` (at `at`) and returns the stored columns that change with it.
fn move_to(history: &mut Vec<StageEntry>, stage: &str, at: DateTime<Utc>) {
    history.push(StageEntry {
        stage: Some(stage.to_owned()),
        at: Some(ms(at)),
    });
}

/// Applies a stage move inside `tx` (history, entered-at, closed-at, reason).
#[allow(clippy::too_many_arguments)]
async fn write_stage(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    history: &[StageEntry],
    stage: &str,
    at: DateTime<Utc>,
    close_reason: Option<&str>,
) -> Result<(), KernelError> {
    let closed = funnel::is_terminal(stage);
    sqlx::query(
        "UPDATE student.admission_enquiry SET stage = $3, stage_history = $4, \
         stage_entered_at = $5, closed_at = CASE WHEN $6 THEN $5 ELSE NULL END, \
         close_reason = CASE WHEN $6 THEN $7 ELSE NULL END, version = version + 1, \
         updated_at = now() WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(tenant.as_uuid())
    .bind(stage)
    .bind(history_json(history))
    .bind(at)
    .bind(closed)
    .bind(close_reason)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(())
}

/// Brings enquiries linked to an application in step with it. Returns how many moved.
async fn sync_linked(
    conn: &mut PgConnection,
    tenant: TenantId,
    inst: InstitutionId,
    settings: &Settings,
) -> Result<u32, KernelError> {
    let stages = settings.stages();
    let order: HashMap<&str, usize> = funnel::open_stages(&stages)
        .iter()
        .enumerate()
        .map(|(i, s)| (s.id.as_str(), i))
        .collect();
    let offered = settings.offered(&stages);
    let rows = sqlx::query(
        "SELECT e.id, e.stage, e.stage_history, a.status AS app_status, d.status AS adm_status \
           FROM student.admission_enquiry e \
           JOIN student.admission_application a ON a.id = e.application_id \
           LEFT JOIN student.admission d ON d.id = a.admission_id \
          WHERE e.tenant_id = $1 AND e.institution_id = $2 \
            AND e.stage NOT IN ('admitted', 'lost') FOR UPDATE OF e",
    )
    .bind(tenant.as_uuid())
    .bind(inst.as_uuid())
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let now = Utc::now();
    let mut moved = 0;
    for r in rows {
        let id: Uuid = r.try_get("id").map_err(map_db_error)?;
        let stage: String = r.try_get("stage").map_err(map_db_error)?;
        let app: String = r.try_get("app_status").map_err(map_db_error)?;
        let adm: Option<String> = r.try_get("adm_status").map_err(map_db_error)?;
        let target: Option<(String, Option<String>)> = match (app.as_str(), adm.as_deref()) {
            (_, Some("student_created" | "completed")) => Some((ADMITTED.into(), None)),
            (_, Some(s @ ("cancelled" | "withdrawn" | "revoked" | "expired"))) => {
                Some((LOST.into(), Some(format!("Admission {s}"))))
            }
            (s @ ("rejected" | "withdrawn"), _) => {
                Some((LOST.into(), Some(format!("Application {s}"))))
            }
            ("offered", _) => offered.as_ref().and_then(|o| {
                let ahead = match (order.get(stage.as_str()), order.get(o.as_str())) {
                    (Some(c), Some(t)) => t > c,
                    (None, Some(_)) => true,
                    _ => false,
                };
                ahead.then(|| (o.clone(), None))
            }),
            _ => None,
        };
        let Some((to, reason)) = target else { continue };
        let raw: serde_json::Value = r.try_get("stage_history").map_err(map_db_error)?;
        let mut history = parse_history(&raw);
        move_to(&mut history, &to, now);
        write_stage(conn, tenant, id, &history, &to, now, reason.as_deref()).await?;
        moved += 1;
    }
    Ok(moved)
}

// ---------------------------------------------------------------- validation

fn validate_intake(field: &str, v: Option<&str>, out: &mut Vec<FieldViolation>) {
    if let Some(s) = v {
        let ok = matches!(s.len(), 4 | 7)
            && s[..4].bytes().all(|b| b.is_ascii_digit())
            && (s.len() == 4
                || (s.as_bytes()[4] == b'-'
                    && s[5..].parse::<u32>().is_ok_and(|m| (1..=12).contains(&m))));
        if !ok {
            out.push(FieldViolation::new(
                field,
                "format",
                "intake must be YYYY or YYYY-MM",
            ));
        }
    }
}

fn validate_confidence(v: Option<f64>, out: &mut Vec<FieldViolation>) {
    if v.is_some_and(|c| !(0.0..=1.0).contains(&c) || !c.is_finite()) {
        out.push(FieldViolation::new(
            "confidence",
            "range",
            "confidence must be between 0 and 1",
        ));
    }
}

fn validate_fee(v: Option<Decimal>, out: &mut Vec<FieldViolation>) {
    if v.is_some_and(|f| f < Decimal::ZERO || f.scale() > 2) {
        out.push(FieldViolation::new(
            "expectedFee",
            "range",
            "expectedFee must be a non-negative amount with at most two decimals",
        ));
    }
}

fn validate_source(v: Option<&str>, out: &mut Vec<FieldViolation>) {
    if let Some(s) = v {
        if !SOURCES.contains(&s) {
            out.push(FieldViolation::new(
                "source",
                "unknown",
                format!("source must be one of {}", SOURCES.join(", ")),
            ));
        }
    }
}

fn to_decimal(v: f64) -> Decimal {
    Decimal::from_f64_retain(v).unwrap_or_default().round_dp(3)
}

// ---------------------------------------------------------------- create / list / get

/// A new enquiry.
#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateEnquiry {
    pub given_name: String,
    pub family_name: Option<String>,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub guardian_name: Option<String>,
    pub guardian_phone: Option<String>,
    pub city: Option<String>,
    /// One of `walk_in` (default), `website`, `phone`, `referral`, `campaign`,
    /// `education_fair`, `social`, `agent`, `school_visit`, `other`.
    pub source: Option<String>,
    pub campaign: Option<String>,
    pub programme_id: Option<Uuid>,
    pub intake: Option<String>,
    /// An open stage (default: the first).
    pub stage: Option<String>,
    pub expected_join: Option<NaiveDate>,
    #[schema(value_type = Option<String>)]
    pub expected_fee: Option<Decimal>,
    pub confidence: Option<f64>,
    pub owner_ref: Option<String>,
    pub next_follow_up: Option<NaiveDate>,
    pub notes: Option<String>,
}

/// A record the new enquiry may duplicate.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DuplicateRef {
    /// `enquiry` or `application`.
    pub kind: String,
    pub id: Uuid,
    pub number: Option<String>,
    pub name: String,
    /// `email`, `name`, `phone`.
    pub reasons: Vec<String>,
}

/// The created enquiry and records that look like the same person.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreatedEnquiry {
    pub enquiry: AdmissionEnquiry,
    pub possible_duplicates: Vec<DuplicateRef>,
}

#[allow(clippy::too_many_arguments)]
fn validate_person(
    given: &str,
    family: Option<&str>,
    email: Option<&str>,
    phone: Option<&str>,
    guardian: Option<&str>,
    guardian_phone: Option<&str>,
    city: Option<&str>,
    campaign: Option<&str>,
    owner: Option<&str>,
    notes: Option<&str>,
    v: &mut Vec<FieldViolation>,
) {
    validate_text("givenName", given, 100, v);
    validate_opt_text("familyName", family, 100, v);
    validate_email("email", email, v);
    validate_phone("phone", phone, v);
    validate_opt_text("guardianName", guardian, 120, v);
    validate_phone("guardianPhone", guardian_phone, v);
    validate_opt_text("city", city, 80, v);
    validate_opt_text("campaign", campaign, 120, v);
    validate_opt_text("ownerRef", owner, 120, v);
    validate_opt_text("notes", notes, 2000, v);
}

/// Record a new enquiry (`admission.manage`).
#[utoipa::path(
    post,
    path = "/api/v1/student/institutions/{id}/admission-enquiries",
    operation_id = "admission_funnel_enquiry_create",
    tag = "admissions",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = CreateEnquiry,
    responses((status = 201, body = CreatedEnquiry, headers(("ETag" = String))), CreateErrors)
)]
pub async fn create_enquiry(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<InstitutionId>,
    headers: HeaderMap,
    Json(cmd): Json<CreateEnquiry>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::AdmissionManage)?;
    require_idempotency_key(&headers)?;
    let scope = institution_scope(ctx, id);
    require_institution_writable(&st, &scope, id).await?;
    let mut v = Vec::new();
    validate_person(
        &cmd.given_name,
        cmd.family_name.as_deref(),
        cmd.email.as_deref(),
        cmd.phone.as_deref(),
        cmd.guardian_name.as_deref(),
        cmd.guardian_phone.as_deref(),
        cmd.city.as_deref(),
        cmd.campaign.as_deref(),
        cmd.owner_ref.as_deref(),
        cmd.notes.as_deref(),
        &mut v,
    );
    validate_source(cmd.source.as_deref(), &mut v);
    validate_intake("intake", cmd.intake.as_deref(), &mut v);
    validate_confidence(cmd.confidence, &mut v);
    validate_fee(cmd.expected_fee, &mut v);
    finish(v)?;
    if let Some(p) = cmd.programme_id {
        refs::programme(&st, &scope, id, p, "programmeId").await?;
    }
    let t = tenant(ctx);
    let a = actor(ctx);
    let today = business_today(&st, &scope, id).await?;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let settings = load_settings(&mut tx, t, id).await?;
    let stages = settings.stages();
    let open = funnel::open_stages(&stages);
    let stage = match cmd.stage.as_deref().map(str::trim) {
        None | Some("") => open.first().map(|s| s.id.clone()).unwrap_or_default(),
        Some(s) if open.iter().any(|o| o.id == s) => s.to_owned(),
        Some(_) => {
            return Err(invalid(
                "stage",
                "unknown",
                "stage must be one of the open funnel stages",
            )
            .into())
        }
    };
    let seq = next_number(
        &mut tx,
        t,
        &format!("enquiry:{}:{}", id.as_uuid(), today.year()),
    )
    .await?;
    let number = format!("ENQ-{}-{seq:05}", today.year());
    let now = Utc::now();
    let mut history = Vec::new();
    move_to(&mut history, &stage, now);
    let sql = format!(
        "INSERT INTO student.admission_enquiry (tenant_id, institution_id, enquiry_number, \
         programme_id, intake, given_name, family_name, email, phone, guardian_name, \
         guardian_phone, city, source, campaign, stage, stage_history, stage_entered_at, \
         expected_join, expected_fee, confidence, owner_ref, next_follow_up, notes, created_by) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21,$22,$23,$24) \
         RETURNING {COLUMNS}"
    );
    let row = sqlx::query(&sql)
        .bind(t.as_uuid())
        .bind(id.as_uuid())
        .bind(&number)
        .bind(cmd.programme_id)
        .bind(norm(cmd.intake.as_deref()))
        .bind(cmd.given_name.trim())
        .bind(norm(cmd.family_name.as_deref()))
        .bind(norm(cmd.email.as_deref()))
        .bind(norm(cmd.phone.as_deref()))
        .bind(norm(cmd.guardian_name.as_deref()))
        .bind(norm(cmd.guardian_phone.as_deref()))
        .bind(norm(cmd.city.as_deref()))
        .bind(cmd.source.as_deref().unwrap_or("walk_in"))
        .bind(norm(cmd.campaign.as_deref()))
        .bind(&stage)
        .bind(history_json(&history))
        .bind(now)
        .bind(cmd.expected_join)
        .bind(cmd.expected_fee.unwrap_or_default())
        .bind(cmd.confidence.map(to_decimal))
        .bind(norm(cmd.owner_ref.as_deref()))
        .bind(cmd.next_follow_up)
        .bind(norm(cmd.notes.as_deref()))
        .bind(a.id.as_deref())
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?;
    let created = from_row(&row).map_err(map_db_error)?;
    let enquiry = created.finish(&stages, &settings, now_ms());
    publish::audit(
        &mut tx,
        &scope,
        "student.admission_enquiry.create",
        "admission_enquiry",
        enquiry.id.to_string(),
        serde_json::json!({ "stage": enquiry.stage, "source": enquiry.source, "number": number }),
        &a,
    )
    .await?;
    let contacts = duplicate_universe(&mut tx, t, id).await?;
    tx.commit().await.map_err(map_db_error)?;
    let me = format!("enquiry:{}", enquiry.id);
    let possible_duplicates = contact_duplicates(
        &contacts
            .iter()
            .map(|c| c.contact.clone())
            .collect::<Vec<_>>(),
    )
    .into_iter()
    .filter_map(|p| {
        let other = if p.a.id == me {
            &p.b.id
        } else if p.b.id == me {
            &p.a.id
        } else {
            return None;
        };
        contacts
            .iter()
            .find(|c| &c.contact.id == other)
            .map(|c| DuplicateRef {
                kind: c.kind.clone(),
                id: c.id,
                number: c.number.clone(),
                name: c.contact.name.clone(),
                reasons: p.reasons.iter().map(|r| (*r).to_owned()).collect(),
            })
    })
    .collect();
    let version = enquiry.version;
    Ok(with_etag(
        StatusCode::CREATED,
        &CreatedEnquiry {
            enquiry,
            possible_duplicates,
        },
        version,
    ))
}

/// List filters.
#[derive(Debug, Clone, Default, serde::Deserialize, utoipa::IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct EnquiryListQuery {
    pub stage: Option<String>,
    pub source: Option<String>,
    pub programme_id: Option<Uuid>,
    /// Name, number, e-mail or phone contains.
    pub q: Option<String>,
    /// Only open enquiries past the stale threshold.
    pub stalled: Option<bool>,
    /// Only open enquiries (not admitted / lost).
    pub open: Option<bool>,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AdmissionEnquiryList {
    pub items: Vec<AdmissionEnquiry>,
    pub stages: Vec<Stage>,
    pub stale_days: i64,
}

/// Enquiries of an institution (`student.read`).
#[utoipa::path(
    get,
    path = "/api/v1/student/institutions/{id}/admission-enquiries",
    operation_id = "admission_funnel_enquiry_list",
    tag = "admissions",
    params(("id" = String, Path, format = Uuid), EnquiryListQuery),
    security(("session_cookie" = [])),
    responses((status = 200, body = AdmissionEnquiryList), ReadErrors)
)]
pub async fn list_enquiries(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<InstitutionId>,
    Query(q): Query<EnquiryListQuery>,
) -> ApiResult<Json<AdmissionEnquiryList>> {
    let ctx = auth.require(Permission::StudentRead)?;
    let t = tenant(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let settings = load_settings(&mut tx, t, id).await?;
    sync_linked(&mut tx, t, id, &settings).await?;
    let rows = load_all(&mut tx, t, id, q.programme_id).await?;
    tx.commit().await.map_err(map_db_error)?;
    let stages = settings.stages();
    let now = now_ms();
    let needle =
        q.q.as_deref()
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty());
    let items = rows
        .into_iter()
        .map(|r| r.finish(&stages, &settings, now))
        .filter(|e| q.stage.as_deref().is_none_or(|s| e.stage == s))
        .filter(|e| q.source.as_deref().is_none_or(|s| e.source == s))
        .filter(|e| !q.stalled.unwrap_or(false) || e.stalled)
        .filter(|e| !q.open.unwrap_or(false) || !funnel::is_terminal(&e.stage))
        .filter(|e| {
            needle.as_deref().is_none_or(|n| {
                [
                    Some(e.display_name.as_str()),
                    Some(e.enquiry_number.as_str()),
                    e.email.as_deref(),
                    e.phone.as_deref(),
                    e.guardian_phone.as_deref(),
                ]
                .iter()
                .flatten()
                .any(|f| f.to_lowercase().contains(n))
            })
        })
        .take(1000)
        .collect();
    Ok(Json(AdmissionEnquiryList {
        items,
        stages,
        stale_days: settings.stale_days,
    }))
}

/// One enquiry (`student.read`).
#[utoipa::path(
    get,
    path = "/api/v1/student/admission-enquiries/{id}",
    operation_id = "admission_funnel_enquiry_get",
    tag = "admissions",
    params(("id" = String, Path, format = Uuid)),
    security(("session_cookie" = [])),
    responses((status = 200, body = AdmissionEnquiry, headers(("ETag" = String))), ReadErrors)
)]
pub async fn get_enquiry(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StudentRead)?;
    let t = tenant(ctx);
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let r = load(&mut conn, t, id, false).await?;
    let settings = load_settings(&mut conn, t, InstitutionId(r.wire.institution_id)).await?;
    let stages = settings.stages();
    Ok(respond(
        StatusCode::OK,
        &r.finish(&stages, &settings, now_ms()),
    ))
}

// ---------------------------------------------------------------- update / link

/// Changes to an enquiry; `stage` moves it (Lost needs `closeReason`). `null` clears a field.
#[derive(Debug, Clone, Default, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateEnquiry {
    pub given_name: Option<String>,
    #[serde(default, with = "patch")]
    #[schema(value_type = Option<String>)]
    pub family_name: Option<Option<String>>,
    #[serde(default, with = "patch")]
    #[schema(value_type = Option<String>)]
    pub email: Option<Option<String>>,
    #[serde(default, with = "patch")]
    #[schema(value_type = Option<String>)]
    pub phone: Option<Option<String>>,
    #[serde(default, with = "patch")]
    #[schema(value_type = Option<String>)]
    pub guardian_name: Option<Option<String>>,
    #[serde(default, with = "patch")]
    #[schema(value_type = Option<String>)]
    pub guardian_phone: Option<Option<String>>,
    #[serde(default, with = "patch")]
    #[schema(value_type = Option<String>)]
    pub city: Option<Option<String>>,
    pub source: Option<String>,
    #[serde(default, with = "patch")]
    #[schema(value_type = Option<String>)]
    pub campaign: Option<Option<String>>,
    #[serde(default, with = "patch")]
    #[schema(value_type = Option<String>, format = Uuid)]
    pub programme_id: Option<Option<Uuid>>,
    #[serde(default, with = "patch")]
    #[schema(value_type = Option<String>)]
    pub intake: Option<Option<String>>,
    pub stage: Option<String>,
    pub close_reason: Option<String>,
    #[serde(default, with = "patch")]
    #[schema(value_type = Option<String>, format = Date)]
    pub expected_join: Option<Option<NaiveDate>>,
    #[schema(value_type = Option<String>)]
    pub expected_fee: Option<Decimal>,
    #[serde(default, with = "patch")]
    #[schema(value_type = Option<f64>)]
    pub confidence: Option<Option<f64>>,
    #[serde(default, with = "patch")]
    #[schema(value_type = Option<String>)]
    pub owner_ref: Option<Option<String>>,
    #[serde(default, with = "patch")]
    #[schema(value_type = Option<String>, format = Date)]
    pub next_follow_up: Option<Option<NaiveDate>>,
    #[serde(default, with = "patch")]
    #[schema(value_type = Option<String>)]
    pub notes: Option<Option<String>>,
}

mod patch {
    use serde::{Deserialize, Deserializer};

    pub fn deserialize<'de, D, T>(d: D) -> Result<Option<Option<T>>, D::Error>
    where
        D: Deserializer<'de>,
        T: Deserialize<'de>,
    {
        Option::<T>::deserialize(d).map(Some)
    }
}

fn pick<T: Clone>(patch: &Option<Option<T>>, old: &Option<T>) -> Option<T> {
    match patch {
        Some(v) => v.clone(),
        None => old.clone(),
    }
}

fn pick_text(patch: &Option<Option<String>>, old: &Option<String>) -> Option<String> {
    match patch {
        Some(v) => norm(v.as_deref()),
        None => old.clone(),
    }
}

/// Edit or move an enquiry (`admission.manage`, `If-Match`).
#[utoipa::path(
    patch,
    path = "/api/v1/student/admission-enquiries/{id}",
    operation_id = "admission_funnel_enquiry_update",
    tag = "admissions",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the version being changed")),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = UpdateEnquiry,
    responses((status = 200, body = AdmissionEnquiry, headers(("ETag" = String))), CommandErrors)
)]
pub async fn update_enquiry(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(cmd): Json<UpdateEnquiry>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::AdmissionManage)?;
    let expected = expect_version!(headers);
    let t = tenant(ctx);
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let cur = load(&mut conn, t, id, false).await?;
    drop(conn);
    let inst = InstitutionId(cur.wire.institution_id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let w = &cur.wire;
    let given = cmd
        .given_name
        .clone()
        .map(|g| g.trim().to_owned())
        .unwrap_or_else(|| w.given_name.clone());
    let family = pick_text(&cmd.family_name, &w.family_name);
    let email = pick_text(&cmd.email, &w.email);
    let phone = pick_text(&cmd.phone, &w.phone);
    let guardian = pick_text(&cmd.guardian_name, &w.guardian_name);
    let guardian_phone = pick_text(&cmd.guardian_phone, &w.guardian_phone);
    let city = pick_text(&cmd.city, &w.city);
    let campaign = pick_text(&cmd.campaign, &w.campaign);
    let owner = pick_text(&cmd.owner_ref, &w.owner_ref);
    let notes = pick_text(&cmd.notes, &w.notes);
    let intake = pick_text(&cmd.intake, &w.intake);
    let programme = pick(&cmd.programme_id, &w.programme_id);
    let expected_join = pick(&cmd.expected_join, &w.expected_join);
    let next_follow_up = pick(&cmd.next_follow_up, &w.next_follow_up);
    let confidence = pick(&cmd.confidence, &w.confidence);
    let fee = cmd.expected_fee.unwrap_or(w.expected_fee);
    let source = cmd.source.clone().unwrap_or_else(|| w.source.clone());
    let mut v = Vec::new();
    validate_person(
        &given,
        family.as_deref(),
        email.as_deref(),
        phone.as_deref(),
        guardian.as_deref(),
        guardian_phone.as_deref(),
        city.as_deref(),
        campaign.as_deref(),
        owner.as_deref(),
        notes.as_deref(),
        &mut v,
    );
    validate_source(Some(&source), &mut v);
    validate_intake("intake", intake.as_deref(), &mut v);
    validate_confidence(confidence, &mut v);
    validate_fee(Some(fee), &mut v);
    validate_opt_text("closeReason", cmd.close_reason.as_deref(), 200, &mut v);
    finish(v)?;
    if let Some(p) = programme.filter(|p| Some(*p) != w.programme_id) {
        refs::programme(&st, &scope, inst, p, "programmeId").await?;
    }
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let locked = load(&mut tx, t, id, true).await?;
    check_version(expected, locked.wire.version.0)?;
    let settings = load_settings(&mut tx, t, inst).await?;
    let stages = settings.stages();
    let mut moved: Option<(String, String)> = None;
    if let Some(to) = cmd
        .stage
        .as_deref()
        .map(str::trim)
        .filter(|s| *s != locked.wire.stage)
    {
        if !stages.iter().any(|s| s.id == to) {
            return Err(invalid(
                "stage",
                "unknown",
                "stage is not in this institution's funnel",
            )
            .into());
        }
        let reason = norm(cmd.close_reason.as_deref());
        if to == LOST && reason.is_none() {
            return Err(invalid("closeReason", "required", "a lost enquiry needs a reason").into());
        }
        let mut history = locked.history.clone();
        let now = Utc::now();
        move_to(&mut history, to, now);
        write_stage(&mut tx, t, id, &history, to, now, reason.as_deref()).await?;
        moved = Some((locked.wire.stage.clone(), to.to_owned()));
    } else if cmd.close_reason.is_some() && funnel::is_terminal(&locked.wire.stage) {
        let reason = norm(cmd.close_reason.as_deref());
        if locked.wire.stage == LOST && reason.is_none() {
            return Err(invalid("closeReason", "required", "a lost enquiry needs a reason").into());
        }
        sqlx::query("UPDATE student.admission_enquiry SET close_reason = $3 WHERE id = $1 AND tenant_id = $2")
            .bind(id)
            .bind(t.as_uuid())
            .bind(reason)
            .execute(&mut *tx)
            .await
            .map_err(map_db_error)?;
    }
    let sql = format!(
        "UPDATE student.admission_enquiry SET given_name = $3, family_name = $4, email = $5, \
         phone = $6, guardian_name = $7, guardian_phone = $8, city = $9, source = $10, \
         campaign = $11, programme_id = $12, intake = $13, expected_join = $14, \
         expected_fee = $15, confidence = $16, owner_ref = $17, next_follow_up = $18, \
         notes = $19, version = CASE WHEN $20 THEN version ELSE version + 1 END, \
         updated_at = now() WHERE id = $1 AND tenant_id = $2 RETURNING {COLUMNS}"
    );
    let row = sqlx::query(&sql)
        .bind(id)
        .bind(t.as_uuid())
        .bind(&given)
        .bind(&family)
        .bind(&email)
        .bind(&phone)
        .bind(&guardian)
        .bind(&guardian_phone)
        .bind(&city)
        .bind(&source)
        .bind(&campaign)
        .bind(programme)
        .bind(&intake)
        .bind(expected_join)
        .bind(fee)
        .bind(confidence.map(to_decimal))
        .bind(&owner)
        .bind(next_follow_up)
        .bind(&notes)
        .bind(moved.is_some())
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?;
    let updated = from_row(&row)
        .map_err(map_db_error)?
        .finish(&stages, &settings, now_ms());
    publish::audit(
        &mut tx,
        &scope,
        if moved.is_some() {
            "student.admission_enquiry.move"
        } else {
            "student.admission_enquiry.update"
        },
        "admission_enquiry",
        id.to_string(),
        serde_json::json!({
            "from": moved.as_ref().map(|m| &m.0), "to": moved.as_ref().map(|m| &m.1),
            "closeReason": updated.close_reason, "version": updated.version.0,
        }),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(respond(StatusCode::OK, &updated))
}

/// The application an enquiry turned into.
#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LinkApplication {
    pub application_id: Uuid,
}

/// Link the enquiry to its admission application (`admission.manage`, `If-Match`). The
/// enquiry moves to the applied stage when it is behind it, then follows the application.
#[utoipa::path(
    post,
    path = "/api/v1/student/admission-enquiries/{id}/link-application",
    operation_id = "admission_funnel_enquiry_link_application",
    tag = "admissions",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the version being changed")),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = LinkApplication,
    responses((status = 200, body = AdmissionEnquiry, headers(("ETag" = String))), CommandErrors)
)]
pub async fn link_application(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(cmd): Json<LinkApplication>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::AdmissionManage)?;
    let expected = expect_version!(headers);
    let t = tenant(ctx);
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let cur = load(&mut tx, t, id, true).await?;
    check_version(expected, cur.wire.version.0)?;
    let inst = InstitutionId(cur.wire.institution_id);
    let scope = institution_scope(ctx, inst);
    if funnel::is_terminal(&cur.wire.stage) {
        return Err(KernelError::Conflict(format!(
            "enquiry is {}; reopen it before linking an application",
            cur.wire.stage
        ))
        .into());
    }
    let app: Option<(Uuid, Option<Uuid>)> = sqlx::query_as(
        "SELECT institution_id, programme_id FROM student.admission_application \
         WHERE id = $1 AND tenant_id = $2",
    )
    .bind(cmd.application_id)
    .bind(t.as_uuid())
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let Some((app_inst, app_programme)) = app else {
        return Err(invalid("applicationId", "unknown", "application not found").into());
    };
    if app_inst != cur.wire.institution_id {
        return Err(invalid(
            "applicationId",
            "other-institution",
            "the application belongs to another institution",
        )
        .into());
    }
    let settings = load_settings(&mut tx, t, inst).await?;
    let stages = settings.stages();
    sqlx::query(
        "UPDATE student.admission_enquiry SET application_id = $3, \
         programme_id = coalesce(programme_id, $4), version = version + 1, updated_at = now() \
         WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(t.as_uuid())
    .bind(cmd.application_id)
    .bind(app_programme)
    .execute(&mut *tx)
    .await
    .map_err(|e| match map_db_error(e) {
        KernelError::Conflict(_) => {
            KernelError::Conflict("that application is already linked to another enquiry".into())
        }
        other => other,
    })?;
    if let Some(applied) = settings.applied(&stages) {
        let order: HashMap<&str, usize> = funnel::open_stages(&stages)
            .iter()
            .enumerate()
            .map(|(i, s)| (s.id.as_str(), i))
            .collect();
        let behind = match (
            order.get(cur.wire.stage.as_str()),
            order.get(applied.as_str()),
        ) {
            (Some(c), Some(t)) => c < t,
            (None, Some(_)) => true,
            _ => false,
        };
        if behind {
            let mut history = cur.history.clone();
            let now = Utc::now();
            move_to(&mut history, &applied, now);
            sqlx::query(
                "UPDATE student.admission_enquiry SET stage = $3, stage_history = $4, \
                 stage_entered_at = $5 WHERE id = $1 AND tenant_id = $2",
            )
            .bind(id)
            .bind(t.as_uuid())
            .bind(&applied)
            .bind(history_json(&history))
            .bind(now)
            .execute(&mut *tx)
            .await
            .map_err(map_db_error)?;
        }
    }
    sync_linked(&mut tx, t, inst, &settings).await?;
    let updated = load(&mut tx, t, id, false)
        .await?
        .finish(&stages, &settings, now_ms());
    publish::audit(
        &mut tx,
        &scope,
        "student.admission_enquiry.link_application",
        "admission_enquiry",
        id.to_string(),
        serde_json::json!({ "applicationId": cmd.application_id, "stage": updated.stage }),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(respond(StatusCode::OK, &updated))
}

// ---------------------------------------------------------------- funnel report

/// Funnel filters.
#[derive(Debug, Clone, Default, serde::Deserialize, utoipa::IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct FunnelQuery {
    pub programme_id: Option<Uuid>,
    pub source: Option<String>,
    /// Cohort by first contact: `all` (default), `quarter`, `last_quarter`, `year` (calendar)
    /// or `fy` (the April–March financial year).
    pub window: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AdmissionFunnel {
    pub stages: Vec<Stage>,
    pub window: String,
    pub report: FunnelReport,
    pub summary: Vec<StageSummary>,
    pub generated_at: DateTime<Utc>,
}

/// First day of the cohort window containing `today`, and its end (exclusive).
fn window_bounds(window: &str, today: NaiveDate) -> Option<(NaiveDate, NaiveDate)> {
    let ymd = |y: i32, m: u32| NaiveDate::from_ymd_opt(y, m, 1);
    let q_start = ymd(today.year(), (today.month0() / 3) * 3 + 1)?;
    let shift = |d: NaiveDate, months: i32| {
        let total = d.year() * 12 + d.month0() as i32 + months;
        ymd(total.div_euclid(12), total.rem_euclid(12) as u32 + 1)
    };
    match window {
        "quarter" => Some((q_start, shift(q_start, 3)?)),
        "last_quarter" => Some((shift(q_start, -3)?, q_start)),
        "year" => Some((ymd(today.year(), 1)?, ymd(today.year() + 1, 1)?)),
        "fy" => {
            let y = if today.month() >= 4 {
                today.year()
            } else {
                today.year() - 1
            };
            Some((ymd(y, 4)?, ymd(y + 1, 4)?))
        }
        _ => None,
    }
}

async fn tz_of(
    st: &StudentState,
    scope: &sutra_kernel::scope::ExecutionScope,
    inst: InstitutionId,
) -> Result<String, KernelError> {
    Ok(st
        .references
        .institution(scope, inst)
        .await?
        .ok_or(KernelError::NotFound {
            entity: "institution",
        })?
        .timezone)
}

/// Offset of `tz` from UTC right now, in minutes (for month keys of close times).
async fn offset_minutes(conn: &mut PgConnection, tz: &str) -> Result<i32, KernelError> {
    let secs: f64 = sqlx::query_scalar(
        "SELECT extract(epoch FROM (now() AT TIME ZONE $1) - (now() AT TIME ZONE 'UTC'))::float8",
    )
    .bind(tz)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok((secs / 60.0).round() as i32)
}

/// The admissions funnel (`student.read`): per stage entered, advanced, dropped, still open.
#[utoipa::path(
    get,
    path = "/api/v1/student/institutions/{id}/admission-funnel",
    operation_id = "admission_funnel_report",
    tag = "admissions",
    params(("id" = String, Path, format = Uuid), FunnelQuery),
    security(("session_cookie" = [])),
    responses((status = 200, body = AdmissionFunnel), ReadErrors)
)]
pub async fn report(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<InstitutionId>,
    Query(q): Query<FunnelQuery>,
) -> ApiResult<Json<AdmissionFunnel>> {
    let ctx = auth.require(Permission::StudentRead)?;
    let t = tenant(ctx);
    let scope = institution_scope(ctx, id);
    let window = q.window.clone().unwrap_or_else(|| "all".into());
    if !matches!(
        window.as_str(),
        "all" | "quarter" | "last_quarter" | "year" | "fy"
    ) {
        return Err(invalid(
            "window",
            "unknown",
            "window must be all, quarter, last_quarter, year or fy",
        )
        .into());
    }
    let today = business_today(&st, &scope, id).await?;
    let tz = tz_of(&st, &scope, id).await?;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let settings = load_settings(&mut tx, t, id).await?;
    sync_linked(&mut tx, t, id, &settings).await?;
    let rows = load_all(&mut tx, t, id, q.programme_id).await?;
    let offset = offset_minutes(&mut tx, &tz).await?;
    tx.commit().await.map_err(map_db_error)?;
    let bounds = window_bounds(&window, today);
    let enquiries: Vec<Enquiry> = rows
        .iter()
        .filter(|r| q.source.as_deref().is_none_or(|s| r.wire.source == s))
        .map(Row_::enquiry)
        .filter(|e| {
            bounds.is_none_or(|(from, to)| {
                let at = funnel::entered_at(e) + f64::from(offset) * 60_000.0;
                let d = from_ms(at).date_naive();
                d >= from && d < to
            })
        })
        .collect();
    let stages = settings.stages();
    Ok(Json(AdmissionFunnel {
        report: funnel_report(&enquiries, &stages),
        summary: stage_summary(&enquiries, &HashMap::new(), &stages),
        stages,
        window,
        generated_at: Utc::now(),
    }))
}

// ---------------------------------------------------------------- forecast

#[derive(Debug, Clone, Default, serde::Deserialize, utoipa::IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct ForecastQuery {
    /// Months to forecast (1–24, default 6).
    pub months: Option<u32>,
    /// First month `YYYY-MM` (default: this month).
    pub from: Option<String>,
    pub programme_id: Option<Uuid>,
}

/// One month of the joining forecast (amounts in rupees, to paise).
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ForecastRow {
    pub key: String,
    pub count: u32,
    pub expected_joiners: f64,
    pub best: String,
    pub weighted: String,
    pub admitted: String,
}

/// Seat fill of one programme and intake.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SeatFill {
    pub programme_id: Uuid,
    pub intake: String,
    pub seats: i32,
    pub admitted: u32,
    pub open: u32,
    /// Probability-weighted joiners among the open enquiries.
    pub expected: f64,
    pub remaining: i32,
    /// Expected joiners over remaining seats (`null` when the intake is full).
    pub coverage: Option<f64>,
}

/// An enquiry that has not moved for the stale threshold.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct StalledRef {
    pub id: Uuid,
    pub enquiry_number: String,
    pub name: String,
    pub stage: String,
    pub days_in_stage: i64,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AdmissionForecast {
    pub months: Vec<ForecastRow>,
    pub pipeline: String,
    pub weighted: String,
    pub win_rate: WinRate,
    pub by_source: Vec<GroupRate>,
    pub by_programme: Vec<GroupRate>,
    pub reasons_admitted: ReasonBreakdown,
    pub reasons_lost: ReasonBreakdown,
    pub avg_closed: ClosedValues,
    pub velocity: Vec<StageVelocity>,
    pub stalled: Vec<StalledRef>,
    pub avg_cycle_days: Option<i64>,
    pub seats: Vec<SeatFill>,
    pub stale_days: i64,
}

fn rupees(v: f64) -> String {
    format!("{v:.2}")
}

/// Joining forecast, conversion by source and programme, velocity, stalled enquiries and seat
/// fill against intake targets (`student.read`).
#[utoipa::path(
    get,
    path = "/api/v1/student/institutions/{id}/admission-forecast",
    operation_id = "admission_funnel_forecast",
    tag = "admissions",
    params(("id" = String, Path, format = Uuid), ForecastQuery),
    security(("session_cookie" = [])),
    responses((status = 200, body = AdmissionForecast), ReadErrors)
)]
pub async fn forecast(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<InstitutionId>,
    Query(q): Query<ForecastQuery>,
) -> ApiResult<Json<AdmissionForecast>> {
    let ctx = auth.require(Permission::StudentRead)?;
    let t = tenant(ctx);
    let scope = institution_scope(ctx, id);
    let months = q.months.unwrap_or(6);
    if !(1..=24).contains(&months) {
        return Err(invalid("months", "range", "months must be 1 to 24").into());
    }
    let today = business_today(&st, &scope, id).await?;
    let now_key = format!("{:04}-{:02}", today.year(), today.month());
    let from = match q.from.as_deref() {
        None => now_key.clone(),
        Some(f) => {
            let mut v = Vec::new();
            validate_intake("from", Some(f), &mut v);
            if f.len() != 7 {
                v.push(FieldViolation::new(
                    "from",
                    "format",
                    "from must be YYYY-MM",
                ));
            }
            finish(v)?;
            f.to_owned()
        }
    };
    let tz = tz_of(&st, &scope, id).await?;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let settings = load_settings(&mut tx, t, id).await?;
    sync_linked(&mut tx, t, id, &settings).await?;
    let rows = load_all(&mut tx, t, id, q.programme_id).await?;
    let offset = offset_minutes(&mut tx, &tz).await?;
    let targets: Vec<(Uuid, String, i32)> = sqlx::query_as(
        "SELECT programme_id, intake, seats FROM student.admission_intake_target \
         WHERE tenant_id = $1 AND institution_id = $2 AND ($3::uuid IS NULL OR programme_id = $3) \
         ORDER BY intake, programme_id",
    )
    .bind(t.as_uuid())
    .bind(id.as_uuid())
    .bind(q.programme_id)
    .fetch_all(&mut *tx)
    .await
    .map_err(map_db_error)?;
    tx.commit().await.map_err(map_db_error)?;
    let stages = settings.stages();
    let none = HashMap::new();
    let enquiries: Vec<Enquiry> = rows.iter().map(Row_::enquiry).collect();
    let fc = forecast_by_month(&enquiries, months, &from, &now_key, offset, &none, &stages);
    let now = now_ms();
    let by_id: HashMap<String, &Row_> = rows.iter().map(|r| (r.wire.id.to_string(), r)).collect();
    let programme_of = |e: &Enquiry| {
        by_id
            .get(&e.id)
            .and_then(|r| r.wire.programme_id)
            .map(|p| p.to_string())
    };
    let seats = targets
        .into_iter()
        .map(|(programme, intake, seats)| {
            let mine: Vec<(&Row_, Enquiry)> = rows
                .iter()
                .filter(|r| {
                    r.wire.programme_id == Some(programme)
                        && r.wire
                            .intake
                            .as_deref()
                            .is_some_and(|i| i.starts_with(&intake) || intake.starts_with(i))
                })
                .map(|r| (r, r.enquiry()))
                .collect();
            let admitted = mine.iter().filter(|(_, e)| e.stage == ADMITTED).count() as u32;
            let open: Vec<&Enquiry> = mine
                .iter()
                .map(|(_, e)| e)
                .filter(|e| e.is_open())
                .collect();
            let expected = funnel::round2(
                open.iter()
                    .fold(0.0, |s, e| s + probability_of(e, &none, &stages)),
            );
            let remaining = (seats - admitted as i32).max(0);
            SeatFill {
                programme_id: programme,
                intake,
                seats,
                admitted,
                open: open.len() as u32,
                expected,
                remaining,
                coverage: pipeline_coverage(expected, f64::from(remaining)),
            }
        })
        .collect();
    Ok(Json(AdmissionForecast {
        months: fc
            .into_iter()
            .map(|r| ForecastRow {
                key: r.key,
                count: r.count,
                expected_joiners: r.weighted_count,
                best: rupees(r.best),
                weighted: rupees(r.weighted),
                admitted: rupees(r.won),
            })
            .collect(),
        pipeline: rupees(funnel::pipeline_value(&enquiries)),
        weighted: rupees(funnel::weighted_pipeline(&enquiries, &none, &stages)),
        win_rate: win_rate(&enquiries),
        by_source: win_rate_by(&enquiries, |e| e.source.clone()),
        by_programme: win_rate_by(&enquiries, programme_of),
        reasons_admitted: close_reason_breakdown(&enquiries, ADMITTED),
        reasons_lost: close_reason_breakdown(&enquiries, LOST),
        avg_closed: avg_closed_value(&enquiries),
        velocity: stage_velocity(&enquiries, &stages),
        stalled: funnel::stalled(&enquiries, settings.stale_days, now)
            .into_iter()
            .filter_map(|e| by_id.get(&e.id))
            .map(|r| StalledRef {
                id: r.wire.id,
                enquiry_number: r.wire.enquiry_number.clone(),
                name: r.wire.display_name.clone(),
                stage: r.wire.stage.clone(),
                days_in_stage: funnel::days_in_stage(&r.enquiry(), now),
            })
            .take(100)
            .collect(),
        avg_cycle_days: avg_cycle_days(&enquiries),
        seats,
        stale_days: settings.stale_days,
    }))
}

// ---------------------------------------------------------------- duplicates

/// A person record the duplicate scan reads.
struct Person {
    kind: String,
    id: Uuid,
    number: Option<String>,
    contact: Contact,
}

/// Open enquiries and in-flight applications of the institution.
async fn duplicate_universe(
    conn: &mut PgConnection,
    tenant: TenantId,
    inst: InstitutionId,
) -> Result<Vec<Person>, KernelError> {
    let rows = sqlx::query(
        "SELECT 'enquiry' AS kind, id, enquiry_number AS number, \
                btrim(concat_ws(' ', given_name, family_name)) AS name, email, \
                coalesce(phone, guardian_phone) AS phone, created_at \
           FROM student.admission_enquiry \
          WHERE tenant_id = $1 AND institution_id = $2 AND stage NOT IN ('admitted', 'lost') \
         UNION ALL \
         SELECT 'application', a.id, NULL, \
                btrim(concat_ws(' ', a.given_name, a.family_name)), a.email, a.phone, a.created_at \
           FROM student.admission_application a \
          WHERE a.tenant_id = $1 AND a.institution_id = $2 \
            AND a.status NOT IN ('rejected', 'withdrawn') AND a.admission_id IS NULL \
            AND NOT EXISTS (SELECT 1 FROM student.admission_enquiry e \
                             WHERE e.tenant_id = $1 AND e.application_id = a.id) \
          ORDER BY created_at LIMIT 5000",
    )
    .bind(tenant.as_uuid())
    .bind(inst.as_uuid())
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    rows.iter()
        .map(|r| {
            let kind: String = r.try_get("kind")?;
            let id: Uuid = r.try_get("id")?;
            let created: DateTime<Utc> = r.try_get("created_at")?;
            Ok(Person {
                contact: Contact {
                    id: format!("{kind}:{id}"),
                    name: r.try_get("name")?,
                    email: r.try_get::<Option<String>, _>("email")?.unwrap_or_default(),
                    phone: r.try_get::<Option<String>, _>("phone")?.unwrap_or_default(),
                    created_at: ms(created),
                },
                kind,
                id,
                number: r.try_get("number")?,
            })
        })
        .collect::<Result<_, sqlx::Error>>()
        .map_err(map_db_error)
}

/// One possible duplicate pair; `a` is the older record (the one to keep).
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DuplicatePairWire {
    pub a: DuplicateRef,
    pub b: DuplicateRef,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AdmissionDuplicates {
    pub pairs: Vec<DuplicatePairWire>,
    /// At most this many pairs are listed.
    pub limit: usize,
}

/// Possible duplicate enquirers across open enquiries and in-flight applications: same e-mail,
/// name or phone after normalisation (`student.read`).
#[utoipa::path(
    get,
    path = "/api/v1/student/institutions/{id}/admission-duplicates",
    operation_id = "admission_funnel_duplicates",
    tag = "admissions",
    params(("id" = String, Path, format = Uuid)),
    security(("session_cookie" = [])),
    responses((status = 200, body = AdmissionDuplicates), ReadErrors)
)]
pub async fn duplicates(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<InstitutionId>,
) -> ApiResult<Json<AdmissionDuplicates>> {
    let ctx = auth.require(Permission::StudentRead)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let people = duplicate_universe(&mut conn, tenant(ctx), id).await?;
    let contacts: Vec<Contact> = people.iter().map(|p| p.contact.clone()).collect();
    let refer = |c: &Contact, reasons: &[String]| {
        people
            .iter()
            .find(|p| p.contact.id == c.id)
            .map(|p| DuplicateRef {
                kind: p.kind.clone(),
                id: p.id,
                number: p.number.clone(),
                name: p.contact.name.clone(),
                reasons: reasons.to_vec(),
            })
    };
    let pairs = contact_duplicates(&contacts)
        .into_iter()
        .filter_map(|p| {
            let reasons: Vec<String> = p.reasons.iter().map(|r| (*r).to_owned()).collect();
            Some(DuplicatePairWire {
                a: refer(p.a, &reasons)?,
                b: refer(p.b, &reasons)?,
                reasons,
            })
        })
        .collect();
    Ok(Json(AdmissionDuplicates {
        pairs,
        limit: funnel::MAX_PAIRS,
    }))
}

// ---------------------------------------------------------------- settings

/// A seat target for one programme and intake.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IntakeTarget {
    pub programme_id: Uuid,
    /// `YYYY` or `YYYY-MM`.
    pub intake: String,
    pub seats: i32,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct FunnelSettings {
    /// The effective stage list (open stages, then Admitted and Lost).
    pub stages: Vec<Stage>,
    /// Whether the open stages are the institution's own (else the defaults).
    pub customised: bool,
    pub stale_days: i64,
    pub applied_stage: Option<String>,
    pub offered_stage: Option<String>,
    pub targets: Vec<IntakeTarget>,
    pub version: i64,
}

/// Replaces the funnel settings. `stages` empty restores the default funnel.
#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PutFunnelSettings {
    pub stages: Vec<Stage>,
    pub stale_days: i64,
    pub applied_stage: Option<String>,
    pub offered_stage: Option<String>,
    #[serde(default)]
    pub targets: Vec<IntakeTarget>,
    /// The version read (0 before anything was saved).
    pub version: i64,
}

async fn settings_wire(
    conn: &mut PgConnection,
    tenant: TenantId,
    inst: InstitutionId,
) -> Result<FunnelSettings, KernelError> {
    let s = load_settings(conn, tenant, inst).await?;
    let targets: Vec<(Uuid, String, i32)> = sqlx::query_as(
        "SELECT programme_id, intake, seats FROM student.admission_intake_target \
         WHERE tenant_id = $1 AND institution_id = $2 ORDER BY intake, programme_id",
    )
    .bind(tenant.as_uuid())
    .bind(inst.as_uuid())
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(FunnelSettings {
        stages: s.stages(),
        customised: !s.custom.is_empty(),
        stale_days: s.stale_days,
        applied_stage: s.applied_stage.clone(),
        offered_stage: s.offered_stage.clone(),
        targets: targets
            .into_iter()
            .map(|(programme_id, intake, seats)| IntakeTarget {
                programme_id,
                intake,
                seats,
            })
            .collect(),
        version: s.version,
    })
}

/// Funnel settings of an institution (`student.read`).
#[utoipa::path(
    get,
    path = "/api/v1/student/institutions/{id}/admission-funnel-settings",
    operation_id = "admission_funnel_settings_get",
    tag = "admissions",
    params(("id" = String, Path, format = Uuid)),
    security(("session_cookie" = [])),
    responses((status = 200, body = FunnelSettings), ReadErrors)
)]
pub async fn get_settings(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<InstitutionId>,
) -> ApiResult<Json<FunnelSettings>> {
    let ctx = auth.require(Permission::StudentRead)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    Ok(Json(settings_wire(&mut conn, tenant(ctx), id).await?))
}

fn validate_settings(cmd: &PutFunnelSettings) -> Result<(), KernelError> {
    let mut v = Vec::new();
    if cmd.stages.len() > funnel::MAX_OPEN_STAGES {
        v.push(FieldViolation::new(
            "stages",
            "too-many",
            format!("at most {} open stages", funnel::MAX_OPEN_STAGES),
        ));
    }
    let mut seen: Vec<&str> = Vec::new();
    for (i, s) in cmd.stages.iter().enumerate() {
        let f = format!("stages[{i}]");
        let id_ok = (2..=32).contains(&s.id.len())
            && s.id.as_bytes()[0].is_ascii_lowercase()
            && s.id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
        if !id_ok {
            v.push(FieldViolation::new(
                format!("{f}.id"),
                "format",
                "stage ids are snake_case, 2-32 characters",
            ));
        } else if funnel::is_terminal(&s.id) {
            v.push(FieldViolation::new(
                format!("{f}.id"),
                "reserved",
                "admitted and lost are always the last stages",
            ));
        } else if seen.contains(&s.id.as_str()) {
            v.push(FieldViolation::new(
                format!("{f}.id"),
                "duplicate",
                "stage ids must be unique",
            ));
        }
        seen.push(&s.id);
        validate_text(&format!("{f}.label"), &s.label, 60, &mut v);
        if !(0.0..=1.0).contains(&s.probability) {
            v.push(FieldViolation::new(
                format!("{f}.probability"),
                "range",
                "probability must be between 0 and 1",
            ));
        }
    }
    if !(0..=365).contains(&cmd.stale_days) {
        v.push(FieldViolation::new(
            "staleDays",
            "range",
            "staleDays must be 0 to 365",
        ));
    }
    let open: Vec<String> = if cmd.stages.is_empty() {
        funnel::default_open_stages()
            .into_iter()
            .map(|s| s.id)
            .collect()
    } else {
        cmd.stages.iter().map(|s| s.id.clone()).collect()
    };
    for (field, val) in [
        ("appliedStage", &cmd.applied_stage),
        ("offeredStage", &cmd.offered_stage),
    ] {
        if let Some(s) = val {
            if !open.contains(s) {
                v.push(FieldViolation::new(
                    field,
                    "unknown",
                    "must name one of the open stages",
                ));
            }
        }
    }
    let mut keys: Vec<(Uuid, &str)> = Vec::new();
    for (i, t) in cmd.targets.iter().enumerate() {
        validate_intake(&format!("targets[{i}].intake"), Some(&t.intake), &mut v);
        if !(0..=100_000).contains(&t.seats) {
            v.push(FieldViolation::new(
                format!("targets[{i}].seats"),
                "range",
                "seats must be 0 to 100000",
            ));
        }
        if keys.contains(&(t.programme_id, t.intake.as_str())) {
            v.push(FieldViolation::new(
                format!("targets[{i}]"),
                "duplicate",
                "one target per programme and intake",
            ));
        }
        keys.push((t.programme_id, &t.intake));
    }
    finish(v)
}

/// Replace the funnel settings and seat targets (`admission.manage`).
#[utoipa::path(
    put,
    path = "/api/v1/student/institutions/{id}/admission-funnel-settings",
    operation_id = "admission_funnel_settings_put",
    tag = "admissions",
    params(("id" = String, Path, format = Uuid)),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = PutFunnelSettings,
    responses((status = 200, body = FunnelSettings), CommandErrors)
)]
pub async fn put_settings(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<InstitutionId>,
    Json(cmd): Json<PutFunnelSettings>,
) -> ApiResult<Json<FunnelSettings>> {
    let ctx = auth.require(Permission::AdmissionManage)?;
    let scope = institution_scope(ctx, id);
    require_institution_writable(&st, &scope, id).await?;
    validate_settings(&cmd)?;
    for (i, tg) in cmd.targets.iter().enumerate() {
        refs::programme(
            &st,
            &scope,
            id,
            tg.programme_id,
            &format!("targets[{i}].programmeId"),
        )
        .await?;
    }
    let t = tenant(ctx);
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1))")
        .bind(format!(
            "student.admission_funnel_settings:{}",
            id.as_uuid()
        ))
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    let cur = load_settings(&mut tx, t, id).await?;
    check_version(Version(cmd.version), cur.version)?;
    let stages = serde_json::to_value(&cmd.stages).map_err(|e| KernelError::Internal(e.into()))?;
    sqlx::query(
        "INSERT INTO student.admission_funnel_settings (tenant_id, institution_id, stages, \
         stale_days, applied_stage, offered_stage, updated_by) VALUES ($1,$2,$3,$4,$5,$6,$7) \
         ON CONFLICT (tenant_id, institution_id) DO UPDATE SET stages = EXCLUDED.stages, \
         stale_days = EXCLUDED.stale_days, applied_stage = EXCLUDED.applied_stage, \
         offered_stage = EXCLUDED.offered_stage, updated_by = EXCLUDED.updated_by, \
         version = student.admission_funnel_settings.version + 1, updated_at = now()",
    )
    .bind(t.as_uuid())
    .bind(id.as_uuid())
    .bind(&stages)
    .bind(cmd.stale_days as i32)
    .bind(&cmd.applied_stage)
    .bind(&cmd.offered_stage)
    .bind(a.id.as_deref())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    sqlx::query(
        "DELETE FROM student.admission_intake_target WHERE tenant_id = $1 AND institution_id = $2",
    )
    .bind(t.as_uuid())
    .bind(id.as_uuid())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    for tg in &cmd.targets {
        sqlx::query(
            "INSERT INTO student.admission_intake_target (tenant_id, institution_id, programme_id, \
             intake, seats, updated_by) VALUES ($1,$2,$3,$4,$5,$6)",
        )
        .bind(t.as_uuid())
        .bind(id.as_uuid())
        .bind(tg.programme_id)
        .bind(&tg.intake)
        .bind(tg.seats)
        .bind(a.id.as_deref())
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    }
    publish::audit(
        &mut tx,
        &scope,
        "student.admission_funnel_settings.update",
        "admission_funnel_settings",
        id.as_uuid().to_string(),
        serde_json::json!({ "stages": stages, "staleDays": cmd.stale_days, "targets": cmd.targets.len() }),
        &a,
    )
    .await?;
    let out = settings_wire(&mut tx, t, id).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(Json(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_follow_calendar_and_fy() {
        let d = NaiveDate::from_ymd_opt(2026, 2, 10).unwrap_or_default();
        let ymd = |y, m, dd| NaiveDate::from_ymd_opt(y, m, dd).unwrap_or_default();
        assert_eq!(
            window_bounds("quarter", d),
            Some((ymd(2026, 1, 1), ymd(2026, 4, 1)))
        );
        assert_eq!(
            window_bounds("last_quarter", d),
            Some((ymd(2025, 10, 1), ymd(2026, 1, 1)))
        );
        assert_eq!(
            window_bounds("fy", d),
            Some((ymd(2025, 4, 1), ymd(2026, 4, 1)))
        );
        assert_eq!(window_bounds("all", d), None);
        let mut v = Vec::new();
        validate_intake("intake", Some("2026-13"), &mut v);
        validate_intake("intake", Some("2026"), &mut v);
        validate_intake("intake", Some("2026-07"), &mut v);
        assert_eq!(v.len(), 1);
    }
}
