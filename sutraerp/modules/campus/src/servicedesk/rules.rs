//! **ERP_EXTENSIONS 2B** — service-desk business rules, ported from HeyPuter/everything
//! `src/help-desk/src/shared/rules.js` (+ `runTriggers` from `src/server/tickets.js`; MIT, see
//! THIRD_PARTY_NOTICES.md): the condition language of views and triggers, trigger actions,
//! `{{placeholders}}`, and service level targets.
//!
//! The engine is parametrised by a [`Vocab`] (status order, unsolved / waiting statuses,
//! priorities, types), so the golden test replays Everything's own vocabulary while the campus
//! desk runs on SutraERP's (`open … resolved`, `low … critical`, ticket categories). Pure
//! functions over plain records; timestamps are milliseconds since the epoch.

use serde_json::{Map, Value};

const HOUR_MS: f64 = 3_600_000.0;
const MIN_MS: f64 = 60_000.0;
/// At most this many tags on a ticket.
pub const MAX_TAGS: usize = 30;
/// Longest tag (characters).
pub const MAX_TAG_LEN: usize = 40;

/// The words a desk uses for statuses, priorities and types.
#[derive(Debug, Clone)]
pub struct Vocab {
    /// Statuses in rank order (`less_than` / `greater_than` compare these positions).
    pub statuses: Vec<String>,
    /// Statuses that still need work.
    pub unsolved: Vec<String>,
    /// Unsolved statuses that wait on someone else (reply targets pause).
    pub waiting: Vec<String>,
    /// A status automation may not set (reached by lifecycle only).
    pub unsettable: Vec<String>,
    /// Priorities, lowest first.
    pub priorities: Vec<String>,
    /// The priority whose SLA targets apply to a ticket without one.
    pub default_priority: String,
    pub types: Vec<String>,
}

fn words(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| (*s).to_owned()).collect()
}

impl Vocab {
    /// Everything help-desk's vocabulary (golden vectors).
    pub fn everything() -> Self {
        Self {
            statuses: words(&["new", "open", "pending", "hold", "solved", "closed"]),
            unsolved: words(&["new", "open", "pending", "hold"]),
            waiting: words(&["pending", "hold"]),
            unsettable: words(&["closed"]),
            priorities: words(&["low", "normal", "high", "urgent"]),
            default_priority: "normal".into(),
            types: words(&["question", "incident", "problem", "task"]),
        }
    }

    /// The campus service desk (`campus.service_ticket`).
    pub fn campus() -> Self {
        Self {
            statuses: words(&[
                "open",
                "reopened",
                "triaged",
                "assigned",
                "in_progress",
                "on_hold",
                "resolved",
                "closed",
                "cancelled",
                "merged",
            ]),
            unsolved: words(&[
                "open",
                "reopened",
                "triaged",
                "assigned",
                "in_progress",
                "on_hold",
            ]),
            waiting: words(&["on_hold"]),
            unsettable: words(&["closed", "cancelled", "merged"]),
            priorities: words(&["low", "medium", "high", "critical"]),
            default_priority: "medium".into(),
            types: words(&[
                "facility",
                "maintenance",
                "hostel",
                "transport",
                "library",
                "laboratory",
                "it",
                "other",
            ]),
        }
    }

    fn status_rank(&self, s: &str) -> Option<usize> {
        self.statuses.iter().position(|x| x == s)
    }
    fn priority_rank(&self, s: &str) -> Option<usize> {
        self.priorities.iter().position(|x| x == s)
    }
    pub fn is_unsolved(&self, s: &str) -> bool {
        self.unsolved.iter().any(|x| x == s)
    }
}

/// Customer satisfaction on a ticket.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Csat {
    pub score: Option<String>,
}

/// A ticket as the rules see it (Everything's record shape).
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Ticket {
    pub status: String,
    pub priority: Option<String>,
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub assignee: Option<String>,
    pub group: Option<String>,
    pub requester: Option<String>,
    pub org: Option<String>,
    pub via: Option<String>,
    pub tags: Vec<String>,
    pub subject: String,
    pub followers: Vec<String>,
    pub created_at: Option<f64>,
    pub updated_at: Option<f64>,
    pub solved_at: Option<f64>,
    pub csat: Option<Csat>,
    pub fields: Map<String, Value>,
    pub spam: bool,
    pub deleted: bool,
    pub merged_into: Option<String>,
    pub first_reply_at: Option<f64>,
    pub last_customer_at: Option<f64>,
    pub last_agent_reply_at: Option<f64>,
}

