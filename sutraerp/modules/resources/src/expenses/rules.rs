//! Pure rules of staff expense claims, ported from Everything's `expense/src/lib/`
//! (`money.js` `parseAmount` / `totalsByCurrency`, `util.js` `normalizeText` / `tokens` /
//! `round2`, `dates.js` `parseLooseDate`, `currencies.js` `resolveCurrency`, `ocr.js`
//! `extractJson` / `matchCategory` / `normalizeOcrResult`, `reports.js` lifecycle). MIT,
//! HeyPuter/everything @ 06802a1. Golden vectors: `tools/golden/specs/expense-claims.mjs`.
//!
//! Amounts here are `f64` exactly as the original computes them (two-decimal `round2`); the HTTP
//! layer stores them as `Decimal` with two places. Additions for SutraERP (policy checks, GST)
//! live in [`policy_flags`] and are not part of the golden port.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use unicode_normalization::UnicodeNormalization;

// ------------------------------------------------------------------ util.js

/// `Math.round(x)`: rounds half up (towards +∞).
fn js_round(x: f64) -> f64 {
    (x + 0.5).floor()
}

/// `round2(n)`: `Math.round((n + EPSILON) * 100) / 100`.
pub fn round2(n: f64) -> f64 {
    js_round((n + f64::EPSILON) * 100.0) / 100.0
}

/// JavaScript truthiness of a JSON value.
pub fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0 && !f.is_nan()),
        Value::String(s) => !s.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

fn js_number_string(f: f64) -> String {
    if f.is_finite() && f.fract() == 0.0 && f.abs() < 1e21 {
        format!("{}", f as i64)
    } else {
        format!("{f}")
    }
}

/// `String(v)` of a JSON value.
pub fn js_string(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.as_f64().map(js_number_string).unwrap_or_default(),
        Value::String(s) => s.clone(),
        Value::Array(a) => a
            .iter()
            .map(|x| match x {
                Value::Null => String::new(),
                other => js_string(other),
            })
            .collect::<Vec<_>>()
            .join(","),
        Value::Object(_) => "[object Object]".into(),
    }
}

/// `String(v || '')`.
fn js_string_or_empty(v: &Value) -> String {
    if truthy(v) {
        js_string(v)
    } else {
        String::new()
    }
}

