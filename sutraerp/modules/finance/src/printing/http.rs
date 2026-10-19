//! HTTP API of counter printing (tag `finance-payments`, ops `finance_receipt_*`; contract
//! `docs/extensions-3e-printing.md`).
//!
//! * `GET|PUT /institutions/{id}/receipt-print-settings` — the header, registrations, footer and
//!   amount-in-words switch printed on the institution's slips.
//! * `GET /receipts/{id}/slip` — one fee receipt as a slip: the doc, the ESC/POS bytes (base64)
//!   for the counter's printer and a plain-text preview for the browser's own print dialog.
//! * `GET /institutions/{id}/receipt-test-slip` — proves a counter's paper width and connection.
//! * `GET /institutions/{id}/counter-report` — the counter's day close by tender.

use super::escpos::{escpos_receipt, escpos_report, readable, ReportRow, SlipDoc};
use super::slip::{
    counter_report, fee_receipt_doc, test_doc, ChannelTotal, CounterDay, PaidLine, PrintSettings,
    ReceiptFacts, Registration,
};
use crate::shared::guard::require_institution;
use crate::shared::http::actor;
use crate::shared::money::{display_inr, fmt};
use crate::shared::publish::audit;
use crate::shared::scope::{institution_scope, tenant};
use crate::shared::FinanceState;
use axum::extract::{Path, Query, State};
use axum::Json;
use base64::Engine;
use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{PgConnection, Row};
use sutra_kernel::auth::Permission;
use sutra_kernel::error::{FieldViolation, KernelError};
use sutra_kernel::scope::{InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

// ------------------------------------------------------------------ wire types

/// What an institution prints on its counter slips.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReceiptPrintSettings {
    pub institution_id: Uuid,
    /// Empty means the institution's own name.
    pub title: String,
    pub address: String,
    /// At most two (GSTIN, PAN, trust registration).
    pub registrations: Vec<Registration>,
    pub footer: String,
    pub show_amount_in_words: bool,
    /// `null` until the institution saves its own settings.
    pub updated_at: Option<DateTime<Utc>>,
}

/// New print settings (the whole set; the server keeps nothing it is not sent).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReceiptPrintSettingsInput {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub address: String,
    #[serde(default)]
    pub registrations: Vec<Registration>,
    #[serde(default)]
    pub footer: String,
    #[serde(default = "yes")]
    pub show_amount_in_words: bool,
}

fn yes() -> bool {
    true
}

/// A slip ready for the counter's printer.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReceiptSlip {
    /// `null` on a test slip.
    pub receipt_id: Option<Uuid>,
    pub receipt_number: Option<String>,
    /// 32 (58 mm paper) or 48 (80 mm paper).
    pub columns: usize,
    pub doc: SlipDoc,
    /// The slip as plain text, for the browser's own print dialog.
    pub preview: String,
    /// ESC/POS bytes, base64 (standard alphabet, padded).
    pub escpos_base64: String,
    /// `true` when the bytes end with the cash-drawer pulse (asked for, and a cash receipt).
    pub kick_drawer: bool,
}

/// One tender's share of the counter's day.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReceiptCounterChannel {
    pub channel: String,
    pub count: i64,
    #[schema(value_type = String, example = "12500.00")]
    pub amount: String,
}

/// The counter's day close.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReceiptCounterReport {
    pub institution_id: Uuid,
    pub date: NaiveDate,
    /// `true` when limited to the caller's own receipts.
    pub mine: bool,
    /// Receipts issued that day and still standing.
    pub receipt_count: i64,
    pub cancelled_count: i64,
    #[schema(value_type = String, example = "62500.00")]
    pub total: String,
    /// `₹62,500.00`.
    pub total_display: String,
    pub by_channel: Vec<ReceiptCounterChannel>,
    pub rows: Vec<ReportRow>,
    pub preview: String,
    pub escpos_base64: String,
}

#[derive(Debug, Clone, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
pub struct SlipQuery {
    /// 32 or 48 (anything else prints at 48).
    pub columns: Option<usize>,
    /// End with the cash-drawer pulse (only a cash receipt opens the drawer).
    pub kick: Option<bool>,
    /// `duplicate` marks a reprint.
    pub copy: Option<String>,
}

#[derive(Debug, Clone, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
pub struct TestSlipQuery {
    /// 32 or 48 (anything else prints at 48).
    pub columns: Option<usize>,
}

