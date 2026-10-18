//! **ERP_EXTENSIONS 5A** — offer letters, onboarding checklists and follow-ups, ported from
//! HeyPuter/everything `src/hiring/src/core/offers.js` (status machine, validation, expiring
//! offers, the letter template with optional lines), `onboard.js` (checklist templates, due
//! offsets from the joining date, due / overdue tasks, progress), `followups.js` and
//! `emails.js` `renderTemplate` (MIT, see THIRD_PARTY_NOTICES.md).
//!
//! Pure: no I/O. Indianised: CTC breakup in ₹ (basic, HRA, DA, allowances, employer PF at 12 %
//! of basic + DA up to the ₹15,000 EPF wage ceiling), Indian digit grouping and amount in words,
//! Indian dates in the letter. Everything's `draft → sent → signed` gains an approval step:
//! `draft → pending_approval → approved → sent → accepted | declined | expired | withdrawn`.

use chrono::{Datelike, Duration, NaiveDate, NaiveDateTime, Timelike};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};
use utoipa::ToSchema;

// ------------------------------------------------------------------ JS helpers

/// `isSafeId`: `[A-Za-z0-9_-]{1,64}`.
pub fn is_safe_id(v: &str) -> bool {
    (1..=64).contains(&v.len())
        && v.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// JS `String(value ?? '')` for the scalar shapes stored settings hold.
pub fn js_str(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(Value::Number(n)) => n.as_f64().map_or_else(|| n.to_string(), js_num_text),
        Some(Value::Array(a)) => a
            .iter()
            .map(|x| {
                if x.is_null() {
                    String::new()
                } else {
                    js_str(Some(x))
                }
            })
            .collect::<Vec<_>>()
            .join(","),
        Some(Value::Object(_)) => "[object Object]".to_owned(),
    }
}

fn js_num_text(f: f64) -> String {
    if f.fract() == 0.0 && f.abs() < 1e21 {
        format!("{}", f as i64)
    } else {
        format!("{f}")
    }
}

/// JS `Number(value)` (`undefined` → NaN, `null` / `''` → 0).
pub fn js_number(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => f64::from(u8::from(*b)),
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => {
            let t = s.trim();
            if t.is_empty() {
                0.0
            } else {
                t.parse::<f64>()
                    .ok()
                    .filter(|f| f.is_finite() || t.contains("Infinity"))
                    .unwrap_or(f64::NAN)
            }
        }
        Some(Value::Array(a)) if a.is_empty() => 0.0,
        Some(_) => f64::NAN,
    }
}

/// `Number.isInteger(Number(v))` → the integer.
pub fn js_integer(v: Option<&Value>) -> Option<i64> {
    let f = js_number(v);
    (f.is_finite() && f.fract() == 0.0).then_some(f as i64)
}

