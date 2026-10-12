//! In-process, outbox-driven event consumer ("inbox projector", ADR-L5-0007 / ADR-L5-0010).
//!
//! Committed publication intents in `platform.outbox` are read by event type — independently of
//! whether the NATS relay is on — and turned into notifications and to-dos. Exactly-once per
//! consumer comes from `platform.event_receipt` (one row per consumer + event id, written in the
//! same transaction as the projection); delivery stays at-least-once and every projection is
//! idempotent on the event id as well. Each event runs inside a savepoint: a poison event is
//! recorded as `failed` and never blocks the rest.
//!
//! The job runs under the sweeper scheduler's advisory lock, so one replica projects at a time.
//! Events older than the look-back window (default = outbox retention) are ignored.
//!
//! Rules ([`plan`] is pure and unit tested):
//!
//! | Event | Projection |
//! |-------|------------|
//! | `institution.registered` | notification to every administrator; to-do "complete the setup" for the registering user |
//! | `institution.renamed` | notification to every administrator |
//! | `platform.notice.published` | notification to every audience role, visible from `publishAt` |
//! | `platform.todo.assigned` | notification to the assignee |

use super::notifications::{self, NewNotification, NotificationCategory, Recipient, Severity};
use super::todos::{self, SystemTodo};
use chrono::Duration;
use sqlx::{PgConnection, Row};
use sutra_contracts::institution::events::{
    InstitutionRegisteredV1, InstitutionRenamedV1, INSTITUTION_REGISTERED, INSTITUTION_RENAMED,
};
use sutra_contracts::platform::events::{
    NoticeChangedV1, TodoAssignedV1, NOTICE_PUBLISHED, TODO_ASSIGNED,
};
use sutra_kernel::auth::Role;
use sutra_kernel::events::EventEnvelope;
use sutra_kernel::scope::TenantId;
use sutra_persistence::jobs::SweepJob;

/// Consumer name in `platform.event_receipt`.
pub const CONSUMER: &str = "platform.inbox";

/// Batch 0/1 event types this consumer projects (Batch 2 types come from [`super::rules`]).
pub const EVENT_TYPES: [&str; 4] = [
    INSTITUTION_REGISTERED,
    INSTITUTION_RENAMED,
    NOTICE_PUBLISHED,
    TODO_ASSIGNED,
];

/// Every event type this consumer projects: [`EVENT_TYPES`] + the Batch 2 rule files.
pub fn event_types() -> Vec<&'static str> {
    let mut v = EVENT_TYPES.to_vec();
    v.extend(super::rules::event_types());
    v
}

/// One projection decided for an event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InboxAction {
    Notify(NewNotification),
    Todo(SystemTodo),
}

fn data<T: serde::de::DeserializeOwned>(env: &EventEnvelope) -> Result<T, String> {
    serde_json::from_value(env.data.clone()).map_err(|e| format!("{}: {e}", env.event_type))
}

