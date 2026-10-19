//! Helpers shared by the 8B areas (`hostel/`, `transport/`): state machines, row heads + locks,
//! the one status-move routine (status + version + `campus.status_history` + event + audit in the
//! caller's transaction), request bodies, person resolution and the campus charge call.

use crate::shared::lifecycle::{record_history, HistoryEntry, StatusChange};
use crate::shared::me::{require_person, PersonRef};
use crate::shared::publish;
use crate::shared::rows::{ticket_from_row, ServiceTicket, TICKET_COLUMNS};
use crate::shared::{Actor, CampusState};
use axum::http::StatusCode;
use axum::response::Response;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
use sutra_contracts::campus::events::StatusChangedV1;
use sutra_contracts::campus::status::{PersonKind, TicketPriority};
use sutra_contracts::finance::ports::{CampusCharge, CampusChargeCategory, CampusChargeRequest};
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::{FieldViolation, KernelError};
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use utoipa::ToSchema;
use uuid::Uuid;

/// Reads the required `If-Match` (428 response returned early when missing).
macro_rules! expected {
    ($headers:expr) => {
        match $crate::shared::http::if_match(&$headers)? {
            Ok(v) => v.0,
            Err(resp) => return Ok(resp),
        }
    };
}
pub(crate) use expected;

/// A slice state machine (`domain.rs` enums).
pub trait Machine: Copy + Sized + 'static {
    /// Human name of the aggregate (error messages).
    const WHAT: &'static str;
    fn parse_str(s: &str) -> Option<Self>;
    fn name(self) -> &'static str;
    fn can(self, to: Self) -> bool;
}

/// Declares a slice status enum with `ALL`, `as_str`, `parse`, `can_transition` and [`Machine`].
macro_rules! machine {
    ($(#[$meta:meta])* $name:ident, $what:literal { $($(#[$vmeta:meta])* $variant:ident => $wire:literal),+ $(,)? }
        moves [$($from:ident -> $to:ident),+ $(,)?]) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum $name { $($(#[$vmeta])* $variant),+ }

        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$variant),+];

            /// Stored / wire name.
            pub fn as_str(&self) -> &'static str {
                match self { $($name::$variant => $wire),+ }
            }

            /// Strict parse of the stored name.
            pub fn parse(s: &str) -> Option<Self> {
                Self::ALL.iter().copied().find(|v| v.as_str() == s)
            }

            /// Legal moves of this machine (illegal → 409).
            pub fn can_transition(self, to: Self) -> bool {
                matches!((self, to), $(($name::$from, $name::$to))|+)
            }
        }

        impl $crate::hostel::common::Machine for $name {
            const WHAT: &'static str = $what;
            fn parse_str(s: &str) -> Option<Self> {
                Self::parse(s)
            }
            fn name(self) -> &'static str {
                self.as_str()
            }
            fn can(self, to: Self) -> bool {
                self.can_transition(to)
            }
        }
    };
}
pub(crate) use machine;

/// How to read the head of one 8B aggregate table.
pub struct Agg {
    /// Table in schema `campus`.
    pub table: &'static str,
    /// Entity name for 404s.
    pub entity: &'static str,
    /// SQL expression giving the document number / label (`NULL::text` when none).
    pub number: &'static str,
    /// SQL expression giving the person's login subject (`NULL::text` when none).
    pub subject: &'static str,
}

/// The common columns of a locked aggregate row.
#[derive(Clone, Debug)]
pub struct Head {
    pub id: Uuid,
    pub institution: InstitutionId,
    pub status: String,
    pub version: i64,
    pub number: Option<String>,
    pub subject: Option<String>,
}