fn take(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

fn trim_take(v: Option<&Value>, max: usize) -> String {
    take(js_str(v).trim(), max)
}

/// `/^\d{4}-\d{2}-\d{2}$/`.
pub fn is_iso_date(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10
        && b.iter().enumerate().all(|(i, c)| {
            if i == 4 || i == 7 {
                *c == b'-'
            } else {
                c.is_ascii_digit()
            }
        })
}

/// `toMinutes`: `H:MM` / `HH:MM` → minutes since midnight (< 24 h), else `None`.
pub fn to_minutes(t: &str) -> Option<u32> {
    let (h, m) = t.split_once(':')?;
    if h.is_empty() || h.len() > 2 || !h.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let mb = m.as_bytes();
    if mb.len() != 2 || !(b'0'..=b'5').contains(&mb[0]) || !mb[1].is_ascii_digit() {
        return None;
    }
    let total = h.parse::<u32>().ok()? * 60 + m.parse::<u32>().ok()?;
    (total < 24 * 60).then_some(total)
}

/// `daysBetween(a, b)` = b − a in whole days.
pub fn days_between(a: NaiveDate, b: NaiveDate) -> i64 {
    (b - a).num_days()
}

// ------------------------------------------------------------------ money (₹)

/// ₹ with Indian digit grouping; paise only when not zero (`₹9,60,000`, `₹1,234.50`).
pub fn format_inr(v: Decimal) -> String {
    let neg = v.is_sign_negative() && !v.is_zero();
    let r = v.abs().round_dp(2);
    let whole = r.trunc().to_u128().unwrap_or(0).to_string();
    let paise = ((r - r.trunc()) * Decimal::from(100))
        .round()
        .to_u32()
        .unwrap_or(0);
    let grouped = if whole.len() <= 3 {
        whole
    } else {
        let (head, tail) = whole.split_at(whole.len() - 3);
        let mut parts: Vec<String> = Vec::new();
        let hb = head.as_bytes();
        let mut end = hb.len();
        while end > 0 {
            let start = end.saturating_sub(2);
            parts.push(head[start..end].to_owned());
            end = start;
        }
        parts.reverse();
        format!("{},{tail}", parts.join(","))
    };
    let sign = if neg { "-" } else { "" };
    if paise == 0 {
        format!("{sign}₹{grouped}")
    } else {
        format!("{sign}₹{grouped}.{paise:02}")
    }
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

fn below_hundred(n: u64) -> String {
    if n < 20 {
        ONES[n as usize].to_owned()
    } else if n.is_multiple_of(10) {
        TENS[(n / 10) as usize].to_owned()
    } else {
        format!("{}-{}", TENS[(n / 10) as usize], ONES[(n % 10) as usize])
    }
}

/// A whole number in Indian words (crore, lakh, thousand, hundred).
pub fn number_in_words(n: u64) -> String {
    if n == 0 {
        return "Zero".into();
    }
    let mut parts = Vec::new();
    let crore = n / 10_000_000;
    let mut rest = n % 10_000_000;
    if crore > 0 {
        parts.push(format!("{} Crore", number_in_words(crore)));
    }
    for (unit, name) in [(100_000, "Lakh"), (1_000, "Thousand"), (100, "Hundred")] {
        let q = rest / unit;
        rest %= unit;
        if q > 0 {
            parts.push(format!("{} {name}", below_hundred(q)));
        }
    }
    if rest > 0 {
        parts.push(below_hundred(rest));
    }
    parts.join(" ")
}

/// `Rupees Nine Lakh Sixty Thousand only` (with paise when present).
pub fn amount_in_words(v: Decimal) -> String {
    let r = v.abs().round_dp(2);
    let rupees = r.trunc().to_u64().unwrap_or(0);
    let paise = ((r - r.trunc()) * Decimal::from(100))
        .round()
        .to_u64()
        .unwrap_or(0);
    if paise == 0 {
        format!("Rupees {} only", number_in_words(rupees))
    } else {
        format!(
            "Rupees {} and {} Paise only",
            number_in_words(rupees),
            number_in_words(paise)
        )
    }
}

/// `1 June 2026` — the Indian long date used in letters.
pub fn letter_date(d: NaiveDate) -> String {
    format!("{} {}", d.day(), d.format("%B %Y"))
}

/// Monthly cost-to-institution breakup of an offer.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = HiringOfferCtc)]
pub struct Ctc {
    /// Basic pay per month (₹).
    #[schema(value_type = String, example = "45000")]
    pub basic: Decimal,
    /// House rent allowance per month.
    #[serde(default)]
    #[schema(value_type = String)]
    pub hra: Decimal,
    /// Dearness allowance per month.
    #[serde(default)]
    #[schema(value_type = String)]
    pub da: Decimal,
    /// Other allowances per month (special, conveyance, …).
    #[serde(default)]
    #[schema(value_type = String)]
    pub allowances: Decimal,
    /// Employer EPF contribution per month.
    #[serde(default)]
    #[schema(value_type = String)]
    pub employer_pf: Decimal,
}

/// The EPF wage ceiling on which the 12 % employer contribution is mandatory.
pub const EPF_WAGE_CEILING: i64 = 15_000;

impl Ctc {
    /// Monthly gross (everything but the employer PF).
    pub fn gross_monthly(&self) -> Decimal {
        self.basic + self.hra + self.da + self.allowances
    }
    /// Monthly CTC.
    pub fn monthly(&self) -> Decimal {
        self.gross_monthly() + self.employer_pf
    }
    /// Annual CTC (12 × monthly).
    pub fn annual(&self) -> Decimal {
        self.monthly() * Decimal::from(12)
    }
}

/// Employer EPF at 12 % of basic + DA, up to the ₹15,000 wage ceiling (whole rupees).
pub fn suggested_employer_pf(basic: Decimal, da: Decimal) -> Decimal {
    let wages = (basic + da).min(Decimal::from(EPF_WAGE_CEILING));
    (wages * Decimal::new(12, 2)).round()
}

/// A problem with offer terms: `field` + machine `code` + message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[schema(as = HiringOfferProblem)]
pub struct Problem {
    pub field: &'static str,
    pub code: &'static str,
    pub message: &'static str,
}

/// `validateOffer` (Everything): a positive annual pay, a non-negative bonus when given, a
/// joining date, and an expiry that is well formed and not after the joining date. Inputs are
/// JS-shaped: `salary` is `Number(salary)`, `bonus` is `None` for null / '' / undefined.
pub fn validate_offer(
    salary: f64,
    bonus: Option<f64>,
    joining: &str,
    expires: Option<&str>,
) -> Vec<Problem> {
    let mut out = Vec::new();
    if !(salary.is_finite() && salary > 0.0) {
        out.push(Problem {
            field: "ctc",
            code: "ctc_required",
            message: "Enter the basic pay; the CTC must be more than ₹0.",
        });
    }
    if let Some(b) = bonus {
        if !(b.is_finite() && b >= 0.0) {
            out.push(Problem {
                field: "joiningBonus",
                code: "bonus_negative",
                message: "The joining bonus cannot be negative.",
            });
        }
    }
    if !is_iso_date(joining) {
        out.push(Problem {
            field: "joiningDate",
            code: "joining_date_required",
            message: "Pick a joining date.",
        });
    }
    let expires = expires.filter(|e| !e.is_empty());
    if let Some(e) = expires {
        if !is_iso_date(e) {
            out.push(Problem {
                field: "expiresOn",
                code: "expiry_malformed",
                message: "The expiry date is malformed.",
            });
        }
        if is_iso_date(joining) && e > joining {
            out.push(Problem {
                field: "expiresOn",
                code: "expiry_after_joining",
                message: "The offer should expire on or before the joining date.",
            });
        }
    }
    out
}

/// The CTC breakup checks on top of [`validate_offer`]: no negative component, and the
/// employer PF no more than 12 % of basic + DA.
pub fn validate_ctc(c: &Ctc) -> Vec<Problem> {
    let mut out = Vec::new();
    let neg = [c.basic, c.hra, c.da, c.allowances, c.employer_pf]
        .iter()
        .any(Decimal::is_sign_negative);
    if neg {
        out.push(Problem {
            field: "ctc",
            code: "negative_component",
            message: "A CTC component cannot be negative.",
        });
    }
    if c.employer_pf > ((c.basic + c.da) * Decimal::new(12, 2)).round_dp(2) {
        out.push(Problem {
            field: "ctc.employerPf",
            code: "pf_above_statutory",
            message: "Employer PF cannot exceed 12% of basic + DA.",
        });
    }
    out
}

// ------------------------------------------------------------------ status machine

/// Offer status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(as = HiringOfferStatus)]
pub enum OfferStatus {
    Draft,
    PendingApproval,
    Approved,
    Sent,
    Accepted,
    Declined,
    Expired,
    Withdrawn,
}

impl OfferStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::PendingApproval => "pending_approval",
            Self::Approved => "approved",
            Self::Sent => "sent",
            Self::Accepted => "accepted",
            Self::Declined => "declined",
            Self::Expired => "expired",
            Self::Withdrawn => "withdrawn",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "draft" => Self::Draft,
            "pending_approval" => Self::PendingApproval,
            "approved" => Self::Approved,
            "sent" => Self::Sent,
            "accepted" => Self::Accepted,
            "declined" => Self::Declined,
            "expired" => Self::Expired,
            "withdrawn" => Self::Withdrawn,
            _ => return None,
        })
    }
}

