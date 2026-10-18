//! **ERP_EXTENSIONS 5A** — hiring analytics, stale candidates, data retention and reminders,
//! ported from HeyPuter/everything `src/hiring/src/core/analytics.js` (stage visits, stage
//! conversion, time to hire, average days in stage, source breakdown, offer outcomes),
//! `pipeline.js` (configurable stages, funnel counts, days in stage, stale candidates),
//! `retention.js` (archived candidates past the retention window — here the DPDP Act 2023
//! "erase once the purpose is served" rule) and `reminders.js` (offer / follow-up / onboarding
//! reminders with stable dedupe keys). MIT, see THIRD_PARTY_NOTICES.md.
//!
//! Pure: a candidate's history (stage moves, archive, restore, re-engage) in, numbers out.
//! Stage keys are whatever the caller uses; SutraERP maps its candidate statuses onto
//! Everything's funnel in [`sutra_stage`].

use super::offers::{
    due_followups, due_onboarding_tasks, expiring_offers, is_safe_id, js_str, letter_date,
    Checklist, Followup, OfferRecord, OfferStatus,
};
use chrono::{Datelike, Duration, NaiveDate, NaiveDateTime};
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeSet, HashMap, HashSet};
use utoipa::ToSchema;

pub const ACTIVE: &str = "active";
pub const REJECTED: &str = "rejected";
pub const WITHDRAWN: &str = "withdrawn";

/// One recorded event of a candidate (`created`, `stage`, `rejected`, `withdrawn`, `restored`,
/// `reengaged`, anything else is ignored).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEvent {
    pub kind: String,
    pub stage: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub at: NaiveDateTime,
}

/// A candidate as the funnel sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PipelineCandidate {
    pub id: String,
    pub name: String,
    pub job_id: Option<String>,
    pub stage: String,
    /// `active`, `rejected`, `withdrawn`.
    pub status: String,
    pub created_at: NaiveDateTime,
    pub stage_changed_at: Option<NaiveDateTime>,
    pub source: Option<String>,
    /// Talent-pool members are kept on purpose (never offered for erasure).
    pub pool: bool,
    pub history: Vec<HistoryEvent>,
}

impl PipelineCandidate {
    pub fn is_active(&self) -> bool {
        self.status == ACTIVE
    }
}

/// SutraERP candidate status → (funnel stage, status). `offer_declined` archives like a
/// rejection; `offer_accepted` is hired.
pub fn sutra_stage(status: &str) -> (&'static str, &'static str) {
    match status {
        "applied" => ("applied", ACTIVE),
        "shortlisted" => ("screen", ACTIVE),
        "interviewed" => ("interview", ACTIVE),
        "offered" => ("offer", ACTIVE),
        "offer_accepted" => ("hired", ACTIVE),
        "withdrawn" => ("applied", WITHDRAWN),
        _ => ("applied", REJECTED),
    }
}

/// The SutraERP funnel.
pub fn default_stage_keys() -> Vec<String> {
    ["applied", "screen", "interview", "offer", "hired"]
        .map(str::to_owned)
        .to_vec()
}

/// Human labels of the funnel stages.
pub fn stage_label(key: &str) -> String {
    match key {
        "applied" => "Applied".into(),
        "screen" => "Shortlisted".into(),
        "interview" => "Interviewed".into(),
        "offer" => "Offered".into(),
        "hired" => "Offer accepted".into(),
        other => other.into(),
    }
}

/// `stagesFrom` (keys): `applied` first, `hired` last, `offer` always present; configured
/// middle stages keep their order, unsafe / unlabeled / duplicate keys are dropped.
pub fn stage_keys_from(raw: &Value) -> Vec<String> {
    let Value::Array(list) = raw else {
        return default_stage_keys();
    };
    if list.is_empty() {
        return default_stage_keys();
    }
    let mut seen: HashSet<String> = ["applied", "hired"].map(str::to_owned).into();
    let mut middle = Vec::new();
    for e in list {
        let key = js_str(e.get("key"));
        let label = js_str(e.get("label"));
        if !is_safe_id(&key) || label.trim().is_empty() || seen.contains(&key) {
            continue;
        }
        seen.insert(key.clone());
        middle.push(key);
    }
    if !middle.iter().any(|k| k == "offer") {
        middle.push("offer".into());
    }
    let mut out = vec!["applied".to_owned()];
    out.extend(middle);
    out.push("hired".into());
    out
}

const DAY_MS: f64 = 86_400_000.0;

