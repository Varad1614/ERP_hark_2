//! Pure GST rules of tax invoices, bills of supply and credit notes.
//!
//! The document arithmetic is a port of Everything's invoicing app (HeyPuter/everything @ 06802a1,
//! MIT, `src/invoicing/src/lib/{money,taxnote,doc}.js`): [`doc_totals`] is `docTotals` (named
//! taxes, one row per distinct `(label, rate)`, a line's own list replacing the document's, an
//! empty list opting the line out, tax-inclusive prices carving the net out once with the sum of
//! the rates, a whole-document discount prorated before tax, each stage rounded to the paisa so
//! the printed rows add up); [`tax_note_value`] / [`tax_note_text`] / [`charges_untaxed`] are
//! `taxnote.js` and [`format_doc_number`] is `formatDocNumber`. The golden vectors in
//! `tests/golden/gst_invoicing.json` replay the original functions.
//!
//! Indianised on top: GST as named taxes (CGST + SGST halves for an intra-state supply, IGST
//! for inter-state, by place of supply), the Indian slabs, exempt / nil-rated / non-GST
//! treatments, tax-note presets worded for Indian law (education services exempt under
//! Notification 12/2017-Central Tax (Rate)), document series per April–March financial year
//! (`{FY}` → `26-27`, numbers at most 16 characters), a charge plan that splits a charge into
//! taxable-value lines plus one line per tax row, proportional reversal by credit notes, and the
//! GSTR-1 friendly summary.

use chrono::{Datelike, NaiveDate};
use rust_decimal::Decimal;
use serde::Serialize;
use std::collections::BTreeMap;

// ================================================================ Everything: money.js

/// `round2`: JavaScript `Math.round(n * 100) / 100` (half towards +∞), exact in decimals.
pub fn round2(n: Decimal) -> Decimal {
    ((n * Decimal::ONE_HUNDRED) + Decimal::new(5, 1)).floor() / Decimal::ONE_HUNDRED
}

/// One named tax: `{ label, rate }` (rate in percent).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedTax {
    pub label: String,
    pub rate: Decimal,
}

impl NamedTax {
    pub fn new(label: &str, rate: Decimal) -> Self {
        Self {
            label: label.to_owned(),
            rate,
        }
    }
}

/// One line: quantity × rate, with its own tax list (`None` takes the document's).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub qty: Decimal,
    pub rate: Decimal,
    pub taxes: Option<Vec<NamedTax>>,
}

/// A whole-document discount.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Discount {
    None,
    Percent(Decimal),
    Fixed(Decimal),
}

/// The parts of a document `docTotals` reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Doc {
    pub items: Vec<Item>,
    /// The document's taxes, applying to every line without a list of its own.
    pub taxes: Vec<NamedTax>,
    pub discount: Discount,
    pub tax_inclusive: bool,
}

/// One printed tax row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaxRow {
    pub label: String,
    pub rate: Decimal,
    pub amount: Decimal,
}

/// All derived money on a document (payments and schedules are not part of a GST document).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Totals {
    pub subtotal: Decimal,
    pub discount: Decimal,
    pub taxable: Decimal,
    pub tax: Decimal,
    pub taxes: Vec<TaxRow>,
    pub total: Decimal,
}

/// Quantity times rate for one line, rounded to paise.
pub fn line_total(it: &Item) -> Decimal {
    round2(it.qty * it.rate)
}

/// The same tax named twice is one tax (label trimmed, rate equal); zero rates are dropped.
fn distinct(list: &[NamedTax]) -> Vec<NamedTax> {
    let positive: Vec<&NamedTax> = list.iter().filter(|t| t.rate > Decimal::ZERO).collect();
    positive
        .iter()
        .enumerate()
        .filter(|(i, t)| {
            positive
                .iter()
                .position(|u| u.label.trim() == t.label.trim() && u.rate == t.rate)
                == Some(*i)
        })
        .map(|(_, t)| (*t).clone())
        .collect()
}

/// The taxes one line carries: its own list, else the document's.
fn own_taxes(doc: &Doc, it: &Item) -> Vec<NamedTax> {
    distinct(it.taxes.as_deref().unwrap_or(&doc.taxes))
}

/// Unrounded tax of each line per row (`(row index, line index, amount)`), and the rows.
fn raw_rows(
    doc: &Doc,
    subtotal: Decimal,
    taxable: Decimal,
) -> (Vec<TaxRow>, Vec<(usize, usize, Decimal)>) {
    let factor = if subtotal > Decimal::ZERO {
        taxable / subtotal
    } else {
        Decimal::ONE
    };
    let mut rows: Vec<TaxRow> = Vec::new();
    let mut parts = Vec::new();
    for (li, it) in doc.items.iter().enumerate() {
        let own = own_taxes(doc, it);
        if own.is_empty() {
            continue;
        }
        let gross = line_total(it) * factor;
        let sum_rate: Decimal = own.iter().map(|t| t.rate).sum();
        let base = if doc.tax_inclusive {
            gross / (Decimal::ONE + sum_rate / Decimal::ONE_HUNDRED)
        } else {
            gross
        };
        for t in &own {
            let ri = match rows
                .iter()
                .position(|r| r.label == t.label && r.rate == t.rate)
            {
                Some(i) => i,
                None => {
                    rows.push(TaxRow {
                        label: t.label.clone(),
                        rate: t.rate,
                        amount: Decimal::ZERO,
                    });
                    rows.len() - 1
                }
            };
            let amount = base * t.rate / Decimal::ONE_HUNDRED;
            rows[ri].amount += amount;
            parts.push((ri, li, amount));
        }
    }
    (rows, parts)
}