/// `canTransition`. Accepted, declined, expired and withdrawn are terminal. A draft goes to
/// approval before it can be sent; approval can send it back to draft.
pub fn can_transition(from: OfferStatus, to: OfferStatus) -> bool {
    use OfferStatus::*;
    matches!(
        (from, to),
        (Draft, PendingApproval)
            | (PendingApproval, Approved)
            | (PendingApproval, Draft)
            | (Approved, Sent)
            | (Sent, Accepted)
            | (Sent, Declined)
            | (Sent, Expired)
            | (Draft | PendingApproval | Approved | Sent, Withdrawn)
    )
}

/// `isOpenOffer` — at most one per candidate.
pub fn is_open(s: OfferStatus) -> bool {
    matches!(
        s,
        OfferStatus::Draft
            | OfferStatus::PendingApproval
            | OfferStatus::Approved
            | OfferStatus::Sent
    )
}

/// What reminders, analytics and checklists read of an offer.
#[derive(Debug, Clone, PartialEq)]
pub struct OfferRecord {
    pub id: String,
    pub candidate_id: String,
    pub status: OfferStatus,
    pub expires: Option<NaiveDate>,
    pub joining_date: Option<NaiveDate>,
    pub sent_at: Option<NaiveDateTime>,
    /// Everything's `signedAt`.
    pub accepted_at: Option<NaiveDateTime>,
    pub resolved_at: Option<NaiveDateTime>,
    pub created_at: Option<NaiveDateTime>,
}

fn midnight(d: NaiveDate) -> NaiveDateTime {
    d.and_hms_opt(0, 0, 0).unwrap_or_default()
}

/// `expiringOffers`: sent, with an expiry, and expiring within `within_days` of `now` (or
/// already lapsed).
pub fn expiring_offers(
    offers: &[OfferRecord],
    within_days: i64,
    now: NaiveDateTime,
) -> Vec<&OfferRecord> {
    offers
        .iter()
        .filter(|o| {
            o.status == OfferStatus::Sent
                && o.expires
                    .is_some_and(|e| midnight(e) - now < Duration::days(within_days))
        })
        .collect()
}

// ------------------------------------------------------------------ letter template

