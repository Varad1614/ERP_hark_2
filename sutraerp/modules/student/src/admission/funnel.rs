//! **ERP_EXTENSIONS 2A** — admissions funnel maths, ported from HeyPuter/everything
//! `src/crm/src/lib/funnel.js`, `forecast.js` and `dupes.js` (MIT; see THIRD_PARTY_NOTICES.md).
//!
//! Pure functions over plain enquiries ("deals" in the original) and the institution's
//! configured stage list. The terminal stages are `admitted` (Everything's `won`) and `lost`;
//! the golden test maps the two names. Figures here are analytics, not ledger money: they mirror
//! the original's float maths (`round2`) so the golden vectors replay bit for bit, and the HTTP
//! layer prints them to paise.
//!
//! * [`funnel_report`] — per stage: entered, advanced, lost here, still open, advance rate.
//! * [`forecast_by_month`] — expected admissions (count and fee value) by expected-joining
//!   month, probability-weighted.
//! * [`win_rate`], [`win_rate_by`], [`close_reason_breakdown`], [`stage_velocity`],
//!   [`stalled`], [`avg_closed_value`], [`avg_cycle_days`] — the forecast page's side tables.
//! * [`contact_duplicates`] — possible duplicate enquirers (same e-mail, name or phone after
//!   normalisation), each paired with the oldest record of its group.

use chrono::{DateTime, Datelike, NaiveDate};
use std::collections::HashMap;
use unicode_normalization::UnicodeNormalization;

/// Terminal stage of an enquiry that became an admission (Everything: `won`).
pub const ADMITTED: &str = "admitted";
/// Terminal stage of an enquiry that dropped out.
pub const LOST: &str = "lost";
/// A board with more columns than this stops being readable.
pub const MAX_OPEN_STAGES: usize = 8;
/// How long an enquiry may sit in one stage before it is flagged.
pub const DEFAULT_STALE_DAYS: i64 = 14;
const DAY_MS: f64 = 86_400_000.0;

/// `Math.round(n * 100) / 100`.
pub fn round2(n: f64) -> f64 {
    js_round(n * 100.0) / 100.0
}

/// JavaScript `Math.round`: halves go towards +∞.
fn js_round(x: f64) -> f64 {
    let f = x.floor();
    if x - f >= 0.5 {
        f + 1.0
    } else {
        f
    }
}

fn clamp01(n: f64) -> f64 {
    n.clamp(0.0, 1.0)
}

pub fn is_terminal(stage: &str) -> bool {
    stage == ADMITTED || stage == LOST
}

/// One funnel stage with its win probability.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Stage {
    pub id: String,
    pub label: String,
    pub probability: f64,
}

impl Stage {
    fn new(id: &str, label: &str, probability: f64) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            probability,
        }
    }
}

/// The out-of-the-box admissions funnel, used until the institution configures its own.
pub fn default_open_stages() -> Vec<Stage> {
    vec![
        Stage::new("enquiry", "Enquiry", 0.1),
        Stage::new("counselling", "Counselling", 0.25),
        Stage::new("campus_visit", "Campus visit", 0.4),
        Stage::new("applied", "Applied", 0.6),
        Stage::new("offered", "Offered", 0.8),
    ]
}

fn terminal_stages() -> [Stage; 2] {
    [
        Stage::new(ADMITTED, "Admitted", 1.0),
        Stage::new(LOST, "Lost", 0.0),
    ]
}

/// Drops anything that would break the board: non-objects, blank / duplicate ids, ids that
/// collide with the terminal stages, and everything past the cap. A probability that is not a
/// number (or a string spelling one) falls back to 0.5.
fn sanitize_open_stages(list: &[serde_json::Value]) -> Vec<Stage> {
    let mut seen: Vec<String> = Vec::new();
    let mut out = Vec::new();
    for s in list {
        let Some(o) = s.as_object() else { continue };
        let id = o
            .get("id")
            .and_then(|v| v.as_str())
            .map(|v| v.trim().to_owned())
            .unwrap_or_default();
        if id.is_empty() || is_terminal(&id) || seen.contains(&id) {
            continue;
        }
        let label = o
            .get("label")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| id.clone());
        let p = match o.get("probability") {
            Some(serde_json::Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
            Some(serde_json::Value::String(t)) if !t.trim().is_empty() => js_number(t),
            _ => f64::NAN,
        };
        seen.push(id.clone());
        out.push(Stage {
            id,
            label,
            probability: if p.is_finite() { clamp01(p) } else { 0.5 },
        });
        if out.len() >= MAX_OPEN_STAGES {
            break;
        }
    }
    out
}

