//! Reading the fields of an Indian receipt out of plain OCR lines (SutraERP; not part of the
//! Everything port). Everything asks a vision model for JSON; PaddleOCR returns text lines, so
//! this turns those lines into the same loose JSON shape and hands it to
//! [`super::rules::normalize_ocr_result`]. Heuristics, never authoritative: the claimant
//! confirms every field.
//!
//! What it looks for: the merchant (first wordy line that is not a heading), the bill date
//! (day first, as Indian receipts print it), the payable total (`Grand Total`, `Net Amount`,
//! `Total`…, never `Sub Total`), GST (`CGST` + `SGST` or `IGST`), the merchant's GSTIN
//! (checksum verified), the payment mode (UPI apps, cash, card) and a category hint.

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::OnceLock;

use super::rules::{parse_amount_str, parse_loose_date};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;

/// One line PaddleOCR read, top to bottom.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OcrLine {
    pub text: String,
    /// 0…1 recognition confidence.
    #[serde(default)]
    pub confidence: f64,
}

fn re(p: &str) -> Regex {
    Regex::new(p).unwrap_or_else(|_| unreachable!())
}

struct Patterns {
    amount: Regex,
    gstin: Regex,
    date_num: Regex,
    date_iso: Regex,
    date_mon: Regex,
    card: Regex,
}

fn pats() -> &'static Patterns {
    static P: OnceLock<Patterns> = OnceLock::new();
    P.get_or_init(|| Patterns {
        amount: re(r"(?:₹|rs\.?|inr)?\s*(\d{1,3}(?:,\d{2,3})+(?:\.\d{1,2})?|\d+(?:\.\d{1,2})?)(\s*%)?"),
        gstin: re(r"\b(\d{2}[A-Z]{5}\d{4}[A-Z][1-9A-Z]Z[0-9A-Z])\b"),
        date_num: re(r"\b(\d{1,2})[-/.](\d{1,2})[-/.](\d{2,4})\b"),
        date_iso: re(r"\b(\d{4})[-/.](\d{1,2})[-/.](\d{1,2})\b"),
        date_mon: re(r"(?i)\b(\d{1,2})[\s-]*(jan|feb|mar|apr|may|jun|jul|aug|sep|oct|nov|dec)[a-z]*[\s,-]*(\d{2,4})\b"),
        card: re(r"(?i)(?:x{2,}|\*{2,})[\s-]*(\d{4})\b"),
    })
}

/// GSTIN checksum (base-36, weights 1/2 alternating): the 15th character. Lives in the kernel
/// since ERP_EXTENSIONS 5D (Finance validates the institution's GSTIN with the same rule).
pub use sutra_kernel::india::gstin_checksum_valid as gstin_valid;

/// Money amounts on a line (percentages skipped), left to right.
fn amounts(line: &str) -> Vec<Decimal> {
    let lower = line.to_lowercase();
    pats()
        .amount
        .captures_iter(&lower)
        .filter(|c| c.get(2).is_none())
        .filter_map(|c| c.get(1).and_then(|m| parse_amount_str(m.as_str())))
        .collect()
}

fn has_any(line: &str, words: &[&str]) -> bool {
    words.iter().any(|w| line.contains(w))
}

const HEADINGS: [&str; 14] = [
    "tax invoice",
    "invoice",
    "receipt",
    "bill",
    "cash memo",
    "gstin",
    "gst no",
    "phone",
    "ph:",
    "mob",
    "tel",
    "date",
    "welcome",
    "estimate",
];

fn merchant(lines: &[OcrLine]) -> Option<String> {
    lines
        .iter()
        .take(6)
        .map(|l| l.text.trim())
        .find(|t| {
            let lower = t.to_lowercase();
            let letters = t.chars().filter(|c| c.is_alphabetic()).count();
            letters >= 3
                && letters * 2 >= t.chars().filter(|c| !c.is_whitespace()).count()
                && !HEADINGS.iter().any(|h| lower.starts_with(h))
                && pats().gstin.find(&t.to_uppercase()).is_none()
        })
        .map(str::to_owned)
}

