//! Part 2 · Stage 4 (S4-pay) — gateway **settlement reconciliation import**.
//!
//! The finance office uploads the gateway's settlement report (Razorpay "Settlement Recon" CSV;
//! columns as in <https://razorpay.com/docs/api/settlements/fetch-recon/>: `entity_id`, `type`,
//! `debit`, `credit`, `amount`, `currency`, `fee`, `tax`, `settlement_id`, `settlement_utr`,
//! `order_id`, …). Every line is matched against this institution's records:
//!
//! | line | result |
//! |------|--------|
//! | `type = payment`, a **completed** gateway payment with that `gateway_payment_ref`, same amount (and order) | `matched` |
//! | … amount differs / order differs | `amount_mismatch` / `order_mismatch` |
//! | … payment unknown here | `unknown_payment` |
//! | … payment known but not completed (failed, reversed, charged back …) | `payment_not_completed` |
//! | … already matched by an earlier import | `already_settled` |
//! | `type = refund`, a refund with that `gateway_refund_ref`, same amount | `refund_matched` (else `refund_amount_mismatch` / `unknown_refund`) |
//! | the same `entity_id` twice in one file | `duplicate_line` |
//! | any other type (adjustment, transfer, dispute …) | `ignored` |
//!
//! The import and its lines are stored append-only (`finance.gateway_settlement_import` /
//! `_line`) and audited (`finance.gateway_settlement.import`); the same file (SHA-256) imported
//! again returns the first import (`alreadyImported = true`). The report posts nothing: matched
//! payments are moved from gateway clearing to the bank with the existing bank settlement
//! (`finance_bank_settlement_create`, fees included).
//!
//! Amounts: the dashboard export is in rupees (`amountUnit=rupees`, default); the API's recon
//! items are in paise (`amountUnit=paise`).

use crate::shared::guard::{require_institution, require_institution_writable};
use crate::shared::http::actor;
use crate::shared::money::fmt;
use crate::shared::publish::audit;
use crate::shared::scope::{institution_scope, tenant};
use crate::shared::FinanceState;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{PgConnection, Row};
use std::collections::HashSet;
use std::str::FromStr;
use sutra_kernel::auth::Permission;
use sutra_kernel::error::{FieldViolation, KernelError};
use sutra_kernel::scope::{InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

/// Most lines one report may carry.
pub const MAX_LINES: usize = 5000;

/// Unit of the report's amounts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AmountUnit {
    /// Decimal rupees (`971.00`) — the dashboard export.
    #[default]
    Rupees,
    /// Integer paise (`97100`) — the recon API.
    Paise,
}

impl AmountUnit {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Rupees => "rupees",
            Self::Paise => "paise",
        }
    }
}

/// One parsed report line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SettlementRow {
    /// 1-based data line number (header excluded).
    pub line_no: i32,
    pub entity_id: String,
    pub entity_type: String,
    pub settlement_id: Option<String>,
    pub settlement_utr: Option<String>,
    pub order_id: Option<String>,
    pub amount: Decimal,
    pub fee: Decimal,
    pub tax: Decimal,
    pub credit: Decimal,
    pub debit: Decimal,
}

fn invalid(field: &str, code: &str, message: impl Into<String>) -> KernelError {
    KernelError::Validation(vec![FieldViolation::new(field, code, message.into())])
}

/// RFC 4180 records (quoted fields, `""` escapes, CRLF / LF); blank lines skipped.
pub fn csv_records(text: &str) -> Result<Vec<Vec<String>>, KernelError> {
    let mut records = Vec::new();
    let mut record: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    let mut touched = false;
    while let Some(c) = chars.next() {
        if quoted {
            match c {
                '"' if chars.peek() == Some(&'"') => {
                    field.push('"');
                    chars.next();
                }
                '"' => quoted = false,
                _ => field.push(c),
            }
            continue;
        }
        match c {
            '"' => {
                quoted = true;
                touched = true;
            }
            ',' => {
                record.push(std::mem::take(&mut field));
                touched = true;
            }
            '\r' => {}
            '\n' => {
                if touched || !field.is_empty() {
                    record.push(std::mem::take(&mut field));
                    records.push(std::mem::take(&mut record));
                }
                touched = false;
            }
            _ => {
                field.push(c);
                touched = true;
            }
        }
    }
    if quoted {
        return Err(invalid(
            "content",
            "unterminated_quote",
            "a quoted field is not closed",
        ));
    }
    if touched || !field.is_empty() {
        record.push(field);
        records.push(record);
    }
    Ok(records)
}

