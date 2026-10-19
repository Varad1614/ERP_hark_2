//! Pure contract and MoU rules: term ends, renewals, notice windows, recorded decisions, term
//! history, repeating obligations and dashboard numbers. Ported from HeyPuter/everything
//! `contracts/src/lib/dates.js` and `store.js` (MIT, pinned 06802a1); see THIRD_PARTY_NOTICES.md.
//!
//! Every function takes `today` explicitly: the server's business date is the only clock. Dates
//! are calendar days; month arithmetic clamps to the target month (Jan 31 + 1 month = Feb 28) and
//! a series is always counted from its anchor, so a month-end schedule never drifts.

use chrono::{Datelike, Duration, NaiveDate};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use utoipa::ToSchema;

/// Iteration cap against pathological data (an end date centuries in the past).
const MAX_STEPS: i32 = 1200;
/// Completed occurrences kept per obligation.
pub const MAX_OCCURRENCE_HISTORY: usize = 120;
/// Default "term ends soon" horizon.
pub const DEFAULT_SOON_DAYS: i64 = 60;

// ------------------------------------------------------------------ dates

/// `YYYY-MM-DD` that is a real calendar day.
pub fn parse_iso(v: &str) -> Option<NaiveDate> {
    let b = v.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    if !b
        .iter()
        .enumerate()
        .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
    {
        return None;
    }
    NaiveDate::parse_from_str(v, "%Y-%m-%d").ok()
}

pub fn add_days(d: NaiveDate, n: i64) -> NaiveDate {
    d + Duration::days(n)
}

/// Same day of month `n` months later, clamped to the target month's length.
pub fn add_months(d: NaiveDate, n: i32) -> NaiveDate {
    let total = d.year() * 12 + d.month0() as i32 + n;
    let y = total.div_euclid(12);
    let m = total.rem_euclid(12) as u32 + 1;
    let last = last_day(y, m);
    NaiveDate::from_ymd_opt(y, m, d.day().min(last)).unwrap_or(d)
}

fn last_day(y: i32, m: u32) -> u32 {
    let (ny, nm) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
    NaiveDate::from_ymd_opt(ny, nm, 1)
        .and_then(|f| f.pred_opt())
        .map_or(28, |d| d.day())
}

/// Whole days from `a` to `b` (positive when `b` is later).
pub fn days_between(a: NaiveDate, b: NaiveDate) -> i64 {
    (b - a).num_days()
}

/// "12 Mar 2026": compact and unambiguous (Indian day-month order).
pub fn format_date(d: NaiveDate) -> String {
    d.format("%-d %b %Y").to_string()
}

// ------------------------------------------------------------------ decisions

/// A decision recorded about a renewal, keyed to the term end it answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "kebab-case")]
pub enum DecisionKind {
    NoticeGiven,
    Renewing,
    NotRenewing,
    Snoozed,
}