fn span_days(from: NaiveDateTime, to: NaiveDateTime) -> f64 {
    ((to - from).num_milliseconds() as f64 / DAY_MS).max(0.0)
}

/// `median`.
pub fn median(values: &[f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let mut v = values.to_vec();
    v.sort_by(f64::total_cmp);
    let mid = v.len() / 2;
    Some(if v.len() % 2 == 1 {
        v[mid]
    } else {
        (v[mid - 1] + v[mid]) / 2.0
    })
}

/// A stay in one stage.
#[derive(Debug, Clone, PartialEq)]
pub struct Visit {
    pub stage: String,
    pub from: NaiveDateTime,
    /// `None` = still there.
    pub to: Option<NaiveDateTime>,
    pub days: f64,
}

/// `stageVisits`: archive events close the open visit, restore / re-engage reopen the clock,
/// the current visit closes at `now` for the arithmetic.
pub fn stage_visits(c: &PipelineCandidate, now: NaiveDateTime) -> Vec<Visit> {
    let mut visits = Vec::new();
    let mut stage: Option<String> = None;
    let mut since: Option<NaiveDateTime> = None;
    fn close(
        visits: &mut Vec<Visit>,
        stage: &Option<String>,
        since: &mut Option<NaiveDateTime>,
        at: NaiveDateTime,
    ) {
        if let (Some(s), Some(f)) = (stage, *since) {
            visits.push(Visit {
                stage: s.clone(),
                from: f,
                to: Some(at),
                days: span_days(f, at),
            });
        }
        *since = None;
    }
    for e in &c.history {
        match e.kind.as_str() {
            "created" => {
                stage = Some(e.stage.clone().unwrap_or_else(|| "applied".into()));
                since = Some(e.at);
            }
            "stage" => {
                close(&mut visits, &stage, &mut since, e.at);
                stage = e.to.clone();
                since = Some(e.at);
            }
            REJECTED | WITHDRAWN => close(&mut visits, &stage, &mut since, e.at),
            "restored" => {
                since = since.or(Some(e.at));
            }
            "reengaged" => {
                close(&mut visits, &stage, &mut since, e.at);
                stage = Some("applied".into());
                since = Some(e.at);
            }
            _ => {}
        }
    }
    if let (Some(s), Some(f)) = (stage, since) {
        visits.push(Visit {
            stage: s,
            from: f,
            to: if c.is_active() { None } else { Some(now) },
            days: span_days(f, now),
        });
    }
    visits
}

/// `reachedStages`: every stage of the current candidacy (re-engaging starts afresh).
pub fn reached_stages(c: &PipelineCandidate) -> BTreeSet<String> {
    let mut reached = BTreeSet::new();
    for e in &c.history {
        match e.kind.as_str() {
            "created" => {
                reached.insert(e.stage.clone().unwrap_or_else(|| "applied".into()));
            }
            "stage" => {
                if let Some(f) = &e.from {
                    reached.insert(f.clone());
                }
                if let Some(t) = &e.to {
                    reached.insert(t.clone());
                }
            }
            "reengaged" => {
                reached = BTreeSet::from(["applied".to_owned()]);
            }
            _ => {}
        }
    }
    reached.insert(c.stage.clone());
    reached
}

/// Conversion of one stage to any later one.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[schema(as = HiringStageConversion)]
pub struct Conversion {
    pub stage: String,
    pub next: String,
    pub reached: u64,
    pub advanced: u64,
    /// `advanced / reached`; null when nobody reached the stage.
    pub rate: Option<f64>,
}

/// `stageConversion`: skipping a stage still counts as advancing past it.
pub fn stage_conversion(cands: &[PipelineCandidate], stages: &[String]) -> Vec<Conversion> {
    let reached: Vec<BTreeSet<String>> = cands.iter().map(reached_stages).collect();
    (0..stages.len().saturating_sub(1))
        .map(|i| {
            let later = &stages[i + 1..];
            let (mut r, mut a) = (0u64, 0u64);
            for v in &reached {
                if !v.contains(&stages[i]) {
                    continue;
                }
                r += 1;
                if later.iter().any(|k| v.contains(k)) {
                    a += 1;
                }
            }
            Conversion {
                stage: stages[i].clone(),
                next: stages[i + 1].clone(),
                reached: r,
                advanced: a,
                rate: (r > 0).then(|| a as f64 / r as f64),
            }
        })
        .collect()
}

/// `timeToHire` result.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[schema(as = HiringTimeToHire)]
pub struct TimeToHire {
    pub count: u64,
    /// Median days from application to offer acceptance.
    pub median: Option<f64>,
}

/// `timeToHire`: days from application (or the last re-engagement) to the move into hired;
/// `since` scopes by when the hire happened.
pub fn time_to_hire(cands: &[PipelineCandidate], since: Option<NaiveDateTime>) -> TimeToHire {
    let mut values = Vec::new();
    for c in cands {
        let mut start = c.created_at;
        for e in &c.history {
            if e.kind == "reengaged" {
                start = e.at;
            }
        }
        let hired = c.history.iter().find(|e| {
            e.at >= start
                && ((e.kind == "stage" && e.to.as_deref() == Some("hired"))
                    || (e.kind == "created" && e.stage.as_deref() == Some("hired")))
        });
        let Some(h) = hired else {
            continue;
        };
        if since.is_some_and(|s| h.at < s) {
            continue;
        }
        values.push(span_days(start, h.at));
    }
    TimeToHire {
        count: values.len() as u64,
        median: median(&values),
    }
}

/// Average days in a stage.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[schema(as = HiringStageTime)]
pub struct StageTime {
    pub stage: String,
    pub average: Option<f64>,
}

/// `averageDaysInStage` over every visit.
pub fn average_days_in_stage(
    cands: &[PipelineCandidate],
    now: NaiveDateTime,
    stages: &[String],
) -> Vec<StageTime> {
    let mut acc: HashMap<&str, (f64, u64)> =
        stages.iter().map(|s| (s.as_str(), (0.0, 0))).collect();
    for c in cands {
        for v in stage_visits(c, now) {
            if let Some(a) = acc.get_mut(v.stage.as_str()) {
                a.0 += v.days;
                a.1 += 1;
            }
        }
    }
    stages
        .iter()
        .map(|s| {
            let (d, n) = acc[s.as_str()];
            StageTime {
                stage: s.clone(),
                average: (n > 0).then(|| d / n as f64),
            }
        })
        .collect()
}

/// Candidates and hires per source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[schema(as = HiringSourceRow)]
pub struct SourceRow {
    pub source: String,
    pub total: u64,
    pub hired: u64,
}

/// `sourceBreakdown`: busiest sources first.
pub fn source_breakdown(cands: &[PipelineCandidate]) -> Vec<SourceRow> {
    let mut rows: Vec<SourceRow> = Vec::new();
    for c in cands {
        let s = c.source.as_deref().unwrap_or("").trim();
        let key = if s.is_empty() { "Unknown" } else { s };
        let hired = reached_stages(c).contains("hired");
        match rows.iter_mut().find(|r| r.source == key) {
            Some(r) => {
                r.total += 1;
                r.hired += u64::from(hired);
            }
            None => rows.push(SourceRow {
                source: key.to_owned(),
                total: 1,
                hired: u64::from(hired),
            }),
        }
    }
    rows.sort_by(|a, b| {
        b.total.cmp(&a.total).then_with(|| {
            a.source
                .to_lowercase()
                .cmp(&b.source.to_lowercase())
                .then_with(|| a.source.cmp(&b.source))
        })
    });
    rows
}

/// Offer outcomes.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[schema(as = HiringOfferOutcomes)]
pub struct OfferOutcomes {
    pub sent: u64,
    pub accepted: u64,
    pub declined: u64,
    /// `accepted / (accepted + declined)`.
    pub acceptance: Option<f64>,
}

/// `offerOutcomes`: `since` scopes by when each thing happened (sent / resolved).
pub fn offer_outcomes(
    offers: &[OfferRecord],
    candidate_ids: Option<&HashSet<String>>,
    since: Option<NaiveDateTime>,
) -> OfferOutcomes {
    let within = |at: Option<NaiveDateTime>| since.is_none_or(|s| at.is_some_and(|a| a >= s));
    let scoped: Vec<&OfferRecord> = offers
        .iter()
        .filter(|o| candidate_ids.is_none_or(|ids| ids.contains(&o.candidate_id)))
        .collect();
    let resolved = |o: &OfferRecord| o.resolved_at.or(o.accepted_at);
    let sent = scoped
        .iter()
        .filter(|o| o.sent_at.is_some() && within(o.sent_at))
        .count() as u64;
    let accepted = scoped
        .iter()
        .filter(|o| o.status == OfferStatus::Accepted && within(resolved(o)))
        .count() as u64;
    let declined = scoped
        .iter()
        .filter(|o| o.status == OfferStatus::Declined && within(resolved(o)))
        .count() as u64;
    OfferOutcomes {
        sent,
        accepted,
        declined,
        acceptance: (accepted + declined > 0)
            .then(|| accepted as f64 / (accepted + declined) as f64),
    }
}

/// `funnelCounts`: active candidates per stage (every stage present).
pub fn funnel_counts(
    cands: &[PipelineCandidate],
    job_id: Option<&str>,
    stages: &[String],
) -> Vec<(String, u64)> {
    let mut counts: Vec<(String, u64)> = stages.iter().map(|s| (s.clone(), 0)).collect();
    for c in cands {
        if !c.is_active() || job_id.is_some_and(|j| c.job_id.as_deref() != Some(j)) {
            continue;
        }
        if let Some(e) = counts.iter_mut().find(|(k, _)| *k == c.stage) {
            e.1 += 1;
        }
    }
    counts
}

/// `daysInStage`: calendar days since the last stage change (never negative).
pub fn days_in_stage(c: &PipelineCandidate, now: NaiveDateTime) -> i64 {
    let then = c.stage_changed_at.unwrap_or(c.created_at).date();
    (now.date() - then).num_days().max(0)
}

/// `staleCandidates`: active, not hired, in the stage for `threshold_days` or more; longest first.
pub fn stale_candidates(
    cands: &[PipelineCandidate],
    threshold_days: i64,
    now: NaiveDateTime,
) -> Vec<&PipelineCandidate> {
    let mut v: Vec<&PipelineCandidate> = cands
        .iter()
        .filter(|c| c.is_active() && c.stage != "hired" && days_in_stage(c, now) >= threshold_days)
        .collect();
    v.sort_by_key(|c| std::cmp::Reverse(days_in_stage(c, now)));
    v
}

/// `archivedAt`: the last archive event, else the last stage change, else creation.
pub fn archived_at(c: &PipelineCandidate) -> NaiveDateTime {
    c.history
        .iter()
        .rev()
        .find(|e| e.kind == REJECTED || e.kind == WITHDRAWN)
        .map(|e| e.at)
        .or(c.stage_changed_at)
        .unwrap_or(c.created_at)
}

fn days_in_month(y: i32, m: u32) -> u32 {
    let (ny, nm) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
    NaiveDate::from_ymd_opt(ny, nm, 1)
        .and_then(|d| d.pred_opt())
        .map_or(28, |d| d.day())
}

/// The retention cutoff: `months` before `now`, clamped to the target month's last day.
pub fn retention_cutoff(now: NaiveDateTime, months: u32) -> NaiveDateTime {
    let total = now.year() * 12 + now.month0() as i32 - months as i32;
    let (y, m) = (total.div_euclid(12), total.rem_euclid(12) as u32 + 1);
    let day = now.day().min(days_in_month(y, m));
    NaiveDate::from_ymd_opt(y, m, day).map_or(now, |d| d.and_time(now.time()))
}

/// `retentionDue`: archived (not talent-pool) candidates past the window, oldest first.
pub fn retention_due(
    cands: &[PipelineCandidate],
    months: i64,
    now: NaiveDateTime,
) -> Vec<(&PipelineCandidate, NaiveDateTime)> {
    if months <= 0 {
        return vec![];
    }
    let cutoff = retention_cutoff(now, u32::try_from(months).unwrap_or(0));
    let mut v: Vec<(&PipelineCandidate, NaiveDateTime)> = cands
        .iter()
        .filter(|c| !c.is_active() && !c.pool)
        .map(|c| (c, archived_at(c)))
        .filter(|(_, at)| *at < cutoff)
        .collect();
    v.sort_by_key(|(_, at)| *at);
    v
}

// ------------------------------------------------------------------ reminders

/// Which reminder kinds are on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReminderPrefs {
    pub offers: bool,
    pub followups: bool,
    pub onboarding: bool,
}