/// Loads (and with `lock` row-locks) the head of `id` in the caller's tenant (404 otherwise).
pub async fn head(
    conn: &mut PgConnection,
    agg: &Agg,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<Head, KernelError> {
    let sql = format!(
        "SELECT id, institution_id, status, version, {}::text AS label, {}::text AS subject \
         FROM campus.{} WHERE id = $1 AND tenant_id = $2{}",
        agg.number,
        agg.subject,
        agg.table,
        if lock { " FOR UPDATE" } else { "" }
    );
    let r = sqlx::query(&sql)
        .bind(id)
        .bind(tenant.as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound { entity: agg.entity })?;
    head_from_row(&r).map_err(map_db_error)
}

fn head_from_row(r: &PgRow) -> Result<Head, sqlx::Error> {
    Ok(Head {
        id: r.try_get("id")?,
        institution: InstitutionId(r.try_get("institution_id")?),
        status: r.try_get("status")?,
        version: r.try_get("version")?,
        number: r.try_get("label")?,
        subject: r.try_get("subject")?,
    })
}

/// `412` when the `If-Match` version is stale.
pub fn ensure_version(expected: i64, actual: i64) -> Result<(), KernelError> {
    if expected == actual {
        Ok(())
    } else {
        Err(KernelError::VersionConflict { expected, actual })
    }
}

/// Checks that `head` may move to `to` (409 otherwise).
pub fn step<M: Machine>(head: &Head, to: M) -> Result<M, KernelError> {
    let from = M::parse_str(&head.status)
        .ok_or_else(|| KernelError::internal_msg(format!("unknown status {}", head.status)))?;
    if from.can(to) {
        Ok(from)
    } else {
        Err(KernelError::Conflict(format!(
            "{} {} is {}; it cannot become {}",
            M::WHAT,
            head.number.as_deref().unwrap_or(""),
            from.name(),
            to.name()
        )))
    }
}

/// One status move of an 8B aggregate.
pub struct Move<'a> {
    pub to: &'a str,
    /// Contract event `(type, dataschema)` to emit, if the move has one.
    pub event: Option<(&'static str, &'static str)>,
    /// Audit action `campus.<aggregate>.<command>` (also the history cause without an event).
    pub action: &'static str,
    pub reason: Option<&'a str>,
    pub process_instance_id: Option<&'a str>,
}

/// Applies `mv` to the locked `head`: status, `status_reason`, `version + 1`, history, event and
/// audit, all in the caller's transaction. Returns the new version and updates `head`.
pub async fn apply(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    agg: &Agg,
    head: &mut Head,
    mv: Move<'_>,
    actor: &Actor,
) -> Result<i64, KernelError> {
    let sql = format!(
        "UPDATE campus.{} SET status = $1, status_reason = $2, version = version + 1, \
         updated_at = now() WHERE id = $3 AND tenant_id = $4 RETURNING version",
        agg.table
    );
    let version: i64 = sqlx::query_scalar(&sql)
        .bind(mv.to)
        .bind(mv.reason)
        .bind(head.id)
        .bind(scope.tenant_id.as_uuid())
        .fetch_one(&mut *conn)
        .await
        .map_err(map_db_error)?;
    let from = head.status.clone();
    let cause = mv.event.map(|e| e.0).unwrap_or(mv.action);
    record_history(
        conn,
        HistoryEntry {
            tenant: scope.tenant_id,
            institution: head.institution,
            aggregate_type: agg.table,
            aggregate_id: head.id,
            from: Some(&from),
            to: mv.to,
        },
        &StatusChange {
            cause_event_type: cause,
            reason: mv.reason,
            process_instance_id: mv.process_instance_id,
        },
        actor,
    )
    .await?;
    if let Some((event_type, schema)) = mv.event {
        publish::event(
            conn,
            scope,
            event_type,
            schema,
            head.id.to_string(),
            &StatusChangedV1 {
                aggregate_id: head.id,
                institution_id: head.institution.as_uuid(),
                from_status: Some(from.clone()),
                to_status: mv.to.to_owned(),
                version,
                reason: mv.reason.map(str::to_owned),
                process_instance_id: mv.process_instance_id.map(str::to_owned),
                number: head.number.clone(),
                subject_ref: head.subject.clone(),
            },
            actor,
        )
        .await?;
    }
    publish::audit(
        conn,
        scope,
        mv.action,
        agg.table,
        head.id.to_string(),
        serde_json::json!({ "from": from, "to": mv.to, "reason": mv.reason }),
        actor,
    )
    .await?;
    head.status = mv.to.to_owned();
    head.version = version;
    Ok(version)
}