/// `Number(text)` for the decimal spellings a settings export carries.
fn js_number(t: &str) -> f64 {
    t.trim().parse::<f64>().unwrap_or(f64::NAN)
}

/// The stage list a settings object describes: the configured open stages when there are any,
/// else `defaults`, with Admitted and Lost pinned on the end either way.
pub fn resolve_stages(custom: &[serde_json::Value], defaults: &[Stage]) -> Vec<Stage> {
    let mut open = sanitize_open_stages(custom);
    if open.is_empty() {
        open = defaults.to_vec();
    }
    open.extend(terminal_stages());
    open
}

pub fn open_stages(stages: &[Stage]) -> Vec<&Stage> {
    stages.iter().filter(|s| !is_terminal(&s.id)).collect()
}

/// The stage with `id`, else the first stage (the original's `stageInfo`).
pub fn stage_info<'a>(id: &str, stages: &'a [Stage]) -> Option<&'a Stage> {
    stages.iter().find(|s| s.id == id).or(stages.first())
}

/// One recorded stage entry: the stage and when it was entered (ms since the epoch).
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StageEntry {
    pub stage: Option<String>,
    pub at: Option<f64>,
}

/// An enquiry as the funnel maths sees it.
#[derive(Debug, Clone, Default, PartialEq, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Enquiry {
    pub id: String,
    pub stage: String,
    pub stage_history: Vec<StageEntry>,
    /// Expected fee value (rupees).
    pub value: f64,
    /// Per-enquiry win probability (0..1); `None` uses the stage's.
    pub confidence: Option<f64>,
    /// Expected joining date (`YYYY-MM-DD`).
    pub expected_close: Option<String>,
    /// When it closed (ms since the epoch).
    pub closed_at: Option<f64>,
    pub created_at: Option<f64>,
    pub updated_at: Option<f64>,
    pub close_reason: Option<String>,
    pub source: Option<String>,
}

impl Enquiry {
    pub fn is_open(&self) -> bool {
        !is_terminal(&self.stage)
    }
}

// ---------------------------------------------------------------- funnel (funnel.js)

/// One stage row of the funnel.
#[derive(Debug, Clone, PartialEq, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct FunnelRow {
    pub stage: String,
    pub label: String,
    /// A stage only the history remembers (removed from the funnel since).
    pub gone: bool,
    pub entered: u32,
    pub advanced: u32,
    pub lost_here: u32,
    pub open: u32,
    pub advance_rate: Option<f64>,
    pub loss_share: Option<f64>,
}

/// Totals over the tracked enquiries.
#[derive(Debug, Clone, PartialEq, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct FunnelTotals {
    pub enquiries: u32,
    pub admitted: u32,
    pub lost: u32,
    pub open: u32,
    pub admitted_rate: Option<f64>,
    pub lost_share_base: u32,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct FunnelReport {
    pub rows: Vec<FunnelRow>,
    pub totals: FunnelTotals,
    /// Enquiries closed with no recorded journey through the open stages (left out).
    pub untracked: u32,
}

/// When the enquiry joined the funnel: its first timed stage entry, else its own clock.
pub fn entered_at(e: &Enquiry) -> f64 {
    e.stage_history
        .iter()
        .find_map(|h| h.at.filter(|a| a.is_finite()))
        .unwrap_or_else(|| e.created_at.unwrap_or(0.0))
}

/// The stage a lost enquiry was in when it dropped: the last open stage in its history.
pub fn lost_from_stage(e: &Enquiry) -> Option<&str> {
    if e.stage != LOST {
        return None;
    }
    e.stage_history
        .iter()
        .rev()
        .filter_map(|h| h.stage.as_deref())
        .find(|s| !s.is_empty() && !is_terminal(s))
}

/// Has the enquiry any recorded journey through the open funnel at all?
pub fn has_open_history(e: &Enquiry) -> bool {
    e.stage_history
        .iter()
        .any(|h| h.stage.as_deref().is_some_and(|s| !s.is_empty() && !is_terminal(s)))
        || (!e.stage.is_empty() && !is_terminal(&e.stage))
}