/// A comment that came with an update.
#[derive(Debug, Clone, Copy, PartialEq, serde::Deserialize)]
pub struct Comment {
    pub public: bool,
}

/// What a condition can see beyond the ticket.
#[derive(Debug, Clone, Default)]
pub struct Ctx<'a> {
    /// The viewing agent (`me` in conditions and actions).
    pub me: Option<&'a str>,
    pub now: f64,
    /// The ticket before the update (triggers).
    pub before: Option<&'a Ticket>,
    /// `agent`, `customer` or `system`.
    pub actor: Option<&'a str>,
    pub comment: Option<Comment>,
}

/// One condition: `{ field, op, value }`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[schema(as = ServiceDeskCondition)]
pub struct Condition {
    pub field: String,
    pub op: String,
    #[serde(default)]
    #[schema(value_type = Object)]
    pub value: Value,
}

/// All of `all`, and at least one of `any` when it is not empty.
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
#[serde(default)]
#[schema(as = ServiceDeskConditions)]
pub struct Conditions {
    pub all: Vec<Condition>,
    pub any: Vec<Condition>,
}

/// One action: `{ field, value }` (+ `to`, `subject`, `body` for notifications).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[schema(as = ServiceDeskAction)]
pub struct Action {
    pub field: String,
    #[serde(default)]
    #[schema(value_type = Object)]
    pub value: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
}

/// A notification an action asked for.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Notification {
    pub kind: String,
    pub to: Option<String>,
    pub subject: String,
    pub body: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trigger: Option<String>,
}

// ------------------------------------------------------------------ JS value helpers

fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0 && !f.is_nan()),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

fn js_number_string(f: f64) -> String {
    if f.fract() == 0.0 && f.abs() < 1e21 {
        format!("{}", f as i64)
    } else {
        format!("{f}")
    }
}

/// `String(v)`.
fn js_string(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.as_f64().map_or_else(|| n.to_string(), js_number_string),
        Value::String(s) => s.clone(),
        Value::Array(a) => a
            .iter()
            .map(|x| {
                if x.is_null() {
                    String::new()
                } else {
                    js_string(x)
                }
            })
            .collect::<Vec<_>>()
            .join(","),
        Value::Object(_) => "[object Object]".into(),
    }
}

/// `String(v || '')`.
fn or_empty(v: &Value) -> String {
    if truthy(v) {
        js_string(v)
    } else {
        String::new()
    }
}

/// `String(v ?? '')`.
fn nullish_empty(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(v) => js_string(v),
    }
}

fn opt_or_empty(v: &Option<String>) -> &str {
    v.as_deref().unwrap_or("")
}

/// `Number(v)` for the values a condition carries.
fn js_number(v: &Value) -> f64 {
    match v {
        Value::Null => 0.0,
        Value::Bool(b) => f64::from(u8::from(*b)),
        Value::Number(n) => n.as_f64().unwrap_or(f64::NAN),
        Value::String(s) => {
            let t = s.trim();
            if t.is_empty() {
                0.0
            } else {
                t.parse::<f64>().unwrap_or(f64::NAN)
            }
        }
        _ => f64::NAN,
    }
}