fn amount_of(raw: &str, unit: AmountUnit, line: i32, col: &str) -> Result<Decimal, KernelError> {
    let t = raw.trim();
    if t.is_empty() {
        return Ok(Decimal::ZERO);
    }
    let bad = || {
        invalid(
            &format!("line {line}.{col}"),
            "invalid_amount",
            format!("`{t}` is not an amount in {}", unit.as_str()),
        )
    };
    match unit {
        AmountUnit::Paise => t
            .parse::<i64>()
            .map(|p| Decimal::new(p, 2))
            .map_err(|_| bad()),
        AmountUnit::Rupees => {
            let d = Decimal::from_str(t).map_err(|_| bad())?;
            if d.normalize().scale() > 2 {
                return Err(bad());
            }
            Ok(d)
        }
    }
}

/// Parses a settlement report (header row with the recon column names, case-insensitive).
pub fn parse_report(text: &str, unit: AmountUnit) -> Result<Vec<SettlementRow>, KernelError> {
    let text = text.trim_start_matches('\u{feff}');
    let mut records = csv_records(text)?.into_iter();
    let header: Vec<String> = records
        .next()
        .ok_or_else(|| invalid("content", "empty", "the report has no header row"))?
        .iter()
        .map(|h| h.trim().to_ascii_lowercase().replace(' ', "_"))
        .collect();
    let col = |name: &str| header.iter().position(|h| h == name);
    let (Some(c_entity), Some(c_type), Some(c_amount)) =
        (col("entity_id"), col("type"), col("amount"))
    else {
        return Err(invalid(
            "content",
            "missing_columns",
            "the header needs at least entity_id, type and amount",
        ));
    };
    let (c_fee, c_tax, c_credit, c_debit) = (col("fee"), col("tax"), col("credit"), col("debit"));
    let (c_sid, c_utr, c_order, c_currency) = (
        col("settlement_id"),
        col("settlement_utr"),
        col("order_id"),
        col("currency"),
    );
    let mut rows = Vec::new();
    for (i, r) in records.enumerate() {
        let line = i32::try_from(i + 1).unwrap_or(i32::MAX);
        if rows.len() >= MAX_LINES {
            return Err(invalid(
                "content",
                "too_many_lines",
                format!("at most {MAX_LINES} lines per report"),
            ));
        }
        let get = |c: Option<usize>| -> Option<String> {
            c.and_then(|c| r.get(c))
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty() && v != "null")
        };
        let entity_id = get(Some(c_entity))
            .ok_or_else(|| invalid(&format!("line {line}.entity_id"), "required", "is empty"))?;
        if let Some(cur) = get(c_currency) {
            if !cur.eq_ignore_ascii_case("INR") {
                return Err(invalid(
                    &format!("line {line}.currency"),
                    "unsupported_currency",
                    format!("{cur}: only INR settlements are reconciled"),
                ));
            }
        }
        let num = |c: Option<usize>, name: &str| -> Result<Decimal, KernelError> {
            amount_of(&get(c).unwrap_or_default(), unit, line, name)
        };
        rows.push(SettlementRow {
            line_no: line,
            entity_type: get(Some(c_type)).unwrap_or_default().to_ascii_lowercase(),
            settlement_id: get(c_sid),
            settlement_utr: get(c_utr),
            order_id: get(c_order),
            amount: num(Some(c_amount), "amount")?,
            fee: num(c_fee, "fee")?,
            tax: num(c_tax, "tax")?,
            credit: num(c_credit, "credit")?,
            debit: num(c_debit, "debit")?,
            entity_id,
        });
    }
    if rows.is_empty() {
        return Err(invalid("content", "empty", "the report has no lines"));
    }
    Ok(rows)
}

/// Matching result of one line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LineResult {
    pub result: &'static str,
    pub detail: Option<String>,
    pub payment_id: Option<Uuid>,
    pub refund_id: Option<Uuid>,
}

impl LineResult {
    fn new(result: &'static str, detail: Option<String>) -> Self {
        Self {
            result,
            detail,
            payment_id: None,
            refund_id: None,
        }
    }

    pub fn is_match(&self) -> bool {
        matches!(self.result, "matched" | "refund_matched")
    }
}