/// The stages an enquiry has been in (insertion order) and the furthest position it held.
fn journey<'a>(e: &'a Enquiry, order: &HashMap<&str, usize>) -> (Vec<&'a str>, Option<usize>) {
    let mut reached: Vec<&str> = Vec::new();
    let mut furthest: Option<usize> = None;
    let mut visit = |stage: &'a str| {
        if !reached.contains(&stage) {
            reached.push(stage);
        }
        if let Some(&at) = order.get(stage) {
            if furthest.is_none_or(|f| at > f) {
                furthest = Some(at);
            }
        }
    };
    for h in &e.stage_history {
        if let Some(s) = h.stage.as_deref().filter(|s| !s.is_empty()) {
            visit(s);
        }
    }
    if !e.stage.is_empty() {
        visit(&e.stage);
    }
    (reached, furthest)
}

/// The funnel: per stage, how many enquiries entered it, advanced past it, dropped there, and
/// are still sitting in it. Rows follow the configured order, then any stage only the history
/// remembers.
pub fn funnel_report(enquiries: &[Enquiry], stages: &[Stage]) -> FunnelReport {
    let open = open_stages(stages);
    let mut order: HashMap<&str, usize> = open
        .iter()
        .enumerate()
        .map(|(i, s)| (s.id.as_str(), i))
        .collect();
    order.insert(ADMITTED, open.len());

    let tracked: Vec<&Enquiry> = enquiries.iter().filter(|e| has_open_history(e)).collect();
    let untracked = (enquiries.len() - tracked.len()) as u32;

    #[derive(Default)]
    struct Bucket {
        entered: u32,
        advanced: u32,
        lost_here: u32,
        open: u32,
    }
    let mut buckets: Vec<(String, Bucket)> = open
        .iter()
        .map(|s| (s.id.clone(), Bucket::default()))
        .collect();
    let mut totals = FunnelTotals {
        enquiries: tracked.len() as u32,
        admitted: 0,
        lost: 0,
        open: 0,
        admitted_rate: None,
        lost_share_base: 0,
    };
    for e in &tracked {
        match e.stage.as_str() {
            ADMITTED => totals.admitted += 1,
            LOST => totals.lost += 1,
            _ => totals.open += 1,
        }
        let (reached, furthest) = journey(e, &order);
        let died_in = lost_from_stage(e);
        for stage in reached {
            if is_terminal(stage) {
                continue;
            }
            let i = match buckets.iter().position(|(s, _)| s == stage) {
                Some(i) => i,
                None => {
                    buckets.push((stage.to_owned(), Bucket::default()));
                    buckets.len() - 1
                }
            };
            let row = &mut buckets[i].1;
            row.entered += 1;
            let advanced = match order.get(stage) {
                None => e.stage == ADMITTED || (e.stage != LOST && e.stage != stage),
                Some(&at) => furthest.is_some_and(|f| f > at),
            };
            if advanced {
                row.advanced += 1;
            } else if died_in == Some(stage) {
                row.lost_here += 1;
            } else if e.stage != LOST {
                row.open += 1;
            }
        }
    }
    let lost_total: u32 = buckets.iter().map(|(_, b)| b.lost_here).sum();
    // Stable sort: unknown stages keep the order they were first met in.
    buckets.sort_by_key(|(s, _)| order.get(s.as_str()).copied().unwrap_or(usize::MAX));
    let rows = buckets
        .into_iter()
        .map(|(stage, b)| FunnelRow {
            label: stages
                .iter()
                .find(|s| s.id == stage)
                .map(|s| s.label.clone())
                .filter(|l| !l.is_empty())
                .unwrap_or_else(|| stage.clone()),
            gone: !open.iter().any(|s| s.id == stage),
            advance_rate: (b.entered > 0).then(|| round2(b.advanced as f64 / b.entered as f64)),
            loss_share: (lost_total > 0).then(|| round2(b.lost_here as f64 / lost_total as f64)),
            stage,
            entered: b.entered,
            advanced: b.advanced,
            lost_here: b.lost_here,
            open: b.open,
        })
        .collect();
    totals.admitted_rate = (totals.enquiries > 0)
        .then(|| round2(totals.admitted as f64 / totals.enquiries as f64));
    totals.lost_share_base = lost_total;
    FunnelReport {
        rows,
        totals,
        untracked,
    }
}

// ---------------------------------------------------------------- forecast (forecast.js)