/// Tags are lowercase words joined by underscores.
pub fn norm_tag(value: &str) -> String {
    let lowered = value.trim().to_lowercase();
    let mut out = String::new();
    let mut in_sep = false;
    for c in lowered.chars() {
        if c.is_whitespace() || c == ',' {
            if !in_sep {
                out.push('_');
                in_sep = true;
            }
            continue;
        }
        in_sep = false;
        out.push(c);
    }
    out.chars()
        .filter(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.'))
        .take(MAX_TAG_LEN)
        .collect()
}

/// A list (or a comma / space separated string) of tags, normalised and de-duplicated.
pub fn norm_tags(list: &Value) -> Vec<String> {
    let items: Vec<String> = match list {
        Value::Array(a) => a.iter().map(nullish_string).collect(),
        other => {
            let s = nullish_empty(Some(other));
            s.split(|c: char| c.is_whitespace() || c == ',')
                .map(str::to_owned)
                .collect()
        }
    };
    let mut out: Vec<String> = Vec::new();
    for t in items {
        let tag = norm_tag(&t);
        if !tag.is_empty() && !out.contains(&tag) {
            out.push(tag);
        }
        if out.len() >= MAX_TAGS {
            break;
        }
    }
    out
}

fn nullish_string(v: &Value) -> String {
    nullish_empty(Some(v))
}

fn hours_since(ts: Option<f64>, now: f64) -> f64 {
    match ts {
        Some(t) if t != 0.0 && !t.is_nan() => (now - t) / HOUR_MS,
        _ => f64::INFINITY,
    }
}

// ------------------------------------------------------------------ conditions

/// Tests one condition.
pub fn test_condition(cond: &Condition, t: &Ticket, ctx: &Ctx<'_>, vocab: &Vocab) -> bool {
    let op = cond.op.as_str();
    let value = &cond.value;
    let val_str = value.as_str();
    let rank_cmp = |a: Option<usize>, b: Option<usize>| match (a, b) {
        (Some(x), Some(y)) => {
            if op == "less_than" {
                x < y
            } else {
                x > y
            }
        }
        _ => false,
    };
    let eq_or = |eq: bool| if op == "is" { eq } else { !eq };
    match cond.field.as_str() {
        "status" => {
            if op == "changed_to" {
                return ctx.before.is_some_and(|b| b.status != t.status)
                    && val_str == Some(t.status.as_str());
            }
            if op == "less_than" || op == "greater_than" {
                return rank_cmp(
                    vocab.status_rank(&t.status),
                    val_str.and_then(|v| vocab.status_rank(v)),
                );
            }
            eq_or(val_str == Some(t.status.as_str()))
        }
        "priority" => {
            if op == "less_than" || op == "greater_than" {
                return rank_cmp(
                    t.priority.as_deref().and_then(|p| vocab.priority_rank(p)),
                    val_str.and_then(|v| vocab.priority_rank(v)),
                );
            }
            eq_or(opt_or_empty(&t.priority) == or_empty(value))
        }
        "type" => eq_or(opt_or_empty(&t.kind) == or_empty(value)),
        "assignee" => {
            if op == "changed" {
                return ctx
                    .before
                    .is_some_and(|b| opt_or_empty(&b.assignee) != opt_or_empty(&t.assignee));
            }
            let want = if val_str == Some("me") {
                ctx.me.unwrap_or("").to_owned()
            } else {
                or_empty(value)
            };
            eq_or(opt_or_empty(&t.assignee) == want)
        }
        "group" => eq_or(opt_or_empty(&t.group) == or_empty(value)),
        "requester" => eq_or(opt_or_empty(&t.requester) == or_empty(value)),
        "organization" => eq_or(opt_or_empty(&t.org) == or_empty(value)),
        "channel" => eq_or(t.via.is_some() && t.via.as_deref() == val_str),
        "tags" => {
            let want = norm_tags(value);
            let has = want.iter().any(|w| t.tags.contains(w));
            if op == "includes" {
                has
            } else {
                !has
            }
        }
        "subject" => {
            let has = t
                .subject
                .to_lowercase()
                .contains(&or_empty(value).to_lowercase());
            if op == "contains" {
                has
            } else {
                !has
            }
        }
        "followers" => ctx
            .me
            .is_some_and(|me| !me.is_empty() && t.followers.iter().any(|f| f == me)),
        f @ ("created" | "updated" | "solved") => {
            let ts = match f {
                "created" => t.created_at,
                "updated" => t.updated_at,
                _ => t.solved_at,
            };
            let h = hours_since(ts, ctx.now);
            let n = js_number(value);
            if nullish_empty(Some(value)).trim().is_empty() || !n.is_finite() {
                return false;
            }
            if op == "less_than" {
                h < n
            } else {
                h > n && h != f64::INFINITY
            }
        }
        "satisfaction" => {
            if val_str == Some("unrated") {
                return t.csat.is_none();
            }
            t.csat
                .as_ref()
                .is_some_and(|c| c.score.is_some() && c.score.as_deref() == val_str)
        }
        "actor" => ctx.actor.is_some() && ctx.actor == val_str,
        "comment" => match val_str {
            Some("public") => ctx.comment.is_some_and(|c| c.public),
            Some("private") => ctx.comment.is_some_and(|c| !c.public),
            Some("present") => ctx.comment.is_some(),
            _ => ctx.comment.is_none(),
        },
        other => {
            let Some(id) = other.strip_prefix("field:") else {
                return false;
            };
            let v = t.fields.get(id);
            let blank =
                matches!(v, None | Some(Value::Null)) || v == Some(&Value::String(String::new()));
            match op {
                "present" => !blank,
                "absent" => blank,
                _ => {
                    let eq = nullish_empty(v) == nullish_empty(Some(value));
                    if op == "is_not" {
                        !eq
                    } else {
                        eq
                    }
                }
            }
        }
    }
}

/// All of `all` must hold and, when `any` is non-empty, at least one of it.
pub fn matches(conditions: &Conditions, t: &Ticket, ctx: &Ctx<'_>, vocab: &Vocab) -> bool {
    conditions
        .all
        .iter()
        .all(|c| test_condition(c, t, ctx, vocab))
        && (conditions.any.is_empty()
            || conditions
                .any
                .iter()
                .any(|c| test_condition(c, t, ctx, vocab)))
}

/// Whether a ticket belongs in a view; `special` views (`spam`, `deleted`) look at flags only.
pub fn in_view(
    special: Option<&str>,
    conditions: &Conditions,
    t: &Ticket,
    ctx: &Ctx<'_>,
    vocab: &Vocab,
) -> bool {
    match special {
        Some("spam") => t.spam && !t.deleted,
        Some("deleted") => t.deleted,
        _ => {
            if t.spam || t.deleted || t.merged_into.as_deref().is_some_and(|m| !m.is_empty()) {
                return false;
            }
            matches(conditions, t, ctx, vocab)
        }
    }
}

// ------------------------------------------------------------------ actions

/// Applies field actions to a copy of the ticket; returns it and the notifications asked for.
pub fn apply_actions(
    ticket: &Ticket,
    actions: &[Action],
    me: Option<&str>,
    vocab: &Vocab,
) -> (Ticket, Vec<Notification>) {
    let mut t = ticket.clone();
    let mut notes = Vec::new();
    let me_or_null = || me.filter(|m| !m.is_empty()).map(str::to_owned);
    for a in actions {
        let s = a.value.as_str();
        match a.field.as_str() {
            "status" => {
                if let Some(v) = s {
                    if vocab.statuses.iter().any(|x| x == v)
                        && !vocab.unsettable.iter().any(|x| x == v)
                    {
                        t.status = v.to_owned();
                    }
                }
            }
            "priority" => {
                t.priority = s
                    .filter(|v| vocab.priorities.iter().any(|p| p == v))
                    .map(str::to_owned);
            }
            "type" => {
                t.kind = s
                    .filter(|v| vocab.types.iter().any(|p| p == v))
                    .map(str::to_owned);
            }
            "assignee" => {
                t.assignee = if s == Some("me") {
                    me_or_null()
                } else if truthy(&a.value) {
                    Some(js_string(&a.value))
                } else {
                    None
                };
            }
            "group" => {
                t.group = truthy(&a.value).then(|| js_string(&a.value));
            }
            "add_tags" => {
                for tag in norm_tags(&a.value) {
                    if !t.tags.contains(&tag) {
                        t.tags.push(tag);
                    }
                }
            }
            "remove_tags" => {
                let drop = norm_tags(&a.value);
                t.tags.retain(|x| !drop.contains(x));
            }
            "set_tags" => t.tags = norm_tags(&a.value),
            "add_follower" => {
                let who = if s == Some("me") {
                    me_or_null()
                } else {
                    truthy(&a.value).then(|| js_string(&a.value))
                };
                if let Some(w) = who {
                    if !t.followers.contains(&w) {
                        t.followers.push(w);
                    }
                }
            }
            k @ ("notify_requester" | "notify_email") => notes.push(Notification {
                kind: k.to_owned(),
                to: a.to.clone().filter(|x| !x.is_empty()),
                subject: a.subject.clone().unwrap_or_default(),
                body: a.body.clone().unwrap_or_default(),
                trigger: None,
            }),
            other => {
                if let Some(id) = other.strip_prefix("field:") {
                    if matches!(&a.value, Value::Null) || a.value == Value::String(String::new()) {
                        t.fields.remove(id);
                    } else {
                        t.fields.insert(id.to_owned(), a.value.clone());
                    }
                }
            }
        }
    }
    (t, notes)
}

/// When a trigger runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Created,
    Updated,
}