impl DecisionKind {
    pub fn parse(v: &str) -> Option<Self> {
        Some(match v {
            "notice-given" => Self::NoticeGiven,
            "renewing" => Self::Renewing,
            "not-renewing" => Self::NotRenewing,
            "snoozed" => Self::Snoozed,
            _ => return None,
        })
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NoticeGiven => "notice-given",
            Self::Renewing => "renewing",
            Self::NotRenewing => "not-renewing",
            Self::Snoozed => "snoozed",
        }
    }
    /// "This term is the last one."
    pub fn ending(self) -> bool {
        matches!(self, Self::NoticeGiven | Self::NotRenewing)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Decision {
    pub kind: DecisionKind,
    /// The day it was recorded.
    pub date: Option<NaiveDate>,
    #[serde(default)]
    pub note: String,
    /// The term end it answers; once that term is over it stops applying.
    pub for_term_end: Option<NaiveDate>,
    /// A snooze holds until this day (inclusive).
    pub until: Option<NaiveDate>,
}

// ------------------------------------------------------------------ contract

/// The fields the rules read.
#[derive(Debug, Clone, Default)]
pub struct Contract {
    /// `active` | `draft` | `terminated`.
    pub status: String,
    pub start_date: Option<NaiveDate>,
    /// End of the first term: the anchor every renewal is counted from.
    pub end_date: Option<NaiveDate>,
    pub auto_renew: bool,
    pub term_months: i32,
    pub notice_days: i64,
    pub decision: Option<Decision>,
    pub value: Option<f64>,
    pub currency: String,
    /// `one-time` | `monthly` | `quarterly` | `annual`.
    pub billing: String,
}

impl Contract {
    fn term(&self) -> i32 {
        self.term_months.max(1)
    }
    fn ending_decision(&self) -> Option<&Decision> {
        self.decision.as_ref().filter(|d| d.kind.ending())
    }
}

/// The decision if it applies to `term_end`, else `None` (stale).
pub fn decision_for(c: &Contract, term_end: Option<NaiveDate>) -> Option<&Decision> {
    let d = c.decision.as_ref()?;
    if let (Some(t), Some(f)) = (term_end, d.for_term_end) {
        if t != f {
            return None;
        }
    }
    Some(d)
}

/// Whether a decision currently answers "what about this renewal?". A snooze stops holding
/// after its date.
pub fn decision_handles(d: Option<&Decision>, today: NaiveDate) -> bool {
    match d {
        None => false,
        Some(d) if d.kind == DecisionKind::Snoozed => d.until.is_some_and(|u| u >= today),
        Some(_) => true,
    }
}

/// Whether `date` is one of the contract's term ends: the end date, or on an auto-renewing
/// contract any renewal boundary after it.
pub fn is_term_end(c: &Contract, date: Option<NaiveDate>) -> bool {
    let (Some(end), Some(date)) = (c.end_date, date) else {
        return false;
    };
    if date == end {
        return true;
    }
    if !c.auto_renew || date < end {
        return false;
    }
    for i in 1..=MAX_STEPS {
        let b = add_months(end, c.term() * i);
        if b == date {
            return true;
        }
        if b > date {
            return false;
        }
    }
    false
}

/// The next end of term on or after `today`. Auto-renewing contracts roll forward by the
/// renewal term; a recorded "notice given" / "not renewing" caps the roll at the term it was
/// recorded for, while that date is still a term end. No end date = evergreen (`None`).
pub fn next_term_end(c: &Contract, today: NaiveDate) -> Option<NaiveDate> {
    let end = c.end_date?;
    let last = c
        .ending_decision()
        .and_then(|d| d.for_term_end)
        .filter(|f| is_term_end(c, Some(*f)));
    if !c.auto_renew {
        return Some(end);
    }
    let mut date = end;
    let mut i = 1;
    while i <= MAX_STEPS && date < today {
        date = add_months(end, c.term() * i);
        i += 1;
    }
    Some(match last {
        Some(l) if date > l => l,
        _ => date,
    })
}

/// The day the agreement itself stops: `None` for evergreen and for an auto-renewing contract
/// that nobody has decided to end.
pub fn agreement_end(c: &Contract, today: NaiveDate) -> Option<NaiveDate> {
    let end = next_term_end(c, today)?;
    if !c.auto_renew {
        return Some(end);
    }
    c.ending_decision()
        .filter(|d| d.for_term_end == Some(end))
        .map(|_| end)
}

/// A finished term.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Term {
    pub start: Option<NaiveDate>,
    pub end: NaiveDate,
}