/// Records a pending approval step on the locked `head` without a status change:
/// `process_instance_id`, `version + 1` and audit (`action`). Returns the new version.
pub async fn mark_pending(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    agg: &Agg,
    head: &mut Head,
    process_instance_id: Option<&str>,
    action: &'static str,
    actor: &Actor,
) -> Result<i64, KernelError> {
    let sql = format!(
        "UPDATE campus.{} SET process_instance_id = $1, version = version + 1, updated_at = now() \
         WHERE id = $2 AND tenant_id = $3 RETURNING version",
        agg.table
    );
    let version: i64 = sqlx::query_scalar(&sql)
        .bind(process_instance_id)
        .bind(head.id)
        .bind(scope.tenant_id.as_uuid())
        .fetch_one(&mut *conn)
        .await
        .map_err(map_db_error)?;
    publish::audit(
        conn,
        scope,
        action,
        agg.table,
        head.id.to_string(),
        serde_json::json!({ "status": head.status, "processInstanceId": process_instance_id }),
        actor,
    )
    .await?;
    head.version = version;
    Ok(version)
}

/// History (`None → status`) + audit of a newly created aggregate row.
#[allow(clippy::too_many_arguments)]
pub async fn created(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    agg: &Agg,
    id: Uuid,
    status: &str,
    action: &'static str,
    details: serde_json::Value,
    actor: &Actor,
) -> Result<(), KernelError> {
    record_history(
        conn,
        HistoryEntry {
            tenant: scope.tenant_id,
            institution: scope_institution(scope)?,
            aggregate_type: agg.table,
            aggregate_id: id,
            from: None,
            to: status,
        },
        &StatusChange {
            cause_event_type: action,
            reason: None,
            process_instance_id: None,
        },
        actor,
    )
    .await?;
    publish::audit(
        conn,
        scope,
        action,
        agg.table,
        id.to_string(),
        details,
        actor,
    )
    .await
}

/// Institution of an institution scope.
pub fn scope_institution(scope: &ExecutionScope) -> Result<InstitutionId, KernelError> {
    scope
        .institution_id
        .ok_or_else(|| KernelError::internal_msg("institution scope expected"))
}

/// Fetches one view row (`select` must alias the main table as `x`) in the caller's tenant.
pub async fn fetch<T>(
    conn: &mut PgConnection,
    select: &str,
    tenant: TenantId,
    id: Uuid,
    entity: &'static str,
) -> Result<T, KernelError>
where
    T: for<'r> sqlx::FromRow<'r, PgRow> + Send + Unpin,
{
    let sql = format!("{select} WHERE x.id = $1 AND x.tenant_id = $2");
    sqlx::query_as::<_, T>(&sql)
        .bind(id)
        .bind(tenant.as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound { entity })
}

/// Lists view rows of an institution (`select` aliases the main table `x`; `extra` adds
/// conditions on `$3` = optional status filter; ordered by `order`).
pub async fn list_of<T>(
    conn: &mut PgConnection,
    select: &str,
    tenant: TenantId,
    parent_column: &str,
    parent: Uuid,
    status: Option<&str>,
    order: &str,
) -> Result<Vec<T>, KernelError>
where
    T: for<'r> sqlx::FromRow<'r, PgRow> + Send + Unpin,
{
    let sql = format!(
        "{select} WHERE x.tenant_id = $1 AND x.{parent_column} = $2 \
         AND ($3::text IS NULL OR x.status = $3) ORDER BY {order} LIMIT 500"
    );
    sqlx::query_as::<_, T>(&sql)
        .bind(tenant.as_uuid())
        .bind(parent)
        .bind(status)
        .fetch_all(&mut *conn)
        .await
        .map_err(map_db_error)
}

/// JSON response with `ETag` = `version`.
pub fn reply<T: Serialize>(status: StatusCode, body: &T, version: i64) -> Response {
    crate::shared::http::with_etag(status, body, Version(version))
}

