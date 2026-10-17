//! **ERP_EXTENSIONS 2B** — service-desk automation at runtime: the institution's triggers run
//! on every ticket opened (any Campus slice, through `shared::tickets::open_ticket`) and every
//! office command (`servicedesk::http::command`), inside the same transaction. Triggers may set
//! the priority (SLA re-timed from the policy), the category, the assignee (an open / triaged
//! ticket moves to `assigned` through the normal path) and tags; never the status. Each firing
//! is logged in `campus.ticket_rule_run` and audited.
//!
//! The SLA policy (`campus.ticket_sla_policy`) gives first-response and resolution targets per
//! priority. The resolution target times `sla_due_at`; the first-response target is stamped
//! off `first_response_at` (set the first time a ticket leaves `open` / `reopened`) and checked
//! by the SLA sweep.

use super::rules::{self, Event, Sla, SlaTarget, Ticket, Trigger, Vocab};
use crate::shared::rows::{lock_ticket, ServiceTicket};
use crate::shared::{publish, Actor};
use chrono::{DateTime, Utc};
use sqlx::{PgConnection, Row};
use std::collections::BTreeMap;
use sutra_contracts::campus::status::{TicketCategory, TicketPriority, TicketStatus};
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use uuid::Uuid;

/// The SLA policy of an institution, or `None` when it has not set one.
pub async fn load_policy(
    conn: &mut PgConnection,
    tenant: TenantId,
    inst: InstitutionId,
) -> Result<Option<Sla>, KernelError> {
    let row = sqlx::query(
        "SELECT enabled, targets FROM campus.ticket_sla_policy \
         WHERE tenant_id = $1 AND institution_id = $2",
    )
    .bind(tenant.as_uuid())
    .bind(inst.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let Some(r) = row else { return Ok(None) };
    let targets: serde_json::Value = r.try_get("targets").map_err(map_db_error)?;
    let targets: BTreeMap<String, SlaTarget> = serde_json::from_value(targets).unwrap_or_default();
    Ok(Some(Sla {
        enabled: r.try_get("enabled").map_err(map_db_error)?,
        targets,
    }))
}

/// The defaults: resolution from `TicketPriority::default_sla_hours`, first response a tenth
/// of it (at least an hour), no next-reply target.
pub fn default_sla() -> Sla {
    let mut targets = BTreeMap::new();
    for p in [
        TicketPriority::Low,
        TicketPriority::Medium,
        TicketPriority::High,
        TicketPriority::Critical,
    ] {
        let resolve = p.default_sla_hours() * 60;
        targets.insert(
            p.as_str().to_owned(),
            SlaTarget {
                first: (resolve / 10).max(60),
                next: 0,
                resolve,
            },
        );
    }
    Sla {
        enabled: true,
        targets,
    }
}

/// Minutes to resolve a ticket of `priority`: the policy's target when set, else the default.
pub async fn resolve_minutes(
    conn: &mut PgConnection,
    tenant: TenantId,
    inst: InstitutionId,
    priority: TicketPriority,
) -> Result<i64, KernelError> {
    let policy = load_policy(conn, tenant, inst).await?;
    Ok(policy
        .filter(|p| p.enabled)
        .and_then(|p| p.targets.get(priority.as_str()).map(|t| t.resolve))
        .filter(|m| *m > 0)
        .unwrap_or(priority.default_sla_hours() * 60))
}

fn ms(t: DateTime<Utc>) -> f64 {
    t.timestamp_millis() as f64
}

/// The rules view of a campus ticket.
pub async fn snapshot(
    conn: &mut PgConnection,
    tenant: TenantId,
    t: &ServiceTicket,
) -> Result<Ticket, KernelError> {
    let row = sqlx::query(
        "SELECT tags, first_response_at, updated_at FROM campus.service_ticket \
         WHERE id = $1 AND tenant_id = $2",
    )
    .bind(t.id.as_uuid())
    .bind(tenant.as_uuid())
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let tags: Vec<String> = row.try_get("tags").map_err(map_db_error)?;
    let first: Option<DateTime<Utc>> = row.try_get("first_response_at").map_err(map_db_error)?;
    let updated: DateTime<Utc> = row.try_get("updated_at").map_err(map_db_error)?;
    Ok(to_rules(t, tags, first, updated))
}

/// Pure mapping of a ticket row onto the rules record.
pub fn to_rules(
    t: &ServiceTicket,
    tags: Vec<String>,
    first_response_at: Option<DateTime<Utc>>,
    updated_at: DateTime<Utc>,
) -> Ticket {
    let mut fields = serde_json::Map::new();
    fields.insert("complaint".into(), serde_json::Value::Bool(t.is_complaint));
    if let Some(n) = t.node_id {
        fields.insert("node".into(), serde_json::Value::String(n.as_uuid().to_string()));
    }
    if let Some(k) = t.requester_kind {
        fields.insert("requester_kind".into(), serde_json::Value::String(k.as_str().to_owned()));
    }
    Ticket {
        status: t.status.as_str().to_owned(),
        priority: Some(t.priority.as_str().to_owned()),
        kind: Some(t.category.as_str().to_owned()),
        assignee: t.assignee.clone(),
        requester: t.requester_id.map(|r| r.to_string()),
        via: Some(t.source.as_str().to_owned()),
        tags,
        subject: t.title.clone(),
        created_at: Some(ms(t.created_at)),
        updated_at: Some(ms(updated_at)),
        solved_at: t.resolved_at.map(ms),
        fields,
        merged_into: t.canonical_ticket_id.map(|c| c.as_uuid().to_string()),
        first_reply_at: first_response_at.map(ms),
        ..Ticket::default()
    }
}

/// The active triggers of an institution, in order.
pub async fn load_triggers(
    conn: &mut PgConnection,
    tenant: TenantId,
    inst: InstitutionId,
) -> Result<Vec<Trigger>, KernelError> {
    let rows = sqlx::query(
        "SELECT id, title, event, conditions, actions FROM campus.ticket_rule \
         WHERE tenant_id = $1 AND institution_id = $2 AND active ORDER BY position, created_at, id",
    )
    .bind(tenant.as_uuid())
    .bind(inst.as_uuid())
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    rows.iter()
        .map(|r| {
            let id: Uuid = r.try_get("id")?;
            let event: String = r.try_get("event")?;
            let conditions: serde_json::Value = r.try_get("conditions")?;
            let actions: serde_json::Value = r.try_get("actions")?;
            Ok(Trigger {
                id: id.to_string(),
                title: r.try_get("title")?,
                active: true,
                event: (event != "any").then_some(event),
                conditions: serde_json::from_value(conditions).unwrap_or_default(),
                actions: serde_json::from_value(actions).unwrap_or_default(),
            })
        })
        .collect::<Result<_, sqlx::Error>>()
        .map_err(map_db_error)
}

fn actor_kind(actor: &Actor, t: &ServiceTicket) -> &'static str {
    match actor.id.as_deref() {
        Some(id) if id.starts_with("system:") => "system",
        _ if t.source.as_str() == "self_service" && t.requester_subject.is_some()
            && t.requester_subject.as_deref() == actor.id.as_deref() =>
        {
            "customer"
        }
        _ => "agent",
    }
}