/// Decides the projections of one event (pure). Unknown types yield nothing.
pub fn plan(env: &EventEnvelope) -> Result<Vec<InboxAction>, String> {
    // Batch 2 (Domain B) rules live in one file per slice (`inbox/rules/`).
    if let Some(planned) = super::rules::plan(env) {
        return planned;
    }
    let tenant = TenantId(env.tenantid);
    // Institution lifecycle events concern the whole tenant (their envelope carries the new
    // institution's id, not the actor's session institution); everything else keeps its scope.
    let institution = if env
        .event_type
        .starts_with("sutraerp.institution.institution.")
    {
        None
    } else {
        env.institutionid
    };
    let base = |recipient: Recipient,
                category: NotificationCategory,
                severity: Severity,
                title: String,
                body: String,
                link: Option<&str>| NewNotification {
        tenant,
        institution,
        recipient,
        category,
        severity,
        title,
        body,
        link: link.map(str::to_owned),
        source_event_id: Some(env.id),
        visible_at: None,
    };
    Ok(match env.event_type.as_str() {
        INSTITUTION_REGISTERED => {
            let d: InstitutionRegisteredV1 = data(env)?;
            let mut v = vec![InboxAction::Notify(base(
                Recipient::Role(Role::Administrator),
                NotificationCategory::Institution,
                Severity::Success,
                format!("Institution registered: {}", d.name),
                format!("{} ({}) was added to SutraERP.", d.name, d.code),
                Some("/academics"),
            ))];
            if let Some(actor) = env.actorref.as_deref().filter(|a| !a.is_empty()) {
                v.push(InboxAction::Todo(SystemTodo {
                    tenant,
                    institution: None,
                    assignee_subject: actor.to_owned(),
                    title: format!("Complete the setup of {}", d.name),
                    detail: "Add campuses, departments, programmes and the academic calendar."
                        .into(),
                    due_at: Some(env.time + Duration::days(7)),
                    link: Some("/academics".into()),
                    source_ref: env.id.to_string(),
                }));
            }
            v
        }
        INSTITUTION_RENAMED => {
            let d: InstitutionRenamedV1 = data(env)?;
            vec![InboxAction::Notify(base(
                Recipient::Role(Role::Administrator),
                NotificationCategory::Institution,
                Severity::Info,
                format!("Institution renamed to {}", d.name),
                format!("The institution record is now at version {}.", d.version),
                Some("/academics"),
            ))]
        }
        NOTICE_PUBLISHED => {
            let d: NoticeChangedV1 = data(env)?;
            d.audience_roles
                .iter()
                .filter_map(|r| Role::parse(r))
                .map(|role| {
                    let mut n = base(
                        Recipient::Role(role),
                        NotificationCategory::Notice,
                        Severity::Info,
                        format!("New notice: {}", d.title),
                        if d.pinned {
                            "Pinned announcement.".into()
                        } else {
                            String::new()
                        },
                        Some("/notices"),
                    );
                    n.visible_at = Some(d.publish_at);
                    InboxAction::Notify(n)
                })
                .collect()
        }
        TODO_ASSIGNED => {
            let d: TodoAssignedV1 = data(env)?;
            vec![InboxAction::Notify(base(
                Recipient::Subject(d.assignee_subject),
                NotificationCategory::Todo,
                Severity::Info,
                format!("New to-do: {}", d.title),
                format!("Assigned by {}.", d.assigned_by_name),
                Some("/todos"),
            ))]
        }
        _ => vec![],
    })
}

/// Applies the projections of one event on the job's transaction.
async fn apply(
    conn: &mut PgConnection,
    actions: &[InboxAction],
    email: bool,
) -> Result<(), sqlx::Error> {
    for a in actions {
        match a {
            InboxAction::Notify(n) => {
                notifications::create(&mut *conn, n, email).await?;
            }
            InboxAction::Todo(t) => {
                todos::create_from_event(&mut *conn, t).await?;
            }
        }
    }
    Ok(())
}

async fn receipt(
    conn: &mut PgConnection,
    event_id: uuid::Uuid,
    outcome: &str,
    detail: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO platform.event_receipt (consumer, event_id, outcome, detail) \
         VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING",
    )
    .bind(CONSUMER)
    .bind(event_id)
    .bind(outcome)
    .bind(detail)
    .execute(conn)
    .await?;
    Ok(())
}

/// Scheduled consumer job.
#[derive(Clone, Debug)]
pub struct InboxProjector {
    pub interval: std::time::Duration,
    /// Ignore events older than this (should match the outbox retention).
    pub lookback: std::time::Duration,
    pub batch_size: u32,
    /// Queue e-mail deliveries for opted-in recipients.
    pub email: bool,
}

