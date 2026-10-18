//! A fee receipt and the counter's day close as slips (pure; no I/O).
//!
//! Indian conventions: amounts carry lakh grouping (`₹1,25,000.00`, printed `Rs.1,25,000.00`),
//! the amount is written out in words the way a counter receipt book has it
//! (`Rupees One Lakh Twenty-Five Thousand Only`), and a cheque or draft number travels with the
//! tender so the slip can be matched to the bank paying-in.

use super::escpos::{ReportHead, ReportRow, SlipDoc, SlipItem, SlipRow};
use crate::shared::money::display_inr;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::{Decimal, RoundingStrategy};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// One registration printed under the header (GSTIN, PAN, trust registration …).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = ReceiptPrintRegistration)]
pub struct Registration {
    pub label: String,
    pub number: String,
}

/// What an institution prints on its counter slips.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrintSettings {
    /// Empty means the institution's own name.
    pub title: String,
    pub address: String,
    pub registrations: Vec<Registration>,
    pub footer: String,
    pub show_amount_in_words: bool,
}

impl Default for PrintSettings {
    fn default() -> Self {
        Self {
            title: String::new(),
            address: String::new(),
            registrations: Vec::new(),
            footer: String::new(),
            show_amount_in_words: true,
        }
    }
}

/// The footer printed when the institution has not written one.
pub const DEFAULT_FOOTER: &str = "Computer-generated receipt. No signature required.";

/// One charge the receipt paid.
#[derive(Debug, Clone, PartialEq)]
pub struct PaidLine {
    pub charge_number: String,
    pub description: String,
    pub amount: Decimal,
}

/// The facts of one fee receipt, already read from the ledger.
#[derive(Debug, Clone, PartialEq)]
pub struct ReceiptFacts {
    pub receipt_number: String,
    pub payment_number: String,
    /// Issue time in the institution's timezone, ready to print (`17 Oct 2026 · 11:42 AM`).
    pub issued_local: String,
    pub student_name: Option<String>,
    pub student_number: Option<String>,
    /// `cash`, `cheque`, `bank_transfer`, `demand_draft`, `gateway` or `sponsor`.
    pub channel: String,
    /// Cheque / draft / UTR number, when the tender has one.
    pub external_ref: Option<String>,
    /// Who paid, when it was not the student (a parent, a sponsor).
    pub payer_name: Option<String>,
    pub amount: Decimal,
    /// The part not yet set against a charge (held as an advance).
    pub unapplied: Decimal,
    pub cancelled: bool,
    pub cancel_reason: Option<String>,
    pub lines: Vec<PaidLine>,
}

/// How a tender reads on paper.
pub fn channel_label(channel: &str) -> &'static str {
    match channel {
        "cash" => "Cash",
        "cheque" => "Cheque",
        "bank_transfer" => "Bank transfer",
        "demand_draft" => "Demand draft",
        "gateway" => "Online payment",
        "sponsor" => "Sponsor",
        _ => "Other",
    }
}

/// The name in the header: the slip's own title, else the institution's name.
fn store_name(settings: &PrintSettings, institution_name: &str) -> String {
    let title = settings.title.trim();
    if title.is_empty() {
        institution_name.trim().to_owned()
    } else {
        title.to_owned()
    }
}

/// `GSTIN 27AAPFU0939F1ZV`; a label without a number prints nothing, at most two lines.
fn registration_lines(settings: &PrintSettings) -> Vec<String> {
    settings
        .registrations
        .iter()
        .map(|r| (r.label.trim(), r.number.trim()))
        .filter(|(_, n)| !n.is_empty())
        .map(|(l, n)| {
            if l.is_empty() {
                n.to_owned()
            } else {
                format!("{l} {n}")
            }
        })
        .take(2)
        .collect()
}