/// The `{{ key }}` placeholders of a text with their byte spans (`/\{\{\s*(\w+)\s*\}\}/g`).
fn placeholders(text: &str) -> Vec<(usize, usize, String)> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i + 1 < b.len() {
        if b[i] == b'{' && b[i + 1] == b'{' {
            let mut j = i + 2;
            while j < b.len() && (b[j] as char).is_ascii_whitespace() {
                j += 1;
            }
            let ks = j;
            while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_') {
                j += 1;
            }
            let ke = j;
            while j < b.len() && (b[j] as char).is_ascii_whitespace() {
                j += 1;
            }
            if ke > ks && j + 1 < b.len() && b[j] == b'}' && b[j + 1] == b'}' {
                out.push((i, j + 2, text[ks..ke].to_owned()));
                i = j + 2;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// Merge values: a key that is present (even with a null value) is "own"; anything else, like
/// `{{constructor}}`, renders empty.
pub type MergeVars = BTreeMap<String, Option<String>>;

/// `renderTemplate`: replaces `{{key}}`; unknown or empty keys become ''.
pub fn render_template(text: &str, vars: &MergeVars) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for (s, e, k) in placeholders(text) {
        out.push_str(&text[last..s]);
        if let Some(Some(v)) = vars.get(&k) {
            out.push_str(v);
        }
        last = e;
    }
    out.push_str(&text[last..]);
    out
}

/// `renderOfferLetter`: lines whose placeholders all render empty are dropped (optional
/// terms disappear), runs of blank lines collapse, and the result is trimmed.
pub fn render_offer_letter(template: &str, vars: &MergeVars) -> String {
    let mut lines = Vec::new();
    for line in template.split('\n') {
        let ph = placeholders(line);
        let all_empty = !ph.is_empty()
            && ph.iter().all(|(_, _, k)| {
                vars.get(k)
                    .is_none_or(|v| v.as_deref().unwrap_or("").trim().is_empty())
            });
        if all_empty {
            continue;
        }
        lines.push(render_template(line, vars));
    }
    let joined = lines.join("\n");
    let mut out = String::with_capacity(joined.len());
    let mut run = 0;
    for ch in joined.chars() {
        if ch == '\n' {
            run += 1;
            if run <= 2 {
                out.push(ch);
            }
        } else {
            run = 0;
            out.push(ch);
        }
    }
    out.trim().to_owned()
}

/// The default Indian offer letter. Lines whose fields are empty (no DA, no joining bonus, no
/// probation, an unsigned letter) drop out at render time, so one template covers every offer.
pub const DEFAULT_OFFER_TEMPLATE: &str = concat!(
    "{{letterDate}}\n",
    "\n",
    "Ref: {{offerNumber}}\n",
    "\n",
    "{{name}}\n",
    "\n",
    "Subject: Offer of appointment as {{designation}}\n",
    "\n",
    "Dear {{name}},\n",
    "\n",
    "We are pleased to offer you the position of {{designation}}{{department}} at {{institution}}, on the terms below.\n",
    "\n",
    "  Designation          {{designation}}\n",
    "  Pay level / grade    {{grade}}\n",
    "  Date of joining      {{joiningDate}}\n",
    "  {{probation}}\n",
    "\n",
    "Compensation (cost to the institution)\n",
    "  Basic pay            {{basic}} per month\n",
    "  House rent allowance {{hra}} per month\n",
    "  Dearness allowance   {{da}} per month\n",
    "  Other allowances     {{allowances}} per month\n",
    "  Employer PF          {{employerPf}} per month\n",
    "  Monthly CTC          {{ctcMonthly}}\n",
    "  Annual CTC           {{ctcAnnual}} ({{ctcAnnualWords}})\n",
    "  Joining bonus        {{joiningBonus}} (one time)\n",
    "\n",
    "Employee PF, professional tax and income tax (TDS) are deducted as per law.\n",
    "\n",
    "This offer remains open until {{expires}}. Please confirm your acceptance by then.\n",
    "\n",
    "{{notes}}\n",
    "\n",
    "Yours sincerely,\n",
    "For {{institution}}\n",
    "\n",
    "{{signature}}",
);

/// The placeholders the letter template can use.
pub const OFFER_TEMPLATE_VARIABLES: &[&str] = &[
    "letterDate",
    "offerNumber",
    "name",
    "designation",
    "department",
    "institution",
    "grade",
    "joiningDate",
    "probation",
    "basic",
    "hra",
    "da",
    "allowances",
    "employerPf",
    "ctcMonthly",
    "ctcAnnual",
    "ctcAnnualWords",
    "joiningBonus",
    "expires",
    "notes",
    "signature",
];

/// `offerTemplate`: the institution's override, or the default.
pub fn offer_template(stored: Option<&str>) -> &str {
    match stored {
        Some(s) if !s.trim().is_empty() => s,
        _ => DEFAULT_OFFER_TEMPLATE,
    }
}

/// Everything a letter merges.
#[derive(Debug, Clone)]
pub struct LetterInput<'a> {
    pub offer_number: &'a str,
    pub candidate_name: &'a str,
    pub designation: &'a str,
    pub department: Option<&'a str>,
    pub institution: &'a str,
    pub grade: Option<&'a str>,
    pub joining_date: NaiveDate,
    pub probation_months: Option<i32>,
    pub ctc: &'a Ctc,
    pub joining_bonus: Option<Decimal>,
    pub expires: Option<NaiveDate>,
    pub notes: Option<&'a str>,
    pub status: OfferStatus,
    pub sent_on: Option<NaiveDate>,
    pub accepted_by: Option<&'a str>,
    pub accepted_on: Option<NaiveDate>,
}

fn money_or_empty(v: Decimal) -> Option<String> {
    (!v.is_zero()).then(|| format_inr(v))
}