#[async_trait::async_trait]
impl SweepJob for InboxProjector {
    fn name(&self) -> &'static str {
        "inbox-projector"
    }
    fn interval(&self) -> std::time::Duration {
        self.interval
    }
    async fn run(&self, conn: &mut PgConnection) -> Result<u64, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT o.id, o.envelope FROM platform.outbox o \
             WHERE o.event_type = ANY($1) \
               AND o.created_at > now() - make_interval(secs => $2) \
               AND NOT EXISTS (SELECT 1 FROM platform.event_receipt r \
                               WHERE r.consumer = $3 AND r.event_id = o.id) \
             ORDER BY o.created_at, o.id LIMIT $4",
        )
        .bind(event_types())
        .bind(self.lookback.as_secs_f64())
        .bind(CONSUMER)
        .bind(i64::from(self.batch_size.max(1)))
        .fetch_all(&mut *conn)
        .await?;
        let mut processed = 0;
        for row in rows {
            let id: uuid::Uuid = row.try_get("id")?;
            let raw: serde_json::Value = row.try_get("envelope")?;
            let planned = serde_json::from_value::<EventEnvelope>(raw)
                .map_err(|e| e.to_string())
                .and_then(|env| plan(&env));
            let actions = match planned {
                Ok(a) => a,
                Err(why) => {
                    tracing::warn!(event = %id, reason = %why, "inbox consumer: undecodable event");
                    receipt(&mut *conn, id, "failed", Some(&why)).await?;
                    continue;
                }
            };
            sqlx::query("SAVEPOINT inbox_event")
                .execute(&mut *conn)
                .await?;
            match apply(&mut *conn, &actions, self.email).await {
                Ok(()) => {
                    sqlx::query("RELEASE SAVEPOINT inbox_event")
                        .execute(&mut *conn)
                        .await?;
                    let outcome = if actions.is_empty() {
                        "ignored"
                    } else {
                        "processed"
                    };
                    receipt(&mut *conn, id, outcome, None).await?;
                    processed += 1;
                }
                Err(e) => {
                    sqlx::query("ROLLBACK TO SAVEPOINT inbox_event")
                        .execute(&mut *conn)
                        .await?;
                    tracing::warn!(event = %id, error = %e, "inbox consumer: projection failed");
                    receipt(&mut *conn, id, "failed", Some(&e.to_string())).await?;
                }
            }
        }
        Ok(processed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sutra_kernel::events::NewEvent;
    use sutra_kernel::scope::{ExecutionScope, InstitutionId};

    fn env(event_type: &'static str, data: serde_json::Value) -> EventEnvelope {
        EventEnvelope::new(
            &ExecutionScope::institution(TenantId::new(), InstitutionId::new()),
            NewEvent {
                source: "/test",
                event_type,
                dataschema: "urn:test",
                subject: None,
                data,
            },
        )
        .with_actor(Some("dev|admin@sutra.dev".into()))
    }

    #[test]
    fn institution_registered_notifies_admins_and_assigns_setup_todo() {
        let e = env(
            INSTITUTION_REGISTERED,
            serde_json::json!({"institutionId": uuid::Uuid::now_v7(), "code": "SIT", "name": "Sutra"}),
        );
        let a = plan(&e).unwrap_or_default();
        assert_eq!(a.len(), 2);
        let InboxAction::Notify(n) = &a[0] else {
            panic!("notification first")
        };
        assert_eq!(n.recipient, Recipient::Role(Role::Administrator));
        assert_eq!(n.institution, None, "tenant-wide");
        assert_eq!(n.source_event_id, Some(e.id));
        assert_eq!(n.tenant.as_uuid(), e.tenantid);
        let InboxAction::Todo(t) = &a[1] else {
            panic!("to-do second")
        };
        assert_eq!(t.assignee_subject, "dev|admin@sutra.dev");
        assert_eq!(t.source_ref, e.id.to_string());
        assert!(t.due_at.is_some());
    }

    #[test]
    fn notice_published_targets_each_audience_role_from_publish_time() {
        let at = chrono::Utc::now() + Duration::hours(2);
        let e = env(
            NOTICE_PUBLISHED,
            serde_json::json!({"noticeId": uuid::Uuid::now_v7(), "title": "Exams", "audienceRoles": ["student", "faculty", "bogus"],
                "publishAt": at, "expiresAt": null, "pinned": true, "version": 1}),
        );
        let a = plan(&e).unwrap_or_default();
        assert_eq!(a.len(), 2, "unknown roles are skipped");
        for x in &a {
            let InboxAction::Notify(n) = x else {
                panic!("notifications only")
            };
            assert_eq!(n.visible_at, Some(at));
            assert_eq!(n.category, NotificationCategory::Notice);
        }
    }

    #[test]
    fn todo_assigned_notifies_the_assignee_and_bad_payloads_fail() {
        let e = env(
            TODO_ASSIGNED,
            serde_json::json!({"todoId": uuid::Uuid::now_v7(), "assigneeSubject": "dev|x", "title": "Grade",
                "assignedBy": "dev|admin", "assignedByName": "Dhruv", "dueAt": null}),
        );
        let a = plan(&e).unwrap_or_default();
        assert!(
            matches!(&a[..], [InboxAction::Notify(n)] if n.recipient == Recipient::Subject("dev|x".into()))
        );
        assert!(plan(&env(INSTITUTION_RENAMED, serde_json::json!({"nope": 1}))).is_err());
        assert!(
            plan(&env("sutraerp.other.thing.happened", serde_json::json!({})))
                .unwrap_or_default()
                .is_empty()
        );
    }
}
