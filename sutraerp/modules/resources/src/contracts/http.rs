//! HTTP layer of contracts and MoUs (rules in [`super::rules`], [`super::ical`] and
//! [`super::naac`]). Operation ids `resources_contract_*`, tag `resources-procurement`; reads
//! need `resources.read`, writes `resources.procurement.manage`.
//!
//! The term clock, renewal phase and "needs attention" are computed on every read from the
//! stored schedule and the institution's business date, never stored. Finished terms are written
//! by `contracts/reconcile-terms` (recorded once each; a term whose notice was given closes the
//! contract out). Deadlines are served as an iCalendar file for any calendar app.
#![allow(clippy::type_complexity, clippy::too_many_arguments)]

use super::ical::{self, DatedObligation, EventContext, EventOptions};
use super::naac::{self, Mou, MouTotals};
use super::rules::{
    self as r, Contract, ContractStats, Decision, DecisionKind, Obligation, Owner, RenewalInfo,
    Repeat, RepeatInput,
};
use crate::shared::guard::{require_institution, require_institution_writable};
use crate::shared::http::actor;
use crate::shared::scope::institution_scope;
use crate::shared::time::business_today;
use crate::shared::{Actor, ResourcesState};
use crate::suppliers::common::{audit_only, check_version, clean, institution_of, invalid};
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::PgConnection;
use std::collections::HashMap;
use std::str::FromStr;
use sutra_kernel::auth::Permission;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

/// Contract kinds: vendor agreements and MoUs.
pub const KINDS: [&str; 10] = [
    "vendor",
    "service",
    "amc",
    "lease",
    "license",
    "nda",
    "mou_academic",
    "mou_industry",
    "mou_research",
    "other",
];
const STATUSES: [&str; 3] = ["active", "draft", "terminated"];
const BILLINGS: [&str; 4] = ["one-time", "monthly", "quarterly", "annual"];
/// MoU activity kinds (NAAC 3.7: internship, on-the-job training, project work, student /
/// faculty exchange, collaborative research…).
pub const ACTIVITY_KINDS: [&str; 12] = [
    "internship",
    "on_job_training",
    "project_work",
    "student_exchange",
    "faculty_exchange",
    "collaborative_research",
    "workshop",
    "training",
    "consultancy",
    "placement",
    "publication",
    "other",
];

fn bad(field: &str, message: &str) -> KernelError {
    invalid(field, "invalid", message)
}

fn to_json<T: Serialize>(v: &T) -> Value {
    serde_json::to_value(v).unwrap_or_default()
}

// ------------------------------------------------------------------ settings

/// Contract settings of an institution.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ContractSettings {
    /// "Term ends soon" horizon (1…365 days; default 60).
    pub soon_days: i32,
    /// Obligations due within this many days show as due (default 30).
    pub obligation_days: i32,
    /// Notice period a new contract starts with (default 30).
    pub default_notice_days: i32,
    /// Calendar reminders before each deadline, in days (default 7 and 1).
    pub alarm_days: Vec<i32>,
}

impl Default for ContractSettings {
    fn default() -> Self {
        Self {
            soon_days: 60,
            obligation_days: 30,
            default_notice_days: 30,
            alarm_days: vec![7, 1],
        }
    }
}

async fn settings_of(
    conn: &mut PgConnection,
    t: TenantId,
    inst: Uuid,
) -> Result<ContractSettings, KernelError> {
    let row: Option<(i32, i32, i32, Vec<i32>)> = sqlx::query_as(
        "SELECT soon_days, obligation_days, default_notice_days, alarm_days FROM \
         resources.contract_settings WHERE tenant_id = $1 AND institution_id = $2",
    )
    .bind(t.as_uuid())
    .bind(inst)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(
        row.map_or_else(ContractSettings::default, |(a, b, c, d)| ContractSettings {
            soon_days: a,
            obligation_days: b,
            default_notice_days: c,
            alarm_days: d,
        }),
    )
}

/// Contract settings (`resources.read`; defaults until saved).
#[utoipa::path(get, path = "/api/v1/resources/institutions/{id}/contract-settings", operation_id = "resources_contract_settings_get", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = ContractSettings), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn settings_get(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<ContractSettings>> {
    let ctx = auth.require(Permission::ResourcesRead)?;
    let inst = InstitutionId(id);
    require_institution(&st, &institution_scope(ctx, inst), inst).await?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    Ok(Json(settings_of(&mut conn, ctx.scope.tenant_id, id).await?))
}

/// Save contract settings (`resources.procurement.manage`; audited).
#[utoipa::path(put, path = "/api/v1/resources/institutions/{id}/contract-settings", operation_id = "resources_contract_settings_put", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid)), request_body = ContractSettings, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ContractSettings), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn settings_put(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Json(mut b): Json<ContractSettings>,
) -> ApiResult<Json<ContractSettings>> {
    let ctx = auth.require(Permission::ProcurementManage)?;
    let inst = InstitutionId(id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    for (field, v, lo, hi) in [
        ("soonDays", b.soon_days, 1, 365),
        ("obligationDays", b.obligation_days, 1, 365),
        ("defaultNoticeDays", b.default_notice_days, 0, 730),
    ] {
        if v < lo || v > hi {
            return Err(invalid(field, "out_of_range", &format!("between {lo} and {hi}")).into());
        }
    }
    b.alarm_days.sort_unstable_by(|a, b| b.cmp(a));
    b.alarm_days.dedup();
    if b.alarm_days.len() > 4 || b.alarm_days.iter().any(|d| !(0..=60).contains(d)) {
        return Err(bad("alarmDays", "up to four reminders, 0…60 days before").into());
    }
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    sqlx::query(
        "INSERT INTO resources.contract_settings (tenant_id, institution_id, soon_days, \
         obligation_days, default_notice_days, alarm_days, updated_by) VALUES ($1, $2, $3, $4, $5, \
         $6, $7) ON CONFLICT (tenant_id, institution_id) DO UPDATE SET soon_days = \
         EXCLUDED.soon_days, obligation_days = EXCLUDED.obligation_days, default_notice_days = \
         EXCLUDED.default_notice_days, alarm_days = EXCLUDED.alarm_days, updated_by = \
         EXCLUDED.updated_by, version = resources.contract_settings.version + 1, updated_at = now()",
    )
    .bind(ctx.scope.tenant_id.as_uuid())
    .bind(id)
    .bind(b.soon_days)
    .bind(b.obligation_days)
    .bind(b.default_notice_days)
    .bind(&b.alarm_days)
    .bind(a.id.as_deref())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    audit_only(
        &mut tx,
        &scope,
        "contract_settings",
        id,
        "resources.contract_settings.update",
        to_json(&b),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(Json(b))
}

// ------------------------------------------------------------------ rows

const CONTRACT_COLUMNS: &str = "id, institution_id, title, kind, status, supplier_id, \
    counterparty_name, counterparty_scope, contact_name, contact_email, contact_phone, value, \
    currency, billing, start_on, end_on, auto_renew, term_months, notice_days, purposes, tags, \
    notes, decision, version, updated_at";

#[derive(Debug, Clone, sqlx::FromRow)]
struct ContractRow {
    id: Uuid,
    institution_id: Uuid,
    title: String,
    kind: String,
    status: String,
    supplier_id: Option<Uuid>,
    counterparty_name: String,
    counterparty_scope: String,
    contact_name: Option<String>,
    contact_email: Option<String>,
    contact_phone: Option<String>,
    value: Option<Decimal>,
    currency: String,
    billing: String,
    start_on: Option<NaiveDate>,
    end_on: Option<NaiveDate>,
    auto_renew: bool,
    term_months: i32,
    notice_days: i32,
    purposes: Vec<String>,
    tags: Vec<String>,
    notes: Option<String>,
    decision: Option<Value>,
    version: i64,
    updated_at: DateTime<Utc>,
}

impl ContractRow {
    fn decision(&self) -> Option<Decision> {
        self.decision
            .clone()
            .and_then(|v| serde_json::from_value(v).ok())
    }
    fn rules(&self) -> Contract {
        Contract {
            status: self.status.clone(),
            start_date: self.start_on,
            end_date: self.end_on,
            auto_renew: self.auto_renew,
            term_months: self.term_months,
            notice_days: i64::from(self.notice_days),
            decision: self.decision(),
            value: self.value.and_then(|v| v.to_f64()),
            currency: self.currency.clone(),
            billing: self.billing.clone(),
        }
    }
}

async fn contract_row(
    conn: &mut PgConnection,
    t: TenantId,
    id: Uuid,
) -> Result<ContractRow, KernelError> {
    sqlx::query_as::<_, ContractRow>(&format!(
        "SELECT {CONTRACT_COLUMNS} FROM resources.contract WHERE id = $1 AND tenant_id = $2"
    ))
    .bind(id)
    .bind(t.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound { entity: "contract" })
}

async fn contracts_of(
    conn: &mut PgConnection,
    t: TenantId,
    inst: Uuid,
) -> Result<Vec<ContractRow>, KernelError> {
    sqlx::query_as::<_, ContractRow>(&format!(
        "SELECT {CONTRACT_COLUMNS} FROM resources.contract WHERE tenant_id = $1 AND \
         institution_id = $2 ORDER BY title, id"
    ))
    .bind(t.as_uuid())
    .bind(inst)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct ObRow {
    id: Uuid,
    contract_id: Uuid,
    text: String,
    owner: String,
    due_on: Option<NaiveDate>,
    done: bool,
    repeat: Option<Value>,
    history: Value,
    version: i64,
}

impl ObRow {
    fn repeat(&self) -> Option<Repeat> {
        self.repeat
            .clone()
            .and_then(|v| serde_json::from_value(v).ok())
    }
    fn history(&self) -> Vec<Value> {
        self.history.as_array().cloned().unwrap_or_default()
    }
    fn schedule(&self) -> Obligation {
        Obligation {
            due: self.due_on,
            done: self.done,
            repeat: self.repeat(),
            history: self
                .history()
                .iter()
                .map(|h| h.get("due").and_then(Value::as_str).and_then(r::parse_iso))
                .collect(),
        }
    }
}

const OB_COLUMNS: &str = "id, contract_id, text, owner, due_on, done, repeat, history, version";

async fn obligations_of(
    conn: &mut PgConnection,
    t: TenantId,
    contracts: &[Uuid],
) -> Result<Vec<ObRow>, KernelError> {
    sqlx::query_as::<_, ObRow>(&format!(
        "SELECT {OB_COLUMNS} FROM resources.contract_obligation WHERE tenant_id = $1 AND \
         contract_id = ANY($2) ORDER BY created_at, id"
    ))
    .bind(t.as_uuid())
    .bind(contracts)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)
}

async fn obligation_row(
    conn: &mut PgConnection,
    t: TenantId,
    id: Uuid,
) -> Result<ObRow, KernelError> {
    sqlx::query_as::<_, ObRow>(&format!(
        "SELECT {OB_COLUMNS} FROM resources.contract_obligation WHERE id = $1 AND tenant_id = $2"
    ))
    .bind(id)
    .bind(t.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "contract obligation",
    })
}

async fn term_ends(
    conn: &mut PgConnection,
    t: TenantId,
    contract: Uuid,
) -> Result<Vec<NaiveDate>, KernelError> {
    sqlx::query_scalar(
        "SELECT end_on FROM resources.contract_term WHERE tenant_id = $1 AND contract_id = $2 \
         ORDER BY end_on",
    )
    .bind(t.as_uuid())
    .bind(contract)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)
}