async fn previously_matched(
    conn: &mut PgConnection,
    tenant: TenantId,
    institution: InstitutionId,
    entity: &str,
    result: &str,
) -> Result<Option<Uuid>, KernelError> {
    sqlx::query_scalar(
        "SELECT import_id FROM finance.gateway_settlement_line WHERE tenant_id = $1 \
         AND institution_id = $2 AND entity_id = $3 AND result = $4 ORDER BY created_at LIMIT 1",
    )
    .bind(tenant.as_uuid())
    .bind(institution.as_uuid())
    .bind(entity)
    .bind(result)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)
}

/// Matches one line against the institution's payments / refunds of `gateway`.
pub async fn match_line(
    conn: &mut PgConnection,
    tenant: TenantId,
    institution: InstitutionId,
    gateway: &str,
    row: &SettlementRow,
) -> Result<LineResult, KernelError> {
    match row.entity_type.as_str() {
        "payment" => {
            let found: Option<(Uuid, String, Decimal, Option<String>)> = sqlx::query_as(
                "SELECT id, status, amount, gateway_order_ref FROM finance.payment WHERE tenant_id = $1 \
                 AND institution_id = $2 AND gateway = $3 AND gateway_payment_ref = $4 LIMIT 1",
            )
            .bind(tenant.as_uuid())
            .bind(institution.as_uuid())
            .bind(gateway)
            .bind(&row.entity_id)
            .fetch_optional(&mut *conn)
            .await
            .map_err(map_db_error)?;
            let Some((id, status, amount, order)) = found else {
                return Ok(LineResult::new(
                    "unknown_payment",
                    Some("no captured payment with this gateway payment id".into()),
                ));
            };
            let mut r = if status != "completed" {
                LineResult::new(
                    "payment_not_completed",
                    Some(format!("payment is {status}")),
                )
            } else if row.amount != amount {
                LineResult::new(
                    "amount_mismatch",
                    Some(format!(
                        "settled {} but the payment is {}",
                        fmt(row.amount),
                        fmt(amount)
                    )),
                )
            } else if row.order_id.is_some() && row.order_id != order {
                LineResult::new(
                    "order_mismatch",
                    Some(format!(
                        "settled for order {} but the payment is for {}",
                        row.order_id.as_deref().unwrap_or_default(),
                        order.as_deref().unwrap_or("-")
                    )),
                )
            } else if let Some(prev) =
                previously_matched(conn, tenant, institution, &row.entity_id, "matched").await?
            {
                LineResult::new(
                    "already_settled",
                    Some(format!("matched by settlement import {prev}")),
                )
            } else {
                LineResult::new("matched", None)
            };
            r.payment_id = Some(id);
            Ok(r)
        }
        "refund" => {
            let found: Option<(Uuid, Decimal)> = sqlx::query_as(
                "SELECT id, amount FROM finance.refund WHERE tenant_id = $1 AND institution_id = $2 \
                 AND gateway_refund_ref = $3 LIMIT 1",
            )
            .bind(tenant.as_uuid())
            .bind(institution.as_uuid())
            .bind(&row.entity_id)
            .fetch_optional(&mut *conn)
            .await
            .map_err(map_db_error)?;
            let Some((id, amount)) = found else {
                return Ok(LineResult::new(
                    "unknown_refund",
                    Some("no refund with this gateway refund id".into()),
                ));
            };
            let mut r = if row.amount != amount {
                LineResult::new(
                    "refund_amount_mismatch",
                    Some(format!(
                        "settled {} but the refund is {}",
                        fmt(row.amount),
                        fmt(amount)
                    )),
                )
            } else if let Some(prev) =
                previously_matched(conn, tenant, institution, &row.entity_id, "refund_matched")
                    .await?
            {
                LineResult::new(
                    "already_settled",
                    Some(format!("matched by settlement import {prev}")),
                )
            } else {
                LineResult::new("refund_matched", None)
            };
            r.refund_id = Some(id);
            Ok(r)
        }
        other => Ok(LineResult::new(
            "ignored",
            Some(format!("line type `{other}` is not reconciled here")),
        )),
    }
}

// ------------------------------------------------------------------ views