/// Terms already finished, oldest first: one per crossed boundary.
pub fn completed_terms(c: &Contract, today: NaiveDate) -> Vec<Term> {
    let Some(end) = c.end_date else {
        return vec![];
    };
    if end >= today {
        return vec![];
    }
    if !c.auto_renew {
        return vec![Term {
            start: c.start_date,
            end,
        }];
    }
    let mut out = vec![];
    let mut start = c.start_date;
    let mut boundary = end;
    let mut i = 1;
    while i <= MAX_STEPS && boundary < today {
        out.push(Term {
            start,
            end: boundary,
        });
        start = Some(add_days(boundary, 1));
        boundary = add_months(end, c.term() * i);
        i += 1;
        if c.ending_decision()
            .is_some_and(|d| d.for_term_end == out.last().map(|t| t.end))
        {
            break;
        }
    }
    out
}

/// Where a contract's clock stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    /// Draft or terminated: no clock.
    None,
    /// No end date.
    Evergreen,
    Expired,
    /// Inside the notice window (cancel or renegotiate now).
    Notice,
    /// Ends within the "soon" horizon.
    Soon,
    Ok,
}

#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RenewalInfo {
    pub phase: Phase,
    /// Next term end (renewal or expiry).
    pub date: Option<NaiveDate>,
    pub days_left: Option<i64>,
    /// Cancellation-notice deadline.
    pub notice_date: Option<NaiveDate>,
    pub notice_days_left: Option<i64>,
    /// The decision recorded for this term end.
    pub decision: Option<Decision>,
    /// The decision answers the deadline.
    pub handled: bool,
}

pub fn renewal_info(c: &Contract, today: NaiveDate, soon_days: i64) -> RenewalInfo {
    let empty = |phase| RenewalInfo {
        phase,
        date: None,
        days_left: None,
        notice_date: None,
        notice_days_left: None,
        decision: None,
        handled: false,
    };
    if c.status == "terminated" || c.status == "draft" {
        return empty(Phase::None);
    }
    let Some(date) = next_term_end(c, today) else {
        return empty(Phase::Evergreen);
    };
    let decision = decision_for(c, Some(date)).cloned();
    let handled = decision_handles(decision.as_ref(), today);
    let days_left = days_between(today, date);
    if days_left < 0 {
        return RenewalInfo {
            phase: Phase::Expired,
            date: Some(date),
            days_left: Some(days_left),
            notice_date: None,
            notice_days_left: None,
            decision,
            handled,
        };
    }
    let notice_days = c.notice_days.max(0);
    let notice_date = (notice_days > 0).then(|| add_days(date, -notice_days));
    let phase = if notice_date.is_some() && days_left <= notice_days {
        Phase::Notice
    } else if days_left <= soon_days {
        Phase::Soon
    } else {
        Phase::Ok
    };
    RenewalInfo {
        phase,
        date: Some(date),
        days_left: Some(days_left),
        notice_date,
        notice_days_left: notice_date.map(|n| days_between(today, n)),
        decision,
        handled,
    }
}

/// "Waiting on you": an active contract in its notice window or past its end, with no decision
/// that answers it.
pub fn needs_attention(c: &Contract, info: &RenewalInfo) -> bool {
    c.status == "active" && !info.handled && matches!(info.phase, Phase::Notice | Phase::Expired)
}

// ------------------------------------------------------------------ term history

/// How a recorded term ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "kebab-case")]
pub enum TermEnding {
    Renewed,
    NoticeGiven,
    Expired,
    Terminated,
}

impl TermEnding {
    pub fn parse(v: &str) -> Self {
        match v {
            "renewed" => Self::Renewed,
            "notice-given" => Self::NoticeGiven,
            "terminated" => Self::Terminated,
            _ => Self::Expired,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Renewed => "renewed",
            Self::NoticeGiven => "notice-given",
            Self::Expired => "expired",
            Self::Terminated => "terminated",
        }
    }
}

/// A term to add to the history.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TermRecord {
    pub start: Option<NaiveDate>,
    pub end: NaiveDate,
    pub ended: TermEnding,
    pub note: String,
}