/// `normalizeText(s)`: lowercase, strip accents, punctuation to spaces, collapse whitespace.
pub fn normalize_text(s: &str) -> String {
    let lowered = s.to_lowercase();
    let mut out = String::with_capacity(lowered.len());
    for c in lowered.nfkd() {
        if ('\u{0300}'..='\u{036f}').contains(&c) {
            continue;
        }
        if c.is_ascii_lowercase() || c.is_ascii_digit() || c.is_whitespace() {
            out.push(c);
        } else {
            out.push(' ');
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

const STOP: [&str; 18] = [
    "the", "and", "inc", "llc", "ltd", "co", "corp", "of", "a", "an", "pos", "card", "purchase",
    "payment", "debit", "credit", "www", "com",
];

/// `tokens(s)`: words worth comparing (no stop words, no bare numbers, longer than one char).
pub fn tokens(s: &str) -> Vec<String> {
    normalize_text(s)
        .split(' ')
        .filter(|t| {
            t.chars().count() > 1 && !STOP.contains(t) && !t.chars().all(|c| c.is_ascii_digit())
        })
        .map(str::to_owned)
        .collect()
}

// ------------------------------------------------------------------ money.js

/// `parseAmount(input)` of a string: "1,234.56", "1.234,56", "₹ 12", "(12.34)", "12.34-".
pub fn parse_amount_str(input: &str) -> Option<f64> {
    let mut s = input.trim().to_owned();
    if s.is_empty() {
        return None;
    }
    let mut negative = false;
    if s.len() >= 2 && s.starts_with('(') && s.ends_with(')') {
        negative = true;
        s = s[1..s.len() - 1].trim().to_owned();
    }
    let trimmed_end = s.trim_end();
    if let Some(stripped) = trimmed_end.strip_suffix('-') {
        negative = true;
        s = stripped.to_owned();
    }
    // A minus anywhere before the first digit ("-₹12", "₹-12", "INR -12").
    let before_digit: String = s.chars().take_while(|c| !c.is_ascii_digit()).collect();
    if before_digit.contains('-') {
        negative = true;
    }
    let mut s: String = s
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == '.' || *c == ',')
        .collect();
    if s.is_empty() {
        return None;
    }
    let last_dot = s.rfind('.');
    let last_comma = s.rfind(',');
    match (last_dot, last_comma) {
        (Some(d), Some(c)) => {
            // Whichever separator comes last is the decimal mark.
            if c > d {
                s = s.replace('.', "").replacen(',', ".", 1);
            } else {
                s = s.replace(',', "");
            }
        }
        (None, Some(c)) => {
            let commas = s.matches(',').count();
            let after = s.len() - c - 1;
            if commas == 1 && after > 0 && after < 3 {
                s = s.replacen(',', ".", 1);
            } else {
                s = s.replace(',', "");
            }
        }
        (Some(_), None) => {
            if s.matches('.').count() > 1 {
                let last = s.rfind('.').unwrap_or(0);
                s = s
                    .char_indices()
                    .filter(|(i, c)| *c != '.' || *i == last)
                    .map(|(_, c)| c)
                    .collect();
            }
        }
        (None, None) => {}
    }
    // JS `Number("")` is 0: only commas ("," or "₹ ,") leave nothing behind.
    let n: f64 = if s.is_empty() { 0.0 } else { s.parse().ok()? };
    if !n.is_finite() {
        return None;
    }
    Some(round2(if negative { -n } else { n }))
}

/// `parseAmount(input)` of any JSON value (numbers pass through `round2`).
pub fn parse_amount(input: &Value) -> Option<f64> {
    match input {
        Value::Number(n) => n.as_f64().filter(|f| f.is_finite()).map(round2),
        Value::Null => None,
        other => parse_amount_str(&js_string(other)),
    }
}

/// `totalsByCurrency`: currency → total, in first-seen order.
pub fn totals_by_currency<'a>(
    items: impl IntoIterator<Item = (&'a str, f64)>,
) -> Vec<(String, f64)> {
    let mut out: Vec<(String, f64)> = Vec::new();
    for (cur, amount) in items {
        let amount = if amount.is_nan() { 0.0 } else { amount };
        if let Some(slot) = out.iter_mut().find(|(c, _)| c == cur) {
            slot.1 = round2(slot.1 + amount);
        } else {
            out.push((cur.to_owned(), round2(amount)));
        }
    }
    out
}

// ------------------------------------------------------------------ dates.js

const MONTHS: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];

fn month_index(name: &str) -> u32 {
    let head: String = name.chars().take(3).collect::<String>().to_lowercase();
    MONTHS
        .iter()
        .position(|m| *m == head)
        .map_or(0, |i| i as u32 + 1)
}

fn build_date(y: i64, mo: i64, d: i64) -> Option<String> {
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || !(1900..=2200).contains(&y) {
        return None;
    }
    let date = chrono::NaiveDate::from_ymd_opt(y as i32, mo as u32, d as u32)?;
    Some(date.format("%Y-%m-%d").to_string())
}

fn digits(s: &str) -> Option<i64> {
    s.parse().ok()
}

/// `parseLooseDate(input, { dayFirst, now })` → `YYYY-MM-DD`. `now_year` is the year a date
/// without one assumes.
pub fn parse_loose_date(input: &str, day_first: bool, now_year: i32) -> Option<String> {
    use std::sync::OnceLock;
    static RES: OnceLock<[regex::Regex; 5]> = OnceLock::new();
    let res = RES.get_or_init(|| {
        let r = |p: &str| regex::Regex::new(p).unwrap_or_else(|_| unreachable!());
        [
            r(r"^(\d{4})[-/.](\d{1,2})[-/.](\d{1,2})(?:[T\s].*)?$"),
            r(r"^(\d{1,2})[-/.](\d{1,2})[-/.](\d{2,4})(?:[T\s].*)?$"),
            r(r"(?i)^(\d{1,2})(?:st|nd|rd|th)?[\s-]+([a-z]{3,})\.?[\s,-]*(\d{4})?$"),
            r(r"(?i)^([a-z]{3,})\.?\s+(\d{1,2})(?:st|nd|rd|th)?[\s,]*(\d{4})?$"),
            r(r"^(\d{4})(\d{2})(\d{2})$"),
        ]
    });
    let s = input.trim();
    if s.is_empty() {
        return None;
    }
    let g = |c: &regex::Captures, i: usize| c.get(i).map(|m| m.as_str().to_owned());
    if let Some(c) = res[0].captures(s) {
        return build_date(
            digits(&g(&c, 1)?)?,
            digits(&g(&c, 2)?)?,
            digits(&g(&c, 3)?)?,
        );
    }
    if let Some(c) = res[1].captures(s) {
        let a = digits(&g(&c, 1)?)?;
        let b = digits(&g(&c, 2)?)?;
        let mut y = digits(&g(&c, 3)?)?;
        if y < 100 {
            y += if y < 70 { 2000 } else { 1900 };
        }
        let (day, month) = if a > 12 && b <= 12 {
            (a, b)
        } else if b > 12 && a <= 12 {
            (b, a)
        } else if day_first {
            (a, b)
        } else {
            (b, a)
        };
        return build_date(y, month, day);
    }
    if let Some(c) = res[2].captures(s) {
        let month = month_index(&g(&c, 2)?);
        if month > 0 {
            let y = g(&c, 3)
                .and_then(|y| digits(&y))
                .unwrap_or(i64::from(now_year));
            return build_date(y, i64::from(month), digits(&g(&c, 1)?)?);
        }
    }
    if let Some(c) = res[3].captures(s) {
        let month = month_index(&g(&c, 1)?);
        if month > 0 {
            let y = g(&c, 3)
                .and_then(|y| digits(&y))
                .unwrap_or(i64::from(now_year));
            return build_date(y, i64::from(month), digits(&g(&c, 2)?)?);
        }
    }
    if let Some(c) = res[4].captures(s) {
        return build_date(
            digits(&g(&c, 1)?)?,
            digits(&g(&c, 2)?)?,
            digits(&g(&c, 3)?)?,
        );
    }
    None
}

// ------------------------------------------------------------------ currencies.js

const CURRENCY_CODES: [&str; 36] = [
    "USD", "EUR", "GBP", "CAD", "AUD", "NZD", "JPY", "CNY", "INR", "CHF", "SEK", "NOK", "DKK",
    "PLN", "CZK", "HUF", "TRY", "BRL", "MXN", "ARS", "CLP", "ZAR", "AED", "SAR", "ILS", "SGD",
    "HKD", "KRW", "THB", "IDR", "PHP", "MYR", "VND", "NGN", "KES", "EGP",
];

const SYMBOLS: [(&str, &str); 15] = [
    ("$", "USD"),
    ("€", "EUR"),
    ("£", "GBP"),
    ("¥", "JPY"),
    ("₹", "INR"),
    ("₩", "KRW"),
    ("₺", "TRY"),
    ("R$", "BRL"),
    ("zł", "PLN"),
    ("Kč", "CZK"),
    ("CHF", "CHF"),
    ("₪", "ILS"),
    ("₫", "VND"),
    ("₦", "NGN"),
    ("฿", "THB"),
];

/// Is this an ISO 4217 code the module knows (Everything's list)?
pub fn is_currency(code: &str) -> bool {
    CURRENCY_CODES.contains(&code)
}

/// `resolveCurrency(hint, fallback)`: an ISO code or a symbol to an ISO code.
pub fn resolve_currency(hint: &Value) -> Option<String> {
    if !truthy(hint) {
        return None;
    }
    let s = js_string(hint);
    let s = s.trim();
    let up = s.to_uppercase();
    if CURRENCY_CODES.contains(&up.as_str()) {
        return Some(up);
    }
    SYMBOLS
        .iter()
        .find(|(sym, _)| s.contains(sym))
        .map(|(_, code)| (*code).to_owned())
}

// ------------------------------------------------------------------ ocr.js

/// `extractJson(text)`: the first balanced JSON object in free text (models wrap JSON in prose
/// or code fences). An object passes through.
pub fn extract_json(text: &Value) -> Option<Value> {
    match text {
        Value::Null => None,
        Value::Object(_) | Value::Array(_) => Some(text.clone()),
        Value::String(s) => extract_json_str(s),
        other => extract_json_str(&js_string(other)),
    }
}

/// [`extract_json`] of a string.
pub fn extract_json_str(text: &str) -> Option<Value> {
    use std::sync::OnceLock;
    static FENCES: OnceLock<(regex::Regex, regex::Regex)> = OnceLock::new();
    let (open, close) = FENCES.get_or_init(|| {
        (
            regex::Regex::new(r"(?i)^```(?:json)?\s*").unwrap_or_else(|_| unreachable!()),
            regex::Regex::new(r"\s*```$").unwrap_or_else(|_| unreachable!()),
        )
    });
    let s = text.trim();
    let s = open.replace(s, "");
    let s = close.replace(&s, "").into_owned();
    let start = s.find('{')?;
    let bytes = s.as_bytes();
    let mut depth = 0i32;
    let mut in_str = false;
    let mut i = start;
    while i < bytes.len() {
        let c = bytes[i];
        if in_str {
            if c == b'\\' {
                i += 1;
            } else if c == b'"' {
                in_str = false;
            }
        } else if c == b'"' {
            in_str = true;
        } else if c == b'{' {
            depth += 1;
        } else if c == b'}' {
            depth -= 1;
            if depth == 0 {
                return serde_json::from_str(&s[start..=i]).ok();
            }
        }
        i += 1;
    }
    None
}

/// An expense category: id, name and (SutraERP) an optional per-expense limit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Category {
    pub id: String,
    pub name: String,
}