#[derive(Debug, Clone, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
pub struct SettlementImportQuery {
    /// `rupees` (dashboard export, default) or `paise` (recon API).
    pub amount_unit: Option<AmountUnit>,
    /// Original file name (recorded only).
    pub file_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SettlementLineView {
    pub line_no: i32,
    pub entity_id: String,
    pub entity_type: String,
    pub settlement_id: Option<String>,
    pub settlement_utr: Option<String>,
    pub order_id: Option<String>,
    pub amount: String,
    pub fee: String,
    pub tax: String,
    /// `matched | amount_mismatch | order_mismatch | unknown_payment | payment_not_completed |
    /// already_settled | duplicate_line | refund_matched | refund_amount_mismatch |
    /// unknown_refund | ignored`.
    pub result: String,
    pub detail: Option<String>,
    pub payment_id: Option<Uuid>,
    pub refund_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SettlementImportView {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub gateway: String,
    pub file_name: Option<String>,
    pub amount_unit: String,
    pub line_count: i32,
    pub matched_count: i32,
    /// Lines that need attention (everything but matches and ignored types).
    pub mismatch_count: i32,
    /// Σ credit − Σ debit of the report.
    pub settled_total: String,
    pub fee_total: String,
    pub tax_total: String,
    pub imported_by: Option<String>,
    pub imported_at: DateTime<Utc>,
    /// `true` when this exact file had been imported before (nothing new recorded).
    pub already_imported: bool,
    pub lines: Vec<SettlementLineView>,
}

async fn load_view(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    already_imported: bool,
) -> Result<SettlementImportView, KernelError> {
    let r = sqlx::query(
        "SELECT id, institution_id, gateway, file_name, amount_unit, line_count, matched_count, \
         mismatch_count, settled_total, fee_total, tax_total, imported_by, imported_at \
         FROM finance.gateway_settlement_import WHERE tenant_id = $1 AND id = $2",
    )
    .bind(tenant.as_uuid())
    .bind(id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "gateway settlement import",
    })?;
    let lines = sqlx::query(
        "SELECT line_no, entity_id, entity_type, settlement_id, settlement_utr, order_id, amount, \
         fee, tax, result, detail, payment_id, refund_id FROM finance.gateway_settlement_line \
         WHERE tenant_id = $1 AND import_id = $2 ORDER BY line_no",
    )
    .bind(tenant.as_uuid())
    .bind(id)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?
    .iter()
    .map(|l| {
        Ok(SettlementLineView {
            line_no: l.try_get("line_no")?,
            entity_id: l.try_get("entity_id")?,
            entity_type: l.try_get("entity_type")?,
            settlement_id: l.try_get("settlement_id")?,
            settlement_utr: l.try_get("settlement_utr")?,
            order_id: l.try_get("order_id")?,
            amount: fmt(l.try_get("amount")?),
            fee: fmt(l.try_get("fee")?),
            tax: fmt(l.try_get("tax")?),
            result: l.try_get("result")?,
            detail: l.try_get("detail")?,
            payment_id: l.try_get("payment_id")?,
            refund_id: l.try_get("refund_id")?,
        })
    })
    .collect::<Result<Vec<_>, sqlx::Error>>()
    .map_err(map_db_error)?;
    Ok(SettlementImportView {
        id: r.try_get("id").map_err(map_db_error)?,
        institution_id: r.try_get("institution_id").map_err(map_db_error)?,
        gateway: r.try_get("gateway").map_err(map_db_error)?,
        file_name: r.try_get("file_name").map_err(map_db_error)?,
        amount_unit: r.try_get("amount_unit").map_err(map_db_error)?,
        line_count: r.try_get("line_count").map_err(map_db_error)?,
        matched_count: r.try_get("matched_count").map_err(map_db_error)?,
        mismatch_count: r.try_get("mismatch_count").map_err(map_db_error)?,
        settled_total: fmt(r.try_get("settled_total").map_err(map_db_error)?),
        fee_total: fmt(r.try_get("fee_total").map_err(map_db_error)?),
        tax_total: fmt(r.try_get("tax_total").map_err(map_db_error)?),
        imported_by: r.try_get("imported_by").map_err(map_db_error)?,
        imported_at: r.try_get("imported_at").map_err(map_db_error)?,
        already_imported,
        lines,
    })
}

// ------------------------------------------------------------------ handlers

/// Imports a gateway settlement report (CSV) and reconciles every line against captured
/// payments and processed refunds of the institution (`treasury.manage`). The same file again
/// returns the first import (200, `alreadyImported`); a new one is 201.
#[utoipa::path(post, path = "/api/v1/finance/institutions/{id}/gateway-settlements", operation_id = "finance_gateway_settlement_import", tag = "finance-payments",
    params(("id" = String, Path, format = Uuid), SettlementImportQuery), request_body(content = String, content_type = "text/csv"), security(("session_cookie" = [])),
    responses((status = 201, body = SettlementImportView), (status = 200, body = SettlementImportView), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn gateway_settlement_import(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<SettlementImportQuery>,
    body: Bytes,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::TreasuryManage)?;
    let institution = InstitutionId(id);
    let scope = institution_scope(ctx, institution);
    require_institution(&st, &scope, institution).await?;
    require_institution_writable(&st, &scope, institution).await?;
    let gateway = st.gateway.name();
    if gateway == crate::payments::gateway::DISABLED_GATEWAY {
        return Err(KernelError::Unavailable("online payments are disabled".into()).into());
    }
    let text = std::str::from_utf8(&body)
        .map_err(|_| invalid("content", "not_utf8", "the report must be UTF-8 text"))?;
    let unit = q.amount_unit.unwrap_or_default();
    let rows = parse_report(text, unit)?;
    let hash: String = Sha256::digest(&body)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let file_name = q
        .file_name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.chars().take(200).collect::<String>());
    let t = tenant(ctx);
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let existing: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM finance.gateway_settlement_import WHERE tenant_id = $1 AND institution_id = $2 \
         AND gateway = $3 AND file_sha256 = $4",
    )
    .bind(t.as_uuid())
    .bind(id)
    .bind(gateway)
    .bind(&hash)
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_db_error)?;
    if let Some(prev) = existing {
        let v = load_view(&mut tx, t, prev, true).await?;
        tx.commit().await.map_err(map_db_error)?;
        return Ok((StatusCode::OK, Json(v)).into_response());
    }
    let mut seen = HashSet::new();
    let mut results = Vec::with_capacity(rows.len());
    for row in &rows {
        let r = if seen.insert(row.entity_id.clone()) {
            match_line(&mut tx, t, institution, gateway, row).await?
        } else {
            LineResult::new(
                "duplicate_line",
                Some("listed earlier in this report".into()),
            )
        };
        results.push(r);
    }
    let matched = results.iter().filter(|r| r.is_match()).count();
    let ignored = results.iter().filter(|r| r.result == "ignored").count();
    let mismatched = results.len() - matched - ignored;
    let settled: Decimal = rows.iter().map(|r| r.credit - r.debit).sum();
    let fees: Decimal = rows.iter().map(|r| r.fee).sum();
    let taxes: Decimal = rows.iter().map(|r| r.tax).sum();
    let count = |n: usize| i32::try_from(n).unwrap_or(i32::MAX);
    let import_id: Uuid = sqlx::query_scalar(
        "INSERT INTO finance.gateway_settlement_import (tenant_id, institution_id, gateway, file_sha256, \
         file_name, amount_unit, line_count, matched_count, mismatch_count, settled_total, fee_total, \
         tax_total, imported_by) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13) RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(id)
    .bind(gateway)
    .bind(&hash)
    .bind(file_name.as_deref())
    .bind(unit.as_str())
    .bind(count(rows.len()))
    .bind(count(matched))
    .bind(count(mismatched))
    .bind(settled)
    .bind(fees)
    .bind(taxes)
    .bind(a.id.as_deref())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    for (row, r) in rows.iter().zip(&results) {
        sqlx::query(
            "INSERT INTO finance.gateway_settlement_line (tenant_id, institution_id, import_id, line_no, \
             entity_id, entity_type, settlement_id, settlement_utr, order_id, amount, fee, tax, credit, \
             debit, payment_id, refund_id, result, detail) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18)",
        )
        .bind(t.as_uuid())
        .bind(id)
        .bind(import_id)
        .bind(row.line_no)
        .bind(&row.entity_id)
        .bind(&row.entity_type)
        .bind(row.settlement_id.as_deref())
        .bind(row.settlement_utr.as_deref())
        .bind(row.order_id.as_deref())
        .bind(row.amount)
        .bind(row.fee)
        .bind(row.tax)
        .bind(row.credit)
        .bind(row.debit)
        .bind(r.payment_id)
        .bind(r.refund_id)
        .bind(r.result)
        .bind(r.detail.as_deref())
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    }
    audit(
        &mut tx,
        &scope,
        "finance.gateway_settlement.import",
        "gateway_settlement_import",
        import_id.to_string(),
        serde_json::json!({"gateway": gateway, "fileSha256": hash, "fileName": file_name,
            "amountUnit": unit.as_str(), "lines": rows.len(), "matched": matched,
            "mismatched": mismatched, "ignored": ignored, "settledTotal": fmt(settled),
            "feeTotal": fmt(fees), "taxTotal": fmt(taxes)}),
        &a,
    )
    .await?;
    let v = load_view(&mut tx, t, import_id, false).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok((StatusCode::CREATED, Json(v)).into_response())
}