// ------------------------------------------------------------------ views

/// A contract or MoU with its clock.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ContractView {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub title: String,
    /// `vendor`, `service`, `amc`, `lease`, `license`, `nda`, `mou_academic`, `mou_industry`,
    /// `mou_research`, `other`.
    pub kind: String,
    /// `active`, `draft`, `terminated`.
    pub status: String,
    pub supplier_id: Option<Uuid>,
    pub counterparty_name: String,
    /// `national` or `international`.
    pub counterparty_scope: String,
    pub contact_name: Option<String>,
    pub contact_email: Option<String>,
    pub contact_phone: Option<String>,
    #[schema(value_type = Option<String>, example = "120000.00")]
    pub value: Option<Decimal>,
    pub currency: String,
    /// `one-time`, `monthly`, `quarterly`, `annual`.
    pub billing: String,
    /// Value as a yearly figure (0 for one-time or unpriced).
    pub annual_value: f64,
    pub start_on: Option<NaiveDate>,
    /// End of the first term: every renewal is counted from it.
    pub end_on: Option<NaiveDate>,
    pub auto_renew: bool,
    pub term_months: i32,
    pub notice_days: i32,
    /// What an MoU is for (internship, research…).
    pub purposes: Vec<String>,
    pub tags: Vec<String>,
    pub notes: Option<String>,
    pub renewal: RenewalInfo,
    /// Waiting on a decision: in the notice window or past its end with nothing recorded.
    pub attention: bool,
    /// When the agreement itself stops (null = no known end).
    pub agreement_end: Option<NaiveDate>,
    pub open_obligations: i64,
    pub version: i64,
}

fn contract_view(c: &ContractRow, open: i64, today: NaiveDate, soon: i64) -> ContractView {
    let rules = c.rules();
    let renewal = r::renewal_info(&rules, today, soon);
    ContractView {
        id: c.id,
        institution_id: c.institution_id,
        title: c.title.clone(),
        kind: c.kind.clone(),
        status: c.status.clone(),
        supplier_id: c.supplier_id,
        counterparty_name: c.counterparty_name.clone(),
        counterparty_scope: c.counterparty_scope.clone(),
        contact_name: c.contact_name.clone(),
        contact_email: c.contact_email.clone(),
        contact_phone: c.contact_phone.clone(),
        value: c.value,
        currency: c.currency.clone(),
        billing: c.billing.clone(),
        annual_value: r::annualized_value(&rules),
        start_on: c.start_on,
        end_on: c.end_on,
        auto_renew: c.auto_renew,
        term_months: c.term_months,
        notice_days: c.notice_days,
        purposes: c.purposes.clone(),
        tags: c.tags.clone(),
        notes: c.notes.clone(),
        attention: r::needs_attention(&rules, &renewal),
        agreement_end: r::agreement_end(&rules, today),
        renewal,
        open_obligations: open,
        version: c.version,
    }
}

/// An obligation: who owes what, when.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ContractObligationView {
    pub id: Uuid,
    pub contract_id: Uuid,
    pub text: String,
    /// `us`, `them` or `mutual`.
    pub owner: String,
    /// The live occurrence (the last one completed once a series is finished).
    pub due_on: Option<NaiveDate>,
    pub done: bool,
    pub repeat: Option<Repeat>,
    /// Occurrences completed so far.
    pub completed: usize,
    /// Days until due (negative = overdue; null = undated).
    pub days_left: Option<i64>,
    pub version: i64,
}

fn obligation_view(o: &ObRow, today: NaiveDate) -> ContractObligationView {
    ContractObligationView {
        id: o.id,
        contract_id: o.contract_id,
        text: o.text.clone(),
        owner: o.owner.clone(),
        due_on: o.due_on,
        done: o.done,
        repeat: o.repeat(),
        completed: o.history().len(),
        days_left: o.due_on.map(|d| r::days_between(today, d)),
        version: o.version,
    }
}

/// A recorded term.
#[derive(Debug, Clone, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct ContractTermView {
    pub start_on: Option<NaiveDate>,
    pub end_on: NaiveDate,
    /// `renewed`, `notice-given`, `expired`, `terminated`.
    pub ended: String,
    #[schema(value_type = Option<String>)]
    pub value: Option<Decimal>,
    pub currency: String,
    pub note: String,
}

/// Something that happened under a contract or MoU.
#[derive(Debug, Clone, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct ContractActivityView {
    pub id: Uuid,
    pub contract_id: Uuid,
    pub held_on: NaiveDate,
    pub kind: String,
    pub title: String,
    pub students: i32,
    pub staff: i32,
    pub outcome: Option<String>,
    pub evidence_ref: Option<String>,
}

const ACTIVITY_COLUMNS: &str =
    "id, contract_id, held_on, kind, title, students, staff, outcome, evidence_ref";