/// What has to change because time passed.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TermPatch {
    pub additions: Vec<TermRecord>,
    /// Notice was given for a term that is now over: close the contract out.
    pub terminate: bool,
    /// The decision is about a term the contract has moved past.
    pub clear_decision: bool,
}

/// Terms that ended get recorded, a contract whose notice was given closes on that term end
/// instead of rolling forward, and a decision whose term is over is cleared. `known_ends` are
/// the term ends already in the history. `None` = nothing to do.
pub fn term_reconciliation(
    c: &Contract,
    known_ends: &[NaiveDate],
    today: NaiveDate,
) -> Option<TermPatch> {
    if c.status != "active" {
        return None;
    }
    let done = completed_terms(c, today);
    if done.is_empty() {
        return None;
    }
    let decision = c.decision.as_ref();
    let mut patch = TermPatch::default();
    for t in &done {
        let by = decision.filter(|d| d.for_term_end == Some(t.end) && d.kind.ending());
        if by.is_some() {
            patch.terminate = true;
        }
        if !known_ends.contains(&t.end) {
            patch.additions.push(TermRecord {
                start: t.start,
                end: t.end,
                ended: match by {
                    Some(d) if d.kind == DecisionKind::NoticeGiven => TermEnding::NoticeGiven,
                    Some(_) => TermEnding::Expired,
                    None if c.auto_renew => TermEnding::Renewed,
                    None => TermEnding::Expired,
                },
                note: by.map(|d| d.note.clone()).unwrap_or_default(),
            });
        }
        if by.is_some() {
            break;
        }
    }
    // A decision goes stale when the contract has moved past the term it was about; a fixed
    // term that lapsed still ends on the same date, so its decision stays.
    patch.clear_decision = decision.is_some_and(|d| {
        d.for_term_end.is_some_and(|f| {
            done.iter().any(|t| t.end == f)
                && (patch.terminate || next_term_end(c, today) != Some(f))
        })
    });
    if patch.additions.is_empty() && !patch.terminate && !patch.clear_decision {
        return None;
    }
    Some(patch)
}

/// The day a closed-out contract actually ended: the last recorded term, else the end date.
pub fn ended_on(c: &Contract, history_ends: &[NaiveDate]) -> Option<NaiveDate> {
    let last = history_ends.iter().max().copied();
    match (last, c.end_date) {
        (Some(l), Some(e)) if l >= e => Some(l),
        (Some(l), None) => Some(l),
        (_, e) => e,
    }
}

// ------------------------------------------------------------------ obligations

/// Who owes an obligation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Owner {
    Us,
    Them,
    Mutual,
}

impl Owner {
    pub fn parse(v: &str) -> Self {
        match v {
            "them" => Self::Them,
            "mutual" => Self::Mutual,
            _ => Self::Us,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Us => "us",
            Self::Them => "them",
            Self::Mutual => "mutual",
        }
    }
}

/// Months per repeat cadence (`monthly` 1, `quarterly` 3, `annually` 12; anything else 0).
pub fn repeat_months(every: &str) -> i32 {
    match every {
        "monthly" => 1,
        "quarterly" => 3,
        "annually" => 12,
        _ => 0,
    }
}

/// A repeating schedule: one live occurrence, completed ones kept as history. Exactly one end
/// condition: a date, a count, or the contract's own end (or none).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Repeat {
    pub every: String,
    pub anchor: NaiveDate,
    pub until: Option<NaiveDate>,
    pub count: Option<i32>,
    #[serde(default)]
    pub contract_end: bool,
}

/// Repeat input as a client sends it.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RepeatInput {
    pub every: Option<String>,
    pub anchor: Option<String>,
    pub until: Option<String>,
    pub count: Option<f64>,
    #[serde(default)]
    pub contract_end: bool,
}