/// `matchCategory(label, categories)`: exact normalised name, else the best token overlap of at
/// least one half.
pub fn match_category<'a>(label: &Value, categories: &'a [Category]) -> Option<&'a Category> {
    if !truthy(label) || categories.is_empty() {
        return None;
    }
    let label = js_string(label);
    let want = normalize_text(&label);
    if want.is_empty() {
        return None;
    }
    if let Some(exact) = categories.iter().find(|c| normalize_text(&c.name) == want) {
        return Some(exact);
    }
    let want_tokens = tokens(&label);
    let mut best = None;
    let mut best_score = 0.0f64;
    for c in categories {
        let ct = tokens(&c.name);
        let hit = ct
            .iter()
            .filter(|t| {
                want_tokens.contains(t)
                    || want_tokens
                        .iter()
                        .any(|w| w.starts_with(t.as_str()) || t.starts_with(w.as_str()))
            })
            .count();
        let score = hit as f64 / ct.len().max(1) as f64;
        if score > best_score {
            best_score = score;
            best = Some(c);
        }
    }
    if best_score >= 0.5 {
        best
    } else {
        None
    }
}

/// Payment methods a receipt can name.
pub const PAYMENT_METHODS: [&str; 5] = ["Card", "Cash", "Bank transfer", "Mobile payment", "Other"];