/// A fee receipt as a slip. `duplicate` marks a reprint so it cannot pass for a second payment.
pub fn fee_receipt_doc(
    facts: &ReceiptFacts,
    settings: &PrintSettings,
    institution_name: &str,
    duplicate: bool,
) -> SlipDoc {
    let mut meta = vec![
        format!("Receipt #{}", facts.receipt_number),
        facts.issued_local.clone(),
    ];
    match (&facts.student_name, &facts.student_number) {
        (Some(n), Some(no)) => meta.push(format!("Student: {n} ({no})")),
        (Some(n), None) => meta.push(format!("Student: {n}")),
        (None, Some(no)) => meta.push(format!("Student no. {no}")),
        (None, None) => {}
    }
    if let Some(p) = facts.payer_name.as_deref().filter(|p| !p.trim().is_empty()) {
        meta.push(format!("Paid by {}", p.trim()));
    }
    meta.push(format!("Payment {}", facts.payment_number));
    if facts.cancelled {
        meta.push("CANCELLED".to_owned());
    }

    let items = facts
        .lines
        .iter()
        .map(|l| SlipItem {
            qty: l.charge_number.clone(),
            name: l.description.clone(),
            amount: display_inr(l.amount),
            subs: Vec::new(),
        })
        .collect();

    let mut totals = Vec::new();
    if facts.unapplied > Decimal::ZERO {
        totals.push(SlipRow {
            label: "Held as advance".to_owned(),
            value: display_inr(facts.unapplied),
            strong: false,
        });
    }
    totals.push(SlipRow {
        label: "Total received".to_owned(),
        value: display_inr(facts.amount),
        strong: true,
    });

    let tender = match facts.external_ref.as_deref().map(str::trim) {
        Some(r) if !r.is_empty() && facts.channel != "cash" => {
            format!("{} {r}", channel_label(&facts.channel))
        }
        _ => channel_label(&facts.channel).to_owned(),
    };
    let payments = vec![SlipRow {
        label: tender,
        value: display_inr(facts.amount),
        strong: false,
    }];

    let mut note = Vec::new();
    if settings.show_amount_in_words {
        note.push(amount_in_words_inr(facts.amount));
    }
    if facts.cancelled {
        match facts.cancel_reason.as_deref().map(str::trim) {
            Some(r) if !r.is_empty() => note.push(format!("Cancelled: {r}")),
            _ => note.push("This receipt was cancelled.".to_owned()),
        }
    }
    let footer = if settings.footer.trim().is_empty() {
        DEFAULT_FOOTER.to_owned()
    } else {
        settings.footer.trim().to_owned()
    };

    SlipDoc {
        store_name: store_name(settings, institution_name),
        address: settings.address.trim().to_owned(),
        registrations: registration_lines(settings),
        voided: facts.cancelled,
        kind: Some(
            if duplicate {
                "DUPLICATE COPY"
            } else {
                "FEE RECEIPT"
            }
            .to_owned(),
        ),
        meta,
        items,
        totals,
        payments,
        note: note.join("\n"),
        reward: String::new(),
        loyalty: String::new(),
        footer,
    }
}

/// The slip a counter prints to prove its paper width and its connection.
pub fn test_doc(settings: &PrintSettings, institution_name: &str, columns: usize) -> SlipDoc {
    SlipDoc {
        store_name: store_name(settings, institution_name),
        address: settings.address.trim().to_owned(),
        registrations: registration_lines(settings),
        voided: false,
        kind: Some("TEST PRINT".to_owned()),
        meta: vec![format!("Paper: {columns} characters a line")],
        items: vec![SlipItem {
            qty: "1".to_owned(),
            name: "Line width check".to_owned(),
            amount: display_inr(Decimal::new(125_000_00, 2)),
            subs: vec!["The amount above should end at the right edge.".to_owned()],
        }],
        totals: vec![SlipRow {
            label: "Total".to_owned(),
            value: display_inr(Decimal::new(125_000_00, 2)),
            strong: true,
        }],
        payments: Vec::new(),
        note: "1234567890"
            .repeat(columns.div_ceil(10))
            .chars()
            .take(columns)
            .collect(),
        reward: String::new(),
        loyalty: String::new(),
        footer: "If this prints cleanly, the printer is ready.".to_owned(),
    }
}