/// A trigger: conditions plus actions, run on created / updated / any (`event` empty).
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Trigger {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub active: bool,
    #[serde(default)]
    pub event: Option<String>,
    #[serde(default)]
    pub conditions: Conditions,
    #[serde(default)]
    pub actions: Vec<Action>,
}

/// One fired trigger.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Fired {
    pub id: String,
    pub title: String,
}

/// Runs the active triggers against an update. Each fires at most once, in order, and sees the
/// ticket as earlier triggers left it.
pub fn run_triggers(
    triggers: &[Trigger],
    after: &Ticket,
    event: Event,
    ctx: &Ctx<'_>,
    vocab: &Vocab,
) -> (Ticket, Vec<Fired>, Vec<Notification>) {
    let mut t = after.clone();
    let mut fired = Vec::new();
    let mut notes = Vec::new();
    for trig in triggers {
        if !trig.active {
            continue;
        }
        match trig.event.as_deref() {
            Some("created") if event != Event::Created => continue,
            Some("updated") if event != Event::Updated => continue,
            _ => {}
        }
        if !matches(&trig.conditions, &t, ctx, vocab) {
            continue;
        }
        let (next, n) = apply_actions(&t, &trig.actions, ctx.me, vocab);
        t = next;
        fired.push(Fired {
            id: trig.id.clone(),
            title: trig.title.clone(),
        });
        notes.extend(n.into_iter().map(|mut x| {
            x.trigger = Some(trig.title.clone());
            x
        }));
    }
    (t, fired, notes)
}