/// Effective win probability: Admitted 1 and Lost 0; else the enquiry's own confidence, else
/// the institution's override for its stage, else the stage's probability.
pub fn probability_of(e: &Enquiry, overrides: &HashMap<String, f64>, stages: &[Stage]) -> f64 {
    if e.stage == ADMITTED {
        return 1.0;
    }
    if e.stage == LOST {
        return 0.0;
    }
    if let Some(c) = e.confidence.filter(|c| c.is_finite()) {
        return clamp01(c);
    }
    if let Some(o) = overrides.get(&e.stage).filter(|o| o.is_finite()) {
        return clamp01(*o);
    }
    stage_info(&e.stage, stages).map_or(0.0, |s| s.probability)
}

pub fn weighted_value(e: &Enquiry, overrides: &HashMap<String, f64>, stages: &[Stage]) -> f64 {
    round2(round2(e.value) * probability_of(e, overrides, stages))
}

/// Unweighted value of the open enquiries.
pub fn pipeline_value(enquiries: &[Enquiry]) -> f64 {
    round2(
        enquiries
            .iter()
            .filter(|e| e.is_open())
            .fold(0.0, |s, e| s + round2(e.value)),
    )
}

/// Probability-weighted value of the open enquiries.
pub fn weighted_pipeline(
    enquiries: &[Enquiry],
    overrides: &HashMap<String, f64>,
    stages: &[Stage],
) -> f64 {
    round2(
        enquiries
            .iter()
            .filter(|e| e.is_open())
            .fold(0.0, |s, e| s + weighted_value(e, overrides, stages)),
    )
}

/// Expected admissions (fee value) per stage, in configured order.
#[derive(Debug, Clone, PartialEq, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct StageSummary {
    pub stage: String,
    pub label: String,
    pub count: u32,
    pub value: f64,
    pub weighted: f64,
}

pub fn stage_summary(
    enquiries: &[Enquiry],
    overrides: &HashMap<String, f64>,
    stages: &[Stage],
) -> Vec<StageSummary> {
    stages
        .iter()
        .map(|s| {
            let here: Vec<&Enquiry> = enquiries.iter().filter(|e| e.stage == s.id).collect();
            StageSummary {
                stage: s.id.clone(),
                label: s.label.clone(),
                count: here.len() as u32,
                value: round2(here.iter().fold(0.0, |a, e| a + round2(e.value))),
                weighted: round2(
                    here.iter()
                        .fold(0.0, |a, e| a + weighted_value(e, overrides, stages)),
                ),
            }
        })
        .collect()
}

/// `YYYY-MM` of a `YYYY-MM-DD…` string.
pub fn month_key_of(date: &str) -> String {
    date.chars().take(7).collect()
}

/// `YYYY-MM` of an instant (ms since the epoch) at a fixed UTC offset in minutes.
pub fn month_key_at(ms: f64, offset_minutes: i32) -> String {
    let secs = (ms / 1000.0).floor() as i64 + i64::from(offset_minutes) * 60;
    let d = DateTime::from_timestamp(secs, 0).map_or_else(
        || NaiveDate::from_ymd_opt(1970, 1, 1).unwrap_or_default(),
        |t| t.date_naive(),
    );
    format!("{:04}-{:02}", d.year(), d.month())
}

/// `key` shifted by `n` months.
pub fn shift_month_key(key: &str, n: i32) -> String {
    let (y, m) = key.split_once('-').unwrap_or(("1970", "01"));
    let total = y.parse::<i32>().unwrap_or(1970) * 12 + (m.parse::<i32>().unwrap_or(1) - 1) + n;
    format!("{:04}-{:02}", total.div_euclid(12), total.rem_euclid(12) + 1)
}

/// One month of the forecast.
#[derive(Debug, Clone, PartialEq, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ForecastMonth {
    pub key: String,
    /// Fee value of the open enquiries expected to join this month (unweighted).
    pub best: f64,
    pub weighted: f64,
    /// Fee value admitted this month.
    pub won: f64,
    /// Enquiries counted in this month.
    pub count: u32,
    /// Probability-weighted number of joiners (not in the original: the seats view).
    pub weighted_count: f64,
}