/// A receipt line item read by OCR.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OcrItem {
    pub name: String,
    pub amount: Option<f64>,
}

/// What a receipt read pre-fills (`normalizeOcrResult`); the claimant confirms every field.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OcrFields {
    pub merchant: Option<String>,
    pub date: Option<String>,
    pub total: Option<f64>,
    pub tax: Option<f64>,
    pub tip: Option<f64>,
    pub currency: String,
    pub category_id: Option<String>,
    pub payment_method: Option<String>,
    pub card_last4: Option<String>,
    pub items: Vec<OcrItem>,
    pub notes: Option<String>,
    /// 0…1.
    pub confidence: f64,
}

/// `Number(v)` of a JSON value (missing → NaN).
fn js_number(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => f64::from(u8::from(*b)),
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => {
            let t = s.trim();
            if t.is_empty() {
                0.0
            } else if t
                .chars()
                .all(|c| c.is_ascii_digit() || matches!(c, '.' | '-' | '+' | 'e' | 'E'))
            {
                t.parse().unwrap_or(f64::NAN)
            } else {
                f64::NAN
            }
        }
        Some(_) => f64::NAN,
    }
}

/// `a ?? b`: the first value that is present and not null.
fn coalesce<'a>(data: &'a Map<String, Value>, keys: &[&str]) -> &'a Value {
    keys.iter()
        .filter_map(|k| data.get(*k))
        .find(|v| !v.is_null())
        .unwrap_or(&Value::Null)
}