/// `docTotals`: discount on the subtotal, tax on what remains (prorated across lines), each
/// stage rounded to paise; tax-inclusive prices make the total the discounted subtotal and the
/// tax the portion inside it.
pub fn doc_totals(doc: &Doc) -> Totals {
    let subtotal = round2(doc.items.iter().map(line_total).sum());
    let discount = match doc.discount {
        Discount::None => Decimal::ZERO,
        Discount::Percent(v) => round2(subtotal * v / Decimal::ONE_HUNDRED),
        Discount::Fixed(v) => round2(v),
    };
    let discount = discount.max(Decimal::ZERO).min(subtotal.max(Decimal::ZERO));
    let taxable = round2(subtotal - discount);
    let (rows, _) = raw_rows(doc, subtotal, taxable);
    let taxes: Vec<TaxRow> = rows
        .into_iter()
        .map(|r| TaxRow {
            amount: round2(r.amount),
            ..r
        })
        .collect();
    let tax = round2(taxes.iter().map(|r| r.amount).sum());
    let total = if doc.tax_inclusive {
        taxable
    } else {
        round2(taxable + tax)
    };
    Totals {
        subtotal,
        discount,
        taxable,
        tax,
        taxes,
        total,
    }
}

// ================================================================ Everything: taxnote.js

/// A shipped tax-note sentence (Indianised wording; Everything's ids where they carry over).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = GstTaxNotePreset)]
pub struct TaxNotePreset {
    pub id: &'static str,
    pub label: &'static str,
    pub text: &'static str,
}

/// The wordings offered in the pickers, in the order they are shown.
pub const TAX_NOTE_PRESETS: [TaxNotePreset; 6] = [
    TaxNotePreset {
        id: "preset:educationExempt",
        label: "Education services to students (exempt)",
        text: "Exempt supply: services provided by an educational institution to its students, \
               faculty and staff (Notification No. 12/2017-Central Tax (Rate), Sl. No. 66; SAC 9992).",
    },
    TaxNotePreset {
        id: "preset:exempt",
        label: "Exempt supply",
        text: "Exempt supply under the GST law; no tax is charged on this supply.",
    },
    TaxNotePreset {
        id: "preset:nilRated",
        label: "Nil-rated supply",
        text: "Nil-rated supply: GST is charged at 0% on this supply.",
    },
    TaxNotePreset {
        id: "preset:composition",
        label: "Composition taxable person",
        text: "Composition taxable person, not eligible to collect tax on supplies.",
    },
    TaxNotePreset {
        id: "preset:zeroRated",
        label: "Zero-rated supply (export / SEZ)",
        text: "Zero-rated supply under Section 16 of the IGST Act, 2017, without payment of \
               integrated tax.",
    },
    TaxNotePreset {
        id: "preset:reverseCharge",
        label: "Reverse charge",
        text: "Tax on this supply is payable by the recipient on reverse charge basis.",
    },
];

/// Is this stored value one of the shipped sentences?
pub fn is_tax_note_preset(value: &str) -> bool {
    TAX_NOTE_PRESETS.iter().any(|p| p.id == value)
}

fn pick(v: Option<&str>) -> Option<&str> {
    v.map(str::trim).filter(|s| !s.is_empty())
}

/// `taxNoteValue`: the document's own note first, then the client's (here: the fee head's),
/// then the default (the GST profile's); blank inherits and `none` stops with "print nothing".
pub fn tax_note_value(settings: Option<&str>, doc: Option<&str>, client: Option<&str>) -> String {
    pick(doc)
        .or_else(|| pick(client))
        .or_else(|| pick(settings))
        .unwrap_or_default()
        .to_owned()
}

/// `taxNoteText`: a preset as its sentence, anything else verbatim, `none` / blank as nothing.
pub fn tax_note_text(value: &str) -> String {
    let stored = value.trim();
    if stored.is_empty() || stored == "none" {
        return String::new();
    }
    TAX_NOTE_PRESETS
        .iter()
        .find(|p| p.id == stored)
        .map_or_else(|| stored.to_owned(), |p| p.text.to_owned())
}

/// `chargesUntaxed`: no tax at all, or at least one line that carries none.
pub fn charges_untaxed(doc: &Doc) -> bool {
    if doc_totals(doc).tax <= Decimal::new(5, 3) {
        return true;
    }
    doc.items.iter().any(|it| own_taxes(doc, it).is_empty())
}

// ================================================================ Everything: doc.js

/// `formatDocNumber`: prefix + the number padded to four digits; `{YYYY}` is the issue year
/// and (Indianised) `{FY}` the April–March financial year, `26-27`.
pub fn format_doc_number(prefix: &str, n: i64, date: NaiveDate) -> String {
    let resolved = prefix
        .replace("{YYYY}", &format!("{:04}", date.year()))
        .replace("{FY}", &fy_short(date));
    format!("{resolved}{:04}", n.max(1))
}

/// First calendar year of the financial year a date falls in (April–March).
pub fn fy_start(date: NaiveDate) -> i32 {
    if date.month() >= 4 {
        date.year()
    } else {
        date.year() - 1
    }
}

/// `2026-27`.
pub fn fy_label(date: NaiveDate) -> String {
    let y = fy_start(date);
    format!("{y}-{:02}", (y + 1) % 100)
}

/// `26-27`.
pub fn fy_short(date: NaiveDate) -> String {
    let y = fy_start(date);
    format!("{:02}-{:02}", y % 100, (y + 1) % 100)
}

/// GST document numbers: at most 16 characters of letters, digits, `-` and `/` (CGST Rule 46).
pub const MAX_DOC_NUMBER: usize = 16;