/// `offerLetterVars` (Indianised).
pub fn letter_vars(i: &LetterInput<'_>) -> MergeVars {
    let mut v = MergeVars::new();
    let mut put = |k: &str, val: Option<String>| {
        v.insert(k.to_owned(), val);
    };
    put(
        "letterDate",
        Some(i.sent_on.map_or_else(|| "Draft".to_owned(), letter_date)),
    );
    put("offerNumber", Some(i.offer_number.to_owned()));
    put("name", Some(i.candidate_name.to_owned()));
    put("designation", Some(i.designation.to_owned()));
    put(
        "department",
        Some(
            i.department
                .map_or(String::new(), |d| format!(" in the {d}")),
        ),
    );
    put("institution", Some(i.institution.to_owned()));
    put("grade", i.grade.map(str::to_owned));
    put("joiningDate", Some(letter_date(i.joining_date)));
    put(
        "probation",
        i.probation_months.filter(|m| *m > 0).map(|m| {
            format!(
                "Probation            {m} month{}",
                if m == 1 { "" } else { "s" }
            )
        }),
    );
    put("basic", money_or_empty(i.ctc.basic));
    put("hra", money_or_empty(i.ctc.hra));
    put("da", money_or_empty(i.ctc.da));
    put("allowances", money_or_empty(i.ctc.allowances));
    put("employerPf", money_or_empty(i.ctc.employer_pf));
    put("ctcMonthly", Some(format_inr(i.ctc.monthly())));
    put("ctcAnnual", Some(format_inr(i.ctc.annual())));
    put("ctcAnnualWords", Some(amount_in_words(i.ctc.annual())));
    put("joiningBonus", i.joining_bonus.and_then(money_or_empty));
    put("expires", i.expires.map(letter_date));
    put("notes", i.notes.map(str::to_owned));
    put(
        "signature",
        match (i.status, i.accepted_by) {
            (OfferStatus::Accepted, Some(name)) => Some(format!(
                "----------------------------------------\nAccepted by {name}{}",
                i.accepted_on
                    .map_or(String::new(), |d| format!(" on {}", letter_date(d)))
            )),
            _ => None,
        },
    );
    v
}

/// `escapeHtml`.
pub fn escape_html(text: &str) -> String {
    let mut s = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => s.push_str("&amp;"),
            '<' => s.push_str("&lt;"),
            '>' => s.push_str("&gt;"),
            '"' => s.push_str("&quot;"),
            '\'' => s.push_str("&#39;"),
            _ => s.push(c),
        }
    }
    s
}

/// The letter as a small self-contained HTML document (paragraphs; aligned term blocks keep
/// their spacing). Every merged value is escaped.
pub fn letter_html(title: &str, text: &str) -> String {
    let body = text
        .split("\n\n")
        .map(|para| {
            let indented = para.lines().any(|l| l.starts_with("  "));
            let inner = para
                .lines()
                .map(escape_html)
                .collect::<Vec<_>>()
                .join("<br>\n");
            if indented {
                format!("<p class=\"terms\">{inner}</p>")
            } else {
                format!("<p>{inner}</p>")
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "<!doctype html>\n<html lang=\"en-IN\"><head><meta charset=\"utf-8\"><title>{}</title>\
<style>body{{font-family:Georgia,'Noto Serif',serif;max-width:720px;margin:40px auto;padding:0 24px;line-height:1.6;color:#1a1a1e}}\
.terms{{font-family:ui-monospace,Menlo,Consolas,monospace;white-space:pre;font-size:14px}}</style></head>\
<body>\n{body}\n</body></html>\n",
        escape_html(title)
    )
}

// ------------------------------------------------------------------ onboarding checklists

/// Who owns an onboarding task.
pub const TASK_OWNERS: &[&str] = &["hr", "it", "department", "accounts", "admin"];

/// One task of a checklist template.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = HiringOnboardingTemplateTask)]
pub struct TemplateTask {
    pub title: String,
    pub category: String,
    /// Days from the joining date (negative = before joining).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub offset_days: Option<i64>,
    /// `hr`, `it`, `department`, `accounts`, `admin`.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub owner: Option<String>,
}

/// A named checklist template, attached to requisitions (`jobIds`) or departments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = HiringOnboardingTemplate)]
pub struct Template {
    pub id: String,
    pub name: String,
    pub tasks: Vec<TemplateTask>,
    pub job_ids: Vec<String>,
    pub departments: Vec<String>,
}

pub const MAX_TEMPLATES: usize = 10;
pub const MAX_TEMPLATE_NAME: usize = 60;
pub const MAX_TEMPLATE_TASKS: usize = 40;

fn task(title: &str, category: &str, offset: Option<i64>, owner: &str) -> TemplateTask {
    TemplateTask {
        title: title.into(),
        category: category.into(),
        offset_days: offset,
        owner: Some(owner.into()),
    }
}

/// The shipped default for an Indian college: paperwork and statutory enrolment, IT access,
/// department induction.
pub fn default_template() -> Template {
    Template {
        id: "default".into(),
        name: "Standard onboarding".into(),
        tasks: vec![
            task("Signed offer letter on file", "Paperwork", Some(-7), "hr"),
            task(
                "Appointment order and service agreement",
                "Paperwork",
                Some(0),
                "hr",
            ),
            task(
                "Verify Aadhaar, PAN and educational certificates",
                "Paperwork",
                Some(-3),
                "hr",
            ),
            task(
                "Police / reference verification",
                "Paperwork",
                Some(14),
                "hr",
            ),
            task(
                "EPF / ESI enrolment (Form 11, UAN)",
                "Statutory",
                Some(7),
                "accounts",
            ),
            task(
                "Add to payroll with bank details and tax regime",
                "Statutory",
                Some(0),
                "accounts",
            ),
            task("Create ERP and e-mail login", "IT & access", Some(-2), "it"),
            task(
                "Laptop / lab access and ID card",
                "IT & access",
                Some(-1),
                "it",
            ),
            task(
                "Department induction and timetable handover",
                "Department",
                Some(0),
                "department",
            ),
            task(
                "Assign a mentor / buddy",
                "Department",
                Some(-1),
                "department",
            ),
            task(
                "30/60/90-day plan agreed with HoD",
                "Department",
                Some(7),
                "department",
            ),
        ],
        job_ids: vec![],
        departments: vec![],
    }
}

fn owner_of(v: Option<&Value>) -> Option<String> {
    let o = js_str(v).trim().to_lowercase();
    TASK_OWNERS.contains(&o.as_str()).then_some(o)
}

/// `normalizeOnboardingTemplates`: safe unique ids with names, tasks with titles, whole-day
/// offsets within a year; an empty result means "use the default". `owner` (an addition)
/// keeps one of [`TASK_OWNERS`].
pub fn normalize_templates(value: &Value) -> Vec<Template> {
    let Value::Array(list) = value else {
        return vec![];
    };
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for entry in list {
        let obj = entry.as_object();
        let get = |k: &str| obj.and_then(|o| o.get(k));
        let id = js_str(get("id"));
        let name = trim_take(get("name"), MAX_TEMPLATE_NAME);
        if !is_safe_id(&id) || name.is_empty() || seen.contains(&id) {
            continue;
        }
        let mut tasks = Vec::new();
        if let Some(Value::Array(ts)) = get("tasks") {
            for t in ts {
                let to = t.as_object();
                let tg = |k: &str| to.and_then(|o| o.get(k));
                let title = trim_take(tg("title"), 140);
                if title.is_empty() {
                    continue;
                }
                let cat = trim_take(tg("category"), 40);
                let offset = js_integer(tg("offsetDays")).filter(|o| o.abs() <= 365);
                tasks.push(TemplateTask {
                    title,
                    category: if cat.is_empty() { "Other".into() } else { cat },
                    offset_days: offset,
                    owner: owner_of(tg("owner")),
                });
                if tasks.len() >= MAX_TEMPLATE_TASKS {
                    break;
                }
            }
        }
        if tasks.is_empty() {
            continue;
        }
        seen.insert(id.clone());
        let mut job_ids: Vec<String> = Vec::new();
        if let Some(Value::Array(js)) = get("jobIds") {
            for j in js {
                let s = js_str(Some(j));
                if is_safe_id(&s) && !job_ids.contains(&s) {
                    job_ids.push(s);
                }
            }
        }
        let mut departments: Vec<String> = Vec::new();
        if let Some(Value::Array(ds)) = get("departments") {
            for d in ds {
                let s = trim_take(Some(d), 60);
                if !s.is_empty() && !departments.contains(&s) {
                    departments.push(s);
                }
            }
        }
        out.push(Template {
            id,
            name,
            tasks,
            job_ids,
            departments,
        });
        if out.len() >= MAX_TEMPLATES {
            break;
        }
    }
    out
}

/// `templateForJob`: a template naming the requisition, else its department, else a general one
/// (attached to nothing), else the first; `fallback` when none are stored.
pub fn template_for_job<'a>(
    stored: &'a [Template],
    job_id: Option<&str>,
    department: Option<&str>,
    fallback: &'a Template,
) -> &'a Template {
    if stored.is_empty() {
        return fallback;
    }
    let dept = department.unwrap_or("").trim().to_lowercase();
    job_id
        .filter(|j| !j.is_empty())
        .and_then(|j| stored.iter().find(|t| t.job_ids.iter().any(|x| x == j)))
        .or_else(|| {
            (!dept.is_empty())
                .then(|| {
                    stored
                        .iter()
                        .find(|t| t.departments.iter().any(|d| d.to_lowercase() == dept))
                })
                .flatten()
        })
        .or_else(|| {
            stored
                .iter()
                .find(|t| t.job_ids.is_empty() && t.departments.is_empty())
        })
        .unwrap_or(&stored[0])
}