/// Monthly forecast for `months` months from `from` (`YYYY-MM`). Open enquiries bucket by
/// expected joining month (a past date rolls into the first month); admitted ones count into
/// the month they closed (`now_key` when no close time is recorded). Close instants are read
/// at `offset_minutes` (IST is 330).
pub fn forecast_by_month(
    enquiries: &[Enquiry],
    months: u32,
    from: &str,
    now_key: &str,
    offset_minutes: i32,
    overrides: &HashMap<String, f64>,
    stages: &[Stage],
) -> Vec<ForecastMonth> {
    let mut rows: Vec<ForecastMonth> = (0..months as i32)
        .map(|i| ForecastMonth {
            key: shift_month_key(from, i),
            best: 0.0,
            weighted: 0.0,
            won: 0.0,
            count: 0,
            weighted_count: 0.0,
        })
        .collect();
    for e in enquiries {
        if e.stage == LOST {
            continue;
        }
        let amount = round2(e.value);
        if e.stage == ADMITTED {
            let start = e
                .closed_at
                .map_or_else(|| now_key.to_owned(), |c| month_key_at(c, offset_minutes));
            if let Some(row) = rows.iter_mut().find(|r| r.key == start) {
                row.won = round2(row.won + amount);
                row.count += 1;
            }
            continue;
        }
        let mut start = e
            .expected_close
            .as_deref()
            .filter(|d| !d.is_empty())
            .map_or_else(|| from.to_owned(), month_key_of);
        if start.as_str() < from {
            start = from.to_owned();
        }
        let p = probability_of(e, overrides, stages);
        if let Some(row) = rows.iter_mut().find(|r| r.key == start) {
            row.best = round2(row.best + amount);
            row.weighted = round2(row.weighted + round2(amount * p));
            row.count += 1;
            row.weighted_count = round2(row.weighted_count + p);
        }
    }
    rows
}

/// Admitted / lost counts and the rate (`None` until something has closed).
#[derive(Debug, Clone, PartialEq, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WinRate {
    pub admitted: u32,
    pub lost: u32,
    pub rate: Option<f64>,
}

pub fn win_rate(enquiries: &[Enquiry]) -> WinRate {
    let admitted = enquiries.iter().filter(|e| e.stage == ADMITTED).count() as u32;
    let lost = enquiries.iter().filter(|e| e.stage == LOST).count() as u32;
    let closed = admitted + lost;
    WinRate {
        admitted,
        lost,
        rate: (closed > 0).then(|| admitted as f64 / closed as f64),
    }
}

/// Win rate of one group (a lead source, a programme…).
#[derive(Debug, Clone, PartialEq, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GroupRate {
    pub key: String,
    pub admitted: u32,
    pub lost: u32,
    pub admitted_value: f64,
    pub total: u32,
    pub rate: f64,
}

/// Win rate grouped by `label_of`; enquiries without a label are skipped. Biggest group first.
pub fn win_rate_by<F: Fn(&Enquiry) -> Option<String>>(
    enquiries: &[Enquiry],
    label_of: F,
) -> Vec<GroupRate> {
    let mut groups: Vec<GroupRate> = Vec::new();
    for e in enquiries {
        if e.stage != ADMITTED && e.stage != LOST {
            continue;
        }
        let key = label_of(e).unwrap_or_default().trim().to_owned();
        if key.is_empty() {
            continue;
        }
        let i = match groups.iter().position(|g| g.key == key) {
            Some(i) => i,
            None => {
                groups.push(GroupRate {
                    key,
                    admitted: 0,
                    lost: 0,
                    admitted_value: 0.0,
                    total: 0,
                    rate: 0.0,
                });
                groups.len() - 1
            }
        };
        let g = &mut groups[i];
        if e.stage == ADMITTED {
            g.admitted += 1;
            g.admitted_value = round2(g.admitted_value + round2(e.value));
        } else {
            g.lost += 1;
        }
    }
    for g in &mut groups {
        g.total = g.admitted + g.lost;
        g.rate = g.admitted as f64 / g.total as f64;
    }
    groups.sort_by(|a, b| {
        b.total
            .cmp(&a.total)
            .then(b.admitted.cmp(&a.admitted))
            .then_with(|| js_locale_cmp(&a.key, &b.key))
    });
    groups
}

/// A stand-in for `localeCompare` on the keys a funnel groups by: case-insensitive first,
/// then lower case before upper, then code points.
fn js_locale_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let upper = |s: &str| s.chars().map(char::is_uppercase).collect::<Vec<_>>();
    a.to_lowercase()
        .cmp(&b.to_lowercase())
        .then_with(|| upper(a).cmp(&upper(b)))
        .then_with(|| a.cmp(b))
}