fn take_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// `normalizeOcrResult(raw, { categories, baseCurrency, now })`: whatever the reader returned,
/// in the shape of the expense form. Never fails; missing pieces come back `None`. `today` is
/// the business date (`YYYY-MM-DD`): a receipt cannot be from the future.
pub fn normalize_ocr_result(
    raw: &Value,
    categories: &[Category],
    base_currency: &str,
    today: &str,
) -> OcrFields {
    let empty = Map::new();
    let parsed = extract_json(raw);
    let data = parsed.as_ref().and_then(Value::as_object).unwrap_or(&empty);
    let get = |k: &str| data.get(k).unwrap_or(&Value::Null);
    let total = parse_amount(coalesce(data, &["total", "amount", "grand_total"]));
    let now_year: i32 = today.get(0..4).and_then(|y| y.parse().ok()).unwrap_or(2026);
    let mut date = parse_loose_date(&js_string_or_empty(get("date")), false, now_year);
    if date.as_deref().is_some_and(|d| d > today) {
        date = None;
    }
    let currency = resolve_currency(get("currency")).unwrap_or_else(|| base_currency.to_owned());
    let category_id = match_category(get("category"), categories).map(|c| c.id.clone());
    let items = match get("items") {
        Value::Array(list) => list
            .iter()
            .map(|it| {
                let obj = it.as_object().unwrap_or(&empty);
                let name = js_string_or_nullish(coalesce(obj, &["name", "description"]));
                OcrItem {
                    name: name.trim().to_owned(),
                    amount: parse_amount(coalesce(obj, &["amount", "price"])),
                }
            })
            .filter(|it| !it.name.is_empty())
            .take(25)
            .collect(),
        _ => Vec::new(),
    };
    let pm = get("paymentMethod");
    let pm_norm = normalize_text(&js_string_or_empty(pm));
    let payment_method = PAYMENT_METHODS
        .iter()
        .find(|m| normalize_text(m) == pm_norm)
        .map(|m| (*m).to_owned())
        .or_else(|| {
            let s = js_string(pm).to_lowercase();
            (truthy(pm)
                && ["card", "visa", "master", "amex", "debit", "credit"]
                    .iter()
                    .any(|w| s.contains(w)))
            .then(|| "Card".to_owned())
        });
    let mut confidence = js_number(data.get("confidence"));
    if !confidence.is_finite() {
        confidence = if total.is_some() && date.is_some() {
            0.6
        } else {
            0.3
        };
    }
    let confidence = confidence.clamp(0.0, 1.0);
    let text_field = |k: &str, n: usize| {
        let v = get(k);
        truthy(v).then(|| take_chars(js_string(v).trim(), n))
    };
    OcrFields {
        merchant: text_field("merchant", 120),
        date,
        total: total.filter(|t| *t >= 0.0),
        tax: parse_amount(get("tax")),
        tip: parse_amount(get("tip")),
        currency,
        category_id,
        payment_method,
        card_last4: {
            let v = get("cardLast4");
            if truthy(v) {
                let d: String = js_string(v).chars().filter(char::is_ascii_digit).collect();
                let tail: String = d
                    .chars()
                    .skip(d.chars().count().saturating_sub(4))
                    .collect();
                (!tail.is_empty()).then_some(tail)
            } else {
                None
            }
        },
        items,
        notes: text_field("notes", 300),
        confidence,
    }
}