#[derive(Debug, Clone, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
pub struct CounterReportQuery {
    /// The business day (default today, institution time).
    pub date: Option<NaiveDate>,
    pub columns: Option<usize>,
    /// Only the receipts the caller recorded.
    pub mine: Option<bool>,
}

// ------------------------------------------------------------------ helpers

fn paper(columns: Option<usize>) -> usize {
    match columns {
        Some(32) => 32,
        _ => 48,
    }
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn registrations_from(v: Value) -> Vec<Registration> {
    serde_json::from_value(v).unwrap_or_default()
}

async fn load_settings(
    conn: &mut PgConnection,
    t: TenantId,
    inst: InstitutionId,
) -> Result<(PrintSettings, Option<DateTime<Utc>>), KernelError> {
    let row = sqlx::query(
        "SELECT title, address, registrations, footer, show_amount_in_words, updated_at \
         FROM finance.receipt_print_settings WHERE tenant_id = $1 AND institution_id = $2",
    )
    .bind(t.as_uuid())
    .bind(inst.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let Some(r) = row else {
        return Ok((PrintSettings::default(), None));
    };
    let read = || -> Result<(PrintSettings, Option<DateTime<Utc>>), sqlx::Error> {
        Ok((
            PrintSettings {
                title: r.try_get("title")?,
                address: r.try_get("address")?,
                registrations: registrations_from(r.try_get("registrations")?),
                footer: r.try_get("footer")?,
                show_amount_in_words: r.try_get("show_amount_in_words")?,
            },
            r.try_get("updated_at")?,
        ))
    };
    read().map_err(map_db_error)
}

fn settings_view(
    inst: InstitutionId,
    s: PrintSettings,
    updated_at: Option<DateTime<Utc>>,
) -> ReceiptPrintSettings {
    ReceiptPrintSettings {
        institution_id: inst.as_uuid(),
        title: s.title,
        address: s.address,
        registrations: s.registrations,
        footer: s.footer,
        show_amount_in_words: s.show_amount_in_words,
        updated_at,
    }
}

fn validate(input: &ReceiptPrintSettingsInput) -> Result<PrintSettings, KernelError> {
    let mut v = Vec::new();
    let mut limit = |field: &str, value: &str, max: usize| {
        if value.chars().count() > max {
            v.push(FieldViolation::new(
                field,
                "too-long",
                format!("at most {max} characters"),
            ));
        }
    };
    limit("title", &input.title, 120);
    limit("address", &input.address, 300);
    limit("footer", &input.footer, 300);
    let registrations: Vec<Registration> = input
        .registrations
        .iter()
        .map(|r| Registration {
            label: r.label.trim().to_owned(),
            number: r.number.trim().to_owned(),
        })
        .filter(|r| !r.label.is_empty() || !r.number.is_empty())
        .collect();
    if registrations.len() > 2 {
        v.push(FieldViolation::new(
            "registrations",
            "too-many",
            "at most two lines",
        ));
    }
    for (i, r) in registrations.iter().enumerate() {
        if r.number.is_empty() {
            v.push(FieldViolation::new(
                format!("registrations[{i}].number"),
                "required",
                "a registration needs its number",
            ));
        }
        if r.label.chars().count() > 30 || r.number.chars().count() > 40 {
            v.push(FieldViolation::new(
                format!("registrations[{i}]"),
                "too-long",
                "label at most 30, number at most 40 characters",
            ));
        }
    }
    if !v.is_empty() {
        return Err(KernelError::Validation(v));
    }
    Ok(PrintSettings {
        title: input.title.trim().to_owned(),
        address: input.address.trim().to_owned(),
        registrations,
        footer: input.footer.trim().to_owned(),
        show_amount_in_words: input.show_amount_in_words,
    })
}

/// `17 Oct 2026 · 11:42 AM` in the institution's timezone.
async fn local_stamp(
    conn: &mut PgConnection,
    at: DateTime<Utc>,
    tz: &str,
) -> Result<String, KernelError> {
    sqlx::query_scalar(
        "SELECT to_char($1::timestamptz AT TIME ZONE $2, 'FMDD Mon YYYY \u{b7} FMHH12:MI AM')",
    )
    .bind(at)
    .bind(tz)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)
}

fn slip(
    receipt_id: Option<Uuid>,
    receipt_number: Option<String>,
    columns: usize,
    doc: SlipDoc,
    kick: bool,
) -> ReceiptSlip {
    let bytes = escpos_receipt(&doc, columns, kick);
    ReceiptSlip {
        receipt_id,
        receipt_number,
        columns,
        preview: readable(&bytes),
        escpos_base64: b64(&bytes),
        kick_drawer: kick,
        doc,
    }
}

// ------------------------------------------------------------------ handlers

/// The institution's slip settings (`finance.read`); defaults until it saves its own.
#[utoipa::path(get, path = "/api/v1/finance/institutions/{id}/receipt-print-settings", operation_id = "finance_receipt_print_settings_get", tag = "finance-payments",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = ReceiptPrintSettings), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn print_settings_get(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<ReceiptPrintSettings>> {
    let ctx = auth.require(Permission::FinanceRead)?;
    let inst = InstitutionId(id);
    require_institution(&st, &institution_scope(ctx, inst), inst).await?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let (s, at) = load_settings(&mut conn, tenant(ctx), inst).await?;
    Ok(Json(settings_view(inst, s, at)))
}