impl Default for ReminderPrefs {
    fn default() -> Self {
        Self {
            offers: true,
            followups: true,
            onboarding: true,
        }
    }
}

/// A reminder due now. `key` is stable so it is recorded once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = HiringReminder)]
pub struct Reminder {
    #[schema(example = "offer-…-2026-06-15-soon")]
    pub key: String,
    /// `offer`, `followup`, `onboarding`.
    pub kind: String,
    pub title: String,
    pub body: String,
    pub candidate_id: String,
}

fn archived(c: Option<&PipelineCandidate>) -> bool {
    c.is_some_and(|c| c.status == REJECTED || c.status == WITHDRAWN)
}

/// `computeReminders` (offers, follow-ups, onboarding): expiring / lapsed sent offers, due
/// follow-ups, and due onboarding tasks of hires still on the board.
pub fn compute_reminders(
    cands: &[PipelineCandidate],
    offers: &[OfferRecord],
    followups: &[Followup],
    plans: &[Checklist],
    prefs: ReminderPrefs,
    now: NaiveDateTime,
) -> Vec<Reminder> {
    let today = now.date();
    let by_id: HashMap<&str, &PipelineCandidate> =
        cands.iter().map(|c| (c.id.as_str(), c)).collect();
    let name = |id: &str| {
        by_id
            .get(id)
            .map_or_else(|| "a candidate".to_owned(), |c| c.name.clone())
    };
    let mut out = Vec::new();
    if prefs.offers {
        for o in expiring_offers(offers, 3, now) {
            if archived(by_id.get(o.candidate_id.as_str()).copied()) {
                continue;
            }
            let Some(exp) = o.expires else {
                continue;
            };
            let expired = exp < today;
            out.push(Reminder {
                key: format!(
                    "offer-{}-{}-{}",
                    o.id,
                    exp.format("%Y-%m-%d"),
                    if expired { "expired" } else { "soon" }
                ),
                kind: "offer".into(),
                title: format!(
                    "{}: {}",
                    if expired {
                        "Offer expired"
                    } else {
                        "Offer expiring"
                    },
                    name(&o.candidate_id)
                ),
                body: if expired {
                    format!(
                        "It lapsed on {}. Follow up or withdraw it.",
                        letter_date(exp)
                    )
                } else {
                    format!("Open until {}.", letter_date(exp))
                },
                candidate_id: o.candidate_id.clone(),
            });
        }
    }
    if prefs.followups {
        for (f, overdue) in due_followups(followups, now) {
            if !by_id.contains_key(f.candidate_id.as_str()) {
                continue;
            }
            out.push(Reminder {
                key: format!(
                    "followup-{}-{}{}",
                    f.id,
                    f.due_date.as_deref().unwrap_or(""),
                    f.due_time
                        .as_deref()
                        .filter(|t| !t.is_empty())
                        .map_or(String::new(), |t| format!("-{t}"))
                ),
                kind: "followup".into(),
                title: format!(
                    "{}: {}",
                    if overdue {
                        "Overdue follow-up"
                    } else {
                        "Follow up"
                    },
                    name(&f.candidate_id)
                ),
                body: f.text.clone(),
                candidate_id: f.candidate_id.clone(),
            });
        }
    }
    if prefs.onboarding {
        for d in due_onboarding_tasks(plans, offers, today, 3) {
            let Some(c) = by_id.get(d.candidate_id).copied() else {
                continue;
            };
            if archived(Some(c)) || c.stage != "hired" {
                continue;
            }
            out.push(Reminder {
                key: format!("onboarding-{}-{}", d.task.id, d.due_date.format("%Y-%m-%d")),
                kind: "onboarding".into(),
                title: format!(
                    "{}: {}",
                    if d.overdue {
                        "Onboarding overdue"
                    } else {
                        "Onboarding due"
                    },
                    c.name
                ),
                body: format!(
                    "{} — {} {}.",
                    d.task.title,
                    if d.overdue { "was due" } else { "due" },
                    letter_date(d.due_date)
                ),
                candidate_id: c.id.clone(),
            });
        }
    }
    out
}

/// Days a reminder of an `expiring` offer looks ahead (Everything's three).
pub const OFFER_REMINDER_DAYS: i64 = 3;

/// `now - days`, for "in the last N days" scopes.
pub fn days_ago(now: NaiveDateTime, days: i64) -> NaiveDateTime {
    now - Duration::days(days)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S").unwrap()
    }

    #[test]
    fn cutoff_clamps_to_month_end() {
        assert_eq!(
            retention_cutoff(t("2026-08-31T12:00:00"), 6),
            t("2026-02-28T12:00:00")
        );
        assert_eq!(
            retention_cutoff(t("2026-01-15T09:00:00"), 12),
            t("2025-01-15T09:00:00")
        );
    }

    #[test]
    fn sutra_statuses_map_onto_the_funnel() {
        assert_eq!(sutra_stage("offer_accepted"), ("hired", ACTIVE));
        assert_eq!(sutra_stage("offer_declined"), ("applied", REJECTED));
        assert_eq!(stage_label("screen"), "Shortlisted");
    }
}