/// A cadence and a first date, else no schedule at all; the first end condition wins.
pub fn normalize_repeat(r: Option<&RepeatInput>, due: Option<NaiveDate>) -> Option<Repeat> {
    let r = r?;
    let every = r.every.as_deref().filter(|e| repeat_months(e) > 0)?;
    let anchor = r.anchor.as_deref().and_then(parse_iso).or(due)?;
    let mut out = Repeat {
        every: every.to_owned(),
        anchor,
        until: None,
        count: None,
        contract_end: false,
    };
    if let Some(u) = r.until.as_deref().and_then(parse_iso) {
        out.until = Some(u);
    } else if let Some(n) = r.count.filter(|n| n.is_finite() && *n >= 1.0) {
        out.count = Some(n.round().min(240.0) as i32);
    } else if r.contract_end {
        out.contract_end = true;
    }
    Some(out)
}

/// Due date of occurrence `index` (0-based), counted from the anchor.
pub fn occurrence_due(r: &Repeat, index: i32) -> NaiveDate {
    add_months(r.anchor, repeat_months(&r.every) * index)
}

/// The anchor that puts `due` at occurrence `completed`, keeping the existing anchor whenever it
/// still does (a month-end anchor cannot be rebuilt from a clamped occurrence).
pub fn repeat_anchor(
    due: NaiveDate,
    every: &str,
    completed: i32,
    existing: Option<&Repeat>,
) -> NaiveDate {
    if let Some(e) = existing {
        if e.every == every && add_months(e.anchor, repeat_months(every) * completed) == due {
            return e.anchor;
        }
    }
    if completed > 0 {
        add_months(due, -repeat_months(every) * completed)
    } else {
        due
    }
}

/// Whether occurrence `index` is still inside the series ("until the contract ends" means the
/// agreement's end, not the next renewal).
pub fn within_series(r: &Repeat, index: i32, c: Option<&Contract>, today: NaiveDate) -> bool {
    if index < 0 {
        return false;
    }
    if r.count.is_some_and(|n| index >= n) {
        return false;
    }
    let due = occurrence_due(r, index);
    if r.until.is_some_and(|u| due > u) {
        return false;
    }
    if r.contract_end {
        if let Some(end) = c.and_then(|c| agreement_end(c, today)) {
            if due > end {
                return false;
            }
        }
    }
    true
}

/// An obligation's schedule state.
#[derive(Debug, Clone, PartialEq)]
pub struct Obligation {
    pub due: Option<NaiveDate>,
    pub done: bool,
    pub repeat: Option<Repeat>,
    /// Due dates of completed occurrences, oldest first.
    pub history: Vec<Option<NaiveDate>>,
}

/// Tick off the live occurrence: record it and schedule the next, or finish the series.
pub fn advance_occurrence(c: &Contract, ob: &Obligation, today: NaiveDate) -> Obligation {
    let mut history = ob.history.clone();
    history.push(ob.due);
    let next = history.len() as i32;
    match &ob.repeat {
        Some(r) if within_series(r, next, Some(c), today) => Obligation {
            due: Some(occurrence_due(r, next)),
            done: false,
            repeat: ob.repeat.clone(),
            history,
        },
        _ => Obligation {
            done: true,
            history,
            ..ob.clone()
        },
    }
}

/// A finished series an edit has given room again gets its next occurrence back.
pub fn reopened_series(c: &Contract, ob: &Obligation, today: NaiveDate) -> Obligation {
    let Some(r) = ob.repeat.as_ref().filter(|_| ob.done) else {
        return ob.clone();
    };
    let completed = ob.history.len() as i32;
    if !within_series(r, completed, Some(c), today) {
        return ob.clone();
    }
    Obligation {
        done: false,
        due: Some(occurrence_due(r, completed)),
        ..ob.clone()
    }
}

/// Un-tick: the most recently completed occurrence comes back to the front.
pub fn rewind_occurrence(ob: &Obligation) -> Obligation {
    let mut history = ob.history.clone();
    match history.pop() {
        None => Obligation {
            done: false,
            ..ob.clone()
        },
        Some(last) => Obligation {
            done: false,
            due: last.or(ob.due),
            history,
            repeat: ob.repeat.clone(),
        },
    }
}