// ------------------------------------------------------------------ placeholders

/// First name for greetings.
pub fn first_name(name: &str) -> &str {
    name.split_whitespace().next().unwrap_or("")
}

/// Fills `{{placeholders}}`; unknown names are left as typed, missing values become empty.
pub fn render_placeholders(text: &str, data: &HashMapLike<'_>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            out.push_str(&rest[start..]);
            return out;
        };
        let inner = &after[..end];
        let name = inner.trim_matches(|c: char| c.is_whitespace());
        let well_formed = !name.is_empty()
            && name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b == b'_' || b == b'.');
        if well_formed {
            match data.get(name) {
                Some(v) => out.push_str(v.unwrap_or("")),
                None => out.push_str(&rest[start..start + 2 + end + 2]),
            }
            rest = &after[end + 2..];
        } else {
            // Not a placeholder: keep "{{" and scan on from the next character.
            out.push_str("{{");
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

/// The placeholder values: `Some(None)` means known but empty.
pub struct HashMapLike<'a> {
    pub entries: Vec<(&'static str, Option<&'a str>)>,
}

impl HashMapLike<'_> {
    fn get(&self, name: &str) -> Option<Option<&str>> {
        self.entries
            .iter()
            .find(|(k, _)| *k == name)
            .map(|(_, v)| *v)
    }
}

/// The placeholder names a desk knows.
pub const PLACEHOLDER_NAMES: &[&str] = &[
    "ticket.id",
    "ticket.subject",
    "ticket.status",
    "ticket.priority",
    "ticket.link",
    "requester.name",
    "requester.first_name",
    "requester.email",
    "agent.name",
    "agent.first_name",
    "desk.name",
    "comment.text",
];

// ------------------------------------------------------------------ SLA

/// Targets of one priority, in minutes (`0` = none).
#[derive(
    Debug, Clone, Copy, Default, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
