//! GST persistence and the hooks into the charge and adjustment paths (one owner-local
//! transaction each, always the caller's):
//!
//! * [`plan_for_insert`] / [`record_pending`] — `shared::charge::insert_charge`: with an enabled
//!   GST profile and at least one line on a fee head with a tax setup, the lines are re-priced
//!   by [`rules::plan_charge`] (taxable value kept on the line, one line per tax row on the
//!   system fee heads `GST-CGST` / `GST-SGST` / `GST-IGST`) and the snapshot stored `pending`.
//! * [`issue_for_charge`] — `charges::repo::publish_locked`: the pending document is numbered
//!   from its FY series (tax invoice, or bill of supply when nothing is taxed) and dated.
//! * [`credit_reversal_for`] / [`issue_credit_note`] / [`cancel_credit_note`] —
//!   `adjustments::repo`: a credit note on a taxed charge reverses tax in proportion (Dr output
//!   tax), gets its own CN series number and references the original invoice; reversing the
//!   adjustment cancels it.

use super::rules::{
    self, charges_untaxed, plan_charge, tax_note_text, tax_note_value, Component, Discount, Doc,
    DocKind, HeadTax, Item, LineIn, LinePlan, PlanRow, Registration, Treatment,
};
use crate::shared::charge::{ChargeRow, NewCharge, NewChargeLine};
use crate::shared::coa::ensure_gst_accounts;
use crate::shared::publish::audit;
use crate::shared::Actor;
use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde_json::json;
use sqlx::{PgConnection, Row};
use std::collections::HashMap;
use sutra_contracts::finance::ids::{FeeHeadId, LedgerAccountId, StudentAccountId};
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use uuid::Uuid;

/// Codes of the system fee heads that carry tax rows, in [`Component::ALL`] order.
pub const GST_HEAD_CODES: [&str; 3] = ["GST-CGST", "GST-SGST", "GST-IGST"];

/// The institution's GST profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    pub registration: Registration,
    pub gstin: Option<String>,
    pub legal_name: String,
    pub trade_name: String,
    pub address: String,
    pub state_code: String,
    pub default_tax_note: String,
    pub invoice_prefix: String,
    pub bill_prefix: String,
    pub credit_note_prefix: String,
    pub enabled: bool,
    pub version: i64,
    pub updated_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl Profile {
    fn prefix(&self, kind: DocKind) -> &str {
        match kind {
            DocKind::TaxInvoice => &self.invoice_prefix,
            DocKind::BillOfSupply => &self.bill_prefix,
            DocKind::CreditNote => &self.credit_note_prefix,
        }
    }
}

pub async fn load_profile(
    conn: &mut PgConnection,
    t: TenantId,
    inst: InstitutionId,
) -> Result<Option<Profile>, KernelError> {
    let row = sqlx::query(
        "SELECT registration_type, gstin, legal_name, trade_name, address, state_code, default_tax_note, \
         invoice_prefix, bill_prefix, credit_note_prefix, enabled, version, updated_at \
         FROM finance.gst_profile WHERE tenant_id = $1 AND institution_id = $2",
    )
    .bind(t.as_uuid())
    .bind(inst.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let Some(r) = row else { return Ok(None) };
    let read = || -> Result<Profile, sqlx::Error> {
        let reg: String = r.try_get("registration_type")?;
        Ok(Profile {
            registration: Registration::parse(&reg).unwrap_or(Registration::Regular),
            gstin: r.try_get("gstin")?,
            legal_name: r.try_get("legal_name")?,
            trade_name: r.try_get("trade_name")?,
            address: r.try_get("address")?,
            state_code: r.try_get("state_code")?,
            default_tax_note: r.try_get("default_tax_note")?,
            invoice_prefix: r.try_get("invoice_prefix")?,
            bill_prefix: r.try_get("bill_prefix")?,
            credit_note_prefix: r.try_get("credit_note_prefix")?,
            enabled: r.try_get("enabled")?,
            version: r.try_get("version")?,
            updated_at: r.try_get("updated_at")?,
        })
    };
    read().map(Some).map_err(map_db_error)
}

/// A fee head's stored tax setup with its own note.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeadSetup {
    pub tax: HeadTax,
    pub tax_note: String,
    pub version: i64,
}