/// What a run changed (for the log and the audit).
#[derive(Debug, Default, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Changes {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
}

impl Changes {
    fn is_empty(&self) -> bool {
        self == &Changes::default()
    }
}

/// Field changes from `before` to `after` the triggers ran (status is never taken).
pub fn diff(before: &Ticket, after: &Ticket) -> Changes {
    let changed = |a: &Option<String>, b: &Option<String>| (a != b).then(|| b.clone()).flatten();
    Changes {
        priority: changed(&before.priority, &after.priority),
        category: changed(&before.kind, &after.kind),
        assignee: changed(&before.assignee, &after.assignee),
        tags: (before.tags != after.tags).then(|| after.tags.clone()),
    }
}

/// Runs automation after `event` on `ticket` (the row as the command left it; `before` is the
/// row before the command). Returns the ticket as it now stands.
pub async fn after_event(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    ticket: ServiceTicket,
    before: Option<&ServiceTicket>,
    event: Event,
    actor: &Actor,
) -> Result<ServiceTicket, KernelError> {
    let tenant = scope.tenant_id;
    // First response: the first time the ticket leaves open / reopened.
    if !matches!(ticket.status, TicketStatus::Open | TicketStatus::Reopened) {
        sqlx::query(
            "UPDATE campus.service_ticket SET first_response_at = now() \
             WHERE id = $1 AND tenant_id = $2 AND first_response_at IS NULL",
        )
        .bind(ticket.id.as_uuid())
        .bind(tenant.as_uuid())
        .execute(&mut *conn)
        .await
        .map_err(map_db_error)?;
    }
    let triggers = load_triggers(conn, tenant, ticket.institution_id).await?;
    if triggers.is_empty() {
        return Ok(ticket);
    }
    let vocab = Vocab::campus();
    let now_snap = snapshot(conn, tenant, &ticket).await?;
    let before_snap = match before {
        Some(b) => {
            let mut s = now_snap.clone();
            s.status = b.status.as_str().to_owned();
            s.priority = Some(b.priority.as_str().to_owned());
            s.kind = Some(b.category.as_str().to_owned());
            s.assignee = b.assignee.clone();
            Some(s)
        }
        None => None,
    };
    let ctx = rules::Ctx {
        me: actor.id.as_deref(),
        now: ms(Utc::now()),
        before: before_snap.as_ref(),
        actor: Some(actor_kind(actor, &ticket)),
        comment: None,
    };
    let (after, fired, _notes) = rules::run_triggers(&triggers, &now_snap, event, &ctx, &vocab);
    if fired.is_empty() {
        return Ok(ticket);
    }
    let changes = diff(&now_snap, &after);
    let mut current = ticket;
    if !changes.is_empty() {
        current = apply(conn, scope, current, &changes, actor).await?;
    }
    let ev = match event {
        Event::Created => "created",
        Event::Updated => "updated",
    };
    for f in &fired {
        sqlx::query(
            "INSERT INTO campus.ticket_rule_run (tenant_id, institution_id, ticket_id, rule_id, \
             rule_title, event, changes) VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(tenant.as_uuid())
        .bind(current.institution_id.as_uuid())
        .bind(current.id.as_uuid())
        .bind(Uuid::parse_str(&f.id).ok())
        .bind(&f.title)
        .bind(ev)
        .bind(serde_json::to_value(&changes).unwrap_or_default())
        .execute(&mut *conn)
        .await
        .map_err(map_db_error)?;
    }
    publish::audit(
        conn,
        scope,
        "campus.service_ticket.automation",
        "service_ticket",
        current.id.as_uuid().to_string(),
        serde_json::json!({
            "event": ev,
            "rules": fired.iter().map(|f| &f.title).collect::<Vec<_>>(),
            "changes": changes,
        }),
        actor,
    )
    .await?;
    Ok(current)
}

async fn apply(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    ticket: ServiceTicket,
    c: &Changes,
    actor: &Actor,
) -> Result<ServiceTicket, KernelError> {
    let tenant = scope.tenant_id;
    let priority = c
        .priority
        .as_deref()
        .and_then(TicketPriority::parse)
        .unwrap_or(ticket.priority);
    let category = c
        .category
        .as_deref()
        .and_then(TicketCategory::parse)
        .unwrap_or(ticket.category);
    let minutes = if c.priority.is_some() {
        Some(resolve_minutes(conn, tenant, ticket.institution_id, priority).await?)
    } else {
        None
    };
    let mins = minutes.map(|m| i32::try_from(m).unwrap_or(i32::MAX));
    sqlx::query(
        "UPDATE campus.service_ticket SET priority = $3, category = $4, \
         tags = coalesce($5, tags), \
         sla_due_at = CASE WHEN $6::int IS NULL THEN sla_due_at \
                           ELSE created_at + make_interval(mins => $6::int) END, \
         sla_breached = CASE WHEN $6::int IS NULL THEN sla_breached \
                             ELSE sla_breached AND created_at + make_interval(mins => $6::int) < now() END, \
         assignee = CASE WHEN $7::text IS NOT NULL AND status NOT IN ('open', 'triaged') THEN $7 \
                         ELSE assignee END, \
         version = version + 1, updated_at = now() WHERE id = $1 AND tenant_id = $2",
    )
    .bind(ticket.id.as_uuid())
    .bind(tenant.as_uuid())
    .bind(priority.as_str())
    .bind(category.as_str())
    .bind(c.tags.as_ref())
    .bind(mins)
    .bind(c.assignee.as_deref())
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let mut current = lock_ticket(conn, tenant, ticket.id).await?;
    // An open / triaged ticket given an assignee is assigned through the normal path.
    if let Some(who) = c.assignee.as_deref() {
        if current.status.can_transition(TicketStatus::Assigned)
            && matches!(current.status, TicketStatus::Open | TicketStatus::Triaged)
        {
            current = super::http::assign_ticket(
                conn,
                scope,
                &current,
                who,
                Some("assigned by a service desk rule"),
                actor,
            )
            .await?;
        }
    }
    Ok(current)
}

/// SLA state of a ticket row for the board.
pub fn board_state(t: &Ticket, sla: &Sla, now: DateTime<Utc>) -> Option<rules::SlaState> {
    rules::sla_status(t, sla, ms(now), &Vocab::campus())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_takes_fields_not_status() {
        let a = Ticket {
            status: "open".into(),
            priority: Some("medium".into()),
            tags: vec!["x".into()],
            ..Ticket::default()
        };
        let mut b = a.clone();
        b.status = "assigned".into();
        assert!(diff(&a, &b).is_empty());
        b.priority = Some("critical".into());
        b.tags.push("y".into());
        let d = diff(&a, &b);
        assert_eq!(d.priority.as_deref(), Some("critical"));
        assert_eq!(d.tags, Some(vec!["x".into(), "y".into()]));
        let s = default_sla();
        assert_eq!(s.targets["critical"].resolve, 240);
        assert_eq!(s.targets["critical"].first, 60);
        assert_eq!(s.targets["low"].first, 1008);
    }
}