/// Keep the newest [`MAX_OCCURRENCE_HISTORY`] completions and re-anchor so the live occurrence
/// stays the one after them.
pub fn trim_history(ob: &mut Obligation) {
    let n = ob.history.len();
    if n <= MAX_OCCURRENCE_HISTORY {
        return;
    }
    ob.history.drain(..n - MAX_OCCURRENCE_HISTORY);
    if let (Some(r), Some(due)) = (ob.repeat.as_mut(), ob.due) {
        let kept = ob.history.len() as i32;
        let live = if ob.done && kept > 0 { kept - 1 } else { kept };
        r.anchor = repeat_anchor(due, &r.every.clone(), live, None);
    }
}

/// One row of the obligations list: `(contract index, obligation index, days left)`.
pub type ObligationRow = (usize, usize, Option<i64>);

/// Every obligation on active contracts, most urgent first, undated after dated, completed
/// last (only when `include_done`).
pub fn all_obligations(
    contracts: &[(&Contract, &[Obligation])],
    today: NaiveDate,
    include_done: bool,
) -> Vec<ObligationRow> {
    let mut out: Vec<(ObligationRow, bool)> = vec![];
    for (ci, (c, obs)) in contracts.iter().enumerate() {
        if c.status != "active" {
            continue;
        }
        for (oi, ob) in obs.iter().enumerate() {
            if ob.done && !include_done {
                continue;
            }
            out.push(((ci, oi, ob.due.map(|d| days_between(today, d))), ob.done));
        }
    }
    out.sort_by(|(a, ad), (b, bd)| {
        if ad != bd {
            return ad.cmp(bd);
        }
        match (a.2, b.2) {
            (None, None) => std::cmp::Ordering::Equal,
            (None, _) => std::cmp::Ordering::Greater,
            (_, None) => std::cmp::Ordering::Less,
            (Some(x), Some(y)) => x.cmp(&y),
        }
    });
    out.into_iter().map(|(r, _)| r).collect()
}

/// Open obligations overdue or due within `days`.
pub fn due_obligations(
    contracts: &[(&Contract, &[Obligation])],
    today: NaiveDate,
    days: i64,
) -> Vec<ObligationRow> {
    all_obligations(contracts, today, false)
        .into_iter()
        .filter(|r| r.2.is_some_and(|d| d <= days))
        .collect()
}

// ------------------------------------------------------------------ money and stats

/// Billing periods per year (`one-time` and unknown = 0).
pub fn per_year(billing: &str) -> f64 {
    match billing {
        "monthly" => 12.0,
        "quarterly" => 4.0,
        "annual" => 1.0,
        _ => 0.0,
    }
}

/// Value as a yearly figure; 0 for one-time or unpriced.
pub fn annualized_value(c: &Contract) -> f64 {
    match c.value {
        Some(v) if v.is_finite() && v > 0.0 => v * per_year(&c.billing),
        _ => 0.0,
    }
}

/// Three-letter code upper-cased, else `fallback`.
pub fn normalize_currency(code: &str, fallback: &str) -> String {
    let c = code.trim().to_ascii_uppercase();
    if c.len() == 3 && c.bytes().all(|b| b.is_ascii_uppercase()) {
        c
    } else {
        fallback.to_owned()
    }
}

/// Dashboard numbers. Values are never summed across currencies.
#[derive(Debug, Clone, Default, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ContractStats {
    pub active: i64,
    pub expiring60: i64,
    pub open_obligations: i64,
    pub expired: i64,
    /// Yearly value by currency.
    pub annual_by_currency: BTreeMap<String, f64>,
}