/// 422 on one field.
pub fn invalid(field: &str, code: &str, message: &str) -> KernelError {
    KernelError::Validation(vec![FieldViolation::new(field, code, message)])
}

/// Trimmed non-empty text or `None`.
pub fn clean(s: Option<String>) -> Option<String> {
    s.map(|v| v.trim().to_owned()).filter(|v| !v.is_empty())
}

/// Required trimmed text of at most `max` characters.
pub fn required_text(field: &str, raw: &str, max: usize) -> Result<String, KernelError> {
    let v = raw.trim();
    if v.is_empty() || v.chars().count() > max {
        return Err(invalid(
            field,
            "invalid",
            &format!("1–{max} characters required"),
        ));
    }
    Ok(v.to_owned())
}

/// Optional trimmed text of at most `max` characters.
pub fn optional_text(
    field: &str,
    raw: Option<String>,
    max: usize,
) -> Result<Option<String>, KernelError> {
    match clean(raw) {
        Some(v) if v.chars().count() > max => Err(invalid(
            field,
            "too_long",
            &format!("at most {max} characters"),
        )),
        other => Ok(other),
    }
}

/// Upper-case code (`^[A-Z0-9][A-Z0-9_-]{0,max-1}$`).
pub fn code(field: &str, raw: &str, max: usize) -> Result<String, KernelError> {
    let v = raw.trim().to_ascii_uppercase();
    let b = v.as_bytes();
    let ok = !b.is_empty()
        && b.len() <= max
        && b[0].is_ascii_alphanumeric()
        && b.iter()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-'));
    if ok {
        Ok(v)
    } else {
        Err(invalid(
            field,
            "invalid_code",
            "letters, digits, '_' or '-' (starting with a letter or digit)",
        ))
    }
}

/// `HH:MM` 24-hour time.
pub fn clock(field: &str, raw: Option<String>) -> Result<Option<String>, KernelError> {
    match clean(raw) {
        None => Ok(None),
        Some(v) => {
            let ok = v.len() == 5
                && v.as_bytes()[2] == b':'
                && v[..2].parse::<u32>().is_ok_and(|h| h < 24)
                && v[3..].parse::<u32>().is_ok_and(|m| m < 60)
                && v[..2].bytes().all(|c| c.is_ascii_digit())
                && v[3..].bytes().all(|c| c.is_ascii_digit());
            if ok {
                Ok(Some(v))
            } else {
                Err(invalid(field, "invalid_time", "HH:MM (24-hour)"))
            }
        }
    }
}

/// A non-negative money amount (string on the wire, ≤ 2 decimals).
pub fn money(field: &str, raw: &str) -> Result<Decimal, KernelError> {
    let v = crate::shared::decimal::parse_decimal(field, raw)?;
    if v < Decimal::ZERO || v > Decimal::new(99_999_999_999_999, 2) {
        return Err(invalid(field, "out_of_range", "0 or more (numeric(14,2))"));
    }
    Ok(v)
}

/// Approve or reject (`POST …/decision`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(as = CampusResidenceDecision)]
pub enum Decision {
    Approve,
    Reject,
}

/// Decision body of every 8B governed decision (a rejection needs a reason).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[schema(as = CampusResidenceDecisionBody)]
pub struct DecisionBody {
    pub decision: Decision,
    pub reason: Option<String>,
}

impl DecisionBody {
    pub fn approve(&self) -> bool {
        self.decision == Decision::Approve
    }

    /// The trimmed reason; required for a rejection.
    pub fn checked_reason(&self) -> Result<Option<String>, KernelError> {
        let r = optional_text("reason", self.reason.clone(), 1000)?;
        if !self.approve() && r.is_none() {
            return Err(invalid("reason", "required", "a rejection needs a reason"));
        }
        Ok(r)
    }
}

/// Optional reason of a state command.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[schema(as = CampusResidenceReasonBody)]
pub struct ReasonBody {
    pub reason: Option<String>,
}