/// One tender's share of a day at the counter.
#[derive(Debug, Clone, PartialEq)]
pub struct ChannelTotal {
    pub channel: String,
    pub count: i64,
    pub amount: Decimal,
}

/// A counter's day, already summed.
#[derive(Debug, Clone, PartialEq)]
pub struct CounterDay {
    /// `17 Oct 2026`.
    pub date_label: String,
    /// `All counters`, or the cashier the report is limited to.
    pub scope_label: String,
    pub printed_local: String,
    pub by_channel: Vec<ChannelTotal>,
    pub receipt_count: i64,
    pub cancelled_count: i64,
    pub cancelled_amount: Decimal,
    pub first_receipt: Option<String>,
    pub last_receipt: Option<String>,
}

impl CounterDay {
    /// The day's takings (cancelled receipts excluded).
    pub fn total(&self) -> Decimal {
        self.by_channel.iter().map(|c| c.amount).sum()
    }
}

fn row(label: impl Into<String>, value: impl Into<String>, strong: bool) -> ReportRow {
    ReportRow {
        label: label.into(),
        value: value.into(),
        section: None,
        strong,
    }
}

fn section(name: &str) -> ReportRow {
    ReportRow {
        label: String::new(),
        value: String::new(),
        section: Some(name.to_owned()),
        strong: false,
    }
}

/// The counter's day close: takings by tender (cash first, the drawer is counted against it),
/// cancellations, and the receipt-number range for the receipt book.
pub fn counter_report(
    day: &CounterDay,
    settings: &PrintSettings,
    institution_name: &str,
) -> (ReportHead, Vec<ReportRow>) {
    let head = ReportHead {
        store_name: store_name(settings, institution_name),
        address: settings.address.trim().to_owned(),
        kind: "COUNTER DAY CLOSE".to_owned(),
        meta: vec![
            day.date_label.clone(),
            day.scope_label.clone(),
            format!("Printed {}", day.printed_local),
        ],
        footer: "Count the cash drawer against the cash line.".to_owned(),
    };
    let mut channels = day.by_channel.clone();
    channels.sort_by_key(|c| (c.channel != "cash", c.channel.clone()));
    let mut rows = vec![section("Collections")];
    for c in &channels {
        rows.push(row(
            format!("{} ({})", channel_label(&c.channel), c.count),
            display_inr(c.amount),
            false,
        ));
    }
    if channels.is_empty() {
        rows.push(row("No receipts", display_inr(Decimal::ZERO), false));
    }
    rows.push(row(
        format!("Total ({})", day.receipt_count),
        display_inr(day.total()),
        true,
    ));
    rows.push(section("Cancelled"));
    rows.push(row(
        format!("Cancelled ({})", day.cancelled_count),
        display_inr(day.cancelled_amount),
        false,
    ));
    if let (Some(first), Some(last)) = (&day.first_receipt, &day.last_receipt) {
        rows.push(section("Receipts"));
        rows.push(row("First", first.clone(), false));
        rows.push(row("Last", last.clone(), false));
    }
    (head, rows)
}

const ONES: [&str; 20] = [
    "",
    "One",
    "Two",
    "Three",
    "Four",
    "Five",
    "Six",
    "Seven",
    "Eight",
    "Nine",
    "Ten",
    "Eleven",
    "Twelve",
    "Thirteen",
    "Fourteen",
    "Fifteen",
    "Sixteen",
    "Seventeen",
    "Eighteen",
    "Nineteen",
];
const TENS: [&str; 10] = [
    "", "", "Twenty", "Thirty", "Forty", "Fifty", "Sixty", "Seventy", "Eighty", "Ninety",
];