pub fn contract_stats(contracts: &[(&Contract, &[Obligation])], today: NaiveDate) -> ContractStats {
    let mut s = ContractStats::default();
    for (c, obs) in contracts {
        let info = renewal_info(c, today, DEFAULT_SOON_DAYS);
        if info.phase == Phase::Expired {
            s.expired += 1;
        }
        if c.status != "active" {
            continue;
        }
        // A lapsed term does not settle what is still owed under it.
        s.open_obligations += obs.iter().filter(|o| !o.done).count() as i64;
        if info.phase == Phase::Expired {
            continue;
        }
        s.active += 1;
        let v = annualized_value(c);
        if v != 0.0 {
            *s.annual_by_currency
                .entry(normalize_currency(&c.currency, "INR"))
                .or_default() += v;
        }
        if info.days_left.is_some_and(|d| d <= 60) {
            s.expiring60 += 1;
        }
    }
    s
}

/// Contracts (indices) whose next term end is within `days`, nearest first.
pub fn upcoming_renewals(contracts: &[&Contract], today: NaiveDate, days: i64) -> Vec<usize> {
    let mut v: Vec<(usize, i64)> = contracts
        .iter()
        .enumerate()
        .filter_map(|(i, c)| {
            let info = renewal_info(c, today, DEFAULT_SOON_DAYS);
            info.date?;
            info.days_left
                .filter(|d| (0..=days).contains(d))
                .map(|d| (i, d))
        })
        .collect();
    v.sort_by_key(|(_, d)| *d);
    v.into_iter().map(|(i, _)| i).collect()
}

/// Indian digit grouping: 1234567.5 → "12,34,567.50".
pub fn format_inr(v: f64) -> String {
    let neg = v < 0.0;
    let s = format!("{:.2}", v.abs());
    let (int, frac) = s.split_once('.').unwrap_or((&s, "00"));
    let mut out = String::new();
    let n = int.len();
    for (i, ch) in int.chars().enumerate() {
        let left = n - i;
        if i > 0 && (left == 3 || (left > 3 && (left - 3) % 2 == 0)) {
            out.push(',');
        }
        out.push(ch);
    }
    format!("{}{out}.{frac}", if neg { "-" } else { "" })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        parse_iso(s).unwrap()
    }

    #[test]
    fn months_clamp_and_anchor() {
        assert_eq!(add_months(d("2026-01-31"), 1), d("2026-02-28"));
        assert_eq!(add_months(d("2024-01-31"), 1), d("2024-02-29"));
        assert_eq!(add_months(d("2026-03-15"), -3), d("2025-12-15"));
        assert!(parse_iso("2026-02-30").is_none());
        assert!(parse_iso("2026-8-5").is_none());
    }

    #[test]
    fn rolls_forward_until_notice() {
        let mut c = Contract {
            status: "active".into(),
            end_date: Some(d("2025-03-31")),
            auto_renew: true,
            term_months: 12,
            notice_days: 30,
            ..Default::default()
        };
        assert_eq!(next_term_end(&c, d("2026-10-17")), Some(d("2027-03-31")));
        assert_eq!(agreement_end(&c, d("2026-10-17")), None);
        c.decision = Some(Decision {
            kind: DecisionKind::NoticeGiven,
            date: None,
            note: String::new(),
            for_term_end: Some(d("2026-03-31")),
            until: None,
        });
        assert_eq!(next_term_end(&c, d("2026-10-17")), Some(d("2026-03-31")));
        let p = term_reconciliation(&c, &[], d("2026-10-17")).unwrap();
        assert!(p.terminate);
        assert_eq!(p.additions.len(), 2);
        assert_eq!(p.additions[1].ended, TermEnding::NoticeGiven);
    }

    #[test]
    fn inr_grouping() {
        assert_eq!(format_inr(1234567.5), "12,34,567.50");
        assert_eq!(format_inr(999.0), "999.00");
        assert_eq!(format_inr(100000.0), "1,00,000.00");
    }
}