/// Checks a series prefix: with `{FY}` resolved and six digits it still fits 16 characters.
pub fn valid_series_prefix(prefix: &str) -> Result<(), &'static str> {
    let sample = format_doc_number(
        prefix,
        999_999,
        NaiveDate::from_ymd_opt(2026, 4, 1).unwrap_or_default(),
    );
    if prefix.contains("{YYYY}") {
        return Err("use {FY} for the financial year");
    }
    if sample.len() > MAX_DOC_NUMBER {
        return Err("with six digits the number would be longer than 16 characters");
    }
    if !sample
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '/')
    {
        return Err("only letters, digits, - and / are allowed");
    }
    Ok(())
}

// ================================================================ GST

/// Allowed GST rates (percent): nil and the slabs of the rate schedules.
pub const GST_RATES: [u32; 6] = [0, 5, 12, 18, 28, 40];

pub fn valid_rate(rate: Decimal) -> bool {
    GST_RATES.iter().any(|r| Decimal::from(*r) == rate)
}

/// How a supply is taxed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Treatment {
    /// Taxed at a positive rate.
    Taxable,
    /// GST at 0%.
    Nil,
    /// Exempt by notification (education services to students).
    Exempt,
    /// No GST set up for the fee head (outside the GST documents).
    NonGst,
}

impl Treatment {
    pub fn as_str(self) -> &'static str {
        match self {
            Treatment::Taxable => "taxable",
            Treatment::Nil => "nil",
            Treatment::Exempt => "exempt",
            Treatment::NonGst => "non_gst",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        [
            Treatment::Taxable,
            Treatment::Nil,
            Treatment::Exempt,
            Treatment::NonGst,
        ]
        .into_iter()
        .find(|t| t.as_str() == s)
    }
}

/// The institution's GST registration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(as = GstRegistrationType)]
pub enum Registration {
    Regular,
    /// Composition levy: never collects tax, issues bills of supply.
    Composition,
    /// Not registered: issues no GST documents.
    Unregistered,
}

impl Registration {
    pub fn as_str(self) -> &'static str {
        match self {
            Registration::Regular => "regular",
            Registration::Composition => "composition",
            Registration::Unregistered => "unregistered",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        [
            Registration::Regular,
            Registration::Composition,
            Registration::Unregistered,
        ]
        .into_iter()
        .find(|t| t.as_str() == s)
    }
}

/// A tax component.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
#[schema(as = GstComponent)]
pub enum Component {
    Cgst,
    Sgst,
    Igst,
}

impl Component {
    pub const ALL: [Component; 3] = [Component::Cgst, Component::Sgst, Component::Igst];
    pub fn label(self) -> &'static str {
        match self {
            Component::Cgst => "CGST",
            Component::Sgst => "SGST",
            Component::Igst => "IGST",
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Component::Cgst => "cgst",
            Component::Sgst => "sgst",
            Component::Igst => "igst",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.as_str() == s)
    }
    fn from_label(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.label() == s)
    }
}

/// `true` when the supplier and the place of supply are in different states (IGST).
pub fn is_inter_state(supplier_state: &str, place_of_supply: &str) -> bool {
    supplier_state != place_of_supply
}

/// GST at `rate` as Everything's named taxes: CGST + SGST halves, or IGST.
pub fn gst_taxes(rate: Decimal, inter_state: bool) -> Vec<NamedTax> {
    if rate <= Decimal::ZERO {
        return Vec::new();
    }
    if inter_state {
        vec![NamedTax::new("IGST", rate)]
    } else {
        let half = rate / Decimal::TWO;
        vec![NamedTax::new("CGST", half), NamedTax::new("SGST", half)]
    }
}

/// A fee head's tax setup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeadTax {
    pub sac: String,
    pub rate: Decimal,
    pub exempt: bool,
    pub inclusive: bool,
}

impl HeadTax {
    pub fn treatment(&self, registration: Registration) -> Treatment {
        if self.exempt {
            Treatment::Exempt
        } else if self.rate.is_zero() || registration != Registration::Regular {
            Treatment::Nil
        } else {
            Treatment::Taxable
        }
    }
}

/// One line of a charge before tax: its amount and its head's setup (`None`: no GST setup).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineIn {
    pub amount: Decimal,
    pub tax: Option<HeadTax>,
}

/// One line of a charge after tax: the taxable value kept on the line, and its share of each
/// tax row (the shares of a row add up to the row exactly).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinePlan {
    pub taxable_value: Decimal,
    pub treatment: Treatment,
    pub sac: Option<String>,
    pub rate: Decimal,
    pub cgst: Decimal,
    pub sgst: Decimal,
    pub igst: Decimal,
}

impl LinePlan {
    pub fn tax(&self) -> Decimal {
        self.cgst + self.sgst + self.igst
    }
}

/// One tax row of a charge, raised as its own charge line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanRow {
    pub component: Component,
    /// The component's own rate (half the slab for CGST / SGST).
    pub rate: Decimal,
    pub amount: Decimal,
}

impl PlanRow {
    /// `CGST @ 9%`.
    pub fn label(&self) -> String {
        format!("{} @ {}%", self.component.label(), self.rate.normalize())
    }
}

/// The kind of GST document a charge carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(as = GstDocKind)]
pub enum DocKind {
    TaxInvoice,
    BillOfSupply,
    CreditNote,
}

impl DocKind {
    pub fn as_str(self) -> &'static str {
        match self {
            DocKind::TaxInvoice => "tax_invoice",
            DocKind::BillOfSupply => "bill_of_supply",
            DocKind::CreditNote => "credit_note",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        [
            DocKind::TaxInvoice,
            DocKind::BillOfSupply,
            DocKind::CreditNote,
        ]
        .into_iter()
        .find(|k| k.as_str() == s)
    }
    /// The heading printed on the document.
    pub fn title(self) -> &'static str {
        match self {
            DocKind::TaxInvoice => "Tax Invoice",
            DocKind::BillOfSupply => "Bill of Supply",
            DocKind::CreditNote => "Credit Note",
        }
    }
}

