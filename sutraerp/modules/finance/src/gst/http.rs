//! HTTP API of GST (tag `finance-gst`, ops `finance_gst_*`; contract
//! `docs/extensions-5d-gst.md`). The server is the source of truth: permission first, then the
//! tenant-scoped institution, then data; every change is audited.
//!
//! * `GET|PUT /institutions/{id}/gst-profile` — GSTIN, legal name, state, registration type,
//!   default zero-tax note, document series (`finance.read` / `finance.config.manage`).
//! * `GET /institutions/{id}/gst-fee-heads`, `PUT|DELETE /institutions/{id}/gst-fee-heads/{feeHeadId}`
//!   — SAC / HSN, rate, exempt + note, tax-inclusive pricing per fee head.
//! * `GET|PUT /student-accounts/{id}/gst-details` — the payer's place of supply and GSTIN (B2B).
//! * `GET /charges/{id}/tax-document`, `GET /gst-documents/{id}`,
//!   `GET /me/charges/{id}/tax-document` — printable tax invoice / bill of supply / credit note.
//! * `GET /institutions/{id}/gst-documents` — documents issued in a period.
//! * `GET /institutions/{id}/gst-summary` and `/gst-summary/csv` — GSTR-1 friendly summary.

use super::repo::{doc_lines, head_setups, load_profile, party, Profile, GST_HEAD_CODES};
use super::rules::{
    self, fy_label, gst_date, summarize, summary_csv, valid_rate, valid_series_prefix, Component,
    DocFact, DocKind, FactLine, Registration, Summary, TaxNotePreset, Treatment, TAX_NOTE_PRESETS,
};
use crate::printing::slip::amount_in_words_inr;
use crate::shared::charge::load_charge;
use crate::shared::guard::{require_institution, require_institution_writable};
use crate::shared::http::actor;
use crate::shared::me::require_own_account;
use crate::shared::money::{display_inr, fmt};
use crate::shared::publish::audit;
use crate::shared::scope::{institution_scope, tenant};
use crate::shared::time::business_today;
use crate::shared::FinanceState;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::{DateTime, Datelike, NaiveDate, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{PgConnection, Row};
use sutra_contracts::finance::ids::{ChargeId, StudentAccountId};
use sutra_kernel::auth::Permission;
use sutra_kernel::error::{FieldViolation, KernelError};
use sutra_kernel::india::{gstin_state, gstin_valid, state_name, GST_STATES};
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

// ------------------------------------------------------------------ wire types

/// A GST state / union territory.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GstState {
    pub code: String,
    pub name: String,
}

/// The institution's GST profile (defaults with `configured: false` until it saves one).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GstProfileView {
    pub institution_id: Uuid,
    pub configured: bool,
    pub registration_type: Registration,
    pub gstin: Option<String>,
    pub legal_name: String,
    pub trade_name: String,
    pub address: String,
    pub state_code: String,
    pub state_name: Option<String>,
    /// '' nothing, `preset:…` a shipped sentence, anything else printed verbatim.
    pub default_tax_note: String,
    /// `TI/{FY}/` — `{FY}` is the financial year (`26-27`).
    pub invoice_prefix: String,
    pub bill_prefix: String,
    pub credit_note_prefix: String,
    /// What the next numbers look like this financial year.
    pub sample_numbers: Vec<String>,
    pub enabled: bool,
    pub version: i64,
    pub updated_at: Option<DateTime<Utc>>,
    pub presets: Vec<TaxNotePreset>,
    /// Allowed GST rates (percent).
    pub rates: Vec<u32>,
    pub states: Vec<GstState>,
}

/// A new GST profile (the whole set).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GstProfileInput {
    pub registration_type: Registration,
    #[serde(default)]
    pub gstin: Option<String>,
    pub legal_name: String,
    #[serde(default)]
    pub trade_name: String,
    #[serde(default)]
    pub address: String,
    /// Defaults to the GSTIN's state.
    #[serde(default)]
    pub state_code: Option<String>,
    #[serde(default = "education_note")]
    pub default_tax_note: String,
    #[serde(default = "invoice_prefix")]
    pub invoice_prefix: String,
    #[serde(default = "bill_prefix")]
    pub bill_prefix: String,
    #[serde(default = "credit_prefix")]
    pub credit_note_prefix: String,
    #[serde(default = "yes")]
    pub enabled: bool,
}

fn education_note() -> String {
    "preset:educationExempt".into()
}
fn invoice_prefix() -> String {
    "TI/{FY}/".into()
}
fn bill_prefix() -> String {
    "BOS/{FY}/".into()
}
fn credit_prefix() -> String {
    "CN/{FY}/".into()
}
fn yes() -> bool {
    true
}

/// A fee head with its GST setup (`null` until it is set up).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GstFeeHead {
    pub fee_head_id: Uuid,
    pub code: String,
    pub name: String,
    pub category: String,
    pub status: String,
    pub tax: Option<GstFeeHeadTax>,
}

/// SAC / HSN, rate and pricing of a fee head.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GstFeeHeadTax {
    pub sac: String,
    /// Percent: 0, 5, 12, 18, 28 or 40.
    #[schema(value_type = String, example = "18")]
    pub gst_rate: String,
    pub exempt: bool,
    pub treatment: Treatment,
    pub tax_note: String,
    pub tax_inclusive: bool,
    pub version: i64,
}

/// New tax setup of a fee head.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GstFeeHeadTaxInput {
    pub sac: String,
    #[schema(value_type = String, example = "18")]
    pub gst_rate: String,
    #[serde(default)]
    pub exempt: bool,
    #[serde(default)]
    pub tax_note: String,
    #[serde(default)]
    pub tax_inclusive: bool,
}

/// The payer's place of supply and GST identity.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GstPartyView {
    pub student_account_id: Uuid,
    pub configured: bool,
    /// Place of supply (defaults to the institution's state).
    pub state_code: Option<String>,
    pub state_name: Option<String>,
    pub gstin: Option<String>,
    pub legal_name: String,
    pub version: i64,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GstPartyInput {
    /// Defaults to the GSTIN's state.
    #[serde(default)]
    pub state_code: Option<String>,
    #[serde(default)]
    pub gstin: Option<String>,
    #[serde(default)]
    pub legal_name: String,
}

/// Supplier block of a tax document.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GstDocSupplier {
    pub legal_name: String,
    pub trade_name: String,
    pub gstin: Option<String>,
    pub address: String,
    pub state_code: String,
    pub state_name: Option<String>,
}

/// Recipient block of a tax document.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GstDocRecipient {
    pub name: String,
    pub student_number: Option<String>,
    pub gstin: Option<String>,
    pub state_code: String,
    pub state_name: Option<String>,
}