pub async fn head_setups(
    conn: &mut PgConnection,
    t: TenantId,
    heads: &[Uuid],
) -> Result<HashMap<Uuid, HeadSetup>, KernelError> {
    let rows = sqlx::query(
        "SELECT fee_head_id, sac, gst_rate, exempt, tax_note, tax_inclusive, version FROM finance.fee_head_tax \
         WHERE tenant_id = $1 AND fee_head_id = ANY($2)",
    )
    .bind(t.as_uuid())
    .bind(heads)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    rows.iter()
        .map(|r| {
            Ok((
                r.try_get::<Uuid, _>("fee_head_id")?,
                HeadSetup {
                    tax: HeadTax {
                        sac: r.try_get("sac")?,
                        rate: r.try_get("gst_rate")?,
                        exempt: r.try_get("exempt")?,
                        inclusive: r.try_get("tax_inclusive")?,
                    },
                    tax_note: r.try_get("tax_note")?,
                    version: r.try_get("version")?,
                },
            ))
        })
        .collect::<Result<_, sqlx::Error>>()
        .map_err(map_db_error)
}

/// The system fee heads carrying CGST / SGST / IGST (seeded with the output-tax accounts).
pub async fn ensure_gst_heads(
    conn: &mut PgConnection,
    t: TenantId,
    inst: InstitutionId,
) -> Result<[FeeHeadId; 3], KernelError> {
    let accounts: [LedgerAccountId; 3] = ensure_gst_accounts(conn, t, inst).await?;
    let mut out = [FeeHeadId(Uuid::nil()); 3];
    for (i, c) in Component::ALL.iter().enumerate() {
        sqlx::query(
            "INSERT INTO finance.fee_head (tenant_id, institution_id, code, name, category, income_account_id) \
             VALUES ($1, $2, $3, $4, 'miscellaneous', $5) ON CONFLICT (tenant_id, institution_id, code) DO NOTHING",
        )
        .bind(t.as_uuid())
        .bind(inst.as_uuid())
        .bind(GST_HEAD_CODES[i])
        .bind(format!("Output {} (GST, system)", c.label()))
        .bind(accounts[i].as_uuid())
        .execute(&mut *conn)
        .await
        .map_err(map_db_error)?;
        let id: Uuid = sqlx::query_scalar(
            "SELECT id FROM finance.fee_head WHERE tenant_id = $1 AND institution_id = $2 AND code = $3",
        )
        .bind(t.as_uuid())
        .bind(inst.as_uuid())
        .bind(GST_HEAD_CODES[i])
        .fetch_one(&mut *conn)
        .await
        .map_err(map_db_error)?;
        out[i] = FeeHeadId(id);
    }
    Ok(out)
}

/// The payer's place of supply and GST identity: `(state, gstin, legal name)`.
pub async fn party(
    conn: &mut PgConnection,
    t: TenantId,
    account: StudentAccountId,
) -> Result<Option<(String, Option<String>, String, i64)>, KernelError> {
    sqlx::query_as(
        "SELECT state_code, gstin, legal_name, version FROM finance.gst_party WHERE tenant_id = $1 AND student_account_id = $2",
    )
    .bind(t.as_uuid())
    .bind(account.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)
}

/// A charge re-priced for GST: the lines to insert and the snapshot to record.
#[derive(Debug, Clone)]
pub struct Planned {
    pub lines: Vec<NewChargeLine>,
    pub plan: rules::ChargePlan,
    pub kind: DocKind,
    pub profile: Profile,
    pub place_of_supply: String,
    pub recipient_gstin: Option<String>,
    pub recipient_name: Option<String>,
    pub tax_note: String,
    /// Charge line number of each tax row (1-based, after the supply lines).
    pub row_lines: Vec<i32>,
    pub original: Vec<NewChargeLine>,
}