/// What raising a charge under GST does to its lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChargePlan {
    pub inter_state: bool,
    /// `None` when no line has a GST setup (the charge is outside the GST documents).
    pub kind: Option<DocKind>,
    pub lines: Vec<LinePlan>,
    pub rows: Vec<PlanRow>,
    pub taxable_value: Decimal,
    pub tax: Decimal,
    pub total: Decimal,
}

/// Splits `total` across `parts` (unrounded) so that the rounded shares add up to `total`
/// exactly: each share is rounded, then the paise left over go one by one to the largest parts.
fn allocate(total: Decimal, parts: &[Decimal]) -> Vec<Decimal> {
    let mut out: Vec<Decimal> = parts.iter().map(|p| round2(*p)).collect();
    let mut left = total - out.iter().copied().sum::<Decimal>();
    let step = Decimal::new(1, 2);
    let mut order: Vec<usize> = (0..parts.len()).collect();
    order.sort_by(|a, b| parts[*b].cmp(&parts[*a]).then(a.cmp(b)));
    let mut i = 0;
    while !left.is_zero() && !order.is_empty() {
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

/// Plans a charge: lines priced tax-exclusive keep their amount as the taxable value and the
/// tax is added on top; lines priced tax-inclusive have the tax carved out of their amount. The
/// two groups are each one Everything document (rows rounded per group, then merged); every
/// tax row's paise are spread over the lines it came from so line shares add up to the row.
pub fn plan_charge(lines: &[LineIn], inter_state: bool, registration: Registration) -> ChargePlan {
    let mut plans: Vec<LinePlan> = lines
        .iter()
        .map(|l| LinePlan {
            taxable_value: l.amount,
            treatment: l
                .tax
                .as_ref()
                .map_or(Treatment::NonGst, |t| t.treatment(registration)),
            sac: l.tax.as_ref().map(|t| t.sac.clone()),
            rate: match &l.tax {
                Some(t) if t.treatment(registration) == Treatment::Taxable => t.rate,
                _ => Decimal::ZERO,
            },
            cgst: Decimal::ZERO,
            sgst: Decimal::ZERO,
            igst: Decimal::ZERO,
        })
        .collect();
    let mut rows: Vec<PlanRow> = Vec::new();
    for inclusive in [false, true] {
        let idx: Vec<usize> = (0..lines.len())
            .filter(|i| {
                plans[*i].treatment == Treatment::Taxable
                    && lines[*i]
                        .tax
                        .as_ref()
                        .is_some_and(|t| t.inclusive == inclusive)
            })
            .collect();
        if idx.is_empty() {
            continue;
        }
        let doc = Doc {
            items: idx
                .iter()
                .map(|i| Item {
                    qty: Decimal::ONE,
                    rate: lines[*i].amount,
                    taxes: Some(gst_taxes(plans[*i].rate, inter_state)),
                })
                .collect(),
            taxes: Vec::new(),
            discount: Discount::None,
            tax_inclusive: inclusive,
        };
        let totals = doc_totals(&doc);
        let subtotal = totals.subtotal;
        let (_, parts) = raw_rows(&doc, subtotal, totals.taxable);
        for (ri, row) in totals.taxes.iter().enumerate() {
            let Some(component) = Component::from_label(&row.label) else {
                continue;
            };
            let mine: Vec<&(usize, usize, Decimal)> =
                parts.iter().filter(|(r, ..)| *r == ri).collect();
            let shares = allocate(row.amount, &mine.iter().map(|p| p.2).collect::<Vec<_>>());
            for (p, share) in mine.iter().zip(shares) {
                let line = &mut plans[idx[p.1]];
                match component {
                    Component::Cgst => line.cgst += share,
                    Component::Sgst => line.sgst += share,
                    Component::Igst => line.igst += share,
                }
            }
            match rows
                .iter_mut()
                .find(|r| r.component == component && r.rate == row.rate)
            {
                Some(r) => r.amount += row.amount,
                None => rows.push(PlanRow {
                    component,
                    rate: row.rate,
                    amount: row.amount,
                }),
            }
        }
        if inclusive {
            for i in &idx {
                let tax = plans[*i].tax();
                plans[*i].taxable_value = lines[*i].amount - tax;
            }
        }
    }
    rows.retain(|r| r.amount > Decimal::ZERO);
    rows.sort_by(|a, b| a.rate.cmp(&b.rate).then(a.component.cmp(&b.component)));
    let any_gst = plans.iter().any(|p| p.treatment != Treatment::NonGst);
    let tax: Decimal = rows.iter().map(|r| r.amount).sum();
    let kind = match (any_gst, registration) {
        (false, _) | (_, Registration::Unregistered) => None,
        _ if tax > Decimal::ZERO => Some(DocKind::TaxInvoice),
        _ => Some(DocKind::BillOfSupply),
    };
    let taxable_value = plans.iter().map(|p| p.taxable_value).sum();
    ChargePlan {
        inter_state,
        kind,
        total: taxable_value + tax,
        lines: plans,
        rows,
        taxable_value,
        tax,
    }
}

/// Tax a credit note reverses: each row in proportion to the share of the charge credited,
/// cumulatively, so that crediting the whole charge in parts reverses every row exactly.
/// Returns `(component, rate, amount)` per row with a non-zero reversal.
pub fn credit_reversal(
    rows: &[PlanRow],
    charge_total: Decimal,
    credited_before: Decimal,
    amount: Decimal,
) -> Vec<PlanRow> {
    if charge_total <= Decimal::ZERO || amount <= Decimal::ZERO {
        return Vec::new();
    }
    let share = |credited: Decimal, row: Decimal| {
        let c = credited.min(charge_total).max(Decimal::ZERO);
        round2(row * c / charge_total).min(row)
    };
    rows.iter()
        .map(|r| PlanRow {
            component: r.component,
            rate: r.rate,
            amount: share(credited_before + amount, r.amount) - share(credited_before, r.amount),
        })
        .filter(|r| r.amount > Decimal::ZERO)
        .collect()
}

// ================================================================ GSTR-1 friendly summary

/// One line of an issued document, as the summary reads it (credit notes carry positive
/// figures; the summary subtracts them).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactLine {
    pub sac: Option<String>,
    pub treatment: Treatment,
    pub rate: Decimal,
    pub taxable_value: Decimal,
    pub cgst: Decimal,
    pub sgst: Decimal,
    pub igst: Decimal,
}

/// One issued GST document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocFact {
    pub kind: DocKind,
    pub number: String,
    pub date: NaiveDate,
    pub cancelled: bool,
    pub recipient_gstin: Option<String>,
    pub recipient_name: String,
    pub place_of_supply: String,
    pub inter_state: bool,
    pub original_number: Option<String>,
    pub original_date: Option<NaiveDate>,
    pub lines: Vec<FactLine>,
}