fn bill_date(text: &str, now_year: i32) -> Option<String> {
    let p = pats();
    if let Some(c) = p.date_iso.captures(text) {
        return parse_loose_date(&c[0], true, now_year);
    }
    if let Some(c) = p.date_num.captures(text) {
        // Indian receipts print the day first.
        return parse_loose_date(&c[0].replace(['.', '-'], "/"), true, now_year);
    }
    if let Some(c) = p.date_mon.captures(text) {
        let y = &c[3];
        let y = if y.len() == 2 {
            format!("20{y}")
        } else {
            y.to_owned()
        };
        return parse_loose_date(&format!("{} {} {y}", &c[1], &c[2]), true, now_year);
    }
    None
}

const TOTAL_KEYS: [&str; 9] = [
    "grand total",
    "net amount",
    "net payable",
    "amount payable",
    "total payable",
    "bill amount",
    "total amount",
    "net total",
    "total",
];

fn total(lines: &[OcrLine]) -> Option<Decimal> {
    let lowered: Vec<String> = lines.iter().map(|l| l.text.to_lowercase()).collect();
    for key in TOTAL_KEYS {
        for (i, l) in lowered.iter().enumerate().rev() {
            if !l.contains(key)
                || has_any(
                    l,
                    &[
                        "sub total",
                        "subtotal",
                        "sub-total",
                        "total qty",
                        "total items",
                        "total quantity",
                        "total tax",
                        "total gst",
                        "total discount",
                        "total savings",
                    ],
                )
            {
                continue;
            }
            if let Some(a) = amounts(l).last() {
                return Some(*a);
            }
            if let Some(a) = lowered.get(i + 1).and_then(|n| amounts(n).last().copied()) {
                return Some(a);
            }
        }
    }
    lowered
        .iter()
        .flat_map(|l| amounts(l))
        .filter(|a| *a < Decimal::from(10_000_000))
        .fold(None, |m: Option<Decimal>, a| Some(m.map_or(a, |m| m.max(a))))
}

fn gst(lines: &[OcrLine]) -> Option<Decimal> {
    let mut parts = Decimal::ZERO;
    let mut found = false;
    for l in lines {
        let lower = l.text.to_lowercase();
        let is_tax = has_any(&lower, &["cgst", "sgst", "utgst", "igst"])
            || (lower.contains("gst") && !lower.contains("gstin") && !lower.contains("gst no"));
        if !is_tax || lower.contains("total") && !has_any(&lower, &["cgst", "sgst", "igst"]) {
            continue;
        }
        if let Some(a) = amounts(&lower).last() {
            parts += a;
            found = true;
        }
    }
    found.then(|| super::rules::round2(parts))
}

fn payment_method(text: &str) -> Option<&'static str> {
    let t = text.to_lowercase();
    if has_any(
        &t,
        &["upi", "gpay", "google pay", "phonepe", "paytm", "bhim"],
    ) {
        Some("Mobile payment")
    } else if has_any(&t, &["neft", "rtgs", "imps", "bank transfer"]) {
        Some("Bank transfer")
    } else if has_any(&t, &["visa", "mastercard", "rupay", "card", "amex"]) {
        Some("Card")
    } else if has_any(&t, &["cash"]) {
        Some("Cash")
    } else {
        None
    }
}

fn category_hint(text: &str) -> Option<&'static str> {
    let t = text.to_lowercase();
    let rules: [(&[&str], &str); 6] = [
        (
            &[
                "petrol",
                "diesel",
                "fuel",
                "hpcl",
                "bpcl",
                "indian oil",
                "iocl",
            ],
            "Fuel",
        ),
        (
            &[
                "uber",
                "ola ",
                "rapido",
                "taxi",
                "cab",
                "auto rickshaw",
                "metro",
            ],
            "Ground transport",
        ),
        (
            &[
                "irctc",
                "railway",
                "airline",
                "indigo",
                "air india",
                "flight",
                "boarding",
            ],
            "Travel",
        ),
        // Meals before lodging: an Indian "Hotel …" bill is usually a restaurant.
        (
            &[
                "restaurant",
                "cafe",
                "café",
                "food",
                "dhaba",
                "canteen",
                "meals",
                "thali",
            ],
            "Meals",
        ),
        (
            &["hotel", "lodge", "guest house", "room tariff", "check-in"],
            "Lodging",
        ),
        (
            &["stationery", "xerox", "printing", "photocopy", "books"],
            "Office supplies",
        ),
    ];
    rules
        .iter()
        .find(|(words, _)| has_any(&t, words))
        .map(|(_, c)| *c)
}