impl ReasonBody {
    pub fn text(&self) -> Result<Option<String>, KernelError> {
        optional_text("reason", self.reason.clone(), 1000)
    }
}

/// One failed item of a bulk command.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = CampusResidenceBulkFailure)]
pub struct BulkFailure {
    pub id: Uuid,
    pub error: String,
}

/// Result of a bulk command.
#[derive(Debug, Clone, Default, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = CampusResidenceBulkSummary)]
pub struct BulkSummary {
    pub processed: i64,
    pub skipped: i64,
    pub failed: Vec<BulkFailure>,
}

/// Status filter of list endpoints.
#[derive(Debug, Clone, Default, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct StatusFilter {
    /// Status wire name.
    pub status: Option<String>,
}

/// Validates an optional status filter against a machine.
pub fn status_filter<M: Machine>(raw: Option<&str>) -> Result<Option<&'static str>, KernelError> {
    raw.map(|r| {
        M::parse_str(r)
            .map(|m| m.name())
            .ok_or_else(|| invalid("status", "invalid", "unknown status"))
    })
    .transpose()
}

/// A complaint (hostel / transport → service desk ticket).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = CampusResidenceComplaintCreate)]
pub struct ComplaintCreate {
    pub title: String,
    pub description: Option<String>,
    /// `low | medium | high | critical` (default `medium`).
    pub priority: Option<String>,
}

impl ComplaintCreate {
    pub fn priority(&self) -> Result<TicketPriority, KernelError> {
        match self.priority.as_deref() {
            None => Ok(TicketPriority::Medium),
            Some(p) => TicketPriority::parse(p)
                .ok_or_else(|| invalid("priority", "invalid", "low, medium, high or critical")),
        }
    }
}

/// A service desk ticket opened from hostel / transport (followed in 8A's service desk).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = CampusResidenceComplaintView)]
pub struct ComplaintView {
    pub id: Uuid,
    #[schema(example = "SR-000001")]
    pub number: String,
    pub category: String,
    pub source: String,
    pub source_ref_id: Option<Uuid>,
    pub title: String,
    pub description: Option<String>,
    pub priority: String,
    pub status: String,
    pub sla_due_at: chrono::DateTime<chrono::Utc>,
    pub version: i64,
}

impl From<&ServiceTicket> for ComplaintView {
    fn from(t: &ServiceTicket) -> Self {
        Self {
            id: t.id.as_uuid(),
            number: t.number.clone(),
            category: t.category.as_str().to_owned(),
            source: t.source.as_str().to_owned(),
            source_ref_id: t.source_ref_id,
            title: t.title.clone(),
            description: t.description.clone(),
            priority: t.priority.as_str().to_owned(),
            status: t.status.as_str().to_owned(),
            sla_due_at: t.sla_due_at,
            version: t.version,
        }
    }
}

/// Complaints of one slice aggregate (read-only on the foundation ticket table).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[schema(as = CampusResidenceComplaintList)]
pub struct ComplaintList {
    pub items: Vec<ComplaintView>,
}

/// Tickets opened from `source` for `source_ref_id`, newest first.
pub async fn complaints_of(
    conn: &mut PgConnection,
    tenant: TenantId,
    source: &str,
    source_ref_id: Uuid,
) -> Result<ComplaintList, KernelError> {
    let sql = format!(
        "SELECT {TICKET_COLUMNS} FROM campus.service_ticket WHERE tenant_id = $1 AND source = $2 \
         AND source_ref_id = $3 ORDER BY created_at DESC LIMIT 200"
    );
    let rows = sqlx::query(&sql)
        .bind(tenant.as_uuid())
        .bind(source)
        .bind(source_ref_id)
        .fetch_all(&mut *conn)
        .await
        .map_err(map_db_error)?;
    let mut items = Vec::with_capacity(rows.len());
    for r in &rows {
        items.push(ComplaintView::from(
            &ticket_from_row(r).map_err(map_db_error)?,
        ));
    }
    Ok(ComplaintList { items })
}