impl DocFact {
    fn taxable(&self) -> Decimal {
        self.lines
            .iter()
            .filter(|l| l.treatment == Treatment::Taxable)
            .map(|l| l.taxable_value)
            .sum()
    }
    fn sum(&self, f: fn(&FactLine) -> Decimal) -> Decimal {
        self.lines.iter().map(f).sum()
    }
}

/// Money columns of a summary row.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Amounts {
    pub taxable_value: Decimal,
    pub cgst: Decimal,
    pub sgst: Decimal,
    pub igst: Decimal,
}

impl Amounts {
    fn add(&mut self, l: &FactLine, sign: Decimal) {
        self.taxable_value += l.taxable_value * sign;
        self.cgst += l.cgst * sign;
        self.sgst += l.sgst * sign;
        self.igst += l.igst * sign;
    }
    pub fn tax(&self) -> Decimal {
        self.cgst + self.sgst + self.igst
    }
}

/// B2B: one taxable invoice to a registered recipient.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct B2bRow {
    pub recipient_gstin: String,
    pub recipient_name: String,
    pub number: String,
    pub date: NaiveDate,
    pub invoice_value: Decimal,
    pub place_of_supply: String,
    pub rate: Decimal,
    pub amounts: Amounts,
}

/// B2C: taxable supplies to unregistered recipients by place of supply and rate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct B2cRow {
    pub place_of_supply: String,
    pub inter_state: bool,
    pub rate: Decimal,
    pub amounts: Amounts,
}

/// HSN / SAC summary (net of credit notes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HsnRow {
    pub sac: String,
    pub rate: Decimal,
    pub count: i64,
    pub amounts: Amounts,
}

/// Nil-rated, exempt and non-GST supplies, intra- and inter-state (net of credit notes).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExemptRow {
    pub intra_nil: Decimal,
    pub intra_exempt: Decimal,
    pub intra_non_gst: Decimal,
    pub inter_nil: Decimal,
    pub inter_exempt: Decimal,
    pub inter_non_gst: Decimal,
}

/// A credit note (CDNR when the recipient is registered, CDNUR otherwise).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreditNoteRow {
    pub registered: bool,
    pub recipient_gstin: Option<String>,
    pub recipient_name: String,
    pub number: String,
    pub date: NaiveDate,
    pub original_number: Option<String>,
    pub original_date: Option<NaiveDate>,
    pub place_of_supply: String,
    pub note_value: Decimal,
    pub amounts: Amounts,
}

/// Documents issued per series (GSTR-1 table 13).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocIssueRow {
    pub kind: DocKind,
    pub from: String,
    pub to: String,
    pub total: i64,
    pub cancelled: i64,
}

/// The GST summary of a period.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Summary {
    pub b2b: Vec<B2bRow>,
    pub b2c: Vec<B2cRow>,
    pub hsn: Vec<HsnRow>,
    pub exempt: ExemptRow,
    pub credit_notes: Vec<CreditNoteRow>,
    pub documents: Vec<DocIssueRow>,
    /// Outward taxable supplies net of credit notes.
    pub net: Amounts,
}