/// One checklist task of a hire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = HiringChecklistTaskDraft)]
pub struct PlanTask {
    pub id: String,
    pub title: String,
    pub category: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub offset_days: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub owner: Option<String>,
    pub done: bool,
}

/// `buildOnboardingPlan`: a fresh checklist from a template (`make_id` injectable).
pub fn build_plan(template: &Template, mut make_id: impl FnMut() -> String) -> Vec<PlanTask> {
    let default;
    let tasks = if template.tasks.is_empty() {
        default = default_template();
        &default.tasks
    } else {
        &template.tasks
    };
    tasks
        .iter()
        .map(|t| PlanTask {
            id: make_id(),
            title: t.title.clone(),
            category: t.category.clone(),
            offset_days: t.offset_days,
            owner: t.owner.clone(),
            done: false,
        })
        .collect()
}

/// `taskDueDate`: joining date + offset, or `None`.
pub fn task_due_date(offset_days: Option<i64>, joining: Option<NaiveDate>) -> Option<NaiveDate> {
    Some(joining? + Duration::days(offset_days?))
}

/// `offsetLabel`: "7 days before the joining date".
pub fn offset_label(offset_days: Option<i64>) -> String {
    match offset_days {
        None => "No date".into(),
        Some(0) => "On the joining date".into(),
        Some(o) => {
            let d = o.abs();
            format!(
                "{d} {} {} the joining date",
                if d == 1 { "day" } else { "days" },
                if o < 0 { "before" } else { "after" }
            )
        }
    }
}