/// `String(x ?? '')`.
fn js_string_or_nullish(v: &Value) -> String {
    if v.is_null() {
        String::new()
    } else {
        js_string(v)
    }
}

// ------------------------------------------------------------------ reports.js

/// Claim (Everything: expense report) statuses, in order.
pub const CLAIM_STATUSES: [&str; 4] = ["draft", "submitted", "approved", "reimbursed"];

fn status_index(s: &str) -> Option<usize> {
    CLAIM_STATUSES.iter().position(|x| *x == s)
}

/// `nextStatus(status)`: the forward step, or `None` at the end of the claim's life.
pub fn next_status(status: &str) -> Option<&'static str> {
    let i = status_index(status)?;
    CLAIM_STATUSES.get(i + 1).copied()
}

/// `canTransition(from, to)`: one step forward, or submitted → draft (withdraw; SutraERP also
/// sends a rejected claim back to draft through this edge).
pub fn can_transition(from: &str, to: &str) -> bool {
    match (status_index(from), status_index(to)) {
        (Some(a), Some(b)) => b == a + 1 || (from == "submitted" && to == "draft"),
        _ => false,
    }
}

/// `isEditable(report)`: lines are added or removed only while the claim is a draft.
pub fn is_editable(status: Option<&str>) -> bool {
    status.is_none_or(|s| s == "draft")
}

/// `applyTransition`'s stamp column for the new status (`draft` clears `submitted_at`).
pub fn stamp_for(to: &str) -> Option<&'static str> {
    match to {
        "submitted" => Some("submitted_at"),
        "approved" => Some("approved_at"),
        "reimbursed" => Some("reimbursed_at"),
        _ => None,
    }
}

/// One expense line as the claim totals see it.
#[derive(Debug, Clone, PartialEq)]
pub struct LineFacts {
    pub amount: f64,
    pub currency: String,
    pub has_receipt: bool,
    pub reconciled: bool,
}

/// `reportTotals`.
#[derive(Debug, Clone, PartialEq)]
pub struct ClaimTotals {
    pub count: usize,
    pub by_currency: Vec<(String, f64)>,
    pub with_receipt: usize,
    pub reconciled: usize,
}

/// `reportTotals(report, expenses)` of the claim's lines.
pub fn claim_totals(lines: &[LineFacts]) -> ClaimTotals {
    ClaimTotals {
        count: lines.len(),
        by_currency: totals_by_currency(lines.iter().map(|l| (l.currency.as_str(), l.amount))),
        with_receipt: lines.iter().filter(|l| l.has_receipt).count(),
        reconciled: lines.iter().filter(|l| l.reconciled).count(),
    }
}

// ------------------------------------------------------------------ SutraERP policy

/// A category's policy (SutraERP; not in the original).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CategoryPolicy {
    /// Most one expense of this category may claim.
    pub limit: Option<f64>,
    /// A receipt is required above this amount (the institution default when `None`).
    pub receipt_above: Option<f64>,
}

/// One expense as the policy checks see it.
#[derive(Debug, Clone, PartialEq)]
pub struct PolicyInput<'a> {
    pub amount: f64,
    pub date: &'a str,
    pub has_receipt: bool,
    pub category: Option<&'a CategoryPolicy>,
    pub category_known: bool,
}

/// Policy findings shown beside an expense and on the approver's screen. They never block a
/// submission by themselves except `receipt_missing` and `too_old` (see [`blocking`]).
pub fn policy_flags(
    e: &PolicyInput<'_>,
    receipt_above: f64,
    max_age_days: i64,
    today: &str,
) -> Vec<&'static str> {
    let mut out = Vec::new();
    if !e.category_known {
        out.push("uncategorised");
    }
    if let Some(limit) = e.category.and_then(|c| c.limit) {
        if e.amount > limit + 1e-9 {
            out.push("over_limit");
        }
    }
    let threshold = e
        .category
        .and_then(|c| c.receipt_above)
        .unwrap_or(receipt_above);
    if !e.has_receipt && e.amount > threshold + 1e-9 {
        out.push("receipt_missing");
    }
    if let (Ok(d), Ok(t)) = (
        chrono::NaiveDate::parse_from_str(e.date, "%Y-%m-%d"),
        chrono::NaiveDate::parse_from_str(today, "%Y-%m-%d"),
    ) {
        if d > t {
            out.push("future_date");
        } else if max_age_days > 0 && (t - d).num_days() > max_age_days {
            out.push("too_old");
        }
    }
    out
}