/// One supply line of a tax document.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GstDocLine {
    pub line_no: i32,
    pub description: String,
    pub sac: Option<String>,
    pub treatment: Treatment,
    pub gst_rate: String,
    pub taxable_value: String,
    pub cgst: String,
    pub sgst: String,
    pub igst: String,
    pub total: String,
}

/// One tax row (`CGST @ 9%`).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GstDocTax {
    pub component: Component,
    pub label: String,
    pub rate: String,
    pub amount: String,
}

/// The invoice a credit note reduces.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GstDocOriginal {
    pub id: Uuid,
    pub number: Option<String>,
    pub date: Option<NaiveDate>,
}

/// A printable tax invoice, bill of supply or credit note.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GstTaxDocument {
    pub id: Uuid,
    pub kind: DocKind,
    /// `Tax Invoice`, `Bill of Supply` or `Credit Note`.
    pub title: String,
    /// `pending` (numbered when the charge is published), `issued` or `cancelled`.
    pub status: String,
    pub number: Option<String>,
    pub date: Option<NaiveDate>,
    /// `17-10-2026`.
    pub date_display: Option<String>,
    /// `2026-27`.
    pub financial_year: Option<String>,
    pub charge_id: Uuid,
    pub charge_number: String,
    pub adjustment_number: Option<String>,
    pub supplier: GstDocSupplier,
    pub recipient: GstDocRecipient,
    /// `27-Maharashtra`.
    pub place_of_supply: String,
    pub inter_state: bool,
    pub reverse_charge: bool,
    pub lines: Vec<GstDocLine>,
    pub taxes: Vec<GstDocTax>,
    pub taxable_value: String,
    pub cgst: String,
    pub sgst: String,
    pub igst: String,
    pub total_tax: String,
    pub total: String,
    /// `₹11,800.00`.
    pub total_display: String,
    pub amount_in_words: String,
    /// The zero-tax sentence (exempt / nil / composition), '' when everything is taxed.
    pub tax_note: String,
    pub original: Option<GstDocOriginal>,
}

/// A GST document in a list.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GstDocumentItem {
    pub id: Uuid,
    pub kind: DocKind,
    pub status: String,
    pub number: Option<String>,
    pub date: Option<NaiveDate>,
    pub charge_id: Uuid,
    pub charge_number: String,
    pub recipient_name: String,
    pub recipient_gstin: Option<String>,
    pub taxable_value: String,
    pub total_tax: String,
    pub total: String,
}