/// Builds the summary from the documents issued in a period (cancelled ones only count in the
/// document-issue table).
pub fn summarize(docs: &[DocFact]) -> Summary {
    let mut s = Summary::default();
    let mut b2c: BTreeMap<(String, bool, Decimal), Amounts> = BTreeMap::new();
    let mut hsn: BTreeMap<(String, Decimal), (i64, Amounts)> = BTreeMap::new();
    let mut series: BTreeMap<&'static str, (DocKind, Vec<&DocFact>)> = BTreeMap::new();
    for d in docs {
        series
            .entry(d.kind.as_str())
            .or_insert_with(|| (d.kind, Vec::new()))
            .1
            .push(d);
        if d.cancelled {
            continue;
        }
        let credit = d.kind == DocKind::CreditNote;
        let sign = if credit { -Decimal::ONE } else { Decimal::ONE };
        for l in &d.lines {
            let bucket = match (l.treatment, d.inter_state) {
                (Treatment::Taxable, _) => None,
                (Treatment::Nil, false) => Some(&mut s.exempt.intra_nil),
                (Treatment::Exempt, false) => Some(&mut s.exempt.intra_exempt),
                (Treatment::NonGst, false) => Some(&mut s.exempt.intra_non_gst),
                (Treatment::Nil, true) => Some(&mut s.exempt.inter_nil),
                (Treatment::Exempt, true) => Some(&mut s.exempt.inter_exempt),
                (Treatment::NonGst, true) => Some(&mut s.exempt.inter_non_gst),
            };
            match bucket {
                Some(b) => *b += l.taxable_value * sign,
                None => s.net.add(l, sign),
            }
            if l.treatment != Treatment::NonGst {
                let e = hsn
                    .entry((l.sac.clone().unwrap_or_default(), l.rate))
                    .or_default();
                e.0 += 1;
                e.1.add(l, sign);
            }
        }
        if credit {
            let mut a = Amounts::default();
            for l in d.lines.iter().filter(|l| l.treatment == Treatment::Taxable) {
                a.add(l, Decimal::ONE);
            }
            s.credit_notes.push(CreditNoteRow {
                registered: d.recipient_gstin.is_some(),
                recipient_gstin: d.recipient_gstin.clone(),
                recipient_name: d.recipient_name.clone(),
                number: d.number.clone(),
                date: d.date,
                original_number: d.original_number.clone(),
                original_date: d.original_date,
                place_of_supply: d.place_of_supply.clone(),
                note_value: d.sum(|l| l.taxable_value + l.cgst + l.sgst + l.igst),
                amounts: a,
            });
            continue;
        }
        if d.taxable().is_zero() {
            continue;
        }
        if let Some(gstin) = &d.recipient_gstin {
            let value = d.sum(|l| l.taxable_value + l.cgst + l.sgst + l.igst);
            let mut rates: BTreeMap<Decimal, Amounts> = BTreeMap::new();
            for l in d.lines.iter().filter(|l| l.treatment == Treatment::Taxable) {
                rates.entry(l.rate).or_default().add(l, Decimal::ONE);
            }
            for (rate, amounts) in rates {
                s.b2b.push(B2bRow {
                    recipient_gstin: gstin.clone(),
                    recipient_name: d.recipient_name.clone(),
                    number: d.number.clone(),
                    date: d.date,
                    invoice_value: value,
                    place_of_supply: d.place_of_supply.clone(),
                    rate,
                    amounts,
                });
            }
        } else {
            for l in d.lines.iter().filter(|l| l.treatment == Treatment::Taxable) {
                b2c.entry((d.place_of_supply.clone(), d.inter_state, l.rate))
                    .or_default()
                    .add(l, Decimal::ONE);
            }
        }
    }
    s.b2c = b2c
        .into_iter()
        .map(|((place_of_supply, inter_state, rate), amounts)| B2cRow {
            place_of_supply,
            inter_state,
            rate,
            amounts,
        })
        .collect();
    s.hsn = hsn
        .into_iter()
        .map(|((sac, rate), (count, amounts))| HsnRow {
            sac,
            rate,
            count,
            amounts,
        })
        .collect();
    s.documents = series
        .into_values()
        .map(|(kind, mut list)| {
            list.sort_by(|a, b| a.number.cmp(&b.number));
            DocIssueRow {
                kind,
                from: list.first().map(|d| d.number.clone()).unwrap_or_default(),
                to: list.last().map(|d| d.number.clone()).unwrap_or_default(),
                total: i64::try_from(list.len()).unwrap_or(i64::MAX),
                cancelled: i64::try_from(list.iter().filter(|d| d.cancelled).count())
                    .unwrap_or(i64::MAX),
            }
        })
        .collect();
    s
}

/// One CSV cell (RFC 4180): quoted when it holds a comma, quote or line break, and a leading
/// `= + - @` (formula injection in spreadsheets) is neutralised with an apostrophe.
pub fn csv_cell(v: &str) -> String {
    let v = if v.starts_with(['=', '+', '@']) || (v.starts_with('-') && v.parse::<f64>().is_err()) {
        format!("'{v}")
    } else {
        v.to_owned()
    };
    if v.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", v.replace('"', "\"\""))
    } else {
        v
    }
}

fn money(d: Decimal) -> String {
    let mut a = d.round_dp(2);
    a.rescale(2);
    a.to_string()
}

fn pct(d: Decimal) -> String {
    d.normalize().to_string()
}

/// Indian date on the paper and in the CSV (`17-10-2026`, as the GST portal's offline tool).
pub fn gst_date(d: NaiveDate) -> String {
    d.format("%d-%m-%Y").to_string()
}