/// Resolves a person who must belong to `institution`: a student with a live context there, or a
/// live staff member of the institution (422 on `field` otherwise).
pub async fn person_in(
    st: &CampusState,
    scope: &ExecutionScope,
    kind: PersonKind,
    id: Uuid,
    institution: InstitutionId,
    field: &str,
) -> Result<PersonRef, KernelError> {
    let p = require_person(st, scope, kind, id, field).await?;
    ensure_member(st, scope, &p, institution, field).await?;
    Ok(p)
}

/// 422 on `field` unless `p` belongs to `institution`.
pub async fn ensure_member(
    st: &CampusState,
    scope: &ExecutionScope,
    p: &PersonRef,
    institution: InstitutionId,
    field: &str,
) -> Result<(), KernelError> {
    let ok = match p.kind {
        PersonKind::Student => st
            .students
            .live_context(
                scope,
                sutra_contracts::student::ids::StudentId(p.id),
                institution,
            )
            .await?
            .is_some(),
        PersonKind::Staff => st
            .workforce
            .staff(scope, sutra_contracts::people::ids::StaffMemberId(p.id))
            .await?
            .is_some_and(|s| s.institution_id == institution),
    };
    if ok {
        Ok(())
    } else {
        Err(invalid(
            field,
            "not_in_institution",
            "the person does not belong to this institution",
        ))
    }
}

/// Parses `personKind`.
pub fn person_kind(raw: &str) -> Result<PersonKind, KernelError> {
    PersonKind::parse(raw).ok_or_else(|| invalid("personKind", "invalid", "student or staff"))
}

/// What a campus charge is for.
pub struct ChargeAsk<'a> {
    pub institution: InstitutionId,
    pub person: (&'a str, Uuid),
    pub fee_code: &'static str,
    pub category: CampusChargeCategory,
    pub description: String,
    pub amount: Decimal,
    pub due_date: Option<chrono::NaiveDate>,
    pub source_id: Uuid,
}

/// Raises a campus charge through `CampusChargePort` when the person is a **student** and the
/// amount is positive (staff charges are recorded on the campus row only; contract §7).
/// Idempotent on `source_id` (the port returns the charge already raised).
pub async fn raise_charge(
    st: &CampusState,
    scope: &ExecutionScope,
    ask: ChargeAsk<'_>,
    actor: &Actor,
) -> Result<Option<CampusCharge>, KernelError> {
    if ask.person.0 != PersonKind::Student.as_str() || ask.amount <= Decimal::ZERO {
        return Ok(None);
    }
    let charge = st
        .charges
        .raise_campus_charge(
            scope,
            CampusChargeRequest {
                institution_id: ask.institution,
                student_id: ask.person.1,
                fee_code: ask.fee_code.to_owned(),
                category: ask.category,
                description: ask.description,
                amount: ask.amount,
                due_date: ask.due_date,
                source_id: ask.source_id,
                actor: actor.id.clone(),
            },
        )
        .await?;
    Ok(Some(charge))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_and_code_rules() {
        assert_eq!(code("code", " r-12 ", 32).ok().as_deref(), Some("R-12"));
        assert!(code("code", "-X", 32).is_err());
        assert!(code("code", "", 32).is_err());
        assert_eq!(
            clock("t", Some("21:30".into())).ok().flatten().as_deref(),
            Some("21:30")
        );
        assert!(clock("t", Some("24:00".into())).is_err());
        assert!(clock("t", Some("9:30".into())).is_err());
        assert!(money("fee", "-1").is_err());
        assert!(money("fee", "4500.50").is_ok());
        assert!(required_text("name", "  ", 10).is_err());
        assert_eq!(clean(Some("  ".into())), None);
    }

    #[test]
    fn decisions_need_a_reason_to_reject() {
        let b = DecisionBody {
            decision: Decision::Reject,
            reason: None,
        };
        assert!(b.checked_reason().is_err());
        let b = DecisionBody {
            decision: Decision::Approve,
            reason: None,
        };
        assert!(b.checked_reason().is_ok());
    }
}