#[serde(default)]
#[schema(as = ServiceDeskSlaTarget)]
pub struct SlaTarget {
    /// First reply / first response.
    pub first: i64,
    /// Next reply after the requester writes again.
    pub next: i64,
    /// Resolution.
    pub resolve: i64,
}

/// SLA targets per priority.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Sla {
    pub enabled: bool,
    pub targets: std::collections::BTreeMap<String, SlaTarget>,
}

/// The target due soonest.
#[derive(Debug, Clone, PartialEq, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = ServiceDeskSlaState)]
pub struct SlaState {
    /// `first`, `next` or `resolve`.
    pub metric: String,
    pub due_at: f64,
    pub remaining: f64,
    pub breached: bool,
}

fn pos(v: Option<f64>) -> Option<f64> {
    v.filter(|x| *x != 0.0 && !x.is_nan())
}

/// The service level target due soonest for a ticket, or `None` when none applies. Reply
/// targets pause while the ticket waits on someone else.
pub fn sla_status(t: &Ticket, sla: &Sla, now: f64, vocab: &Vocab) -> Option<SlaState> {
    if !sla.enabled || !vocab.is_unsolved(&t.status) || t.spam || t.deleted {
        return None;
    }
    let key = t
        .priority
        .as_deref()
        .filter(|p| !p.is_empty())
        .unwrap_or(&vocab.default_priority);
    let target = sla.targets.get(key)?;
    let waiting = vocab.waiting.contains(&t.status);
    let created = t.created_at.unwrap_or(f64::NAN);
    let mut active: Vec<(&str, f64)> = Vec::new();
    if !waiting && pos(t.first_reply_at).is_none() && target.first != 0 {
        let from = match pos(t.last_agent_reply_at) {
            None => Some(created),
            Some(agent) => t.last_customer_at.filter(|c| *c > agent),
        };
        if let Some(f) = from.filter(|f| *f != 0.0 && !f.is_nan()) {
            active.push(("first", f + target.first as f64 * MIN_MS));
        }
    }
    if !waiting && target.next != 0 {
        if let (Some(_), Some(c)) = (pos(t.first_reply_at), pos(t.last_customer_at)) {
            if c > t.last_agent_reply_at.filter(|a| *a != 0.0).unwrap_or(0.0) {
                active.push(("next", c + target.next as f64 * MIN_MS));
            }
        }
    }
    if target.resolve != 0 {
        active.push(("resolve", created + target.resolve as f64 * MIN_MS));
    }
    // Stable, like Array.prototype.sort; NaN compares as "equal" there too.
    active.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
    let (metric, due) = *active.first()?;
    Some(SlaState {
        metric: metric.to_owned(),
        due_at: due,
        remaining: due - now,
        breached: due <= now,
    })
}

fn js_round(x: f64) -> f64 {
    let f = x.floor();
    if x - f >= 0.5 {
        f + 1.0
    } else {
        f
    }
}