/// Closed enquiries of one stage grouped by recorded reason, plus how many have none.
#[derive(Debug, Clone, PartialEq, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReasonBreakdown {
    pub rows: Vec<ReasonRow>,
    pub unrecorded: u32,
    pub total: u32,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReasonRow {
    pub reason: String,
    pub count: u32,
    pub value: f64,
}

pub fn close_reason_breakdown(enquiries: &[Enquiry], stage: &str) -> ReasonBreakdown {
    let mut rows: Vec<ReasonRow> = Vec::new();
    let mut unrecorded = 0;
    for e in enquiries.iter().filter(|e| e.stage == stage) {
        let reason = e.close_reason.as_deref().unwrap_or("").trim();
        if reason.is_empty() {
            unrecorded += 1;
            continue;
        }
        let i = match rows.iter().position(|r| r.reason == reason) {
            Some(i) => i,
            None => {
                rows.push(ReasonRow {
                    reason: reason.to_owned(),
                    count: 0,
                    value: 0.0,
                });
                rows.len() - 1
            }
        };
        rows[i].count += 1;
        rows[i].value = round2(rows[i].value + round2(e.value));
    }
    let counted: u32 = rows.iter().map(|r| r.count).sum();
    rows.sort_by(|a, b| {
        b.count
            .cmp(&a.count)
            .then(b.value.partial_cmp(&a.value).unwrap_or(std::cmp::Ordering::Equal))
    });
    ReasonBreakdown {
        rows,
        unrecorded,
        total: counted + unrecorded,
    }
}

/// Average fee value of what closed, admitted vs lost.
#[derive(Debug, Clone, PartialEq, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ClosedValues {
    pub admitted: Option<f64>,
    pub lost: Option<f64>,
}

pub fn avg_closed_value(enquiries: &[Enquiry]) -> ClosedValues {
    let avg = |stage: &str| {
        let list: Vec<&Enquiry> = enquiries.iter().filter(|e| e.stage == stage).collect();
        (!list.is_empty())
            .then(|| round2(list.iter().fold(0.0, |s, e| s + round2(e.value)) / list.len() as f64))
    };
    ClosedValues {
        admitted: avg(ADMITTED),
        lost: avg(LOST),
    }
}

/// When the enquiry entered its current stage (falls back to its own clock).
pub fn stage_entered_at(e: &Enquiry) -> f64 {
    match e.stage_history.last().and_then(|h| h.at) {
        Some(at) if at.is_finite() => at,
        _ => e
            .updated_at
            .filter(|u| *u != 0.0)
            .or(e.created_at)
            .unwrap_or(0.0),
    }
}

/// Whole days spent in the current stage.
pub fn days_in_stage(e: &Enquiry, now_ms: f64) -> i64 {
    (((now_ms - stage_entered_at(e)) / DAY_MS).floor() as i64).max(0)
}

/// An open enquiry that hasn't moved in `stale_days` days (0 turns the flag off).
pub fn is_stalled(e: &Enquiry, stale_days: i64, now_ms: f64) -> bool {
    e.is_open() && stale_days > 0 && days_in_stage(e, now_ms) >= stale_days
}

/// Stalled enquiries, longest-stuck first.
pub fn stalled<'a>(enquiries: &'a [Enquiry], stale_days: i64, now_ms: f64) -> Vec<&'a Enquiry> {
    let mut out: Vec<&Enquiry> = enquiries
        .iter()
        .filter(|e| is_stalled(e, stale_days, now_ms))
        .collect();
    out.sort_by(|a, b| {
        stage_entered_at(a)
            .partial_cmp(&stage_entered_at(b))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out
}

/// Average days spent in a stage, over completed transitions only.
#[derive(Debug, Clone, PartialEq, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct StageVelocity {
    pub stage: String,
    pub label: String,
    pub days: Option<i64>,
    pub count: u32,
}

pub fn stage_velocity(enquiries: &[Enquiry], stages: &[Stage]) -> Vec<StageVelocity> {
    let mut totals: HashMap<String, (f64, u32)> = HashMap::new();
    for e in enquiries {
        let h = &e.stage_history;
        for w in h.windows(2) {
            let (Some(a), Some(b)) = (w[0].at, w[1].at) else {
                continue;
            };
            let days = ((b - a) / DAY_MS).max(0.0);
            let t = totals
                .entry(w[0].stage.clone().unwrap_or_default())
                .or_insert((0.0, 0));
            t.0 += days;
            t.1 += 1;
        }
    }
    open_stages(stages)
        .into_iter()
        .map(|s| {
            let t = totals.get(&s.id);
            StageVelocity {
                stage: s.id.clone(),
                label: s.label.clone(),
                days: t.map(|(d, c)| js_round(d / f64::from(*c)) as i64),
                count: t.map_or(0, |(_, c)| *c),
            }
        })
        .collect()
}