/// The summary as one CSV with a section per GSTR-1 table (`section` first column).
pub fn summary_csv(s: &Summary, from: NaiveDate, to: NaiveDate, gstin: &str) -> String {
    let mut out = String::new();
    let mut line = |cells: &[String]| {
        out.push_str(
            &cells
                .iter()
                .map(|c| csv_cell(c))
                .collect::<Vec<_>>()
                .join(","),
        );
        out.push_str("\r\n");
    };
    let st = |c: &str| c.to_owned();
    line(&[st("GST summary"), st(gstin), gst_date(from), gst_date(to)]);
    line(&[
        st("section"),
        st("gstin_or_place"),
        st("name"),
        st("number"),
        st("date"),
        st("value"),
        st("place_of_supply"),
        st("rate"),
        st("taxable_value"),
        st("cgst"),
        st("sgst"),
        st("igst"),
        st("original_number"),
        st("original_date"),
    ]);
    for r in &s.b2b {
        line(&[
            st("b2b"),
            r.recipient_gstin.clone(),
            r.recipient_name.clone(),
            r.number.clone(),
            gst_date(r.date),
            money(r.invoice_value),
            r.place_of_supply.clone(),
            pct(r.rate),
            money(r.amounts.taxable_value),
            money(r.amounts.cgst),
            money(r.amounts.sgst),
            money(r.amounts.igst),
            String::new(),
            String::new(),
        ]);
    }
    for r in &s.b2c {
        line(&[
            st(if r.inter_state { "b2cs_inter" } else { "b2cs" }),
            r.place_of_supply.clone(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            r.place_of_supply.clone(),
            pct(r.rate),
            money(r.amounts.taxable_value),
            money(r.amounts.cgst),
            money(r.amounts.sgst),
            money(r.amounts.igst),
            String::new(),
            String::new(),
        ]);
    }
    for r in &s.credit_notes {
        line(&[
            st(if r.registered { "cdnr" } else { "cdnur" }),
            r.recipient_gstin.clone().unwrap_or_default(),
            r.recipient_name.clone(),
            r.number.clone(),
            gst_date(r.date),
            money(r.note_value),
            r.place_of_supply.clone(),
            String::new(),
            money(r.amounts.taxable_value),
            money(r.amounts.cgst),
            money(r.amounts.sgst),
            money(r.amounts.igst),
            r.original_number.clone().unwrap_or_default(),
            r.original_date.map(gst_date).unwrap_or_default(),
        ]);
    }
    for r in &s.hsn {
        line(&[
            st("hsn"),
            r.sac.clone(),
            String::new(),
            r.count.to_string(),
            String::new(),
            money(r.amounts.taxable_value + r.amounts.tax()),
            String::new(),
            pct(r.rate),
            money(r.amounts.taxable_value),
            money(r.amounts.cgst),
            money(r.amounts.sgst),
            money(r.amounts.igst),
            String::new(),
            String::new(),
        ]);
    }
    let e = &s.exempt;
    for (kind, nil, exempt, non_gst) in [
        ("exemp_intra", e.intra_nil, e.intra_exempt, e.intra_non_gst),
        ("exemp_inter", e.inter_nil, e.inter_exempt, e.inter_non_gst),
    ] {
        line(&[
            st(kind),
            st("nil / exempt / non-GST"),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            money(nil),
            money(exempt),
            money(non_gst),
            String::new(),
            String::new(),
            String::new(),
        ]);
    }
    for r in &s.documents {
        line(&[
            st("docs"),
            st(r.kind.title()),
            String::new(),
            format!("{} to {}", r.from, r.to),
            String::new(),
            r.total.to_string(),
            String::new(),
            String::new(),
            r.cancelled.to_string(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
        ]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn d(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap_or_default()
    }
    fn date(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap_or_default()
    }
    fn head(rate: &str, inclusive: bool) -> Option<HeadTax> {
        Some(HeadTax {
            sac: "996331".into(),
            rate: d(rate),
            exempt: false,
            inclusive,
        })
    }

    #[test]
    fn rounding_matches_javascript() {
        assert_eq!(round2(d("1.005")), d("1.01"));
        assert_eq!(round2(d("-1.005")), d("-1"));
        assert_eq!(round2(d("2.344999")), d("2.34"));
    }

    #[test]
    fn intra_state_hostel_fee_at_twelve_percent() {
        let p = plan_charge(
            &[LineIn {
                amount: d("10000"),
                tax: head("12", false),
            }],
            false,
            Registration::Regular,
        );
        assert_eq!(p.kind, Some(DocKind::TaxInvoice));
        assert_eq!(p.rows.len(), 2);
        assert_eq!(p.rows[0].label(), "CGST @ 6%");
        assert_eq!(p.rows[0].amount, d("600"));
        assert_eq!(p.total, d("11200"));
        assert_eq!(p.lines[0].taxable_value, d("10000"));
    }

    #[test]
    fn inclusive_prices_carve_the_tax_out() {
        let p = plan_charge(
            &[
                LineIn {
                    amount: d("1180"),
                    tax: head("18", true),
                },
                LineIn {
                    amount: d("333.33"),
                    tax: head("18", true),
                },
                LineIn {
                    amount: d("50000"),
                    tax: Some(HeadTax {
                        sac: "9992".into(),
                        rate: Decimal::ZERO,
                        exempt: true,
                        inclusive: false,
                    }),
                },
            ],
            true,
            Registration::Regular,
        );
        assert_eq!(p.total, d("51513.33"), "inclusive: the total does not move");
        assert_eq!(
            p.rows,
            vec![PlanRow {
                component: Component::Igst,
                rate: d("18"),
                amount: d("230.85")
            }]
        );
        assert_eq!(
            p.lines[0].taxable_value + p.lines[1].taxable_value,
            d("1282.48")
        );
        assert_eq!(p.lines[0].igst + p.lines[1].igst, d("230.85"));
        assert_eq!(p.lines[2].treatment, Treatment::Exempt);
    }

    #[test]
    fn exempt_only_charges_get_a_bill_of_supply_and_unconfigured_ones_nothing() {
        let exempt = Some(HeadTax {
            sac: "9992".into(),
            rate: Decimal::ZERO,
            exempt: true,
            inclusive: false,
        });
        let p = plan_charge(
            &[LineIn {
                amount: d("45000"),
                tax: exempt,
            }],
            false,
            Registration::Regular,
        );
        assert_eq!(p.kind, Some(DocKind::BillOfSupply));
        assert!(p.rows.is_empty());
        let none = plan_charge(
            &[LineIn {
                amount: d("45000"),
                tax: None,
            }],
            false,
            Registration::Regular,
        );
        assert_eq!(none.kind, None);
        let composition = plan_charge(
            &[LineIn {
                amount: d("100"),
                tax: head("18", false),
            }],
            false,
            Registration::Composition,
        );
        assert_eq!(composition.kind, Some(DocKind::BillOfSupply));
        assert_eq!(composition.total, d("100"));
    }

    #[test]
    fn line_shares_add_up_to_each_row() {
        let lines: Vec<LineIn> = ["0.07", "0.07", "0.07", "1999.99"]
            .iter()
            .map(|a| LineIn {
                amount: d(a),
                tax: head("5", false),
            })
            .collect();
        let p = plan_charge(&lines, false, Registration::Regular);
        for c in [Component::Cgst, Component::Sgst] {
            let row: Decimal = p
                .rows
                .iter()
                .filter(|r| r.component == c)
                .map(|r| r.amount)
                .sum();
            let shares: Decimal = p
                .lines
                .iter()
                .map(|l| if c == Component::Cgst { l.cgst } else { l.sgst })
                .sum();
            assert_eq!(row, shares);
        }
        assert_eq!(p.total, p.taxable_value + p.tax);
    }

    #[test]
    fn credit_notes_reverse_tax_in_proportion_and_exactly_in_full() {
        let rows = vec![
            PlanRow {
                component: Component::Cgst,
                rate: d("9"),
                amount: d("90.01"),
            },
            PlanRow {
                component: Component::Sgst,
                rate: d("9"),
                amount: d("90.01"),
            },
        ];
        let total = d("1180.02");
        let first = credit_reversal(&rows, total, Decimal::ZERO, d("590.01"));
        assert_eq!(first[0].amount, d("45.01"));
        let rest = credit_reversal(&rows, total, d("590.01"), d("590.01"));
        assert_eq!(first[0].amount + rest[0].amount, d("90.01"));
        assert!(credit_reversal(&rows, total, total, d("10")).is_empty());
    }

    #[test]
    fn numbers_and_financial_years() {
        let day = date("2027-02-10");
        assert_eq!(fy_label(day), "2026-27");
        assert_eq!(format_doc_number("TI/{FY}/", 7, day), "TI/26-27/0007");
        assert_eq!(format_doc_number("INV-{YYYY}-", 0, day), "INV-2027-0001");
        assert!(valid_series_prefix("TI/{FY}/").is_ok());
        assert!(valid_series_prefix("TAXINVOICE/{FY}/").is_err());
        assert!(valid_series_prefix("TI {FY}").is_err());
        assert!(valid_series_prefix("TI/{YYYY}/").is_err());
    }

    #[test]
    fn notes_resolve_and_print() {
        assert_eq!(
            tax_note_value(Some("global"), Some(" "), Some("head")),
            "head"
        );
        assert_eq!(tax_note_value(Some("global"), Some("none"), None), "none");
        assert_eq!(tax_note_text("none"), "");
        assert!(tax_note_text("preset:educationExempt").contains("12/2017"));
        assert_eq!(tax_note_text("Own words"), "Own words");
        assert!(is_tax_note_preset("preset:nilRated"));
    }

    #[test]
    fn summary_nets_credit_notes_and_splits_b2b_b2c() {
        let line = |t: Treatment, v: &str, c: &str| FactLine {
            sac: Some("996331".into()),
            treatment: t,
            rate: if t == Treatment::Taxable {
                d("18")
            } else {
                Decimal::ZERO
            },
            taxable_value: d(v),
            cgst: d(c),
            sgst: d(c),
            igst: Decimal::ZERO,
        };
        let doc = |kind, number: &str, gstin: Option<&str>, lines| DocFact {
            kind,
            number: number.into(),
            date: date("2026-10-17"),
            cancelled: false,
            recipient_gstin: gstin.map(str::to_owned),
            recipient_name: "Isha".into(),
            place_of_supply: "27".into(),
            inter_state: false,
            original_number: None,
            original_date: None,
            lines,
        };
        let s = summarize(&[
            doc(
                DocKind::TaxInvoice,
                "TI/26-27/0001",
                None,
                vec![
                    line(Treatment::Taxable, "1000", "90"),
                    line(Treatment::Exempt, "5000", "0"),
                ],
            ),
            doc(
                DocKind::TaxInvoice,
                "TI/26-27/0002",
                Some("27AAPFU0939F1ZV"),
                vec![line(Treatment::Taxable, "2000", "180")],
            ),
            doc(
                DocKind::CreditNote,
                "CN/26-27/0001",
                None,
                vec![line(Treatment::Taxable, "500", "45")],
            ),
        ]);
        assert_eq!(s.b2b.len(), 1);
        assert_eq!(s.b2b[0].invoice_value, d("2360"));
        assert_eq!(s.b2c[0].amounts.taxable_value, d("1000"));
        assert_eq!(s.net.taxable_value, d("2500"));
        assert_eq!(s.net.cgst, d("225"));
        assert_eq!(s.exempt.intra_exempt, d("5000"));
        assert_eq!(s.credit_notes.len(), 1);
        assert_eq!(s.documents.len(), 2);
        let csv = summary_csv(
            &s,
            date("2026-10-01"),
            date("2026-10-31"),
            "27AAPFU0939F1ZV",
        );
        assert!(csv.contains("b2b,27AAPFU0939F1ZV,Isha,TI/26-27/0002,17-10-2026,2360.00,27,18,2000.00,180.00,180.00,0.00"));
        assert!(csv.contains("cdnur"));
    }

    #[test]
    fn csv_cells_are_safe() {
        assert_eq!(csv_cell("a,b"), "\"a,b\"");
        assert_eq!(csv_cell("=SUM(A1)"), "'=SUM(A1)");
        assert_eq!(csv_cell("-12.50"), "-12.50");
        assert_eq!(csv_cell("say \"hi\""), "\"say \"\"hi\"\"\"");
    }
}