/// One settlement reconciliation import with its line results (`finance.read`).
#[utoipa::path(get, path = "/api/v1/finance/gateway-settlements/{id}", operation_id = "finance_gateway_settlement_get", tag = "finance-payments",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = SettlementImportView), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn gateway_settlement_get(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<SettlementImportView>> {
    let ctx = auth.require(Permission::FinanceRead)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    Ok(Json(load_view(&mut conn, tenant(ctx), id, false).await?))
}

pub fn router(state: FinanceState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(gateway_settlement_import))
        .routes(routes!(gateway_settlement_get))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap()
    }

    // Column names of https://razorpay.com/docs/api/settlements/fetch-recon/ (items[]).
    const REPORT: &str = "entity_id,type,debit,credit,amount,currency,fee,tax,on_hold,settled,created_at,settled_at,settlement_id,posted_at,credit_type,description,notes,payment_id,settlement_utr,order_id,order_receipt,method,card_network,card_issuer,card_type,dispute_id\r\n\
pay_DEXrnipqTmWVGE,payment,0,971.00,1000.00,INR,29.00,0.00,false,true,1567692556,1568176960,setl_DGlQ1Rj8os78Ec,,default,\"Fee, term 1\",\"Beam me up \"\"Scotty\"\"\",,1568176960vxp0rj,order_DEXrnRiR3SNDHA,,card,MasterCard,KARB,credit,\r\n\
rfnd_DGRcGzZSLyEdg1,refund,2425.00,0,2425.00,INR,0,0,false,true,1567692556,1568176960,setl_DGlQ1Rj8os78Ec,,,,,pay_DEXrnipqTmWVGE,1568176960vxp0rj,,,,,,,\r\n\
\r\n";

    #[test]
    fn csv_handles_quotes_commas_and_blank_lines() {
        let r = csv_records("a,b\n\"x, y\",\"q\"\"z\"\n\n1,\n").unwrap();
        assert_eq!(
            r,
            vec![
                vec!["a".to_owned(), "b".to_owned()],
                vec!["x, y".to_owned(), "q\"z".to_owned()],
                vec!["1".to_owned(), String::new()],
            ]
        );
        assert!(csv_records("a,\"b\n").is_err());
    }

    #[test]
    fn razorpay_recon_reports_parse_in_rupees_and_paise() {
        let rows = parse_report(REPORT, AmountUnit::Rupees).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].entity_id, "pay_DEXrnipqTmWVGE");
        assert_eq!(rows[0].entity_type, "payment");
        assert_eq!(
            (rows[0].amount, rows[0].fee, rows[0].credit),
            (d("1000.00"), d("29.00"), d("971.00"))
        );
        assert_eq!(rows[0].order_id.as_deref(), Some("order_DEXrnRiR3SNDHA"));
        assert_eq!(
            rows[0].settlement_id.as_deref(),
            Some("setl_DGlQ1Rj8os78Ec")
        );
        assert_eq!(rows[1].entity_type, "refund");
        assert_eq!(rows[1].debit, d("2425.00"));
        let paise = "\u{feff}Entity_ID,Type,Amount,Fee,Credit\npay_A,payment,100000,2900,97100\n";
        let rows = parse_report(paise, AmountUnit::Paise).unwrap();
        assert_eq!((rows[0].amount, rows[0].fee), (d("1000.00"), d("29.00")));
        // Errors: no columns, bad amounts, foreign currency, paise with decimals, no lines.
        assert!(parse_report("foo,bar\n1,2\n", AmountUnit::Rupees).is_err());
        assert!(parse_report(
            "entity_id,type,amount\npay_A,payment,abc\n",
            AmountUnit::Rupees
        )
        .is_err());
        assert!(parse_report(
            "entity_id,type,amount\npay_A,payment,1.005\n",
            AmountUnit::Rupees
        )
        .is_err());
        assert!(parse_report(
            "entity_id,type,amount,currency\npay_A,payment,1,USD\n",
            AmountUnit::Rupees
        )
        .is_err());
        assert!(parse_report(
            "entity_id,type,amount\npay_A,payment,10.5\n",
            AmountUnit::Paise
        )
        .is_err());
        assert!(parse_report("entity_id,type,amount\n", AmountUnit::Rupees).is_err());
        assert!(parse_report("entity_id,type,amount\n,payment,1\n", AmountUnit::Rupees).is_err());
    }
}