/// Flags that stop a claim from being submitted.
pub fn blocking(flag: &str) -> bool {
    matches!(flag, "receipt_missing" | "too_old" | "future_date")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn cats() -> Vec<Category> {
        [
            ("meals", "Meals"),
            ("transport", "Ground transport"),
            ("office", "Office supplies"),
        ]
        .iter()
        .map(|(i, n)| Category {
            id: (*i).into(),
            name: (*n).into(),
        })
        .collect()
    }

    #[test]
    fn amounts_parse_like_the_original() {
        assert_eq!(parse_amount_str("1,234.56"), Some(1234.56));
        assert_eq!(parse_amount_str("1.234,56"), Some(1234.56));
        assert_eq!(parse_amount_str("₹ 1,23,456.50"), Some(123_456.5));
        assert_eq!(parse_amount_str("(12.34)"), Some(-12.34));
        assert_eq!(parse_amount_str("12.34-"), Some(-12.34));
        assert_eq!(parse_amount_str("12,5"), Some(12.5));
        assert_eq!(parse_amount_str("abc"), None);
        assert_eq!(parse_amount(&json!(4.505)), Some(4.51));
    }

    #[test]
    fn ocr_results_normalise() {
        let r = normalize_ocr_result(
            &json!({"merchant": " Blue Fern Cafe ", "date": "08/20/2026", "total": "14.60",
                "tax": 1.2, "currency": "$", "category": "Meals", "paymentMethod": "VISA",
                "cardLast4": "**** 4471", "items": [{"name": "Latte", "amount": "4.5"},
                {"name": "", "amount": 1}], "confidence": 0.9}),
            &cats(),
            "EUR",
            "2026-08-26",
        );
        assert_eq!(r.merchant.as_deref(), Some("Blue Fern Cafe"));
        assert_eq!(r.date.as_deref(), Some("2026-08-20"));
        assert_eq!((r.total, r.currency.as_str()), (Some(14.6), "USD"));
        assert_eq!(r.category_id.as_deref(), Some("meals"));
        assert_eq!(r.payment_method.as_deref(), Some("Card"));
        assert_eq!(r.card_last4.as_deref(), Some("4471"));
        assert_eq!(r.items.len(), 1);
        let r = normalize_ocr_result(
            &json!({"total": -5, "date": "2031-01-01", "currency": "XXX", "confidence": "high"}),
            &cats(),
            "INR",
            "2026-08-26",
        );
        assert_eq!((r.total, r.date, r.currency.as_str()), (None, None, "INR"));
        assert!((r.confidence - 0.3).abs() < 1e-9);
        assert_eq!(
            extract_json_str("Here: {\"a\":{\"b\":\"}\"}} ok"),
            Some(json!({"a": {"b": "}"}}))
        );
    }

    #[test]
    fn claim_lifecycle_and_policy() {
        assert!(can_transition("draft", "submitted"));
        assert!(can_transition("submitted", "draft"));
        assert!(!can_transition("approved", "draft"));
        assert_eq!(next_status("approved"), Some("reimbursed"));
        assert_eq!(next_status("reimbursed"), None);
        let pol = CategoryPolicy {
            limit: Some(500.0),
            receipt_above: None,
        };
        let flags = policy_flags(
            &PolicyInput {
                amount: 800.0,
                date: "2026-06-01",
                has_receipt: false,
                category: Some(&pol),
                category_known: true,
            },
            250.0,
            90,
            "2026-10-17",
        );
        assert_eq!(flags, vec!["over_limit", "receipt_missing", "too_old"]);
    }
}