/// Re-prices a new charge under GST (`None`: no enabled profile, an unregistered institution, or
/// no line on a fee head with a tax setup). Lines on the system GST heads are refused: the
/// server adds them.
pub async fn plan_for_insert(
    conn: &mut PgConnection,
    c: &NewCharge,
) -> Result<Option<Planned>, KernelError> {
    let Some(profile) = load_profile(conn, c.tenant, c.institution).await? else {
        return Ok(None);
    };
    if !profile.enabled || profile.registration == Registration::Unregistered {
        return Ok(None);
    }
    let ids: Vec<Uuid> = c.lines.iter().map(|l| l.fee_head_id.as_uuid()).collect();
    let system: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM finance.fee_head WHERE tenant_id = $1 AND id = ANY($2) AND code = ANY($3)",
    )
    .bind(c.tenant.as_uuid())
    .bind(&ids)
    .bind(&GST_HEAD_CODES[..])
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    if system > 0 {
        return Err(KernelError::Conflict(
            "GST lines are added by the server; charge the fee head itself".into(),
        ));
    }
    let setups = head_setups(conn, c.tenant, &ids).await?;
    if setups.is_empty() {
        return Ok(None);
    }
    let (place, recipient_gstin, recipient_name) =
        match party(conn, c.tenant, c.student_account_id).await? {
            Some((s, g, n, _)) => (s, g, Some(n).filter(|n| !n.trim().is_empty())),
            None => (profile.state_code.clone(), None, None),
        };
    let inter = rules::is_inter_state(&profile.state_code, &place);
    let ins: Vec<LineIn> = c
        .lines
        .iter()
        .map(|l| LineIn {
            amount: l.amount,
            tax: setups.get(&l.fee_head_id.as_uuid()).map(|s| s.tax.clone()),
        })
        .collect();
    let plan = plan_charge(&ins, inter, profile.registration);
    let Some(kind) = plan.kind else {
        return Ok(None);
    };
    let heads = ensure_gst_heads(conn, c.tenant, c.institution).await?;
    let mut lines: Vec<NewChargeLine> = c
        .lines
        .iter()
        .zip(&plan.lines)
        .map(|(l, p)| NewChargeLine {
            fee_head_id: l.fee_head_id,
            description: l.description.clone(),
            amount: p.taxable_value,
        })
        .collect();
    let mut row_lines = Vec::new();
    for r in &plan.rows {
        let i = Component::ALL
            .iter()
            .position(|c| *c == r.component)
            .unwrap_or(0);
        lines.push(NewChargeLine {
            fee_head_id: heads[i],
            description: r.label(),
            amount: r.amount,
        });
        row_lines.push(i32::try_from(lines.len()).unwrap_or(i32::MAX));
    }
    // The zero-tax sentence: the fee head's note of the first untaxed line, else the profile's.
    let doc = Doc {
        items: plan
            .lines
            .iter()
            .map(|p| Item {
                qty: Decimal::ONE,
                rate: p.taxable_value,
                taxes: Some(rules::gst_taxes(p.rate, inter)),
            })
            .collect(),
        taxes: Vec::new(),
        discount: Discount::None,
        tax_inclusive: false,
    };
    let head_note = c
        .lines
        .iter()
        .zip(&plan.lines)
        .find(|(_, p)| p.treatment != Treatment::Taxable)
        .and_then(|(l, _)| setups.get(&l.fee_head_id.as_uuid()))
        .map(|s| s.tax_note.as_str());
    let tax_note = if charges_untaxed(&doc) || profile.registration == Registration::Composition {
        let fallback = if profile.registration == Registration::Composition {
            "preset:composition"
        } else {
            profile.default_tax_note.as_str()
        };
        tax_note_text(&tax_note_value(Some(fallback), None, head_note))
    } else {
        String::new()
    };
    Ok(Some(Planned {
        lines,
        plan,
        kind,
        place_of_supply: place,
        recipient_gstin,
        recipient_name,
        tax_note,
        row_lines,
        original: c.lines.clone(),
        profile,
    }))
}

fn component_sums(lines: &[LinePlan]) -> (Decimal, Decimal, Decimal) {
    lines
        .iter()
        .fold((Decimal::ZERO, Decimal::ZERO, Decimal::ZERO), |a, l| {
            (a.0 + l.cgst, a.1 + l.sgst, a.2 + l.igst)
        })
}