/// A contract with its obligations, term history and activities.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ContractDetail {
    pub contract: ContractView,
    pub obligations: Vec<ContractObligationView>,
    pub terms: Vec<ContractTermView>,
    pub activities: Vec<ContractActivityView>,
}

async fn detail(
    conn: &mut PgConnection,
    t: TenantId,
    c: &ContractRow,
    today: NaiveDate,
    soon: i64,
) -> Result<ContractDetail, KernelError> {
    let mut obs = obligations_of(conn, t, &[c.id]).await?;
    let rows: Vec<(usize, &ObRow)> = obs.iter().enumerate().collect();
    let scheds: Vec<Obligation> = rows.iter().map(|(_, o)| o.schedule()).collect();
    let rules = c.rules();
    let order = r::all_obligations(&[(&rules, &scheds)], today, true);
    // Inactive contracts list nothing from all_obligations; keep stored order then.
    if !order.is_empty() {
        let mut sorted = Vec::with_capacity(obs.len());
        for (_, oi, _) in order {
            sorted.push(obs[oi].clone());
        }
        obs = sorted;
    }
    let open = obs.iter().filter(|o| !o.done).count() as i64;
    let terms = sqlx::query_as::<_, ContractTermView>(
        "SELECT start_on, end_on, ended, value, currency, note FROM resources.contract_term \
         WHERE tenant_id = $1 AND contract_id = $2 ORDER BY end_on",
    )
    .bind(t.as_uuid())
    .bind(c.id)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let activities = sqlx::query_as::<_, ContractActivityView>(&format!(
        "SELECT {ACTIVITY_COLUMNS} FROM resources.contract_activity WHERE tenant_id = $1 AND \
         contract_id = $2 ORDER BY held_on DESC, id"
    ))
    .bind(t.as_uuid())
    .bind(c.id)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(ContractDetail {
        contract: contract_view(c, open, today, soon),
        obligations: obs.iter().map(|o| obligation_view(o, today)).collect(),
        terms,
        activities,
    })
}

// ------------------------------------------------------------------ contracts

/// A contract or MoU as written.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ContractInput {
    pub title: String,
    pub kind: String,
    /// Default `active`.
    pub status: Option<String>,
    /// A registered supplier (its legal name is the counterparty unless one is given).
    pub supplier_id: Option<Uuid>,
    pub counterparty_name: Option<String>,
    /// `national` (default) or `international`.
    pub counterparty_scope: Option<String>,
    pub contact_name: Option<String>,
    pub contact_email: Option<String>,
    pub contact_phone: Option<String>,
    /// Money as a string (`"120000.00"`).
    pub value: Option<String>,
    /// ISO code, default `INR`.
    pub currency: Option<String>,
    pub billing: Option<String>,
    pub start_on: Option<NaiveDate>,
    pub end_on: Option<NaiveDate>,
    #[serde(default)]
    pub auto_renew: bool,
    /// Renewal term (1…120 months; default 12).
    pub term_months: Option<i32>,
    /// Days of notice to cancel before a term end (default from settings).
    pub notice_days: Option<i32>,
    #[serde(default)]
    pub purposes: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    pub notes: Option<String>,
    /// Required on update.
    pub version: Option<i64>,
}

struct Clean {
    title: String,
    kind: String,
    status: String,
    supplier_id: Option<Uuid>,
    counterparty_name: String,
    counterparty_scope: String,
    contact_name: Option<String>,
    contact_email: Option<String>,
    contact_phone: Option<String>,
    value: Option<Decimal>,
    currency: String,
    billing: String,
    start_on: Option<NaiveDate>,
    end_on: Option<NaiveDate>,
    auto_renew: bool,
    term_months: i32,
    notice_days: i32,
    purposes: Vec<String>,
    tags: Vec<String>,
    notes: Option<String>,
}

fn short(field: &str, v: Option<String>, max: usize) -> Result<Option<String>, KernelError> {
    let v = clean(v);
    if v.as_deref().is_some_and(|s| s.chars().count() > max) {
        return Err(invalid(
            field,
            "too_long",
            &format!("at most {max} characters"),
        ));
    }
    Ok(v)
}

fn words(field: &str, v: Vec<String>) -> Result<Vec<String>, KernelError> {
    let mut out: Vec<String> = vec![];
    for w in v {
        let w = w.trim().to_owned();
        if w.is_empty() || out.contains(&w) {
            continue;
        }
        if w.chars().count() > 40 {
            return Err(invalid(field, "too_long", "each at most 40 characters"));
        }
        out.push(w);
    }
    if out.len() > 12 {
        return Err(invalid(field, "too_many", "at most 12"));
    }
    Ok(out)
}

async fn validate(
    conn: &mut PgConnection,
    t: TenantId,
    inst: Uuid,
    b: ContractInput,
    default_notice: i32,
) -> Result<Clean, KernelError> {
    let title = b.title.trim().to_owned();
    if title.is_empty() || title.chars().count() > 200 {
        return Err(bad("title", "1…200 characters"));
    }
    if !KINDS.contains(&b.kind.as_str()) {
        return Err(bad("kind", &format!("one of {}", KINDS.join(", "))));
    }
    let status = b.status.unwrap_or_else(|| "active".into());
    if !STATUSES.contains(&status.as_str()) {
        return Err(bad("status", "active, draft or terminated"));
    }
    let mut name = short("counterpartyName", b.counterparty_name, 200)?;
    if let Some(sid) = b.supplier_id {
        let row: Option<(Uuid, String)> = sqlx::query_as(
            "SELECT institution_id, legal_name FROM resources.supplier WHERE id = $1 AND \
             tenant_id = $2",
        )
        .bind(sid)
        .bind(t.as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?;
        match row {
            Some((i, legal)) if i == inst => {
                name = name.or(Some(legal));
            }
            _ => return Err(bad("supplierId", "not a supplier of this institution")),
        }
    }
    let counterparty_name = name.ok_or_else(|| bad("counterpartyName", "required"))?;
    let scope = b.counterparty_scope.unwrap_or_else(|| "national".into());
    if scope != "national" && scope != "international" {
        return Err(bad("counterpartyScope", "national or international"));
    }
    let value = match clean(b.value) {
        None => None,
        Some(v) => {
            let d = Decimal::from_str(&v).map_err(|_| bad("value", "a number like 120000.00"))?;
            if d < Decimal::ZERO || d.scale() > 2 {
                return Err(bad("value", "not negative, at most two decimals"));
            }
            Some(d)
        }
    };
    let currency = b
        .currency
        .map_or("INR".into(), |c| r::normalize_currency(&c, ""));
    if currency.is_empty() {
        return Err(bad("currency", "a three-letter code"));
    }
    let billing = b.billing.unwrap_or_else(|| "one-time".into());
    if !BILLINGS.contains(&billing.as_str()) {
        return Err(bad("billing", "one-time, monthly, quarterly or annual"));
    }
    if let (Some(s), Some(e)) = (b.start_on, b.end_on) {
        if e < s {
            return Err(bad("endOn", "on or after the start"));
        }
    }
    let term_months = b.term_months.unwrap_or(12);
    if !(1..=120).contains(&term_months) {
        return Err(invalid("termMonths", "out_of_range", "1…120 months"));
    }
    if b.auto_renew && b.end_on.is_none() {
        return Err(bad(
            "endOn",
            "an auto-renewing contract needs its first term's end",
        ));
    }
    let notice_days = b.notice_days.unwrap_or(default_notice);
    if !(0..=730).contains(&notice_days) {
        return Err(invalid("noticeDays", "out_of_range", "0…730 days"));
    }
    Ok(Clean {
        title,
        kind: b.kind,
        status,
        supplier_id: b.supplier_id,
        counterparty_name,
        counterparty_scope: scope,
        contact_name: short("contactName", b.contact_name, 120)?,
        contact_email: short("contactEmail", b.contact_email, 200)?,
        contact_phone: short("contactPhone", b.contact_phone, 40)?,
        value,
        currency,
        billing,
        start_on: b.start_on,
        end_on: b.end_on,
        auto_renew: b.auto_renew,
        term_months,
        notice_days,
        purposes: words("purposes", b.purposes)?,
        tags: words("tags", b.tags)?,
        notes: short("notes", b.notes, 4000)?,
    })
}

/// Query of the contract list.
#[derive(Debug, Clone, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct ContractListQuery {
    /// A kind, or `mou` for every MoU.
    pub kind: Option<String>,
    pub status: Option<String>,
    /// Only the contracts waiting on a decision.
    pub attention: Option<bool>,
    /// Matches title, counterparty, tags and notes.
    pub q: Option<String>,
}