/// 1..=99 in words (`Forty-Two`).
fn two_digits(n: u64) -> String {
    let n = usize::try_from(n % 100).unwrap_or(0);
    if n < 20 {
        ONES[n].to_owned()
    } else if n % 10 == 0 {
        TENS[n / 10].to_owned()
    } else {
        format!("{}-{}", TENS[n / 10], ONES[n % 10])
    }
}

/// 1..=999 in words (`Four Hundred Five`).
fn three_digits(n: u64) -> String {
    let hundreds = n / 100;
    let rest = n % 100;
    let mut parts = Vec::new();
    if hundreds > 0 {
        parts.push(format!("{} Hundred", two_digits(hundreds)));
    }
    if rest > 0 {
        parts.push(two_digits(rest));
    }
    parts.join(" ")
}

/// A whole number in the Indian system: crore, lakh, thousand, hundred.
pub fn number_in_words_indian(n: u64) -> String {
    if n == 0 {
        return "Zero".to_owned();
    }
    let crore = n / 10_000_000;
    let lakh = (n / 100_000) % 100;
    let thousand = (n / 1_000) % 100;
    let rest = n % 1_000;
    let mut parts = Vec::new();
    if crore > 0 {
        // Above 99 crore the crore count itself is read in the Indian system.
        parts.push(format!("{} Crore", number_in_words_indian(crore)));
    }
    if lakh > 0 {
        parts.push(format!("{} Lakh", two_digits(lakh)));
    }
    if thousand > 0 {
        parts.push(format!("{} Thousand", two_digits(thousand)));
    }
    if rest > 0 {
        parts.push(three_digits(rest));
    }
    parts.join(" ")
}