/// Stores the GST snapshot of a freshly inserted charge as a `pending` document.
pub async fn record_pending(
    conn: &mut PgConnection,
    charge: &ChargeRow,
    p: &Planned,
) -> Result<Uuid, KernelError> {
    let t = charge.tenant_id;
    let name: String = match &p.recipient_name {
        Some(n) => n.clone(),
        None => sqlx::query_scalar(
            "SELECT display_name FROM finance.student_account WHERE tenant_id = $1 AND id = $2",
        )
        .bind(t.as_uuid())
        .bind(charge.student_account_id.as_uuid())
        .fetch_one(&mut *conn)
        .await
        .map_err(map_db_error)?,
    };
    let (cgst, sgst, igst) = component_sums(&p.plan.lines);
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO finance.gst_document (tenant_id, institution_id, kind, charge_id, supplier_gstin, supplier_state, \
         place_of_supply, inter_state, recipient_name, recipient_gstin, taxable_value, cgst, sgst, igst, total, tax_note) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16) RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(charge.institution_id.as_uuid())
    .bind(p.kind.as_str())
    .bind(charge.id.as_uuid())
    .bind(&p.profile.gstin)
    .bind(&p.profile.state_code)
    .bind(&p.place_of_supply)
    .bind(p.plan.inter_state)
    .bind(&name)
    .bind(&p.recipient_gstin)
    .bind(p.plan.taxable_value)
    .bind(cgst)
    .bind(sgst)
    .bind(igst)
    .bind(p.plan.total)
    .bind(&p.tax_note)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    for (i, (l, plan)) in p.original.iter().zip(&p.plan.lines).enumerate() {
        let no = i32::try_from(i + 1).unwrap_or(i32::MAX);
        insert_line(conn, t, id, no, Some(no), &l.description, plan).await?;
    }
    for (r, line_no) in p.plan.rows.iter().zip(&p.row_lines) {
        insert_tax(conn, t, id, r, Some(*line_no)).await?;
    }
    Ok(id)
}

async fn insert_line(
    conn: &mut PgConnection,
    t: TenantId,
    doc: Uuid,
    no: i32,
    charge_line_no: Option<i32>,
    description: &str,
    l: &LinePlan,
) -> Result<(), KernelError> {
    sqlx::query(
        "INSERT INTO finance.gst_document_line (tenant_id, gst_document_id, line_no, charge_line_no, description, sac, \
         treatment, gst_rate, taxable_value, cgst, sgst, igst) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
    )
    .bind(t.as_uuid())
    .bind(doc)
    .bind(no)
    .bind(charge_line_no)
    .bind(description)
    .bind(&l.sac)
    .bind(l.treatment.as_str())
    .bind(l.rate)
    .bind(l.taxable_value)
    .bind(l.cgst)
    .bind(l.sgst)
    .bind(l.igst)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(())
}