/// What the lines say, in the loose JSON shape `normalize_ocr_result` reads, plus `gstin`.
pub fn fields_from_lines(lines: &[OcrLine], today: &str) -> Value {
    let now_year: i32 = today.get(0..4).and_then(|y| y.parse().ok()).unwrap_or(2026);
    let text = lines
        .iter()
        .map(|l| l.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let upper = text.to_uppercase();
    let gstin = pats()
        .gstin
        .captures_iter(&upper)
        .map(|c| c[1].to_owned())
        .find(|g| gstin_valid(g));
    let total = total(lines);
    let date = lines.iter().find_map(|l| bill_date(&l.text, now_year));
    let mean = if lines.is_empty() {
        0.0
    } else {
        lines
            .iter()
            .map(|l| l.confidence.clamp(0.0, 1.0))
            .sum::<f64>()
            / lines.len() as f64
    };
    let found = [total.is_some(), date.is_some(), merchant(lines).is_some()]
        .iter()
        .filter(|b| **b)
        .count();
    let confidence = super::rules::round2_f64(mean * (0.4 + 0.2 * found as f64));
    // The loose shape carries amounts as JSON numbers, as an OCR reader would; the exact value
    // comes back through `parse_amount` (shortest decimal form). `to_f64` is total.
    let num = |d: Option<Decimal>| d.and_then(|x| x.to_f64());
    let lower = text.to_lowercase();
    let currency =
        (text.contains('₹') || lower.contains("rs.") || lower.contains("inr") || gstin.is_some())
            .then_some("INR");
    json!({
        "merchant": merchant(lines),
        "date": date,
        "total": num(total),
        "tax": num(gst(lines)),
        "currency": currency,
        "category": category_hint(&text),
        "paymentMethod": payment_method(&text),
        "cardLast4": pats().card.captures(&text).map(|c| c[1].to_owned()),
        "gstin": gstin,
        "confidence": confidence,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &str) -> Vec<OcrLine> {
        text.lines()
            .map(|t| OcrLine {
                text: t.to_owned(),
                confidence: 0.95,
            })
            .collect()
    }

    #[test]
    fn gstin_checksum() {
        assert!(gstin_valid("27AAPFU0939F1ZV"));
        assert!(!gstin_valid("27AAPFU0939F1ZX"));
    }

    #[test]
    fn reads_a_restaurant_bill() {
        let l = lines(
            "TAX INVOICE\nHotel Shivneri Pure Veg Restaurant\nGSTIN: 27AAPFU0939F1ZV\n\
             Date: 05/03/2026  Time 13:42\nVeg Thali 2 x 180.00   360.00\nSub Total 360.00\n\
             CGST @2.5% 9.00\nSGST @2.5% 9.00\nGrand Total ₹ 378.00\nPaid by UPI",
        );
        let f = fields_from_lines(&l, "2026-10-17");
        assert_eq!(f["merchant"], "Hotel Shivneri Pure Veg Restaurant");
        assert_eq!(f["date"], "2026-03-05");
        assert_eq!(f["total"], 378.0);
        assert_eq!(f["tax"], 18.0);
        assert_eq!(f["currency"], "INR");
        assert_eq!(f["gstin"], "27AAPFU0939F1ZV");
        assert_eq!(f["paymentMethod"], "Mobile payment");
        assert_eq!(f["category"], "Meals");
    }

    #[test]
    fn falls_back_to_the_largest_amount() {
        let l = lines("Sharma Xerox Centre\n12-Aug-26\nPrints 40\nBinding 60\nRs. 100");
        let f = fields_from_lines(&l, "2026-10-17");
        assert_eq!(f["date"], "2026-08-12");
        assert_eq!(f["total"], 100.0);
        assert_eq!(f["category"], "Office supplies");
    }
}