/// `offsetShort`: "7d before".
pub fn offset_short(offset_days: Option<i64>) -> String {
    match offset_days {
        None => "No date".into(),
        Some(0) => "Joining date".into(),
        Some(o) => format!("{}d {}", o.abs(), if o < 0 { "before" } else { "after" }),
    }
}

fn signed_at_of(o: &OfferRecord) -> Option<NaiveDateTime> {
    o.accepted_at.or(o.resolved_at).or(o.created_at)
}

/// `hireStartDate`: the joining date on the candidate's most recently accepted offer.
pub fn hire_start_date(offers: &[OfferRecord], candidate_id: &str) -> Option<NaiveDate> {
    let mut latest: Option<&OfferRecord> = None;
    for o in offers {
        if o.candidate_id != candidate_id
            || o.status != OfferStatus::Accepted
            || o.joining_date.is_none()
        {
            continue;
        }
        if latest.is_none_or(|l| signed_at_of(o) >= signed_at_of(l)) {
            latest = Some(o);
        }
    }
    latest.and_then(|o| o.joining_date)
}

/// A task of a hire's checklist with its state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChecklistTask {
    pub id: String,
    pub title: String,
    pub category: String,
    pub offset_days: Option<i64>,
    pub done: bool,
}

/// A hire's checklist (`state.onboarding[candidateId]`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checklist {
    pub candidate_id: String,
    pub tasks: Vec<ChecklistTask>,
}

/// A task that has come due.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DueTask<'a> {
    pub candidate_id: &'a str,
    pub task: &'a ChecklistTask,
    pub due_date: NaiveDate,
    pub overdue: bool,
    pub days_left: i64,
}

/// `dueOnboardingTasks`: unfinished tasks due by `today + within_days`, soonest first; the
/// joining date comes from the latest accepted offer, so a plan without one has no due dates.
pub fn due_onboarding_tasks<'a>(
    plans: &'a [Checklist],
    offers: &[OfferRecord],
    today: NaiveDate,
    within_days: i64,
) -> Vec<DueTask<'a>> {
    let horizon = today + Duration::days(within_days);
    let mut out = Vec::new();
    for p in plans {
        let Some(start) = hire_start_date(offers, &p.candidate_id) else {
            continue;
        };
        for t in &p.tasks {
            if t.done {
                continue;
            }
            let Some(due) = task_due_date(t.offset_days, Some(start)) else {
                continue;
            };
            if due > horizon {
                continue;
            }
            out.push(DueTask {
                candidate_id: &p.candidate_id,
                task: t,
                due_date: due,
                overdue: due < today,
                days_left: days_between(today, due),
            });
        }
    }
    out.sort_by_key(|d| d.due_date);
    out
}

/// `onboardingProgress`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[schema(as = HiringChecklistProgress)]
pub struct Progress {
    pub done: usize,
    pub total: usize,
    pub pct: u32,
}

pub fn progress(done_flags: impl IntoIterator<Item = bool>) -> Progress {
    let (mut done, mut total) = (0usize, 0usize);
    for d in done_flags {
        total += 1;
        done += usize::from(d);
    }
    let pct = if total == 0 {
        0
    } else {
        ((done as f64 / total as f64) * 100.0).round() as u32
    };
    Progress { done, total, pct }
}

/// `groupTasks`: categories in first-appearance order ('' → Other).
pub fn group_tasks<T>(tasks: &[T], category: impl Fn(&T) -> &str) -> Vec<(String, Vec<&T>)> {
    let mut groups: Vec<(String, Vec<&T>)> = Vec::new();
    for t in tasks {
        let c = category(t);
        let key = if c.is_empty() { "Other" } else { c };
        match groups.iter_mut().find(|(k, _)| k == key) {
            Some((_, v)) => v.push(t),
            None => groups.push((key.to_owned(), vec![t])),
        }
    }
    groups
}

// ------------------------------------------------------------------ follow-ups

/// A follow-up promise on a candidate ("call about the notice period on Thursday").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Followup {
    pub id: String,
    pub candidate_id: String,
    pub text: String,
    /// The stored day, as text (`YYYY-MM-DD`; anything else never comes due).
    pub due_date: Option<String>,
    pub due_time: Option<String>,
    pub done: bool,
    pub completed_at: Option<NaiveDateTime>,
}

/// `validateFollowup`: codes `text_required`, `due_date_required`, `due_time_malformed`.
pub fn validate_followup(text: &str, due_date: &str, due_time: Option<&str>) -> Vec<&'static str> {
    let mut out = Vec::new();
    if text.trim().is_empty() {
        out.push("text_required");
    }
    if !is_iso_date(due_date) {
        out.push("due_date_required");
    }
    if let Some(t) = due_time.filter(|t| !t.is_empty()) {
        if to_minutes(t).is_none() {
            out.push("due_time_malformed");
        }
    }
    out
}

/// `followupDue`: `None` when not due (or done), else `Some(overdue)`. Date-only follow-ups come
/// due at the start of their day, timed ones at their time.
pub fn followup_due(f: &Followup, now: NaiveDateTime) -> Option<bool> {
    if f.done {
        return None;
    }
    let due = f.due_date.as_deref().filter(|d| !d.is_empty())?;
    let today = now.date().format("%Y-%m-%d").to_string();
    if due > today.as_str() {
        return None;
    }
    let minutes = f.due_time.as_deref().and_then(to_minutes);
    let now_min = now.hour() * 60 + now.minute();
    if due == today && minutes.is_some_and(|m| m > now_min) {
        return None;
    }
    Some(due < today.as_str() || (due == today && minutes.is_some_and(|m| m < now_min)))
}