/// Replaces the institution's slip settings (`finance.config.manage`; audited).
#[utoipa::path(put, path = "/api/v1/finance/institutions/{id}/receipt-print-settings", operation_id = "finance_receipt_print_settings_put", tag = "finance-payments",
    params(("id" = String, Path, format = Uuid)), request_body = ReceiptPrintSettingsInput, security(("session_cookie" = [])),
    responses((status = 200, body = ReceiptPrintSettings), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn print_settings_put(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Json(input): Json<ReceiptPrintSettingsInput>,
) -> ApiResult<Json<ReceiptPrintSettings>> {
    let ctx = auth.require(Permission::FinanceConfigManage)?;
    let inst = InstitutionId(id);
    let scope = institution_scope(ctx, inst);
    require_institution(&st, &scope, inst).await?;
    let s = validate(&input)?;
    let t = tenant(ctx);
    let a = actor(ctx);
    let regs = serde_json::to_value(&s.registrations).unwrap_or_else(|_| json!([]));
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    sqlx::query(
        "INSERT INTO finance.receipt_print_settings (tenant_id, institution_id, title, address, \
         registrations, footer, show_amount_in_words, updated_by, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, now()) \
         ON CONFLICT (tenant_id, institution_id) DO UPDATE SET title = EXCLUDED.title, \
         address = EXCLUDED.address, registrations = EXCLUDED.registrations, \
         footer = EXCLUDED.footer, show_amount_in_words = EXCLUDED.show_amount_in_words, \
         updated_by = EXCLUDED.updated_by, updated_at = now()",
    )
    .bind(t.as_uuid())
    .bind(inst.as_uuid())
    .bind(&s.title)
    .bind(&s.address)
    .bind(&regs)
    .bind(&s.footer)
    .bind(s.show_amount_in_words)
    .bind(a.id.as_deref())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    audit(
        &mut tx,
        &scope,
        "finance.receipt_print_settings.update",
        "receipt_print_settings",
        inst.as_uuid().to_string(),
        json!({"title": s.title, "registrations": regs, "showAmountInWords": s.show_amount_in_words}),
        &a,
    )
    .await?;
    let (saved, at) = load_settings(&mut tx, t, inst).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(Json(settings_view(inst, saved, at)))
}

/// One fee receipt as a counter slip (`finance.read`). A cancelled receipt prints as VOID;
/// `copy=duplicate` marks a reprint; `kick=true` opens the cash drawer for a cash receipt.
#[utoipa::path(get, path = "/api/v1/finance/receipts/{id}/slip", operation_id = "finance_receipt_slip", tag = "finance-payments",
    params(("id" = String, Path, format = Uuid), SlipQuery), security(("session_cookie" = [])),
    responses((status = 200, body = ReceiptSlip), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn receipt_slip(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<SlipQuery>,
) -> ApiResult<Json<ReceiptSlip>> {
    let ctx = auth.require(Permission::FinanceRead)?;
    let t = tenant(ctx);
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let r = sqlx::query(
        "SELECT r.receipt_number, r.institution_id, r.payment_id, r.amount, r.status, r.issued_at, \
         r.cancel_reason, p.payment_number, p.channel, p.external_ref, p.payer_name, \
         p.unapplied_amount, a.display_name, a.student_number \
         FROM finance.receipt r JOIN finance.payment p ON p.id = r.payment_id \
         JOIN finance.student_account a ON a.id = r.student_account_id \
         WHERE r.tenant_id = $1 AND r.id = $2",
    )
    .bind(t.as_uuid())
    .bind(id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound { entity: "receipt" })?;
    let get = || -> Result<_, sqlx::Error> {
        Ok((
            r.try_get::<String, _>("receipt_number")?,
            r.try_get::<Uuid, _>("institution_id")?,
            r.try_get::<Uuid, _>("payment_id")?,
            r.try_get::<Decimal, _>("amount")?,
            r.try_get::<String, _>("status")?,
            r.try_get::<DateTime<Utc>, _>("issued_at")?,
            r.try_get::<Option<String>, _>("cancel_reason")?,
        ))
    };
    let (number, inst_id, payment_id, amount, status, issued_at, cancel_reason) =
        get().map_err(map_db_error)?;
    let tender = || -> Result<_, sqlx::Error> {
        Ok((
            r.try_get::<String, _>("payment_number")?,
            r.try_get::<String, _>("channel")?,
            r.try_get::<Option<String>, _>("external_ref")?,
            r.try_get::<Option<String>, _>("payer_name")?,
            r.try_get::<Decimal, _>("unapplied_amount")?,
            r.try_get::<Option<String>, _>("display_name")?,
            r.try_get::<Option<String>, _>("student_number")?,
        ))
    };
    let (payment_number, channel, external_ref, payer_name, unapplied, name, student_no) =
        tender().map_err(map_db_error)?;
    let inst = InstitutionId(inst_id);
    let institution = require_institution(&st, &institution_scope(ctx, inst), inst).await?;
    let lines: Vec<(String, String, Decimal)> = sqlx::query_as(
        "SELECT c.charge_number, c.description, sum(al.amount) \
         FROM finance.payment_allocation al JOIN finance.charge c ON c.id = al.charge_id \
         WHERE al.tenant_id = $1 AND al.payment_id = $2 AND al.status <> 'reversed' \
         GROUP BY c.charge_number, c.description ORDER BY c.charge_number",
    )
    .bind(t.as_uuid())
    .bind(payment_id)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let (settings, _) = load_settings(&mut conn, t, inst).await?;
    let issued_local = local_stamp(&mut conn, issued_at, &institution.timezone).await?;
    let facts = ReceiptFacts {
        receipt_number: number.clone(),
        payment_number,
        issued_local,
        student_name: name,
        student_number: student_no,
        channel: channel.clone(),
        external_ref,
        payer_name,
        amount,
        unapplied,
        cancelled: status == "cancelled",
        cancel_reason,
        lines: lines
            .into_iter()
            .map(|(charge_number, description, amount)| PaidLine {
                charge_number,
                description,
                amount,
            })
            .collect(),
    };
    let duplicate = q.copy.as_deref() == Some("duplicate");
    let kick = q.kick.unwrap_or(false) && channel == "cash" && !facts.cancelled && !duplicate;
    let doc = fee_receipt_doc(&facts, &settings, &institution.name, duplicate);
    Ok(Json(slip(
        Some(id),
        Some(number),
        paper(q.columns),
        doc,
        kick,
    )))
}

/// A test slip for a counter's printer (`finance.read`).
#[utoipa::path(get, path = "/api/v1/finance/institutions/{id}/receipt-test-slip", operation_id = "finance_receipt_test_slip", tag = "finance-payments",
    params(("id" = String, Path, format = Uuid), TestSlipQuery), security(("session_cookie" = [])),
    responses((status = 200, body = ReceiptSlip), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn receipt_test_slip(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<TestSlipQuery>,
) -> ApiResult<Json<ReceiptSlip>> {
    let ctx = auth.require(Permission::FinanceRead)?;
    let inst = InstitutionId(id);
    let institution = require_institution(&st, &institution_scope(ctx, inst), inst).await?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let (settings, _) = load_settings(&mut conn, tenant(ctx), inst).await?;
    let columns = paper(q.columns);
    let doc = test_doc(&settings, &institution.name, columns);
    Ok(Json(slip(None, None, columns, doc, false)))
}

/// The counter's day close (`finance.read`): standing receipts by tender, cancellations and the
/// receipt-number range for one business day; `mine=true` limits it to the caller's receipts.
#[utoipa::path(get, path = "/api/v1/finance/institutions/{id}/counter-report", operation_id = "finance_receipt_counter_report", tag = "finance-payments",
    params(("id" = String, Path, format = Uuid), CounterReportQuery), security(("session_cookie" = [])),
    responses((status = 200, body = ReceiptCounterReport), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn counter_report_get(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<CounterReportQuery>,
) -> ApiResult<Json<ReceiptCounterReport>> {
    let ctx = auth.require(Permission::FinanceRead)?;
    let inst = InstitutionId(id);
    let institution = require_institution(&st, &institution_scope(ctx, inst), inst).await?;
    let t = tenant(ctx);
    let mine = q.mine.unwrap_or(false);
    let who = if mine {
        Some(ctx.subject.clone())
    } else {
        None
    };
    let tz = institution.timezone.clone();
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let date: NaiveDate = match q.date {
        Some(d) => d,
        None => sqlx::query_scalar("SELECT (now() AT TIME ZONE $1)::date")
            .bind(&tz)
            .fetch_one(&mut *conn)
            .await
            .map_err(map_db_error)?,
    };
    const DAY: &str = "r.tenant_id = $1 AND r.institution_id = $2 \
         AND (r.issued_at AT TIME ZONE $3)::date = $4 AND ($5::text IS NULL OR p.created_by = $5)";
    let channels: Vec<(String, i64, Decimal)> = sqlx::query_as(&format!(
        "SELECT p.channel, count(*), sum(r.amount) FROM finance.receipt r \
         JOIN finance.payment p ON p.id = r.payment_id \
         WHERE {DAY} AND r.status = 'issued' GROUP BY p.channel ORDER BY p.channel"
    ))
    .bind(t.as_uuid())
    .bind(inst.as_uuid())
    .bind(&tz)
    .bind(date)
    .bind(who.as_deref())
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let (cancelled_count, cancelled_amount, first, last): (
        i64,
        Option<Decimal>,
        Option<String>,
        Option<String>,
    ) = sqlx::query_as(&format!(
        "SELECT count(*) FILTER (WHERE r.status = 'cancelled'), \
         sum(r.amount) FILTER (WHERE r.status = 'cancelled'), \
         (array_agg(r.receipt_number ORDER BY r.issued_at, r.receipt_number))[1], \
         (array_agg(r.receipt_number ORDER BY r.issued_at DESC, r.receipt_number DESC))[1] \
         FROM finance.receipt r JOIN finance.payment p ON p.id = r.payment_id WHERE {DAY}"
    ))
    .bind(t.as_uuid())
    .bind(inst.as_uuid())
    .bind(&tz)
    .bind(date)
    .bind(who.as_deref())
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let (settings, _) = load_settings(&mut conn, t, inst).await?;
    let printed_local = local_stamp(&mut conn, Utc::now(), &tz).await?;
    let by_channel: Vec<ChannelTotal> = channels
        .into_iter()
        .map(|(channel, count, amount)| ChannelTotal {
            channel,
            count,
            amount,
        })
        .collect();
    let day = CounterDay {
        date_label: date.format("%-d %b %Y").to_string(),
        scope_label: match &who {
            Some(w) => format!("Cashier: {w}"),
            None => "All counters".to_owned(),
        },
        printed_local,
        receipt_count: by_channel.iter().map(|c| c.count).sum(),
        by_channel,
        cancelled_count,
        cancelled_amount: cancelled_amount.unwrap_or(Decimal::ZERO),
        first_receipt: first,
        last_receipt: last,
    };
    let (head, rows) = counter_report(&day, &settings, &institution.name);
    let columns = paper(q.columns);
    let bytes = escpos_report(&head, &rows, columns);
    let total = day.total();
    Ok(Json(ReceiptCounterReport {
        institution_id: inst.as_uuid(),
        date,
        mine,
        receipt_count: day.receipt_count,
        cancelled_count: day.cancelled_count,
        total: fmt(total),
        total_display: display_inr(total),
        by_channel: day
            .by_channel
            .iter()
            .map(|c| ReceiptCounterChannel {
                channel: c.channel.clone(),
                count: c.count,
                amount: fmt(c.amount),
            })
            .collect(),
        rows,
        preview: readable(&bytes),
        escpos_base64: b64(&bytes),
    }))
}

/// Routes of counter printing.
pub fn router(state: FinanceState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(print_settings_get, print_settings_put))
        .routes(routes!(receipt_slip))
        .routes(routes!(receipt_test_slip))
        .routes(routes!(counter_report_get))
        .with_state(state)
}