async fn insert_tax(
    conn: &mut PgConnection,
    t: TenantId,
    doc: Uuid,
    r: &PlanRow,
    charge_line_no: Option<i32>,
) -> Result<(), KernelError> {
    sqlx::query(
        "INSERT INTO finance.gst_document_tax (tenant_id, gst_document_id, component, rate, amount, charge_line_no) \
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(t.as_uuid())
    .bind(doc)
    .bind(r.component.as_str())
    .bind(r.rate)
    .bind(r.amount)
    .bind(charge_line_no)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(())
}

/// Takes the next number of a GST series for the financial year of `date`.
async fn next_number(
    conn: &mut PgConnection,
    t: TenantId,
    inst: InstitutionId,
    kind: DocKind,
    prefix: &str,
    date: NaiveDate,
) -> Result<(String, i32), KernelError> {
    let fy = rules::fy_start(date);
    let n: i64 = sqlx::query_scalar(
        "INSERT INTO finance.gst_series (tenant_id, institution_id, kind, fy_start, next_value) VALUES ($1, $2, $3, $4, 2) \
         ON CONFLICT (tenant_id, institution_id, kind, fy_start) DO UPDATE SET next_value = finance.gst_series.next_value + 1 \
         RETURNING next_value - 1",
    )
    .bind(t.as_uuid())
    .bind(inst.as_uuid())
    .bind(kind.as_str())
    .bind(fy)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let number = rules::format_doc_number(prefix, n, date);
    if number.len() > rules::MAX_DOC_NUMBER {
        return Err(KernelError::Conflict(format!(
            "GST document number {number} is longer than 16 characters; shorten the series prefix"
        )));
    }
    Ok((number, fy))
}

/// Numbers and dates the pending GST document of a charge being published (no-op without one).
pub async fn issue_for_charge(
    conn: &mut PgConnection,
    c: &ChargeRow,
    date: NaiveDate,
    actor: &Actor,
) -> Result<Option<String>, KernelError> {
    let row: Option<(Uuid, String)> = sqlx::query_as(
        "SELECT id, kind FROM finance.gst_document WHERE tenant_id = $1 AND charge_id = $2 \
         AND kind <> 'credit_note' AND status = 'pending' FOR UPDATE",
    )
    .bind(c.tenant_id.as_uuid())
    .bind(c.id.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let Some((id, kind)) = row else {
        return Ok(None);
    };
    let kind = DocKind::parse(&kind).unwrap_or(DocKind::TaxInvoice);
    let profile = load_profile(conn, c.tenant_id, c.institution_id)
        .await?
        .ok_or_else(|| KernelError::Conflict("the GST profile is missing".into()))?;
    let (number, fy) = next_number(
        conn,
        c.tenant_id,
        c.institution_id,
        kind,
        profile.prefix(kind),
        date,
    )
    .await?;
    sqlx::query(
        "UPDATE finance.gst_document SET status = 'issued', document_number = $3, fy_start = $4, document_date = $5, \
         issued_at = now() WHERE tenant_id = $1 AND id = $2",
    )
    .bind(c.tenant_id.as_uuid())
    .bind(id)
    .bind(&number)
    .bind(fy)
    .bind(date)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    audit(
        conn,
        &ExecutionScope::institution(c.tenant_id, c.institution_id),
        "finance.gst.document.issue",
        "gst_document",
        id.to_string(),
        json!({"kind": kind.as_str(), "number": number, "chargeId": c.id, "chargeNumber": c.charge_number}),
        actor,
    )
    .await?;
    Ok(Some(number))
}

/// The issued invoice of a charge: `(document id, number, date, tax rows)`.
async fn charge_invoice(
    conn: &mut PgConnection,
    t: TenantId,
    charge: Uuid,
) -> Result<Option<(Uuid, Vec<PlanRow>)>, KernelError> {
    let id: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM finance.gst_document WHERE tenant_id = $1 AND charge_id = $2 AND kind <> 'credit_note' \
         AND status = 'issued'",
    )
    .bind(t.as_uuid())
    .bind(charge)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let Some(id) = id else { return Ok(None) };
    Ok(Some((id, tax_rows(conn, t, id).await?)))
}

async fn tax_rows(
    conn: &mut PgConnection,
    t: TenantId,
    doc: Uuid,
) -> Result<Vec<PlanRow>, KernelError> {
    let rows: Vec<(String, Decimal, Decimal)> = sqlx::query_as(
        "SELECT component, rate, amount FROM finance.gst_document_tax WHERE tenant_id = $1 AND gst_document_id = $2 \
         ORDER BY rate, component",
    )
    .bind(t.as_uuid())
    .bind(doc)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(rows
        .into_iter()
        .filter_map(|(c, rate, amount)| {
            Component::parse(&c).map(|component| PlanRow {
                component,
                rate,
                amount,
            })
        })
        .collect())
}

/// The tax a credit note of `amount` on `charge` reverses (empty when the charge carries no
/// issued tax invoice). Counts the credit notes already posted on the charge.
pub async fn credit_reversal_for(
    conn: &mut PgConnection,
    charge: &ChargeRow,
    adjustment: Uuid,
    amount: Decimal,
) -> Result<Vec<PlanRow>, KernelError> {
    let t = charge.tenant_id;
    let Some((_, rows)) = charge_invoice(conn, t, charge.id.as_uuid()).await? else {
        return Ok(Vec::new());
    };
    let before: Decimal = sqlx::query_scalar(
        "SELECT coalesce(sum(amount), 0) FROM finance.adjustment WHERE tenant_id = $1 AND charge_id = $2 \
         AND kind = 'credit_note' AND status = 'posted' AND id <> $3",
    )
    .bind(t.as_uuid())
    .bind(charge.id.as_uuid())
    .bind(adjustment)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(rules::credit_reversal(
        &rows,
        charge.total_amount,
        before,
        amount,
    ))
}

/// Output-tax account of a component (seeding the GST accounts on first use).
pub async fn output_account(
    conn: &mut PgConnection,
    t: TenantId,
    inst: InstitutionId,
    c: Component,
) -> Result<LedgerAccountId, KernelError> {
    let ids = ensure_gst_accounts(conn, t, inst).await?;
    let i = Component::ALL.iter().position(|x| *x == c).unwrap_or(0);
    Ok(ids[i])
}

/// `(id, supplier GSTIN, supplier state, place of supply, inter-state, recipient, recipient GSTIN)`.
type OriginalHead = (
    Uuid,
    Option<String>,
    String,
    String,
    bool,
    String,
    Option<String>,
);

/// Issues the GST credit note of a credit-note adjustment being posted on a charge with an
/// issued tax invoice (no-op otherwise): its own CN series number, the original invoice
/// referenced, the supply lines reduced in proportion.
#[allow(clippy::too_many_arguments)]
pub async fn issue_credit_note(
    conn: &mut PgConnection,
    charge: &ChargeRow,
    adjustment: Uuid,
    adjustment_number: &str,
    amount: Decimal,
    reversal: &[PlanRow],
    date: NaiveDate,
    actor: &Actor,
) -> Result<Option<String>, KernelError> {
    let t = charge.tenant_id;
    let original = sqlx::query(
        "SELECT id, supplier_gstin, supplier_state, place_of_supply, inter_state, recipient_name, recipient_gstin \
         FROM finance.gst_document WHERE tenant_id = $1 AND charge_id = $2 AND kind <> 'credit_note' AND status = 'issued'",
    )
    .bind(t.as_uuid())
    .bind(charge.id.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let Some(o) = original else { return Ok(None) };
    let profile = load_profile(conn, t, charge.institution_id)
        .await?
        .ok_or_else(|| KernelError::Conflict("the GST profile is missing".into()))?;
    let read = || -> Result<OriginalHead, sqlx::Error> {
        Ok((
            o.try_get("id")?,
            o.try_get("supplier_gstin")?,
            o.try_get("supplier_state")?,
            o.try_get("place_of_supply")?,
            o.try_get("inter_state")?,
            o.try_get("recipient_name")?,
            o.try_get("recipient_gstin")?,
        ))
    };
    let (orig_id, s_gstin, s_state, place, inter, r_name, r_gstin) =
        read().map_err(map_db_error)?;
    let tax: Decimal = reversal.iter().map(|r| r.amount).sum();
    let taxable = amount - tax;
    let sum = |c: Component| -> Decimal {
        reversal
            .iter()
            .filter(|r| r.component == c)
            .map(|r| r.amount)
            .sum()
    };
    let (number, fy) = next_number(
        conn,
        t,
        charge.institution_id,
        DocKind::CreditNote,
        &profile.credit_note_prefix,
        date,
    )
    .await?;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO finance.gst_document (tenant_id, institution_id, kind, status, charge_id, adjustment_id, \
         original_document_id, document_number, fy_start, document_date, supplier_gstin, supplier_state, place_of_supply, \
         inter_state, recipient_name, recipient_gstin, taxable_value, cgst, sgst, igst, total, issued_at) \
         VALUES ($1, $2, 'credit_note', 'issued', $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, now()) \
         RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(charge.institution_id.as_uuid())
    .bind(charge.id.as_uuid())
    .bind(adjustment)
    .bind(orig_id)
    .bind(&number)
    .bind(fy)
    .bind(date)
    .bind(&s_gstin)
    .bind(&s_state)
    .bind(&place)
    .bind(inter)
    .bind(&r_name)
    .bind(&r_gstin)
    .bind(taxable)
    .bind(sum(Component::Cgst))
    .bind(sum(Component::Sgst))
    .bind(sum(Component::Igst))
    .bind(amount)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    // Supply lines of the original reduced in proportion to the taxable value credited; the
    // tax shares follow the rows reversed.
    let orig_lines = doc_lines(conn, t, orig_id).await?;
    let orig_taxable: Decimal = orig_lines.iter().map(|l| l.1.taxable_value).sum();
    let parts: Vec<Decimal> = orig_lines
        .iter()
        .map(|l| {
            if orig_taxable.is_zero() {
                Decimal::ZERO
            } else {
                taxable * l.1.taxable_value / orig_taxable
            }
        })
        .collect();
    let values = spread(taxable, &parts);
    let comp_parts = |c: Component| -> Vec<Decimal> {
        orig_lines
            .iter()
            .map(|l| match c {
                Component::Cgst => l.1.cgst,
                Component::Sgst => l.1.sgst,
                Component::Igst => l.1.igst,
            })
            .collect()
    };
    let shares: Vec<Vec<Decimal>> = Component::ALL
        .iter()
        .map(|c| {
            let base = comp_parts(*c);
            let total: Decimal = base.iter().copied().sum();
            let want = sum(*c);
            let parts: Vec<Decimal> = base
                .iter()
                .map(|b| {
                    if total.is_zero() {
                        Decimal::ZERO
                    } else {
                        want * b / total
                    }
                })
                .collect();
            spread(want, &parts)
        })
        .collect();
    for (i, (desc, l)) in orig_lines.iter().enumerate() {
        let plan = LinePlan {
            taxable_value: values[i],
            treatment: l.treatment,
            sac: l.sac.clone(),
            rate: l.rate,
            cgst: shares[0][i],
            sgst: shares[1][i],
            igst: shares[2][i],
        };
        let no = i32::try_from(i + 1).unwrap_or(i32::MAX);
        insert_line(conn, t, id, no, None, desc, &plan).await?;
    }
    for r in reversal {
        insert_tax(conn, t, id, r, None).await?;
    }
    audit(
        conn,
        &ExecutionScope::institution(t, charge.institution_id),
        "finance.gst.credit_note.issue",
        "gst_document",
        id.to_string(),
        json!({"number": number, "adjustmentId": adjustment, "adjustmentNumber": adjustment_number,
               "chargeId": charge.id, "amount": crate::shared::money::fmt(amount),
               "taxReversed": crate::shared::money::fmt(tax)}),
        actor,
    )
    .await?;
    Ok(Some(number))
}

/// Rounds `parts` to paise so that they add up to `total` (largest parts take the leftovers).
fn spread(total: Decimal, parts: &[Decimal]) -> Vec<Decimal> {
    let mut out: Vec<Decimal> = parts.iter().map(|p| rules::round2(*p)).collect();
    let mut left = total - out.iter().copied().sum::<Decimal>();
    let step = Decimal::new(1, 2);
    let mut order: Vec<usize> = (0..parts.len()).collect();
    order.sort_by(|a, b| parts[*b].cmp(&parts[*a]).then(a.cmp(b)));
    let mut i = 0;
    while !left.is_zero() && !order.is_empty() && i < 10_000 {
        let k = order[i % order.len()];
        if left > Decimal::ZERO {
            out[k] += step;
            left -= step;
        } else {
            out[k] -= step;
            left += step;
        }
        i += 1;
    }
    out
}

/// Supply lines of a document: `(description, plan)`.
pub async fn doc_lines(
    conn: &mut PgConnection,
    t: TenantId,
    doc: Uuid,
) -> Result<Vec<(String, LinePlan)>, KernelError> {
    let rows = sqlx::query(
        "SELECT description, sac, treatment, gst_rate, taxable_value, cgst, sgst, igst FROM finance.gst_document_line \
         WHERE tenant_id = $1 AND gst_document_id = $2 ORDER BY line_no",
    )
    .bind(t.as_uuid())
    .bind(doc)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    rows.iter()
        .map(|r| {
            let tr: String = r.try_get("treatment")?;
            Ok((
                r.try_get("description")?,
                LinePlan {
                    taxable_value: r.try_get("taxable_value")?,
                    treatment: Treatment::parse(&tr).unwrap_or(Treatment::NonGst),
                    sac: r.try_get("sac")?,
                    rate: r.try_get("gst_rate")?,
                    cgst: r.try_get("cgst")?,
                    sgst: r.try_get("sgst")?,
                    igst: r.try_get("igst")?,
                },
            ))
        })
        .collect::<Result<_, sqlx::Error>>()
        .map_err(map_db_error)
}

/// Cancels the GST credit note of an adjustment being reversed or superseded.
pub async fn cancel_credit_note(
    conn: &mut PgConnection,
    t: TenantId,
    inst: InstitutionId,
    adjustment: Uuid,
    actor: &Actor,
) -> Result<(), KernelError> {
    let id: Option<Uuid> = sqlx::query_scalar(
        "UPDATE finance.gst_document SET status = 'cancelled', cancelled_at = now() WHERE tenant_id = $1 \
         AND adjustment_id = $2 AND status = 'issued' RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(adjustment)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    if let Some(id) = id {
        audit(
            conn,
            &ExecutionScope::institution(t, inst),
            "finance.gst.credit_note.cancel",
            "gst_document",
            id.to_string(),
            json!({"adjustmentId": adjustment}),
            actor,
        )
        .await?;
    }
    Ok(())
}