/// `Rupees One Lakh Twenty-Five Thousand and Fifty Paise Only` (rounded to the paisa).
pub fn amount_in_words_inr(amount: Decimal) -> String {
    let abs = amount
        .abs()
        .round_dp_with_strategy(2, RoundingStrategy::MidpointAwayFromZero);
    let rupees = abs.trunc().to_u64().unwrap_or(0);
    let paise = ((abs - abs.trunc()) * Decimal::from(100))
        .round()
        .to_u64()
        .unwrap_or(0);
    let minus = if amount.is_sign_negative() && !abs.is_zero() {
        "Minus "
    } else {
        ""
    };
    match (rupees, paise) {
        (0, 0) => "Rupees Zero Only".to_owned(),
        (0, p) => format!("{minus}{} Paise Only", two_digits(p)),
        (r, 0) => format!("{minus}Rupees {} Only", number_in_words_indian(r)),
        (r, p) => format!(
            "{minus}Rupees {} and {} Paise Only",
            number_in_words_indian(r),
            two_digits(p)
        ),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn d(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap()
    }

    #[test]
    fn writes_amounts_the_indian_way() {
        assert_eq!(
            amount_in_words_inr(d("125000")),
            "Rupees One Lakh Twenty-Five Thousand Only"
        );
        assert_eq!(
            amount_in_words_inr(d("12345678.90")),
            "Rupees One Crore Twenty-Three Lakh Forty-Five Thousand Six Hundred Seventy-Eight \
             and Ninety Paise Only"
        );
        assert_eq!(amount_in_words_inr(d("0.50")), "Fifty Paise Only");
        assert_eq!(amount_in_words_inr(d("0")), "Rupees Zero Only");
        assert_eq!(amount_in_words_inr(d("100")), "Rupees One Hundred Only");
        assert_eq!(
            amount_in_words_inr(d("1005")),
            "Rupees One Thousand Five Only"
        );
        assert_eq!(
            amount_in_words_inr(d("90000")),
            "Rupees Ninety Thousand Only"
        );
        assert_eq!(number_in_words_indian(1_000_000_000), "One Hundred Crore");
        assert_eq!(
            amount_in_words_inr(d("10.005")),
            "Rupees Ten and One Paise Only"
        );
    }

    fn facts() -> ReceiptFacts {
        ReceiptFacts {
            receipt_number: "RCT-2026-00042".into(),
            payment_number: "PAY-2026-00042".into(),
            issued_local: "17 Oct 2026 · 11:42 AM".into(),
            student_name: Some("Ananya Kulkarni".into()),
            student_number: Some("2026BCS014".into()),
            channel: "cheque".into(),
            external_ref: Some("004512".into()),
            payer_name: None,
            amount: d("25500"),
            unapplied: d("500"),
            cancelled: false,
            cancel_reason: None,
            lines: vec![PaidLine {
                charge_number: "CHG-0091".into(),
                description: "Tuition fee, Term 1".into(),
                amount: d("25000"),
            }],
        }
    }

    #[test]
    fn a_fee_receipt_carries_the_counter_facts() {
        let doc = fee_receipt_doc(
            &facts(),
            &PrintSettings::default(),
            "Ahilyanagar College",
            false,
        );
        assert_eq!(doc.store_name, "Ahilyanagar College");
        assert_eq!(doc.kind.as_deref(), Some("FEE RECEIPT"));
        assert_eq!(doc.meta[0], "Receipt #RCT-2026-00042");
        assert!(doc
            .meta
            .contains(&"Student: Ananya Kulkarni (2026BCS014)".to_owned()));
        assert_eq!(doc.items[0].amount, "₹25,000.00");
        assert_eq!(doc.totals[0].label, "Held as advance");
        assert_eq!(doc.totals[1].value, "₹25,500.00");
        assert!(doc.totals[1].strong);
        assert_eq!(doc.payments[0].label, "Cheque 004512");
        assert_eq!(doc.note, "Rupees Twenty-Five Thousand Five Hundred Only");
        assert_eq!(doc.footer, DEFAULT_FOOTER);
        assert!(!doc.voided);
    }

    #[test]
    fn a_cancelled_reprint_says_so_twice() {
        let mut f = facts();
        f.cancelled = true;
        f.cancel_reason = Some("Cheque bounced".into());
        let settings = PrintSettings {
            title: "SPPU Affiliated College".into(),
            registrations: vec![
                Registration {
                    label: "PAN".into(),
                    number: "AAATS1234K".into(),
                },
                Registration {
                    label: "GSTIN".into(),
                    number: String::new(),
                },
            ],
            show_amount_in_words: false,
            ..PrintSettings::default()
        };
        let doc = fee_receipt_doc(&f, &settings, "Ignored", true);
        assert_eq!(doc.store_name, "SPPU Affiliated College");
        assert_eq!(doc.registrations, vec!["PAN AAATS1234K".to_owned()]);
        assert!(doc.voided);
        assert_eq!(doc.kind.as_deref(), Some("DUPLICATE COPY"));
        assert!(doc.meta.contains(&"CANCELLED".to_owned()));
        assert_eq!(doc.note, "Cancelled: Cheque bounced");
    }

    #[test]
    fn the_day_close_puts_cash_first() {
        let day = CounterDay {
            date_label: "17 Oct 2026".into(),
            scope_label: "All counters".into(),
            printed_local: "17 Oct 2026 · 6:05 PM".into(),
            by_channel: vec![
                ChannelTotal {
                    channel: "cheque".into(),
                    count: 2,
                    amount: d("50000"),
                },
                ChannelTotal {
                    channel: "cash".into(),
                    count: 5,
                    amount: d("12500"),
                },
            ],
            receipt_count: 7,
            cancelled_count: 1,
            cancelled_amount: d("2000"),
            first_receipt: Some("RCT-1".into()),
            last_receipt: Some("RCT-8".into()),
        };
        let (head, rows) = counter_report(&day, &PrintSettings::default(), "College");
        assert_eq!(head.kind, "COUNTER DAY CLOSE");
        assert_eq!(rows[1].label, "Cash (5)");
        assert_eq!(rows[2].label, "Cheque (2)");
        assert_eq!(rows[3].label, "Total (7)");
        assert_eq!(rows[3].value, "₹62,500.00");
        assert!(rows.iter().any(|r| r.label == "Last" && r.value == "RCT-8"));
    }
}