/// Money columns of a summary row.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GstAmounts {
    pub taxable_value: String,
    pub cgst: String,
    pub sgst: String,
    pub igst: String,
    pub total_tax: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GstB2bRow {
    pub recipient_gstin: String,
    pub recipient_name: String,
    pub number: String,
    pub date: NaiveDate,
    pub invoice_value: String,
    pub place_of_supply: String,
    pub rate: String,
    pub amounts: GstAmounts,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GstB2cRow {
    pub place_of_supply: String,
    pub inter_state: bool,
    pub rate: String,
    pub amounts: GstAmounts,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GstHsnRow {
    pub sac: String,
    pub rate: String,
    pub count: i64,
    pub amounts: GstAmounts,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GstExemptRow {
    pub intra_nil: String,
    pub intra_exempt: String,
    pub intra_non_gst: String,
    pub inter_nil: String,
    pub inter_exempt: String,
    pub inter_non_gst: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GstCreditNoteRow {
    pub registered: bool,
    pub recipient_gstin: Option<String>,
    pub recipient_name: String,
    pub number: String,
    pub date: NaiveDate,
    pub original_number: Option<String>,
    pub original_date: Option<NaiveDate>,
    pub place_of_supply: String,
    pub note_value: String,
    pub amounts: GstAmounts,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GstDocIssueRow {
    pub kind: DocKind,
    pub from: String,
    pub to: String,
    pub total: i64,
    pub cancelled: i64,
}

/// GSTR-1 friendly summary of a period.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GstSummaryView {
    pub institution_id: Uuid,
    pub gstin: Option<String>,
    pub from: NaiveDate,
    pub to: NaiveDate,
    pub b2b: Vec<GstB2bRow>,
    pub b2c: Vec<GstB2cRow>,
    pub hsn: Vec<GstHsnRow>,
    pub exempt: GstExemptRow,
    pub credit_notes: Vec<GstCreditNoteRow>,
    pub documents: Vec<GstDocIssueRow>,
    /// Outward taxable supplies net of credit notes.
    pub net: GstAmounts,
}

#[derive(Debug, Clone, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
pub struct GstPeriodQuery {
    /// First day (default: the 1st of this month, institution time).
    pub from: Option<NaiveDate>,
    /// Last day (default: today).
    pub to: Option<NaiveDate>,
}

// ------------------------------------------------------------------ helpers

fn rate_str(d: Decimal) -> String {
    d.normalize().to_string()
}

fn states() -> Vec<GstState> {
    GST_STATES
        .iter()
        .map(|(c, n)| GstState {
            code: (*c).to_owned(),
            name: (*n).to_owned(),
        })
        .collect()
}

fn invalid(field: &str, code: &str, msg: impl Into<String>) -> FieldViolation {
    FieldViolation::new(field, code, msg.into())
}

fn check_note(field: &str, note: &str, v: &mut Vec<FieldViolation>) {
    if note.chars().count() > 400 {
        v.push(invalid(field, "too-long", "at most 400 characters"));
    }
    if note.trim().starts_with("preset:") && !rules::is_tax_note_preset(note.trim()) {
        v.push(invalid(
            field,
            "unknown-preset",
            "not one of the shipped sentences",
        ));
    }
}

fn normal_gstin(g: Option<&str>) -> Option<String> {
    g.map(|s| s.trim().to_uppercase()).filter(|s| !s.is_empty())
}

fn profile_view(inst: InstitutionId, p: Option<Profile>, today: NaiveDate) -> GstProfileView {
    let configured = p.is_some();
    let p = p.unwrap_or(Profile {
        registration: Registration::Regular,
        gstin: None,
        legal_name: String::new(),
        trade_name: String::new(),
        address: String::new(),
        state_code: "27".into(),
        default_tax_note: education_note(),
        invoice_prefix: invoice_prefix(),
        bill_prefix: bill_prefix(),
        credit_note_prefix: credit_prefix(),
        enabled: true,
        version: 0,
        updated_at: None,
    });
    GstProfileView {
        institution_id: inst.as_uuid(),
        configured,
        registration_type: p.registration,
        state_name: state_name(&p.state_code).map(str::to_owned),
        sample_numbers: [&p.invoice_prefix, &p.bill_prefix, &p.credit_note_prefix]
            .iter()
            .map(|x| rules::format_doc_number(x, 1, today))
            .collect(),
        gstin: p.gstin,
        legal_name: p.legal_name,
        trade_name: p.trade_name,
        address: p.address,
        state_code: p.state_code,
        default_tax_note: p.default_tax_note,
        invoice_prefix: p.invoice_prefix,
        bill_prefix: p.bill_prefix,
        credit_note_prefix: p.credit_note_prefix,
        enabled: p.enabled,
        version: p.version,
        updated_at: p.updated_at,
        presets: TAX_NOTE_PRESETS.to_vec(),
        rates: rules::GST_RATES.to_vec(),
        states: states(),
    }
}

/// Validates a profile input into `(gstin, state)`.
fn validate_profile(i: &GstProfileInput) -> Result<(Option<String>, String), KernelError> {
    let mut v = Vec::new();
    let gstin = normal_gstin(i.gstin.as_deref());
    match (&gstin, i.registration_type) {
        (None, Registration::Regular | Registration::Composition) => v.push(invalid(
            "gstin",
            "required",
            "a registered institution needs its GSTIN",
        )),
        (Some(g), _) if !gstin_valid(g) => v.push(invalid(
            "gstin",
            "invalid",
            "not a valid GSTIN (15 characters, state code, PAN, check character)",
        )),
        _ => {}
    }
    let state = i
        .state_code
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .or_else(|| gstin.as_deref().and_then(gstin_state).map(str::to_owned))
        .unwrap_or_default();
    if state_name(&state).is_none() {
        v.push(invalid("stateCode", "invalid", "not a GST state code"));
    } else if let Some(g) = &gstin {
        if gstin_state(g).is_some_and(|s| s != state) {
            v.push(invalid(
                "stateCode",
                "mismatch",
                "the GSTIN is registered in another state",
            ));
        }
    }
    let name = i.legal_name.trim();
    if name.is_empty() || name.chars().count() > 200 {
        v.push(invalid("legalName", "required", "1 to 200 characters"));
    }
    if i.trade_name.chars().count() > 200 {
        v.push(invalid("tradeName", "too-long", "at most 200 characters"));
    }
    if i.address.chars().count() > 300 {
        v.push(invalid("address", "too-long", "at most 300 characters"));
    }
    check_note("defaultTaxNote", &i.default_tax_note, &mut v);
    for (field, prefix) in [
        ("invoicePrefix", &i.invoice_prefix),
        ("billPrefix", &i.bill_prefix),
        ("creditNotePrefix", &i.credit_note_prefix),
    ] {
        if let Err(m) = valid_series_prefix(prefix.trim()) {
            v.push(invalid(field, "invalid", m));
        }
    }
    let p = [
        i.invoice_prefix.trim(),
        i.bill_prefix.trim(),
        i.credit_note_prefix.trim(),
    ];
    if p[0] == p[1] || p[0] == p[2] || p[1] == p[2] {
        v.push(invalid(
            "invoicePrefix",
            "duplicate",
            "each series needs its own prefix (numbers are unique per financial year)",
        ));
    }
    if v.is_empty() {
        Ok((gstin, state))
    } else {
        Err(KernelError::Validation(v))
    }
}

async fn require_head(
    conn: &mut PgConnection,
    t: TenantId,
    inst: InstitutionId,
    head: Uuid,
) -> Result<String, KernelError> {
    let code: Option<String> = sqlx::query_scalar(
        "SELECT code FROM finance.fee_head WHERE tenant_id = $1 AND institution_id = $2 AND id = $3",
    )
    .bind(t.as_uuid())
    .bind(inst.as_uuid())
    .bind(head)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let code = code.ok_or(KernelError::NotFound { entity: "fee head" })?;
    if GST_HEAD_CODES.contains(&code.as_str()) {
        return Err(KernelError::Conflict(
            "the GST system fee heads carry tax rows; they have no tax setup of their own".into(),
        ));
    }
    Ok(code)
}

fn amounts(a: &rules::Amounts) -> GstAmounts {
    GstAmounts {
        taxable_value: fmt(a.taxable_value),
        cgst: fmt(a.cgst),
        sgst: fmt(a.sgst),
        igst: fmt(a.igst),
        total_tax: fmt(a.tax()),
    }
}

fn place_label(code: &str) -> String {
    match state_name(code) {
        Some(n) => format!("{code}-{n}"),
        None => code.to_owned(),
    }
}

/// Builds the printable view of one GST document.
async fn document(
    conn: &mut PgConnection,
    t: TenantId,
    id: Uuid,
) -> Result<GstTaxDocument, KernelError> {
    let r = sqlx::query(
        "SELECT d.id, d.institution_id, d.kind, d.status, d.document_number, d.document_date, d.charge_id, \
         c.charge_number, a.adjustment_number, d.supplier_gstin, d.supplier_state, d.place_of_supply, d.inter_state, \
         d.recipient_name, d.recipient_gstin, s.student_number, d.taxable_value, d.cgst, d.sgst, d.igst, d.total, \
         d.tax_note, d.original_document_id, o.document_number AS original_number, o.document_date AS original_date \
         FROM finance.gst_document d JOIN finance.charge c ON c.id = d.charge_id \
         JOIN finance.student_account s ON s.id = c.student_account_id \
         LEFT JOIN finance.adjustment a ON a.id = d.adjustment_id \
         LEFT JOIN finance.gst_document o ON o.id = d.original_document_id \
         WHERE d.tenant_id = $1 AND d.id = $2",
    )
    .bind(t.as_uuid())
    .bind(id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "GST document",
    })?;
    let get = || -> Result<_, sqlx::Error> {
        Ok((
            r.try_get::<Uuid, _>("institution_id")?,
            r.try_get::<String, _>("kind")?,
            r.try_get::<String, _>("status")?,
            r.try_get::<Option<String>, _>("document_number")?,
            r.try_get::<Option<NaiveDate>, _>("document_date")?,
            r.try_get::<Uuid, _>("charge_id")?,
            r.try_get::<String, _>("charge_number")?,
            r.try_get::<Option<String>, _>("adjustment_number")?,
        ))
    };
    let (inst, kind, status, number, date, charge_id, charge_number, adjustment_number) =
        get().map_err(map_db_error)?;
    let get2 = || -> Result<_, sqlx::Error> {
        Ok((
            r.try_get::<Option<String>, _>("supplier_gstin")?,
            r.try_get::<String, _>("supplier_state")?,
            r.try_get::<String, _>("place_of_supply")?,
            r.try_get::<bool, _>("inter_state")?,
            r.try_get::<String, _>("recipient_name")?,
            r.try_get::<Option<String>, _>("recipient_gstin")?,
            r.try_get::<Option<String>, _>("student_number")?,
            r.try_get::<String, _>("tax_note")?,
        ))
    };
    let (s_gstin, s_state, place, inter, r_name, r_gstin, student_number, tax_note) =
        get2().map_err(map_db_error)?;
    let get3 = || -> Result<_, sqlx::Error> {
        Ok((
            r.try_get::<Decimal, _>("taxable_value")?,
            r.try_get::<Decimal, _>("cgst")?,
            r.try_get::<Decimal, _>("sgst")?,
            r.try_get::<Decimal, _>("igst")?,
            r.try_get::<Decimal, _>("total")?,
            r.try_get::<Option<Uuid>, _>("original_document_id")?,
            r.try_get::<Option<String>, _>("original_number")?,
            r.try_get::<Option<NaiveDate>, _>("original_date")?,
        ))
    };
    let (taxable, cgst, sgst, igst, total, orig_id, orig_number, orig_date) =
        get3().map_err(map_db_error)?;
    let kind = DocKind::parse(&kind).unwrap_or(DocKind::TaxInvoice);
    let profile = load_profile(conn, t, InstitutionId(inst)).await?;
    let lines = doc_lines(conn, t, id).await?;
    let taxes: Vec<(String, Decimal, Decimal)> = sqlx::query_as(
        "SELECT component, rate, amount FROM finance.gst_document_tax WHERE tenant_id = $1 AND gst_document_id = $2 \
         ORDER BY rate, component",
    )
    .bind(t.as_uuid())
    .bind(id)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let (legal_name, trade_name, address) = profile
        .map(|p| (p.legal_name, p.trade_name, p.address))
        .unwrap_or_default();
    Ok(GstTaxDocument {
        id,
        kind,
        title: kind.title().to_owned(),
        status,
        date_display: date.map(gst_date),
        financial_year: date.map(fy_label),
        number,
        date,
        charge_id,
        charge_number,
        adjustment_number,
        supplier: GstDocSupplier {
            legal_name,
            trade_name,
            gstin: s_gstin,
            address,
            state_name: state_name(&s_state).map(str::to_owned),
            state_code: s_state,
        },
        recipient: GstDocRecipient {
            name: r_name,
            student_number,
            gstin: r_gstin,
            state_name: state_name(&place).map(str::to_owned),
            state_code: place.clone(),
        },
        place_of_supply: place_label(&place),
        inter_state: inter,
        reverse_charge: false,
        lines: lines
            .iter()
            .enumerate()
            .map(|(i, (desc, l))| GstDocLine {
                line_no: i32::try_from(i + 1).unwrap_or(i32::MAX),
                description: desc.clone(),
                sac: l.sac.clone(),
                treatment: l.treatment,
                gst_rate: rate_str(l.rate),
                taxable_value: fmt(l.taxable_value),
                cgst: fmt(l.cgst),
                sgst: fmt(l.sgst),
                igst: fmt(l.igst),
                total: fmt(l.taxable_value + l.tax()),
            })
            .collect(),
        taxes: taxes
            .into_iter()
            .filter_map(|(c, rate, amount)| {
                Component::parse(&c).map(|component| GstDocTax {
                    label: rules::PlanRow {
                        component,
                        rate,
                        amount,
                    }
                    .label(),
                    component,
                    rate: rate_str(rate),
                    amount: fmt(amount),
                })
            })
            .collect(),
        taxable_value: fmt(taxable),
        cgst: fmt(cgst),
        sgst: fmt(sgst),
        igst: fmt(igst),
        total_tax: fmt(cgst + sgst + igst),
        total: fmt(total),
        total_display: display_inr(total),
        amount_in_words: amount_in_words_inr(total),
        tax_note,
        original: orig_id.map(|id| GstDocOriginal {
            id,
            number: orig_number,
            date: orig_date,
        }),
    })
}

async fn charge_document_id(
    conn: &mut PgConnection,
    t: TenantId,
    charge: Uuid,
) -> Result<Uuid, KernelError> {
    sqlx::query_scalar(
        "SELECT id FROM finance.gst_document WHERE tenant_id = $1 AND charge_id = $2 AND kind <> 'credit_note'",
    )
    .bind(t.as_uuid())
    .bind(charge)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "GST document",
    })
}

async fn period(
    st: &FinanceState,
    scope: &ExecutionScope,
    inst: InstitutionId,
    q: &GstPeriodQuery,
) -> Result<(NaiveDate, NaiveDate), KernelError> {
    let today = business_today(st, scope, inst).await?;
    let from = q.from.unwrap_or_else(|| today.with_day(1).unwrap_or(today));
    let to = q.to.unwrap_or(today);
    if to < from {
        return Err(KernelError::Validation(vec![invalid(
            "to",
            "before-from",
            "the period ends before it starts",
        )]));
    }
    if (to - from).num_days() > 400 {
        return Err(KernelError::Validation(vec![invalid(
            "to",
            "too-long",
            "at most 400 days at a time",
        )]));
    }
    Ok((from, to))
}

/// Documents issued in a period, as summary facts.
async fn facts(
    conn: &mut PgConnection,
    t: TenantId,
    inst: InstitutionId,
    from: NaiveDate,
    to: NaiveDate,
) -> Result<Vec<DocFact>, KernelError> {
    let rows = sqlx::query(
        "SELECT d.id, d.kind, d.status, c.status AS charge_status, d.document_number, d.document_date, \
         d.recipient_gstin, d.recipient_name, d.place_of_supply, d.inter_state, \
         o.document_number AS original_number, o.document_date AS original_date \
         FROM finance.gst_document d JOIN finance.charge c ON c.id = d.charge_id \
         LEFT JOIN finance.gst_document o ON o.id = d.original_document_id \
         WHERE d.tenant_id = $1 AND d.institution_id = $2 AND d.status <> 'pending' \
         AND d.document_date BETWEEN $3 AND $4 ORDER BY d.document_date, d.document_number",
    )
    .bind(t.as_uuid())
    .bind(inst.as_uuid())
    .bind(from)
    .bind(to)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let mut out = Vec::with_capacity(rows.len());
    for r in &rows {
        let read = || -> Result<_, sqlx::Error> {
            Ok((
                r.try_get::<Uuid, _>("id")?,
                r.try_get::<String, _>("kind")?,
                r.try_get::<String, _>("status")?,
                r.try_get::<String, _>("charge_status")?,
                r.try_get::<String, _>("document_number")?,
                r.try_get::<NaiveDate, _>("document_date")?,
                r.try_get::<Option<String>, _>("recipient_gstin")?,
                r.try_get::<String, _>("recipient_name")?,
                r.try_get::<String, _>("place_of_supply")?,
                r.try_get::<bool, _>("inter_state")?,
                r.try_get::<Option<String>, _>("original_number")?,
                r.try_get::<Option<NaiveDate>, _>("original_date")?,
            ))
        };
        let (id, kind, status, charge_status, number, date, gstin, name, place, inter, on, od) =
            read().map_err(map_db_error)?;
        let kind = DocKind::parse(&kind).unwrap_or(DocKind::TaxInvoice);
        let lines = doc_lines(conn, t, id)
            .await?
            .into_iter()
            .map(|(_, l)| FactLine {
                sac: l.sac.clone(),
                treatment: l.treatment,
                rate: l.rate,
                taxable_value: l.taxable_value,
                cgst: l.cgst,
                sgst: l.sgst,
                igst: l.igst,
            })
            .collect();
        out.push(DocFact {
            kind,
            number,
            date,
            cancelled: status == "cancelled"
                || (kind != DocKind::CreditNote && charge_status == "cancelled"),
            recipient_gstin: gstin,
            recipient_name: name,
            place_of_supply: place,
            inter_state: inter,
            original_number: on,
            original_date: od,
            lines,
        });
    }
    Ok(out)
}

fn summary_view(
    inst: InstitutionId,
    gstin: Option<String>,
    from: NaiveDate,
    to: NaiveDate,
    s: &Summary,
) -> GstSummaryView {
    let e = &s.exempt;
    GstSummaryView {
        institution_id: inst.as_uuid(),
        gstin,
        from,
        to,
        b2b: s
            .b2b
            .iter()
            .map(|r| GstB2bRow {
                recipient_gstin: r.recipient_gstin.clone(),
                recipient_name: r.recipient_name.clone(),
                number: r.number.clone(),
                date: r.date,
                invoice_value: fmt(r.invoice_value),
                place_of_supply: place_label(&r.place_of_supply),
                rate: rate_str(r.rate),
                amounts: amounts(&r.amounts),
            })
            .collect(),
        b2c: s
            .b2c
            .iter()
            .map(|r| GstB2cRow {
                place_of_supply: place_label(&r.place_of_supply),
                inter_state: r.inter_state,
                rate: rate_str(r.rate),
                amounts: amounts(&r.amounts),
            })
            .collect(),
        hsn: s
            .hsn
            .iter()
            .map(|r| GstHsnRow {
                sac: r.sac.clone(),
                rate: rate_str(r.rate),
                count: r.count,
                amounts: amounts(&r.amounts),
            })
            .collect(),
        exempt: GstExemptRow {
            intra_nil: fmt(e.intra_nil),
            intra_exempt: fmt(e.intra_exempt),
            intra_non_gst: fmt(e.intra_non_gst),
            inter_nil: fmt(e.inter_nil),
            inter_exempt: fmt(e.inter_exempt),
            inter_non_gst: fmt(e.inter_non_gst),
        },
        credit_notes: s
            .credit_notes
            .iter()
            .map(|r| GstCreditNoteRow {
                registered: r.registered,
                recipient_gstin: r.recipient_gstin.clone(),
                recipient_name: r.recipient_name.clone(),
                number: r.number.clone(),
                date: r.date,
                original_number: r.original_number.clone(),
                original_date: r.original_date,
                place_of_supply: place_label(&r.place_of_supply),
                note_value: fmt(r.note_value),
                amounts: amounts(&r.amounts),
            })
            .collect(),
        documents: s
            .documents
            .iter()
            .map(|r| GstDocIssueRow {
                kind: r.kind,
                from: r.from.clone(),
                to: r.to.clone(),
                total: r.total,
                cancelled: r.cancelled,
            })
            .collect(),
        net: amounts(&s.net),
    }
}

// ------------------------------------------------------------------ handlers

/// The institution's GST profile (`finance.read`); defaults until it saves one.
#[utoipa::path(get, path = "/api/v1/finance/institutions/{id}/gst-profile", operation_id = "finance_gst_profile_get", tag = "finance-gst",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = GstProfileView), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn profile_get(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<GstProfileView>> {
    let ctx = auth.require(Permission::FinanceRead)?;
    let inst = InstitutionId(id);
    let scope = institution_scope(ctx, inst);
    require_institution(&st, &scope, inst).await?;
    let today = business_today(&st, &scope, inst).await?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let p = load_profile(&mut conn, tenant(ctx), inst).await?;
    Ok(Json(profile_view(inst, p, today)))
}