/// Average days from first contact to admission (`None` until one is admitted).
pub fn avg_cycle_days(enquiries: &[Enquiry]) -> Option<i64> {
    let won: Vec<(f64, f64)> = enquiries
        .iter()
        .filter(|e| e.stage == ADMITTED)
        .filter_map(|e| match (e.closed_at, e.created_at) {
            (Some(c), Some(s)) if c != 0.0 && s != 0.0 => Some((c, s)),
            _ => None,
        })
        .collect();
    if won.is_empty() {
        return None;
    }
    let days = won
        .iter()
        .fold(0.0, |sum, (c, s)| sum + ((c - s) / DAY_MS).max(0.0));
    Some(js_round(days / won.len() as f64) as i64)
}

/// Weighted pipeline over what is left of a target (`None` with nothing left to cover).
pub fn pipeline_coverage(weighted: f64, remaining: f64) -> Option<f64> {
    (remaining > 0.0).then(|| round2(weighted / remaining))
}

// ---------------------------------------------------------------- duplicates (dupes.js)

/// `"  Dana  FOX "` and `"dana fox"` are the same name; accents are dropped.
pub fn norm_name(s: &str) -> String {
    let folded: String = s
        .nfd()
        .filter(|c| !('\u{0300}'..='\u{036f}').contains(c))
        .collect::<String>()
        .to_lowercase();
    folded.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn norm_email(s: &str) -> String {
    s.trim().to_lowercase()
}

/// Digits only; fewer than 7 digits is too little signal to pair on.
pub fn norm_phone(s: &str) -> String {
    let digits: String = s.chars().filter(char::is_ascii_digit).collect();
    if digits.len() >= 7 {
        digits
    } else {
        String::new()
    }
}

/// Pairs beyond this stop being a review and start being an import redo.
pub const MAX_PAIRS: usize = 50;

/// A record the duplicate scan reads.
#[derive(Debug, Clone, Default, PartialEq, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Contact {
    pub id: String,
    pub name: String,
    pub email: String,
    pub phone: String,
    pub created_at: f64,
}

/// One possible duplicate: `a` is the older record (the natural survivor).
#[derive(Debug, Clone, PartialEq)]
pub struct DuplicatePair<'a> {
    pub a: &'a Contact,
    pub b: &'a Contact,
    /// `email`, `name`, `phone`, in the order they matched.
    pub reasons: Vec<&'static str>,
}