fn followup_key(f: &Followup) -> String {
    format!(
        "{} {}",
        f.due_date.as_deref().unwrap_or("null"),
        f.due_time.as_deref().unwrap_or("")
    )
}

/// `dueFollowups`: open follow-ups due now, soonest first, as `(follow-up, overdue)`.
pub fn due_followups(all: &[Followup], now: NaiveDateTime) -> Vec<(&Followup, bool)> {
    let mut due: Vec<(&Followup, bool)> = all
        .iter()
        .filter_map(|f| followup_due(f, now).map(|o| (f, o)))
        .collect();
    due.sort_by_key(|(f, _)| followup_key(f));
    due
}

/// `sortFollowups`: open first (soonest due first), completed last (latest first).
pub fn sort_followups(list: &[Followup]) -> Vec<&Followup> {
    let mut v: Vec<&Followup> = list.iter().collect();
    v.sort_by(|a, b| {
        if a.done != b.done {
            return if a.done {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Less
            };
        }
        if a.done {
            return b.completed_at.cmp(&a.completed_at);
        }
        followup_key(a).cmp(&followup_key(b))
    });
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    #[test]
    fn inr_grouping_and_words() {
        assert_eq!(format_inr(Decimal::from(960_000)), "₹9,60,000");
        assert_eq!(format_inr(Decimal::new(123_450, 2)), "₹1,234.50");
        assert_eq!(format_inr(Decimal::from(12_345_678)), "₹1,23,45,678");
        assert_eq!(format_inr(Decimal::from(999)), "₹999");
        assert_eq!(
            amount_in_words(Decimal::from(960_000)),
            "Rupees Nine Lakh Sixty Thousand only"
        );
        assert_eq!(
            number_in_words(12_34_56_789),
            "Twelve Crore Thirty-Four Lakh Fifty-Six Thousand Seven Hundred Eighty-Nine"
        );
    }

    #[test]
    fn ctc_and_pf() {
        let c = Ctc {
            basic: Decimal::from(45_000),
            hra: Decimal::from(18_000),
            da: Decimal::ZERO,
            allowances: Decimal::from(5_000),
            employer_pf: suggested_employer_pf(Decimal::from(45_000), Decimal::ZERO),
        };
        assert_eq!(c.employer_pf, Decimal::from(1_800));
        assert_eq!(c.monthly(), Decimal::from(69_800));
        assert_eq!(c.annual(), Decimal::from(837_600));
        assert!(validate_ctc(&c).is_empty());
        let bad = Ctc {
            employer_pf: Decimal::from(9_000),
            ..c
        };
        assert_eq!(validate_ctc(&bad)[0].code, "pf_above_statutory");
    }

    #[test]
    fn transitions_need_approval() {
        use OfferStatus::*;
        assert!(!can_transition(Draft, Sent));
        assert!(can_transition(Draft, PendingApproval));
        assert!(can_transition(PendingApproval, Approved));
        assert!(can_transition(Approved, Sent));
        assert!(can_transition(Sent, Expired));
        assert!(!can_transition(Accepted, Withdrawn));
    }

    #[test]
    fn default_letter_drops_empty_lines() {
        let ctc = Ctc {
            basic: Decimal::from(57_700),
            hra: Decimal::from(5_770),
            ..Ctc::default()
        };
        let i = LetterInput {
            offer_number: "OFR-000001",
            candidate_name: "Priya Kulkarni",
            designation: "Assistant Professor",
            department: Some("Department of Commerce"),
            institution: "Ahilyanagar College",
            grade: Some("Academic Level 10"),
            joining_date: d("2026-07-01"),
            probation_months: Some(12),
            ctc: &ctc,
            joining_bonus: None,
            expires: Some(d("2026-06-15")),
            notes: None,
            status: OfferStatus::Sent,
            sent_on: Some(d("2026-06-01")),
            accepted_by: None,
            accepted_on: None,
        };
        let text = render_offer_letter(DEFAULT_OFFER_TEMPLATE, &letter_vars(&i));
        assert!(text.starts_with("1 June 2026"));
        assert!(text.contains("Annual CTC           ₹7,61,640 (Rupees Seven Lakh Sixty-One Thousand Six Hundred Forty only)"));
        assert!(!text.contains("Dearness allowance"));
        assert!(!text.contains("Joining bonus"));
        assert!(text.contains("Probation            12 months"));
        assert!(!text.contains("{{"));
        let html = letter_html("Offer", &text);
        assert!(html.contains("<p class=\"terms\">"));
    }

    #[test]
    fn default_template_is_dated_and_owned() {
        let t = default_template();
        let plan = build_plan(&t, {
            let mut n = 0;
            move || {
                n += 1;
                format!("t{n}")
            }
        });
        assert_eq!(plan.len(), t.tasks.len());
        assert!(plan.iter().all(|p| p.owner.is_some()));
        assert_eq!(
            task_due_date(Some(-7), Some(d("2026-07-01"))),
            Some(d("2026-06-24"))
        );
        assert_eq!(offset_label(Some(-1)), "1 day before the joining date");
    }
}