/// Compact duration: 45s, 12m, 3h, 2d, 5w; negative values keep their sign.
pub fn short_duration(ms: f64) -> String {
    let sign = if ms < 0.0 { "-" } else { "" };
    let s = ms.abs() / 1000.0;
    if s < 60.0 {
        return format!("{sign}{}", js_number_string(js_round(s).max(1.0)) + "s");
    }
    let m = s / 60.0;
    if m < 60.0 {
        return format!("{sign}{}m", js_number_string(js_round(m)));
    }
    let h = m / 60.0;
    if h < 48.0 {
        return format!("{sign}{}h", js_number_string(js_round(h)));
    }
    let d = h / 24.0;
    if d < 14.0 {
        return format!("{sign}{}d", js_number_string(js_round(d)));
    }
    format!("{sign}{}w", js_number_string(js_round(d / 7.0)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn cond(field: &str, op: &str, value: Value) -> Condition {
        Condition {
            field: field.into(),
            op: op.into(),
            value,
        }
    }

    #[test]
    fn campus_conditions_rank_statuses_and_priorities() {
        let v = Vocab::campus();
        let t = Ticket {
            status: "assigned".into(),
            priority: Some("high".into()),
            subject: "Projector not working in LH-2".into(),
            tags: vec!["av".into()],
            created_at: Some(1.0),
            ..Ticket::default()
        };
        let ctx = Ctx {
            now: 5.0 * HOUR_MS,
            ..Ctx::default()
        };
        assert!(test_condition(
            &cond("status", "less_than", json!("resolved")),
            &t,
            &ctx,
            &v
        ));
        assert!(test_condition(
            &cond("priority", "greater_than", json!("medium")),
            &t,
            &ctx,
            &v
        ));
        assert!(test_condition(
            &cond("subject", "contains", json!("PROJECTOR")),
            &t,
            &ctx,
            &v
        ));
        assert!(test_condition(
            &cond("tags", "includes", json!("AV, it")),
            &t,
            &ctx,
            &v
        ));
        assert!(test_condition(
            &cond("created", "greater_than", json!(4)),
            &t,
            &ctx,
            &v
        ));
        assert!(!test_condition(
            &cond("created", "greater_than", json!("")),
            &t,
            &ctx,
            &v
        ));
    }

    #[test]
    fn actions_and_triggers() {
        let v = Vocab::campus();
        let t = Ticket {
            status: "open".into(),
            subject: "Water leak".into(),
            ..Ticket::default()
        };
        let trig = Trigger {
            id: "1".into(),
            title: "Leaks are urgent".into(),
            active: true,
            event: Some("created".into()),
            conditions: Conditions {
                all: vec![cond("subject", "contains", json!("leak"))],
                any: vec![],
            },
            actions: vec![
                Action {
                    field: "priority".into(),
                    value: json!("critical"),
                    to: None,
                    subject: None,
                    body: None,
                },
                Action {
                    field: "add_tags".into(),
                    value: json!("plumbing Urgent"),
                    to: None,
                    subject: None,
                    body: None,
                },
                Action {
                    field: "assignee".into(),
                    value: json!("team:estate"),
                    to: None,
                    subject: None,
                    body: None,
                },
            ],
        };
        let (out, fired, _) = run_triggers(
            std::slice::from_ref(&trig),
            &t,
            Event::Created,
            &Ctx::default(),
            &v,
        );
        assert_eq!(fired.len(), 1);
        assert_eq!(out.priority.as_deref(), Some("critical"));
        assert_eq!(out.tags, vec!["plumbing", "urgent"]);
        assert_eq!(out.assignee.as_deref(), Some("team:estate"));
        let (_, fired, _) = run_triggers(&[trig], &t, Event::Updated, &Ctx::default(), &v);
        assert!(fired.is_empty());
        assert_eq!(short_duration(-90.0 * 60_000.0), "-2h");
        assert_eq!(short_duration(500.0), "1s");
    }

    #[test]
    fn sla_pauses_replies_on_hold() {
        let v = Vocab::campus();
        let mut targets = std::collections::BTreeMap::new();
        targets.insert(
            "medium".into(),
            SlaTarget {
                first: 60,
                next: 0,
                resolve: 600,
            },
        );
        let sla = Sla {
            enabled: true,
            targets,
        };
        let mut t = Ticket {
            status: "open".into(),
            created_at: Some(1.0),
            ..Ticket::default()
        };
        let s = sla_status(&t, &sla, 30.0 * MIN_MS, &v).unwrap();
        assert_eq!((s.metric.as_str(), s.breached), ("first", false));
        t.status = "on_hold".into();
        let s = sla_status(&t, &sla, 30.0 * MIN_MS, &v).unwrap();
        assert_eq!(s.metric, "resolve");
        t.status = "resolved".into();
        assert!(sla_status(&t, &sla, 0.0, &v).is_none());
    }

    #[test]
    fn placeholders_keep_unknown_names() {
        let data = HashMapLike {
            entries: vec![
                ("ticket.id", Some("SR-000007")),
                ("requester.first_name", None),
            ],
        };
        assert_eq!(
            render_placeholders(
                "Hi {{ requester.first_name }}, #{{ticket.id}} {{nope}} {{Bad}}",
                &data
            ),
            "Hi , #SR-000007 {{nope}} {{Bad}}"
        );
    }
}