/// Possible duplicates: identical e-mail, normalised name or phone. Each member of a group is
/// paired with the group's oldest record; at most [`MAX_PAIRS`], oldest survivor first.
pub fn contact_duplicates(contacts: &[Contact]) -> Vec<DuplicatePair<'_>> {
    type Keyer = fn(&Contact) -> String;
    let keyers: [(&'static str, Keyer); 3] = [
        ("email", |c| norm_email(&c.email)),
        ("name", |c| norm_name(&c.name)),
        ("phone", |c| norm_phone(&c.phone)),
    ];
    let mut pairs: Vec<(String, DuplicatePair)> = Vec::new();
    for (reason, key_of) in keyers {
        let mut groups: Vec<(String, Vec<&Contact>)> = Vec::new();
        let mut index: HashMap<String, usize> = HashMap::new();
        for c in contacts {
            let key = key_of(c);
            if key.is_empty() {
                continue;
            }
            match index.get(&key) {
                Some(&i) => groups[i].1.push(c),
                None => {
                    index.insert(key.clone(), groups.len());
                    groups.push((key, vec![c]));
                }
            }
        }
        for (_, group) in groups {
            if group.len() < 2 {
                continue;
            }
            let oldest = group
                .iter()
                .copied()
                .reduce(|best, r| if r.created_at < best.created_at { r } else { best })
                .unwrap_or(group[0]);
            for r in group {
                if std::ptr::eq(r, oldest) {
                    continue;
                }
                let mut ids = [oldest.id.as_str(), r.id.as_str()];
                ids.sort_by(|x, y| js_str_cmp(x, y));
                let pair_key = ids.join(":");
                match pairs.iter_mut().find(|(k, _)| *k == pair_key) {
                    Some((_, hit)) => {
                        if !hit.reasons.contains(&reason) {
                            hit.reasons.push(reason);
                        }
                    }
                    None => pairs.push((
                        pair_key,
                        DuplicatePair {
                            a: oldest,
                            b: r,
                            reasons: vec![reason],
                        },
                    )),
                }
            }
        }
    }
    let mut out: Vec<DuplicatePair> = pairs
        .into_iter()
        .map(|(_, p)| {
            if p.a.created_at <= p.b.created_at {
                p
            } else {
                DuplicatePair {
                    a: p.b,
                    b: p.a,
                    reasons: p.reasons,
                }
            }
        })
        .collect();
    out.sort_by(|x, y| {
        x.a.created_at
            .partial_cmp(&y.a.created_at)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out.truncate(MAX_PAIRS);
    out
}

/// JavaScript's default string sort (UTF-16 code units).
fn js_str_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    a.encode_utf16().cmp(b.encode_utf16())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enq(id: &str, stage: &str, hist: &[(&str, f64)]) -> Enquiry {
        Enquiry {
            id: id.into(),
            stage: stage.into(),
            stage_history: hist
                .iter()
                .map(|(s, at)| StageEntry {
                    stage: Some((*s).into()),
                    at: Some(*at),
                })
                .collect(),
            value: 100_000.0,
            ..Enquiry::default()
        }
    }

    fn stages() -> Vec<Stage> {
        resolve_stages(&[], &default_open_stages())
    }

    #[test]
    fn funnel_counts_entered_advanced_and_lost_here() {
        let day = DAY_MS;
        let list = vec![
            enq("a", "applied", &[("enquiry", 0.0), ("counselling", day), ("applied", 2.0 * day)]),
            enq("b", LOST, &[("enquiry", 0.0), ("counselling", day), (LOST, 3.0 * day)]),
            enq("c", ADMITTED, &[("applied", 0.0), (ADMITTED, 9.0 * day)]),
            enq("d", LOST, &[(LOST, 0.0)]),
        ];
        let r = funnel_report(&list, &stages());
        assert_eq!(r.untracked, 1);
        assert_eq!(r.totals.enquiries, 3);
        let row = |s: &str| r.rows.iter().find(|x| x.stage == s).unwrap().clone();
        assert_eq!((row("enquiry").entered, row("enquiry").advanced), (2, 2));
        let c = row("counselling");
        assert_eq!((c.entered, c.advanced, c.lost_here), (2, 1, 1));
        let a = row("applied");
        assert_eq!((a.entered, a.advanced, a.open), (2, 1, 1));
        assert_eq!(r.totals.admitted_rate, Some(0.33));
    }

    #[test]
    fn forecast_rolls_overdue_into_first_month_and_weights() {
        let mut e = enq("a", "applied", &[("applied", 0.0)]);
        e.expected_close = Some("2026-01-15".into());
        let rows = forecast_by_month(
            &[e],
            3,
            "2026-06",
            "2026-06",
            330,
            &HashMap::new(),
            &stages(),
        );
        assert_eq!(rows[0].best, 100_000.0);
        assert_eq!(rows[0].weighted, 60_000.0);
        assert_eq!(rows[0].weighted_count, 0.6);
        assert_eq!(shift_month_key("2026-11", 3), "2027-02");
        assert_eq!(month_key_at(1_767_205_800_000.0, 330), "2026-01"); // 2026-01-01 00:00 IST
    }

    #[test]
    fn duplicates_pair_with_oldest() {
        let c = |id: &str, name: &str, email: &str, phone: &str, at: f64| Contact {
            id: id.into(),
            name: name.into(),
            email: email.into(),
            phone: phone.into(),
            created_at: at,
        };
        let list = vec![
            c("2", "Aarav  SHARMA", "a@x.in", "", 2.0),
            c("1", "aarav sharma", "A@X.IN ", "+91 98200 12345", 1.0),
            c("3", "Áarav Sharma", "", "98200-12345", 3.0),
        ];
        let p = contact_duplicates(&list);
        assert_eq!(p.len(), 2);
        assert_eq!((p[0].a.id.as_str(), p[0].b.id.as_str()), ("1", "2"));
        assert_eq!(p[0].reasons, vec!["email", "name"]);
        assert_eq!(p[1].b.id, "3");
        assert_eq!(p[1].reasons, vec!["name"]);
        assert_eq!(norm_phone("911"), "");
    }
}