/// Contracts and MoUs of an institution, soonest term end first (`resources.read`).
#[utoipa::path(get, path = "/api/v1/resources/institutions/{id}/contracts", operation_id = "resources_contract_list", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid), ContractListQuery), security(("session_cookie" = [])),
    responses((status = 200, body = Vec<ContractView>), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn list(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<ContractListQuery>,
) -> ApiResult<Json<Vec<ContractView>>> {
    let ctx = auth.require(Permission::ResourcesRead)?;
    let inst = InstitutionId(id);
    let scope = institution_scope(ctx, inst);
    require_institution(&st, &scope, inst).await?;
    let today = business_today(&st, &scope, inst).await?;
    let t = ctx.scope.tenant_id;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let settings = settings_of(&mut conn, t, id).await?;
    let rows = contracts_of(&mut conn, t, id).await?;
    let ids: Vec<Uuid> = rows.iter().map(|c| c.id).collect();
    let mut open: HashMap<Uuid, i64> = HashMap::new();
    for o in obligations_of(&mut conn, t, &ids).await? {
        if !o.done {
            *open.entry(o.contract_id).or_default() += 1;
        }
    }
    let needle = clean(q.q).map(|s| s.to_lowercase());
    let mut out: Vec<ContractView> = rows
        .iter()
        .filter(|c| match q.kind.as_deref() {
            Some("mou") => naac::is_mou(&c.kind),
            Some(k) => c.kind == k,
            None => true,
        })
        .filter(|c| q.status.as_deref().is_none_or(|s| c.status == s))
        .filter(|c| {
            needle.as_deref().is_none_or(|n| {
                [
                    c.title.as_str(),
                    c.counterparty_name.as_str(),
                    c.notes.as_deref().unwrap_or(""),
                    &c.tags.join(" "),
                ]
                .join(" ")
                .to_lowercase()
                .contains(n)
            })
        })
        .map(|c| {
            let n = open.get(&c.id).copied().unwrap_or(0);
            contract_view(c, n, today, i64::from(settings.soon_days))
        })
        .filter(|v| !q.attention.unwrap_or(false) || v.attention)
        .collect();
    out.sort_by(|a, b| match (a.renewal.date, b.renewal.date) {
        (Some(x), Some(y)) => x.cmp(&y),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => a.title.cmp(&b.title),
    });
    Ok(Json(out))
}

/// Add a contract or MoU (`resources.procurement.manage`; Idempotency-Key).
#[utoipa::path(post, path = "/api/v1/resources/institutions/{id}/contracts", operation_id = "resources_contract_create", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = ContractInput, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = ContractDetail), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn create(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ContractInput>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ProcurementManage)?;
    require_idempotency_key(&headers)?;
    let inst = InstitutionId(id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let today = business_today(&st, &scope, inst).await?;
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let settings = settings_of(&mut tx, t, id).await?;
    let c = validate(&mut tx, t, id, b, settings.default_notice_days).await?;
    let cid: Uuid = sqlx::query_scalar(
        "INSERT INTO resources.contract (tenant_id, institution_id, title, kind, status, \
         supplier_id, counterparty_name, counterparty_scope, contact_name, contact_email, \
         contact_phone, value, currency, billing, start_on, end_on, auto_renew, term_months, \
         notice_days, purposes, tags, notes, created_by) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, \
         $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20, $21, $22, $23) RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(id)
    .bind(&c.title)
    .bind(&c.kind)
    .bind(&c.status)
    .bind(c.supplier_id)
    .bind(&c.counterparty_name)
    .bind(&c.counterparty_scope)
    .bind(&c.contact_name)
    .bind(&c.contact_email)
    .bind(&c.contact_phone)
    .bind(c.value)
    .bind(&c.currency)
    .bind(&c.billing)
    .bind(c.start_on)
    .bind(c.end_on)
    .bind(c.auto_renew)
    .bind(c.term_months)
    .bind(c.notice_days)
    .bind(&c.purposes)
    .bind(&c.tags)
    .bind(&c.notes)
    .bind(a.id.as_deref())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let row = contract_row(&mut tx, t, cid).await?;
    let out = detail(&mut tx, t, &row, today, i64::from(settings.soon_days)).await?;
    audit_only(
        &mut tx,
        &scope,
        "contract",
        cid,
        "resources.contract.create",
        json!({"title": c.title, "kind": c.kind, "counterparty": c.counterparty_name}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok((StatusCode::CREATED, Json(out)).into_response())
}

/// A contract with its obligations, terms and activities (`resources.read`).
#[utoipa::path(get, path = "/api/v1/resources/contracts/{id}", operation_id = "resources_contract_get", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = ContractDetail), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn get(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<ContractDetail>> {
    let ctx = auth.require(Permission::ResourcesRead)?;
    let t = ctx.scope.tenant_id;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let row = contract_row(&mut conn, t, id).await?;
    let inst = InstitutionId(row.institution_id);
    let scope = institution_scope(ctx, inst);
    require_institution(&st, &scope, inst).await?;
    let today = business_today(&st, &scope, inst).await?;
    let settings = settings_of(&mut conn, t, row.institution_id).await?;
    Ok(Json(
        detail(&mut conn, t, &row, today, i64::from(settings.soon_days)).await?,
    ))
}

/// Reopen finished "until the contract ends" series an edit gave room again.
async fn reopen_series(
    conn: &mut PgConnection,
    t: TenantId,
    c: &ContractRow,
    today: NaiveDate,
) -> Result<(), KernelError> {
    let rules = c.rules();
    for o in obligations_of(conn, t, &[c.id]).await? {
        if !o.done || o.repeat.is_none() {
            continue;
        }
        let s = o.schedule();
        let n = r::reopened_series(&rules, &s, today);
        if n != s {
            sqlx::query(
                "UPDATE resources.contract_obligation SET done = $3, due_on = $4, version = \
                 version + 1, updated_at = now() WHERE id = $1 AND tenant_id = $2",
            )
            .bind(o.id)
            .bind(t.as_uuid())
            .bind(n.done)
            .bind(n.due)
            .execute(&mut *conn)
            .await
            .map_err(map_db_error)?;
        }
    }
    Ok(())
}

/// Change a contract (`resources.procurement.manage`; `version` required). A recorded decision
/// stays with the term end it answered; a finished series the change gives room again resumes.
#[utoipa::path(put, path = "/api/v1/resources/contracts/{id}", operation_id = "resources_contract_update", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid)), request_body = ContractInput, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ContractDetail), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn update(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Json(b): Json<ContractInput>,
) -> ApiResult<Json<ContractDetail>> {
    let ctx = auth.require(Permission::ProcurementManage)?;
    let t = ctx.scope.tenant_id;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let old = contract_row(&mut tx, t, id).await?;
    let inst = InstitutionId(old.institution_id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let today = business_today(&st, &scope, inst).await?;
    let expected = b.version.ok_or_else(|| bad("version", "required"))?;
    check_version(expected, old.version)?;
    let settings = settings_of(&mut tx, t, old.institution_id).await?;
    let c = validate(&mut tx, t, old.institution_id, b, old.notice_days).await?;
    sqlx::query(
        "UPDATE resources.contract SET title = $3, kind = $4, status = $5, supplier_id = $6, \
         counterparty_name = $7, counterparty_scope = $8, contact_name = $9, contact_email = $10, \
         contact_phone = $11, value = $12, currency = $13, billing = $14, start_on = $15, \
         end_on = $16, auto_renew = $17, term_months = $18, notice_days = $19, purposes = $20, \
         tags = $21, notes = $22, version = version + 1, updated_at = now() \
         WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(t.as_uuid())
    .bind(&c.title)
    .bind(&c.kind)
    .bind(&c.status)
    .bind(c.supplier_id)
    .bind(&c.counterparty_name)
    .bind(&c.counterparty_scope)
    .bind(&c.contact_name)
    .bind(&c.contact_email)
    .bind(&c.contact_phone)
    .bind(c.value)
    .bind(&c.currency)
    .bind(&c.billing)
    .bind(c.start_on)
    .bind(c.end_on)
    .bind(c.auto_renew)
    .bind(c.term_months)
    .bind(c.notice_days)
    .bind(&c.purposes)
    .bind(&c.tags)
    .bind(&c.notes)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let row = contract_row(&mut tx, t, id).await?;
    reopen_series(&mut tx, t, &row, today).await?;
    let out = detail(&mut tx, t, &row, today, i64::from(settings.soon_days)).await?;
    let a = actor(ctx);
    audit_only(
        &mut tx,
        &scope,
        "contract",
        id,
        "resources.contract.update",
        json!({"title": c.title, "status": c.status, "endOn": c.end_on, "autoRenew": c.auto_renew}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(Json(out))
}

/// A renewal decision.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ContractDecisionInput {
    /// `notice-given`, `renewing`, `not-renewing`, `snoozed`; null clears the decision.
    pub kind: Option<String>,
    pub note: Option<String>,
    /// Required for `snoozed`: the day the snooze stops holding.
    pub until: Option<NaiveDate>,
}

/// Record (or clear) the decision about the contract's next term end
/// (`resources.procurement.manage`). It answers that term end only; once the term is over it
/// stops applying.
#[utoipa::path(put, path = "/api/v1/resources/contracts/{id}/decision", operation_id = "resources_contract_decision", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid)), request_body = ContractDecisionInput, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ContractDetail), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn decision(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Json(b): Json<ContractDecisionInput>,
) -> ApiResult<Json<ContractDetail>> {
    let ctx = auth.require(Permission::ProcurementManage)?;
    let t = ctx.scope.tenant_id;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let row = contract_row(&mut tx, t, id).await?;
    let inst = InstitutionId(row.institution_id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let today = business_today(&st, &scope, inst).await?;
    let settings = settings_of(&mut tx, t, row.institution_id).await?;
    let value = match b.kind.as_deref() {
        None => None,
        Some(k) => {
            let kind = DecisionKind::parse(k)
                .ok_or_else(|| bad("kind", "notice-given, renewing, not-renewing or snoozed"))?;
            if row.status != "active" {
                return Err(KernelError::Conflict(format!(
                    "contract is {}; only an active contract takes a renewal decision",
                    row.status
                ))
                .into());
            }
            let info = r::renewal_info(&row.rules(), today, i64::from(settings.soon_days));
            let term_end = info.date.ok_or_else(|| {
                KernelError::Conflict("an evergreen contract has no term end to decide".into())
            })?;
            let until = if kind == DecisionKind::Snoozed {
                let u = b
                    .until
                    .ok_or_else(|| bad("until", "a snooze needs its end date"))?;
                if u < today {
                    return Err(bad("until", "today or later").into());
                }
                Some(u)
            } else {
                None
            };
            let note = short("note", b.note, 500)?.unwrap_or_default();
            Some(to_json(&Decision {
                kind,
                date: Some(today),
                note,
                for_term_end: Some(term_end),
                until,
            }))
        }
    };
    sqlx::query(
        "UPDATE resources.contract SET decision = $3, version = version + 1, updated_at = now() \
         WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(t.as_uuid())
    .bind(&value)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let row = contract_row(&mut tx, t, id).await?;
    reopen_series(&mut tx, t, &row, today).await?;
    let out = detail(&mut tx, t, &row, today, i64::from(settings.soon_days)).await?;
    let a = actor(ctx);
    audit_only(
        &mut tx,
        &scope,
        "contract",
        id,
        "resources.contract.decision",
        json!({"decision": value}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(Json(out))
}

// ------------------------------------------------------------------ obligations

/// An obligation as written.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ContractObligationInput {
    pub text: String,
    /// `us` (default), `them` or `mutual`.
    pub owner: Option<String>,
    pub due_on: Option<NaiveDate>,
    /// Monthly / quarterly / annually, ending at a date, after a count, or with the contract.
    pub repeat: Option<RepeatInput>,
    /// Required on update.
    pub version: Option<i64>,
}

fn obligation_fields(
    b: &ContractObligationInput,
) -> Result<(String, Owner, Option<Repeat>), KernelError> {
    let text = b.text.trim().to_owned();
    if text.is_empty() || text.chars().count() > 200 {
        return Err(bad("text", "1…200 characters"));
    }
    let owner = match b.owner.as_deref() {
        None | Some("us") => Owner::Us,
        Some("them") => Owner::Them,
        Some("mutual") => Owner::Mutual,
        Some(_) => return Err(bad("owner", "us, them or mutual")),
    };
    let repeat = match &b.repeat {
        Some(ri) if ri.every.as_deref().is_some_and(|e| e != "none") => {
            let due = b
                .due_on
                .ok_or_else(|| bad("dueOn", "a repeating obligation needs its first date"))?;
            let rep = r::normalize_repeat(Some(ri), Some(due))
                .ok_or_else(|| bad("repeat.every", "monthly, quarterly or annually"))?;
            Some(rep)
        }
        _ => None,
    };
    Ok((text, owner, repeat))
}

async fn writable_contract(
    st: &ResourcesState,
    conn: &mut PgConnection,
    ctx: &sutra_kernel::auth::AuthContext,
    id: Uuid,
) -> Result<(ContractRow, ExecutionScope, NaiveDate), KernelError> {
    let row = contract_row(conn, ctx.scope.tenant_id, id).await?;
    let inst = InstitutionId(row.institution_id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(st, &scope, inst).await?;
    let today = business_today(st, &scope, inst).await?;
    Ok((row, scope, today))
}

/// Add an obligation (`resources.procurement.manage`; Idempotency-Key).
#[utoipa::path(post, path = "/api/v1/resources/contracts/{id}/obligations", operation_id = "resources_contract_obligation_create", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = ContractObligationInput, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = ContractObligationView), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn obligation_create(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ContractObligationInput>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ProcurementManage)?;
    require_idempotency_key(&headers)?;
    let t = ctx.scope.tenant_id;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let (row, scope, today) = writable_contract(&st, &mut tx, ctx, id).await?;
    let (text, owner, mut repeat) = obligation_fields(&b)?;
    if let (Some(rep), Some(due)) = (repeat.as_mut(), b.due_on) {
        // The first occurrence is the due date.
        rep.anchor = due;
    }
    let oid: Uuid = sqlx::query_scalar(
        "INSERT INTO resources.contract_obligation (tenant_id, institution_id, contract_id, text, \
         owner, due_on, repeat) VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(row.institution_id)
    .bind(id)
    .bind(&text)
    .bind(owner.as_str())
    .bind(b.due_on)
    .bind(repeat.as_ref().map(to_json))
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let ob = obligation_row(&mut tx, t, oid).await?;
    let a = actor(ctx);
    audit_only(
        &mut tx,
        &scope,
        "contract_obligation",
        oid,
        "resources.contract_obligation.create",
        json!({"contractId": id, "text": text, "dueOn": b.due_on, "repeat": repeat}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok((StatusCode::CREATED, Json(obligation_view(&ob, today))).into_response())
}

async fn save_schedule(
    conn: &mut PgConnection,
    t: TenantId,
    id: Uuid,
    s: &Obligation,
    history: &[Value],
) -> Result<(), KernelError> {
    sqlx::query(
        "UPDATE resources.contract_obligation SET due_on = $3, done = $4, repeat = $5, history = \
         $6, version = version + 1, updated_at = now() WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(t.as_uuid())
    .bind(s.due)
    .bind(s.done)
    .bind(s.repeat.as_ref().map(to_json))
    .bind(Value::Array(history.to_vec()))
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(())
}

/// Change an obligation (`resources.procurement.manage`; `version` required). A repeating one
/// keeps its anchor whenever it still puts the due date where it is.
#[utoipa::path(put, path = "/api/v1/resources/contract-obligations/{id}", operation_id = "resources_contract_obligation_update", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid)), request_body = ContractObligationInput, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ContractObligationView), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn obligation_update(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Json(b): Json<ContractObligationInput>,
) -> ApiResult<Json<ContractObligationView>> {
    let ctx = auth.require(Permission::ProcurementManage)?;
    let t = ctx.scope.tenant_id;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let ob = obligation_row(&mut tx, t, id).await?;
    let (row, scope, today) = writable_contract(&st, &mut tx, ctx, ob.contract_id).await?;
    check_version(
        b.version.ok_or_else(|| bad("version", "required"))?,
        ob.version,
    )?;
    let (text, owner, repeat) = obligation_fields(&b)?;
    let old = ob.schedule();
    let completed = old.history.len() as i32;
    let repeat = match (repeat, b.due_on) {
        (Some(mut rep), Some(due)) => {
            let live = if old.done && completed > 0 {
                completed - 1
            } else {
                completed
            };
            rep.anchor = r::repeat_anchor(due, &rep.every.clone(), live, old.repeat.as_ref());
            Some(rep)
        }
        _ => None,
    };
    let mut s = Obligation {
        due: b.due_on,
        done: old.done,
        repeat,
        history: old.history.clone(),
    };
    s = r::reopened_series(&row.rules(), &s, today);
    sqlx::query(
        "UPDATE resources.contract_obligation SET text = $3, owner = $4 WHERE id = $1 AND \
         tenant_id = $2",
    )
    .bind(id)
    .bind(t.as_uuid())
    .bind(&text)
    .bind(owner.as_str())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    save_schedule(&mut tx, t, id, &s, &ob.history()).await?;
    let ob = obligation_row(&mut tx, t, id).await?;
    let a = actor(ctx);
    audit_only(
        &mut tx,
        &scope,
        "contract_obligation",
        id,
        "resources.contract_obligation.update",
        json!({"text": text, "dueOn": s.due, "repeat": s.repeat}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(Json(obligation_view(&ob, today)))
}

/// Tick off the live occurrence (`resources.procurement.manage`): a repeating obligation moves
/// to its next occurrence, or finishes when the series is over.
#[utoipa::path(post, path = "/api/v1/resources/contract-obligations/{id}/complete", operation_id = "resources_contract_obligation_complete", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ContractObligationView), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn obligation_complete(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<ContractObligationView>> {
    tick(st, auth, id, true).await
}

/// Undo the last completion (`resources.procurement.manage`).
#[utoipa::path(post, path = "/api/v1/resources/contract-obligations/{id}/undo", operation_id = "resources_contract_obligation_undo", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ContractObligationView), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn obligation_undo(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<ContractObligationView>> {
    tick(st, auth, id, false).await
}

async fn tick(
    st: ResourcesState,
    auth: Authenticated,
    id: Uuid,
    complete: bool,
) -> ApiResult<Json<ContractObligationView>> {
    let ctx = auth.require(Permission::ProcurementManage)?;
    let t = ctx.scope.tenant_id;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let ob = obligation_row(&mut tx, t, id).await?;
    let (row, scope, today) = writable_contract(&st, &mut tx, ctx, ob.contract_id).await?;
    let old = ob.schedule();
    let mut history = ob.history();
    let a = actor(ctx);
    let mut s = if complete {
        if old.done {
            return Err(KernelError::Conflict("obligation is already done".into()).into());
        }
        history.push(json!({"due": old.due, "doneAt": Utc::now(), "by": a.id}));
        r::advance_occurrence(&row.rules(), &old, today)
    } else {
        if !old.done && old.history.is_empty() {
            return Err(KernelError::Conflict("nothing to undo".into()).into());
        }
        history.pop();
        r::rewind_occurrence(&old)
    };
    let before = s.history.len();
    r::trim_history(&mut s);
    let dropped = before - s.history.len();
    history.drain(..dropped.min(history.len()));
    save_schedule(&mut tx, t, id, &s, &history).await?;
    let ob = obligation_row(&mut tx, t, id).await?;
    let action = if complete {
        "resources.contract_obligation.complete"
    } else {
        "resources.contract_obligation.undo"
    };
    audit_only(
        &mut tx,
        &scope,
        "contract_obligation",
        id,
        action,
        json!({"dueOn": old.due, "next": s.due, "done": s.done}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(Json(obligation_view(&ob, today)))
}

/// Remove an obligation (`resources.procurement.manage`).
#[utoipa::path(delete, path = "/api/v1/resources/contract-obligations/{id}", operation_id = "resources_contract_obligation_delete", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 204), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn obligation_delete(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let ctx = auth.require(Permission::ProcurementManage)?;
    let t = ctx.scope.tenant_id;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let ob = obligation_row(&mut tx, t, id).await?;
    let (_, scope, _) = writable_contract(&st, &mut tx, ctx, ob.contract_id).await?;
    sqlx::query("DELETE FROM resources.contract_obligation WHERE id = $1 AND tenant_id = $2")
        .bind(id)
        .bind(t.as_uuid())
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    let a = actor(ctx);
    audit_only(
        &mut tx,
        &scope,
        "contract_obligation",
        id,
        "resources.contract_obligation.delete",
        json!({"contractId": ob.contract_id, "text": ob.text}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(StatusCode::NO_CONTENT)
}

// ------------------------------------------------------------------ activities

/// An activity under an MoU or contract.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ContractActivityInput {
    pub held_on: NaiveDate,
    /// `internship`, `on_job_training`, `project_work`, `student_exchange`, `faculty_exchange`,
    /// `collaborative_research`, `workshop`, `training`, `consultancy`, `placement`,
    /// `publication`, `other`.
    pub kind: String,
    pub title: String,
    #[serde(default)]
    pub students: i32,
    #[serde(default)]
    pub staff: i32,
    pub outcome: Option<String>,
    /// Where the evidence is kept (a document reference or link).
    pub evidence_ref: Option<String>,
}

/// Record an activity (`resources.procurement.manage`; Idempotency-Key). It cannot be dated in
/// the future, nor before the agreement started.
#[utoipa::path(post, path = "/api/v1/resources/contracts/{id}/activities", operation_id = "resources_contract_activity_create", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = ContractActivityInput, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = ContractActivityView), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn activity_create(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ContractActivityInput>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ProcurementManage)?;
    require_idempotency_key(&headers)?;
    let t = ctx.scope.tenant_id;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let (row, scope, today) = writable_contract(&st, &mut tx, ctx, id).await?;
    if !ACTIVITY_KINDS.contains(&b.kind.as_str()) {
        return Err(bad("kind", &format!("one of {}", ACTIVITY_KINDS.join(", "))).into());
    }
    let title = b.title.trim().to_owned();
    if title.is_empty() || title.chars().count() > 200 {
        return Err(bad("title", "1…200 characters").into());
    }
    if b.held_on > today {
        return Err(bad("heldOn", "not in the future").into());
    }
    if row.start_on.is_some_and(|s| b.held_on < s) {
        return Err(bad("heldOn", "on or after the agreement's start").into());
    }
    if b.students < 0 || b.staff < 0 {
        return Err(bad("students", "not negative").into());
    }
    let a = actor(ctx);
    let view = sqlx::query_as::<_, ContractActivityView>(&format!(
        "INSERT INTO resources.contract_activity (tenant_id, institution_id, contract_id, held_on, \
         kind, title, students, staff, outcome, evidence_ref, created_by) VALUES ($1, $2, $3, $4, \
         $5, $6, $7, $8, $9, $10, $11) RETURNING {ACTIVITY_COLUMNS}"
    ))
    .bind(t.as_uuid())
    .bind(row.institution_id)
    .bind(id)
    .bind(b.held_on)
    .bind(&b.kind)
    .bind(&title)
    .bind(b.students)
    .bind(b.staff)
    .bind(short("outcome", b.outcome, 2000)?)
    .bind(short("evidenceRef", b.evidence_ref, 500)?)
    .bind(a.id.as_deref())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    audit_only(
        &mut tx,
        &scope,
        "contract_activity",
        view.id,
        "resources.contract_activity.create",
        json!({"contractId": id, "kind": b.kind, "heldOn": b.held_on}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok((StatusCode::CREATED, Json(view)).into_response())
}

/// Remove an activity (`resources.procurement.manage`).
#[utoipa::path(delete, path = "/api/v1/resources/contract-activities/{id}", operation_id = "resources_contract_activity_delete", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 204), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn activity_delete(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let ctx = auth.require(Permission::ProcurementManage)?;
    let t = ctx.scope.tenant_id;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let inst = institution_of(&mut tx, t, "contract_activity", "contract activity", id).await?;
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    sqlx::query("DELETE FROM resources.contract_activity WHERE id = $1 AND tenant_id = $2")
        .bind(id)
        .bind(t.as_uuid())
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    let a = actor(ctx);
    audit_only(
        &mut tx,
        &scope,
        "contract_activity",
        id,
        "resources.contract_activity.delete",
        json!({}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(StatusCode::NO_CONTENT)
}

// ------------------------------------------------------------------ dashboard

/// An obligation due soon, with its contract.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ContractDueObligation {
    pub obligation: ContractObligationView,
    pub contract_title: String,
    pub counterparty_name: String,
}

/// The contract book at a glance.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ContractDashboard {
    pub today: NaiveDate,
    pub stats: ContractStats,
    /// Waiting on a decision (notice window open or past the end), soonest first.
    pub attention: Vec<ContractView>,
    /// Term ends within the "soon" horizon.
    pub upcoming: Vec<ContractView>,
    /// Open obligations overdue or due within the obligation horizon, most urgent first.
    pub due_obligations: Vec<ContractDueObligation>,
}

/// Dashboard (`resources.read`). Values are totalled per currency, never across currencies.
#[utoipa::path(get, path = "/api/v1/resources/institutions/{id}/contract-dashboard", operation_id = "resources_contract_dashboard", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = ContractDashboard), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn dashboard(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<ContractDashboard>> {
    let ctx = auth.require(Permission::ResourcesRead)?;
    let inst = InstitutionId(id);
    let scope = institution_scope(ctx, inst);
    require_institution(&st, &scope, inst).await?;
    let today = business_today(&st, &scope, inst).await?;
    let t = ctx.scope.tenant_id;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let settings = settings_of(&mut conn, t, id).await?;
    let soon = i64::from(settings.soon_days);
    let rows = contracts_of(&mut conn, t, id).await?;
    let ids: Vec<Uuid> = rows.iter().map(|c| c.id).collect();
    let all = obligations_of(&mut conn, t, &ids).await?;
    let rules: Vec<Contract> = rows.iter().map(ContractRow::rules).collect();
    let mut obs: Vec<Vec<&ObRow>> = vec![vec![]; rows.len()];
    let pos: HashMap<Uuid, usize> = ids.iter().enumerate().map(|(i, c)| (*c, i)).collect();
    for o in &all {
        if let Some(i) = pos.get(&o.contract_id) {
            obs[*i].push(o);
        }
    }
    let scheds: Vec<Vec<Obligation>> = obs
        .iter()
        .map(|v| v.iter().map(|o| o.schedule()).collect())
        .collect();
    let pairs: Vec<(&Contract, &[Obligation])> =
        rules.iter().zip(scheds.iter().map(Vec::as_slice)).collect();
    let views: Vec<ContractView> = rows
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let open = obs[i].iter().filter(|o| !o.done).count() as i64;
            contract_view(c, open, today, soon)
        })
        .collect();
    let mut attention: Vec<ContractView> = views.iter().filter(|v| v.attention).cloned().collect();
    attention.sort_by_key(|v| v.renewal.date);
    let refs: Vec<&Contract> = rules.iter().collect();
    let upcoming = r::upcoming_renewals(&refs, today, soon)
        .into_iter()
        .map(|i| views[i].clone())
        .collect();
    let due = r::due_obligations(&pairs, today, i64::from(settings.obligation_days))
        .into_iter()
        .map(|(ci, oi, _)| ContractDueObligation {
            obligation: obligation_view(obs[ci][oi], today),
            contract_title: rows[ci].title.clone(),
            counterparty_name: rows[ci].counterparty_name.clone(),
        })
        .collect();
    Ok(Json(ContractDashboard {
        today,
        stats: r::contract_stats(&pairs, today),
        attention,
        upcoming,
        due_obligations: due,
    }))
}

/// What a term reconciliation did.
#[derive(Debug, Clone, Default, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ContractReconcileResult {
    pub contracts_updated: i64,
    pub terms_recorded: i64,
    /// Closed out on the term end their notice was given for.
    pub terminated: i64,
    pub decisions_cleared: i64,
}

/// Bring the book up to date (`resources.procurement.manage`): finished terms are recorded once
/// each, a contract whose notice was given closes on that term end instead of rolling forward,
/// and a decision about a term the contract has moved past is cleared. Safe to repeat.
#[utoipa::path(post, path = "/api/v1/resources/institutions/{id}/contracts/reconcile-terms", operation_id = "resources_contract_reconcile_terms", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ContractReconcileResult), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn reconcile(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<ContractReconcileResult>> {
    let ctx = auth.require(Permission::ProcurementManage)?;
    let inst = InstitutionId(id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let today = business_today(&st, &scope, inst).await?;
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let rows = contracts_of(&mut tx, t, id).await?;
    let mut out = ContractReconcileResult::default();
    for c in rows.iter().filter(|c| c.status == "active") {
        let known = term_ends(&mut tx, t, c.id).await?;
        let Some(p) = r::term_reconciliation(&c.rules(), &known, today) else {
            continue;
        };
        apply_patch(&mut tx, t, c, &p, &a, &scope).await?;
        out.contracts_updated += 1;
        out.terms_recorded += p.additions.len() as i64;
        out.terminated += i64::from(p.terminate);
        out.decisions_cleared += i64::from(p.clear_decision);
    }
    tx.commit().await.map_err(map_db_error)?;
    Ok(Json(out))
}

async fn apply_patch(
    conn: &mut PgConnection,
    t: TenantId,
    c: &ContractRow,
    p: &r::TermPatch,
    a: &Actor,
    scope: &ExecutionScope,
) -> Result<(), KernelError> {
    for add in &p.additions {
        sqlx::query(
            "INSERT INTO resources.contract_term (tenant_id, institution_id, contract_id, \
             start_on, end_on, ended, value, currency, note) VALUES ($1, $2, $3, $4, $5, $6, $7, \
             $8, $9) ON CONFLICT (tenant_id, contract_id, end_on) DO NOTHING",
        )
        .bind(t.as_uuid())
        .bind(c.institution_id)
        .bind(c.id)
        .bind(add.start)
        .bind(add.end)
        .bind(add.ended.as_str())
        .bind(c.value)
        .bind(&c.currency)
        .bind(&add.note)
        .execute(&mut *conn)
        .await
        .map_err(map_db_error)?;
    }
    if p.terminate || p.clear_decision {
        sqlx::query(
            "UPDATE resources.contract SET status = CASE WHEN $3 THEN 'terminated' ELSE status END, \
             decision = CASE WHEN $4 THEN NULL ELSE decision END, version = version + 1, \
             updated_at = now() WHERE id = $1 AND tenant_id = $2",
        )
        .bind(c.id)
        .bind(t.as_uuid())
        .bind(p.terminate)
        .bind(p.clear_decision)
        .execute(&mut *conn)
        .await
        .map_err(map_db_error)?;
    }
    let ended: Vec<Value> = p
        .additions
        .iter()
        .map(|x| json!({"end": x.end, "ended": x.ended.as_str()}))
        .collect();
    audit_only(
        conn,
        scope,
        "contract",
        c.id,
        "resources.contract.terms_reconciled",
        json!({"terms": ended, "terminated": p.terminate, "decisionCleared": p.clear_decision}),
        a,
    )
    .await
}

// ------------------------------------------------------------------ calendar

fn ics_response(text: String, name: &str) -> Response {
    let slug: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let slug = if slug.is_empty() {
        "contract-deadlines".to_owned()
    } else {
        slug.chars().take(40).collect()
    };
    (
        StatusCode::OK,
        [
            (
                header::CONTENT_TYPE,
                "text/calendar; charset=utf-8".to_owned(),
            ),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{slug}.ics\""),
            ),
        ],
        text,
    )
        .into_response()
}

async fn events_for(
    conn: &mut PgConnection,
    t: TenantId,
    rows: &[ContractRow],
    settings: &ContractSettings,
    today: NaiveDate,
) -> Result<Vec<ical::Event>, KernelError> {
    let ids: Vec<Uuid> = rows.iter().map(|c| c.id).collect();
    let obs = obligations_of(conn, t, &ids).await?;
    let opts = EventOptions {
        today,
        terms: 3,
        occurrences: 6,
        alarms: settings.alarm_days.iter().map(|d| i64::from(*d)).collect(),
    };
    let mut events = vec![];
    for c in rows {
        let dated: Vec<DatedObligation> = obs
            .iter()
            .filter(|o| o.contract_id == c.id)
            .map(|o| DatedObligation {
                id: o.id.to_string(),
                text: o.text.clone(),
                owner: Owner::parse(&o.owner),
                schedule: o.schedule(),
            })
            .collect();
        let x = EventContext {
            id: c.id.to_string(),
            title: c.title.clone(),
            counterparty: c.counterparty_name.clone(),
            updated_at_ms: c.updated_at.timestamp_millis(),
            url: String::new(),
        };
        events.extend(ical::contract_events(&c.rules(), &x, &dated, &opts));
    }
    ical::sort_events(&mut events);
    Ok(events)
}

/// Every deadline of the contract book as an iCalendar file (`resources.read`): notice
/// deadlines, term ends and the next renewals, and dated obligations, with reminders.
#[utoipa::path(get, path = "/api/v1/resources/institutions/{id}/contracts/calendar.ics", operation_id = "resources_contract_calendar", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = String, content_type = "text/calendar"), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn calendar(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ResourcesRead)?;
    let inst = InstitutionId(id);
    let scope = institution_scope(ctx, inst);
    require_institution(&st, &scope, inst).await?;
    let today = business_today(&st, &scope, inst).await?;
    let t = ctx.scope.tenant_id;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let settings = settings_of(&mut conn, t, id).await?;
    let rows = contracts_of(&mut conn, t, id).await?;
    let events = events_for(&mut conn, t, &rows, &settings, today).await?;
    let stamp = ical::ics_stamp(Utc::now());
    let text = ical::build_calendar(&events, "Contract and MoU deadlines", &stamp);
    Ok(ics_response(text, "contract-deadlines"))
}

/// One contract's deadlines as an iCalendar file (`resources.read`).
#[utoipa::path(get, path = "/api/v1/resources/contracts/{id}/calendar.ics", operation_id = "resources_contract_calendar_one", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = String, content_type = "text/calendar"), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn calendar_one(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ResourcesRead)?;
    let t = ctx.scope.tenant_id;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let row = contract_row(&mut conn, t, id).await?;
    let inst = InstitutionId(row.institution_id);
    let scope = institution_scope(ctx, inst);
    require_institution(&st, &scope, inst).await?;
    let today = business_today(&st, &scope, inst).await?;
    let settings = settings_of(&mut conn, t, row.institution_id).await?;
    let title = row.title.clone();
    let events = events_for(&mut conn, t, &[row], &settings, today).await?;
    let stamp = ical::ics_stamp(Utc::now());
    let text = ical::build_calendar(&events, &format!("{title} deadlines"), &stamp);
    Ok(ics_response(text, &title))
}

// ------------------------------------------------------------------ NAAC

/// Query of the MoU evidence.
#[derive(Debug, Clone, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct MouEvidenceQuery {
    /// AQAR year, e.g. `2025-26` (1 July – 30 June); default the year containing today.
    pub year: Option<String>,
}

/// One MoU in the evidence.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MouEvidenceRow {
    pub contract_id: Uuid,
    pub title: String,
    pub partner: String,
    pub kind: String,
    pub scope: String,
    pub status: String,
    pub start_on: Option<NaiveDate>,
    /// When the agreement stops or stopped (null = still running).
    pub ends_on: Option<NaiveDate>,
    pub purposes: Vec<String>,
    /// In force for some of the year and with an activity in it.
    pub functional: bool,
    /// Activities in the year, newest first.
    pub activities: Vec<ContractActivityView>,
}

/// MoU evidence for one AQAR year.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MouEvidence {
    pub year: String,
    pub from: NaiveDate,
    pub to: NaiveDate,
    pub totals: MouTotals,
    /// MoUs in force during the year.
    pub mous: Vec<MouEvidenceRow>,
}

/// Functional MoUs and their activities for an AQAR year (`resources.read`): the evidence NAAC
/// criterion 3.7 asks for (MoUs with institutions and industries, in India and abroad, for
/// internships, on-the-job training, project work, exchanges and collaborative research).
#[utoipa::path(get, path = "/api/v1/resources/institutions/{id}/mou-evidence", operation_id = "resources_contract_mou_evidence", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid), MouEvidenceQuery), security(("session_cookie" = [])),
    responses((status = 200, body = MouEvidence), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn mou_evidence(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<MouEvidenceQuery>,
) -> ApiResult<Json<MouEvidence>> {
    let ctx = auth.require(Permission::ResourcesRead)?;
    let inst = InstitutionId(id);
    let scope = institution_scope(ctx, inst);
    require_institution(&st, &scope, inst).await?;
    let today = business_today(&st, &scope, inst).await?;
    let (from, to, year) = match clean(q.year) {
        None => naac::aqar_year(today),
        Some(y) => naac::parse_aqar(&y).ok_or_else(|| bad("year", "an AQAR year like 2025-26"))?,
    };
    let t = ctx.scope.tenant_id;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let rows: Vec<ContractRow> = contracts_of(&mut conn, t, id)
        .await?
        .into_iter()
        .filter(|c| naac::is_mou(&c.kind))
        .collect();
    let ids: Vec<Uuid> = rows.iter().map(|c| c.id).collect();
    let acts = sqlx::query_as::<_, ContractActivityView>(&format!(
        "SELECT {ACTIVITY_COLUMNS} FROM resources.contract_activity WHERE tenant_id = $1 AND \
         contract_id = ANY($2) ORDER BY held_on DESC, id"
    ))
    .bind(t.as_uuid())
    .bind(&ids)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let mut mous = vec![];
    let mut rows_out = vec![];
    for c in &rows {
        let rules = c.rules();
        let ends = if c.status == "terminated" {
            let known = term_ends(&mut conn, t, c.id).await?;
            r::ended_on(&rules, &known)
        } else {
            r::agreement_end(&rules, today)
        };
        let mine: Vec<&ContractActivityView> =
            acts.iter().filter(|a| a.contract_id == c.id).collect();
        let m = Mou {
            kind: c.kind.clone(),
            status: c.status.clone(),
            scope: c.counterparty_scope.clone(),
            start: c.start_on,
            ends,
            activities: mine
                .iter()
                .map(|a| {
                    (
                        a.held_on,
                        a.kind.clone(),
                        i64::from(a.students),
                        i64::from(a.staff),
                    )
                })
                .collect(),
        };
        if !naac::in_force(&m, from, to) {
            continue;
        }
        rows_out.push(MouEvidenceRow {
            contract_id: c.id,
            title: c.title.clone(),
            partner: c.counterparty_name.clone(),
            kind: c.kind.clone(),
            scope: c.counterparty_scope.clone(),
            status: c.status.clone(),
            start_on: c.start_on,
            ends_on: ends,
            purposes: c.purposes.clone(),
            functional: naac::functional(&m, from, to),
            activities: mine
                .into_iter()
                .filter(|a| a.held_on >= from && a.held_on <= to)
                .cloned()
                .collect(),
        });
        mous.push(m);
    }
    rows_out.sort_by(|a, b| b.functional.cmp(&a.functional).then(a.title.cmp(&b.title)));
    Ok(Json(MouEvidence {
        year,
        from,
        to,
        totals: naac::totals(&mous, from, to),
        mous: rows_out,
    }))
}

/// Routes of contracts and MoUs.
pub fn router(st: ResourcesState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(settings_get, settings_put))
        .routes(routes!(list, create))
        .routes(routes!(get, update))
        .routes(routes!(decision))
        .routes(routes!(obligation_create))
        .routes(routes!(obligation_update, obligation_delete))
        .routes(routes!(obligation_complete))
        .routes(routes!(obligation_undo))
        .routes(routes!(activity_create))
        .routes(routes!(activity_delete))
        .routes(routes!(dashboard))
        .routes(routes!(reconcile))
        .routes(routes!(calendar))
        .routes(routes!(calendar_one))
        .routes(routes!(mou_evidence))
        .with_state(st)
}