/// Saves the GST profile (`finance.config.manage`). The GSTIN is checked with its check
/// character and must be registered in the profile's state.
#[utoipa::path(put, path = "/api/v1/finance/institutions/{id}/gst-profile", operation_id = "finance_gst_profile_put", tag = "finance-gst",
    params(("id" = String, Path, format = Uuid)), request_body = GstProfileInput, security(("session_cookie" = [])),
    responses((status = 200, body = GstProfileView), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn profile_put(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Json(input): Json<GstProfileInput>,
) -> ApiResult<Json<GstProfileView>> {
    let ctx = auth.require(Permission::FinanceConfigManage)?;
    let inst = InstitutionId(id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let (gstin, state) = validate_profile(&input)?;
    let t = tenant(ctx);
    let a = actor(ctx);
    let today = business_today(&st, &scope, inst).await?;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    sqlx::query(
        "INSERT INTO finance.gst_profile (tenant_id, institution_id, registration_type, gstin, legal_name, trade_name, \
         address, state_code, default_tax_note, invoice_prefix, bill_prefix, credit_note_prefix, enabled, updated_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14) \
         ON CONFLICT (tenant_id, institution_id) DO UPDATE SET registration_type = EXCLUDED.registration_type, \
         gstin = EXCLUDED.gstin, legal_name = EXCLUDED.legal_name, trade_name = EXCLUDED.trade_name, \
         address = EXCLUDED.address, state_code = EXCLUDED.state_code, default_tax_note = EXCLUDED.default_tax_note, \
         invoice_prefix = EXCLUDED.invoice_prefix, bill_prefix = EXCLUDED.bill_prefix, \
         credit_note_prefix = EXCLUDED.credit_note_prefix, enabled = EXCLUDED.enabled, \
         updated_by = EXCLUDED.updated_by, updated_at = now(), version = finance.gst_profile.version + 1",
    )
    .bind(t.as_uuid())
    .bind(inst.as_uuid())
    .bind(input.registration_type.as_str())
    .bind(&gstin)
    .bind(input.legal_name.trim())
    .bind(input.trade_name.trim())
    .bind(input.address.trim())
    .bind(&state)
    .bind(input.default_tax_note.trim())
    .bind(input.invoice_prefix.trim())
    .bind(input.bill_prefix.trim())
    .bind(input.credit_note_prefix.trim())
    .bind(input.enabled)
    .bind(a.id.as_deref())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    audit(
        &mut tx,
        &scope,
        "finance.gst.profile.update",
        "gst_profile",
        inst.as_uuid().to_string(),
        json!({"registrationType": input.registration_type, "gstin": gstin, "stateCode": state,
               "enabled": input.enabled}),
        &a,
    )
    .await?;
    let p = load_profile(&mut tx, t, inst).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(Json(profile_view(inst, p, today)))
}

/// Every fee head of the institution with its GST setup (`finance.read`); the system GST heads
/// are left out.
#[utoipa::path(get, path = "/api/v1/finance/institutions/{id}/gst-fee-heads", operation_id = "finance_gst_fee_head_list", tag = "finance-gst",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = Vec<GstFeeHead>), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn fee_head_list(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Vec<GstFeeHead>>> {
    let ctx = auth.require(Permission::FinanceRead)?;
    let inst = InstitutionId(id);
    require_institution(&st, &institution_scope(ctx, inst), inst).await?;
    let t = tenant(ctx);
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let heads: Vec<(Uuid, String, String, String, String)> = sqlx::query_as(
        "SELECT id, code, name, category, status FROM finance.fee_head WHERE tenant_id = $1 AND institution_id = $2 \
         AND NOT (code = ANY($3)) ORDER BY code",
    )
    .bind(t.as_uuid())
    .bind(inst.as_uuid())
    .bind(&GST_HEAD_CODES[..])
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let ids: Vec<Uuid> = heads.iter().map(|h| h.0).collect();
    let setups = head_setups(&mut conn, t, &ids).await?;
    let registration = load_profile(&mut conn, t, inst)
        .await?
        .map_or(Registration::Regular, |p| p.registration);
    Ok(Json(
        heads
            .into_iter()
            .map(|(id, code, name, category, status)| GstFeeHead {
                tax: setups.get(&id).map(|s| GstFeeHeadTax {
                    sac: s.tax.sac.clone(),
                    gst_rate: rate_str(s.tax.rate),
                    exempt: s.tax.exempt,
                    treatment: s.tax.treatment(registration),
                    tax_note: s.tax_note.clone(),
                    tax_inclusive: s.tax.inclusive,
                    version: s.version,
                }),
                fee_head_id: id,
                code,
                name,
                category,
                status,
            })
            .collect(),
    ))
}

/// Sets a fee head's GST: SAC / HSN, rate, exempt (rate 0) with its note, tax-inclusive pricing
/// (`finance.config.manage`). Applies to charges raised from now on.
#[utoipa::path(put, path = "/api/v1/finance/institutions/{id}/gst-fee-heads/{feeHeadId}", operation_id = "finance_gst_fee_head_put", tag = "finance-gst",
    params(("id" = String, Path, format = Uuid), ("feeHeadId" = String, Path, format = Uuid)), request_body = GstFeeHeadTaxInput, security(("session_cookie" = [])),
    responses((status = 200, body = GstFeeHead), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn fee_head_put(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path((id, head)): Path<(Uuid, Uuid)>,
    Json(input): Json<GstFeeHeadTaxInput>,
) -> ApiResult<Json<GstFeeHead>> {
    let ctx = auth.require(Permission::FinanceConfigManage)?;
    let inst = InstitutionId(id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let mut v = Vec::new();
    let sac = input.sac.trim().to_owned();
    if !(4..=8).contains(&sac.len()) || !sac.chars().all(|c| c.is_ascii_digit()) {
        v.push(invalid(
            "sac",
            "invalid",
            "a SAC / HSN code of 4 to 8 digits",
        ));
    }
    let rate: Option<Decimal> = input
        .gst_rate
        .trim()
        .parse()
        .ok()
        .filter(|r| valid_rate(*r));
    match rate {
        None => v.push(invalid("gstRate", "invalid", "one of 0, 5, 12, 18, 28, 40")),
        Some(r) if input.exempt && !r.is_zero() => v.push(invalid(
            "gstRate",
            "exempt-rate",
            "an exempt supply is at rate 0",
        )),
        _ => {}
    }
    check_note("taxNote", &input.tax_note, &mut v);
    if !v.is_empty() {
        return Err(KernelError::Validation(v).into());
    }
    let rate = rate.unwrap_or_default();
    let t = tenant(ctx);
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let code = require_head(&mut tx, t, inst, head).await?;
    sqlx::query(
        "INSERT INTO finance.fee_head_tax (tenant_id, institution_id, fee_head_id, sac, gst_rate, exempt, tax_note, \
         tax_inclusive, updated_by) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) \
         ON CONFLICT (tenant_id, fee_head_id) DO UPDATE SET sac = EXCLUDED.sac, gst_rate = EXCLUDED.gst_rate, \
         exempt = EXCLUDED.exempt, tax_note = EXCLUDED.tax_note, tax_inclusive = EXCLUDED.tax_inclusive, \
         updated_by = EXCLUDED.updated_by, updated_at = now(), version = finance.fee_head_tax.version + 1",
    )
    .bind(t.as_uuid())
    .bind(inst.as_uuid())
    .bind(head)
    .bind(&sac)
    .bind(rate)
    .bind(input.exempt)
    .bind(input.tax_note.trim())
    .bind(input.tax_inclusive)
    .bind(a.id.as_deref())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    audit(
        &mut tx,
        &scope,
        "finance.gst.fee_head.update",
        "fee_head",
        head.to_string(),
        json!({"code": code, "sac": sac, "gstRate": rate_str(rate), "exempt": input.exempt,
               "taxInclusive": input.tax_inclusive}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let Json(list) = fee_head_list(State(st), auth, Path(id)).await?;
    list.into_iter()
        .find(|h| h.fee_head_id == head)
        .map(Json)
        .ok_or_else(|| KernelError::NotFound { entity: "fee head" }.into())
}

/// Removes a fee head's GST setup (`finance.config.manage`): its charges are outside the GST
/// documents from now on.
#[utoipa::path(delete, path = "/api/v1/finance/institutions/{id}/gst-fee-heads/{feeHeadId}", operation_id = "finance_gst_fee_head_delete", tag = "finance-gst",
    params(("id" = String, Path, format = Uuid), ("feeHeadId" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 204), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn fee_head_delete(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path((id, head)): Path<(Uuid, Uuid)>,
) -> ApiResult<StatusCode> {
    let ctx = auth.require(Permission::FinanceConfigManage)?;
    let inst = InstitutionId(id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let t = tenant(ctx);
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let code = require_head(&mut tx, t, inst, head).await?;
    sqlx::query("DELETE FROM finance.fee_head_tax WHERE tenant_id = $1 AND fee_head_id = $2")
        .bind(t.as_uuid())
        .bind(head)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    audit(
        &mut tx,
        &scope,
        "finance.gst.fee_head.clear",
        "fee_head",
        head.to_string(),
        json!({"code": code}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn account_institution(
    conn: &mut PgConnection,
    t: TenantId,
    account: Uuid,
) -> Result<InstitutionId, KernelError> {
    sqlx::query_scalar::<_, Uuid>(
        "SELECT institution_id FROM finance.student_account WHERE tenant_id = $1 AND id = $2",
    )
    .bind(t.as_uuid())
    .bind(account)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .map(InstitutionId)
    .ok_or(KernelError::NotFound {
        entity: "student account",
    })
}

/// The payer's place of supply and GSTIN (`finance.read`).
#[utoipa::path(get, path = "/api/v1/finance/student-accounts/{id}/gst-details", operation_id = "finance_gst_party_get", tag = "finance-gst",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = GstPartyView), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn party_get(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<GstPartyView>> {
    let ctx = auth.require(Permission::FinanceRead)?;
    let t = tenant(ctx);
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let inst = account_institution(&mut conn, t, id).await?;
    require_institution(&st, &institution_scope(ctx, inst), inst).await?;
    let p = party(&mut conn, t, StudentAccountId(id)).await?;
    Ok(Json(party_view(id, p)))
}

fn party_view(id: Uuid, p: Option<(String, Option<String>, String, i64)>) -> GstPartyView {
    match p {
        Some((state, gstin, name, version)) => GstPartyView {
            student_account_id: id,
            configured: true,
            state_name: state_name(&state).map(str::to_owned),
            state_code: Some(state),
            gstin,
            legal_name: name,
            version,
        },
        None => GstPartyView {
            student_account_id: id,
            configured: false,
            state_code: None,
            state_name: None,
            gstin: None,
            legal_name: String::new(),
            version: 0,
        },
    }
}

/// Sets the payer's place of supply and, for a registered payer, its GSTIN (`charge.manage`).
/// Applies to charges raised from now on.
#[utoipa::path(put, path = "/api/v1/finance/student-accounts/{id}/gst-details", operation_id = "finance_gst_party_put", tag = "finance-gst",
    params(("id" = String, Path, format = Uuid)), request_body = GstPartyInput, security(("session_cookie" = [])),
    responses((status = 200, body = GstPartyView), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn party_put(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Json(input): Json<GstPartyInput>,
) -> ApiResult<Json<GstPartyView>> {
    let ctx = auth.require(Permission::ChargeManage)?;
    let t = tenant(ctx);
    let mut v = Vec::new();
    let gstin = normal_gstin(input.gstin.as_deref());
    if gstin.as_deref().is_some_and(|g| !gstin_valid(g)) {
        v.push(invalid("gstin", "invalid", "not a valid GSTIN"));
    }
    let state = input
        .state_code
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .or_else(|| gstin.as_deref().and_then(gstin_state).map(str::to_owned))
        .unwrap_or_default();
    if state_name(&state).is_none() {
        v.push(invalid("stateCode", "invalid", "not a GST state code"));
    } else if gstin
        .as_deref()
        .and_then(gstin_state)
        .is_some_and(|s| s != state)
    {
        v.push(invalid(
            "stateCode",
            "mismatch",
            "the GSTIN is registered in another state",
        ));
    }
    if input.legal_name.chars().count() > 200 {
        v.push(invalid("legalName", "too-long", "at most 200 characters"));
    }
    if !v.is_empty() {
        return Err(KernelError::Validation(v).into());
    }
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let inst = account_institution(&mut conn, t, id).await?;
    drop(conn);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    sqlx::query(
        "INSERT INTO finance.gst_party (tenant_id, student_account_id, state_code, gstin, legal_name, updated_by) \
         VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT (tenant_id, student_account_id) DO UPDATE SET \
         state_code = EXCLUDED.state_code, gstin = EXCLUDED.gstin, legal_name = EXCLUDED.legal_name, \
         updated_by = EXCLUDED.updated_by, updated_at = now(), version = finance.gst_party.version + 1",
    )
    .bind(t.as_uuid())
    .bind(id)
    .bind(&state)
    .bind(&gstin)
    .bind(input.legal_name.trim())
    .bind(a.id.as_deref())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    audit(
        &mut tx,
        &scope,
        "finance.gst.party.update",
        "student_account",
        id.to_string(),
        json!({"stateCode": state, "gstin": gstin}),
        &a,
    )
    .await?;
    let p = party(&mut tx, t, StudentAccountId(id)).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(Json(party_view(id, p)))
}

/// The tax invoice / bill of supply of a charge, printable (`finance.read`); 404 when the
/// charge is outside the GST documents.
#[utoipa::path(get, path = "/api/v1/finance/charges/{id}/tax-document", operation_id = "finance_gst_charge_document", tag = "finance-gst",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = GstTaxDocument), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn charge_document(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<GstTaxDocument>> {
    let ctx = auth.require(Permission::FinanceRead)?;
    let t = tenant(ctx);
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let c = load_charge(&mut conn, t, ChargeId(id)).await?;
    require_institution(
        &st,
        &institution_scope(ctx, c.institution_id),
        c.institution_id,
    )
    .await?;
    let doc = charge_document_id(&mut conn, t, id).await?;
    Ok(Json(document(&mut conn, t, doc).await?))
}

/// The student's own tax invoice / bill of supply of one of their charges (`finance.self`).
#[utoipa::path(get, path = "/api/v1/finance/me/charges/{id}/tax-document", operation_id = "finance_gst_self_charge_document", tag = "finance-gst",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = GstTaxDocument), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn my_charge_document(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<GstTaxDocument>> {
    let ctx = auth.require(Permission::FinanceSelfService)?;
    let t = tenant(ctx);
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let c = load_charge(&mut conn, t, ChargeId(id)).await?;
    require_own_account(&st, ctx, c.student_account_id).await?;
    let doc = charge_document_id(&mut conn, t, id).await?;
    let view = document(&mut conn, t, doc).await?;
    if view.status == "pending" {
        // A student sees a document only once it is issued.
        return Err(KernelError::NotFound {
            entity: "GST document",
        }
        .into());
    }
    Ok(Json(view))
}

/// One GST document (credit notes included), printable (`finance.read`).
#[utoipa::path(get, path = "/api/v1/finance/gst-documents/{id}", operation_id = "finance_gst_document_get", tag = "finance-gst",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = GstTaxDocument), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn document_get(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<GstTaxDocument>> {
    let ctx = auth.require(Permission::FinanceRead)?;
    let t = tenant(ctx);
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let view = document(&mut conn, t, id).await?;
    let inst: Uuid = sqlx::query_scalar(
        "SELECT institution_id FROM finance.gst_document WHERE tenant_id = $1 AND id = $2",
    )
    .bind(t.as_uuid())
    .bind(id)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let inst = InstitutionId(inst);
    require_institution(&st, &institution_scope(ctx, inst), inst).await?;
    Ok(Json(view))
}

/// GST documents issued in a period, newest first (`finance.read`).
#[utoipa::path(get, path = "/api/v1/finance/institutions/{id}/gst-documents", operation_id = "finance_gst_document_list", tag = "finance-gst",
    params(("id" = String, Path, format = Uuid), GstPeriodQuery), security(("session_cookie" = [])),
    responses((status = 200, body = Vec<GstDocumentItem>), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn document_list(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<GstPeriodQuery>,
) -> ApiResult<Json<Vec<GstDocumentItem>>> {
    let ctx = auth.require(Permission::FinanceRead)?;
    let inst = InstitutionId(id);
    let scope = institution_scope(ctx, inst);
    require_institution(&st, &scope, inst).await?;
    let (from, to) = period(&st, &scope, inst, &q).await?;
    let rows = sqlx::query(
        "SELECT d.id, d.kind, d.status, d.document_number, d.document_date, d.charge_id, c.charge_number, \
         d.recipient_name, d.recipient_gstin, d.taxable_value, d.cgst + d.sgst + d.igst AS tax, d.total \
         FROM finance.gst_document d JOIN finance.charge c ON c.id = d.charge_id \
         WHERE d.tenant_id = $1 AND d.institution_id = $2 AND d.status <> 'pending' \
         AND d.document_date BETWEEN $3 AND $4 ORDER BY d.document_date DESC, d.document_number DESC LIMIT 500",
    )
    .bind(tenant(ctx).as_uuid())
    .bind(inst.as_uuid())
    .bind(from)
    .bind(to)
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    let items = rows
        .iter()
        .map(|r| {
            let kind: String = r.try_get("kind")?;
            Ok(GstDocumentItem {
                id: r.try_get("id")?,
                kind: DocKind::parse(&kind).unwrap_or(DocKind::TaxInvoice),
                status: r.try_get("status")?,
                number: r.try_get("document_number")?,
                date: r.try_get("document_date")?,
                charge_id: r.try_get("charge_id")?,
                charge_number: r.try_get("charge_number")?,
                recipient_name: r.try_get("recipient_name")?,
                recipient_gstin: r.try_get("recipient_gstin")?,
                taxable_value: fmt(r.try_get("taxable_value")?),
                total_tax: fmt(r.try_get("tax")?),
                total: fmt(r.try_get("total")?),
            })
        })
        .collect::<Result<Vec<_>, sqlx::Error>>()
        .map_err(map_db_error)?;
    Ok(Json(items))
}

async fn summary_of(
    st: &FinanceState,
    auth: &Authenticated,
    id: Uuid,
    q: &GstPeriodQuery,
) -> ApiResult<(InstitutionId, Option<String>, NaiveDate, NaiveDate, Summary)> {
    let ctx = auth.require(Permission::FinanceRead)?;
    let inst = InstitutionId(id);
    let scope = institution_scope(ctx, inst);
    require_institution(st, &scope, inst).await?;
    let (from, to) = period(st, &scope, inst, q).await?;
    let t = tenant(ctx);
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let gstin = load_profile(&mut conn, t, inst)
        .await?
        .and_then(|p| p.gstin);
    let facts = facts(&mut conn, t, inst, from, to).await?;
    Ok((inst, gstin, from, to, summarize(&facts)))
}

/// GSTR-1 friendly summary of a period: B2B invoices, B2C by place of supply and rate, HSN / SAC
/// summary, nil / exempt / non-GST, credit notes and documents issued (`finance.read`).
#[utoipa::path(get, path = "/api/v1/finance/institutions/{id}/gst-summary", operation_id = "finance_gst_summary_get", tag = "finance-gst",
    params(("id" = String, Path, format = Uuid), GstPeriodQuery), security(("session_cookie" = [])),
    responses((status = 200, body = GstSummaryView), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn summary_get(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<GstPeriodQuery>,
) -> ApiResult<Json<GstSummaryView>> {
    let (inst, gstin, from, to, s) = summary_of(&st, &auth, id, &q).await?;
    Ok(Json(summary_view(inst, gstin, from, to, &s)))
}

/// The same summary as one CSV, a section per GSTR-1 table (`finance.read`).
#[utoipa::path(get, path = "/api/v1/finance/institutions/{id}/gst-summary/csv", operation_id = "finance_gst_summary_csv", tag = "finance-gst",
    params(("id" = String, Path, format = Uuid), GstPeriodQuery), security(("session_cookie" = [])),
    responses((status = 200, body = String, content_type = "text/csv"), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn summary_csv_get(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<GstPeriodQuery>,
) -> ApiResult<Response> {
    let (_, gstin, from, to, s) = summary_of(&st, &auth, id, &q).await?;
    let body = summary_csv(&s, from, to, gstin.as_deref().unwrap_or(""));
    let name = format!("gst-summary-{from}-to-{to}.csv");
    let disposition = HeaderValue::from_str(&format!("attachment; filename=\"{name}\""))
        .map_err(|e| KernelError::internal_msg(format!("file name header: {e}")))?;
    let mut out = (StatusCode::OK, body).into_response();
    out.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/csv; charset=utf-8"),
    );
    out.headers_mut()
        .insert(header::CONTENT_DISPOSITION, disposition);
    out.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(out)
}

/// Routes of GST.
pub fn router(state: FinanceState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(profile_get, profile_put))
        .routes(routes!(fee_head_list))
        .routes(routes!(fee_head_put, fee_head_delete))
        .routes(routes!(party_get, party_put))
        .routes(routes!(charge_document))
        .routes(routes!(my_charge_document))
        .routes(routes!(document_get))
        .routes(routes!(document_list))
        .routes(routes!(summary_get))
        .routes(routes!(summary_csv_get))
        .with_state(state)
}
