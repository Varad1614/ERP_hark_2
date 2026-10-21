//! Part 1 (DEF-X-002): who requested a governed decision, read from a module's append-only
//! status history (`<schema>.status_history` — `actor` of the row that moved the aggregate into
//! its pending state, plus the actor of its creation row). Modules pass the result to
//! [`sutra_kernel::approval::ensure_independent_decider`] on their decision path.

use crate::map_db_error;
use sqlx::PgConnection;
use sutra_kernel::error::KernelError;
use uuid::Uuid;

/// A module status history table with the common shape
/// `(tenant_id, aggregate_type, aggregate_id, from_status, to_status, actor)`.
#[derive(Clone, Copy, Debug)]
pub enum HistoryTable {
    Finance,
    Exams,
    People,
    Resources,
    Campus,
    Services,
    Academic,
}

impl HistoryTable {
    fn name(self) -> &'static str {
        match self {
            Self::Finance => "finance.status_history",
            Self::Exams => "exams.status_history",
            Self::People => "people.status_history",
            Self::Resources => "resources.status_history",
            Self::Campus => "campus.status_history",
            Self::Services => "services.status_history",
            Self::Academic => "academic.status_history",
        }
    }
}

/// Distinct non-empty actors that created the aggregate (`from_status IS NULL`) or moved it into
/// one of `pending` statuses — the request's makers.
pub async fn requesters(
    conn: &mut PgConnection,
    table: HistoryTable,
    tenant_id: Uuid,
    aggregate_type: &str,
    aggregate_id: Uuid,
    pending: &[&str],
) -> Result<Vec<String>, KernelError> {
    let pending: Vec<String> = pending.iter().map(|s| (*s).to_owned()).collect();
    let sql = format!(
        "SELECT DISTINCT actor FROM {} WHERE tenant_id = $1 AND aggregate_type = $2 \
         AND aggregate_id = $3 AND actor IS NOT NULL AND actor <> '' \
         AND (from_status IS NULL OR to_status = ANY($4))",
        table.name()
    );
    sqlx::query_scalar::<_, String>(&sql)
        .bind(tenant_id)
        .bind(aggregate_type)
        .bind(aggregate_id)
        .bind(&pending)
        .fetch_all(&mut *conn)
        .await
        .map_err(map_db_error)
}

// ------------------------------------------------------------------ approval request ledger
//
// Part 1 · S3-appr: every module's `request_approval` records who asked for a governed decision
// in `platform.approval_request` (append-only). Every decision endpoint calls one of the guards
// below with the request it decides: the decider must be an identified human and must not be one
// of the users who requested the pending request (all requests of the latest subject version, so
// a concurrent losing submission still counts as a maker).

/// The subject id and version of a deterministic business key `<aggregate>:<subject>:v<version>`
/// (the subject may itself contain `:`, e.g. `section_enrolment_override:<section>:<student>:v3`).
pub fn parse_business_key(business_key: &str) -> (String, Option<i64>) {
    let rest = business_key
        .split_once(':')
        .map_or(business_key, |(_, rest)| rest);
    match rest.rsplit_once(":v") {
        Some((subject, v)) => match v.parse::<i64>() {
            Ok(v) => (subject.to_owned(), Some(v)),
            Err(_) => (rest.to_owned(), None),
        },
        None => (rest.to_owned(), None),
    }
}

// Audit C2 (finding M1): the ledger row is written in the **owning domain transaction**.
//
// A module's `request_approval` runs before the owner-local transaction opens (the workflow
// process is external and cannot join it). Inside a web request (the authentication middleware
// wraps every authenticated request in the kernel's `with_request_ledger`) [`record_request`]
// therefore only *queues* the row; the next [`crate::outbox::enqueue`] — the "submitted" /
// decision event every governed state change emits in its own transaction — writes the queued
// rows in the same SQL statement as the event. So a ledger failure fails that statement and rolls
// the decision back, and a failed / rolled-back decision leaves no ledger row. Rows still queued
// when the request ends (a flow that emitted no event) are written by the middleware only when
// the response succeeded ([`write_leftover`]). Outside a request (background jobs, consumers)
// the row is written at once, as before. [`flush_pending`] writes the queue on an explicit
// connection for a transaction that emits no event.

/// One approval-ledger row waiting for the owning transaction.
#[derive(Clone, Debug)]
pub struct PendingRequest {
    pool: sqlx::PgPool,
    tenant_id: Uuid,
    process_definition_key: String,
    business_key: String,
    subject_id: String,
    subject_version: Option<i64>,
    process_instance_id: String,
    requested_by: Option<String>,
}

impl sutra_kernel::approval::QueuedLedgerRow for PendingRequest {
    fn write_alone(
        self: Box<Self>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
        Box::pin(write_leftover(vec![*self]))
    }
    fn into_any(self: Box<Self>) -> Box<dyn std::any::Any + Send> {
        self
    }
}

/// Runs `f` with a request-scoped approval ledger queue (the web authentication middleware does
/// this for every authenticated request through the kernel); returns its output and the rows no
/// owning transaction wrote — write them with [`write_leftover`] only when `f` succeeded.
pub async fn with_request_ledger<F: std::future::Future>(f: F) -> (F::Output, Vec<PendingRequest>) {
    let (out, rest) = sutra_kernel::approval::with_request_ledger(f).await;
    (out, downcast(rest))
}

fn downcast(rows: Vec<Box<dyn sutra_kernel::approval::QueuedLedgerRow>>) -> Vec<PendingRequest> {
    rows.into_iter()
        .filter_map(|r| r.into_any().downcast::<PendingRequest>().ok().map(|b| *b))
        .collect()
}

/// Drains the queued ledger rows of the current request (empty outside a request).
pub fn take_pending() -> Vec<PendingRequest> {
    downcast(sutra_kernel::approval::take_ledger_rows())
}

/// Column arrays of `rows` for [`ledger_insert_sql`].
#[allow(clippy::type_complexity)]
pub(crate) fn ledger_columns(
    rows: &[PendingRequest],
) -> (
    Vec<Uuid>,
    Vec<String>,
    Vec<String>,
    Vec<String>,
    Vec<Option<i64>>,
    Vec<String>,
    Vec<Option<String>>,
) {
    (
        rows.iter().map(|r| r.tenant_id).collect(),
        rows.iter()
            .map(|r| r.process_definition_key.clone())
            .collect(),
        rows.iter().map(|r| r.business_key.clone()).collect(),
        rows.iter().map(|r| r.subject_id.clone()).collect(),
        rows.iter().map(|r| r.subject_version).collect(),
        rows.iter().map(|r| r.process_instance_id.clone()).collect(),
        rows.iter().map(|r| r.requested_by.clone()).collect(),
    )
}

/// `INSERT … SELECT FROM UNNEST(…)` of the ledger columns bound from parameter `$first` on.
pub(crate) fn ledger_insert_sql(first: usize) -> String {
    let p: Vec<String> = (first..first + 7).map(|i| format!("${i}")).collect();
    format!(
        "INSERT INTO platform.approval_request (tenant_id, process_definition_key, business_key, \
         subject_id, subject_version, process_instance_id, requested_by) \
         SELECT * FROM UNNEST({}::uuid[], {}::text[], {}::text[], {}::text[], {}::bigint[], \
         {}::text[], {}::text[])",
        p[0], p[1], p[2], p[3], p[4], p[5], p[6]
    )
}

async fn insert_rows(
    conn: &mut sqlx::PgConnection,
    rows: &[PendingRequest],
) -> Result<(), sqlx::Error> {
    let (a, b, c, d, e, f, g) = ledger_columns(rows);
    sqlx::query(&ledger_insert_sql(1))
        .bind(a)
        .bind(b)
        .bind(c)
        .bind(d)
        .bind(e)
        .bind(f)
        .bind(g)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// Writes the request's queued rows on `conn` (an owning transaction that emits no event).
pub async fn flush_pending(conn: &mut sqlx::PgConnection) -> Result<(), KernelError> {
    let rows = take_pending();
    if rows.is_empty() {
        return Ok(());
    }
    insert_rows(conn, &rows).await.map_err(map_db_error)
}

/// Writes rows a request left queued (its transaction emitted no event). Call only when the
/// request succeeded; a failed request's rows are dropped with it.
pub async fn write_leftover(rows: Vec<PendingRequest>) {
    let Some(pool) = rows.first().map(|r| r.pool.clone()) else {
        return;
    };
    let res = async {
        let mut conn = pool.acquire().await?;
        insert_rows(&mut conn, &rows).await
    }
    .await;
    if let Err(e) = res {
        tracing::error!(error = %e, rows = rows.len(), "approval ledger rows could not be written");
    }
}

/// Records one approval request (called by `request_approval` right after the process started).
/// `requested_by` is the authenticated user of the request (`None` for background work). Inside a
/// web request the row is queued for the owning transaction (see the section comment).
pub async fn record_request(
    pool: &sqlx::PgPool,
    tenant_id: Uuid,
    process_definition_key: &str,
    business_key: &str,
    process_instance_id: &str,
    requested_by: Option<&str>,
) -> Result<(), KernelError> {
    let (subject, version) = parse_business_key(business_key);
    let row = PendingRequest {
        pool: pool.clone(),
        tenant_id,
        process_definition_key: process_definition_key.to_owned(),
        business_key: business_key.to_owned(),
        subject_id: subject,
        subject_version: version,
        process_instance_id: process_instance_id.to_owned(),
        requested_by: requested_by.map(str::to_owned),
    };
    let Err(row) = sutra_kernel::approval::queue_ledger_row(Box::new(row)) else {
        return Ok(());
    };
    let rows = downcast(vec![row]);
    let mut conn = pool.acquire().await.map_err(map_db_error)?;
    insert_rows(&mut conn, &rows).await.map_err(map_db_error)
}

/// Who requested the latest approval request recorded for `subject` (every request of the same
/// subject version). `None` when no request was ever recorded for it.
pub async fn latest_requesters(
    pool: &sqlx::PgPool,
    tenant_id: Uuid,
    subject: &str,
) -> Result<Option<Vec<String>>, KernelError> {
    let rows: Vec<(Option<String>,)> = sqlx::query_as(
        "WITH latest AS (SELECT subject_version FROM platform.approval_request \
             WHERE tenant_id = $1 AND subject_id = $2 ORDER BY requested_at DESC, id DESC LIMIT 1) \
         SELECT r.requested_by FROM platform.approval_request r, latest l \
         WHERE r.tenant_id = $1 AND r.subject_id = $2 \
           AND r.subject_version IS NOT DISTINCT FROM l.subject_version",
    )
    .bind(tenant_id)
    .bind(subject)
    .fetch_all(pool)
    .await
    .map_err(map_db_error)?;
    if rows.is_empty() {
        return Ok(None);
    }
    Ok(Some(
        rows.into_iter()
            .filter_map(|(r,)| r)
            .filter(|r| !r.trim().is_empty())
            .collect(),
    ))
}

/// Separation of duties for a decision endpoint (Part 1 · S3-appr, DEF-X-002): refuses (`403`)
/// a decision by an unidentified / system actor, or by a user who requested the pending request
/// of `subject` (the business-key subject of the governed request, usually the path id). The
/// handler still checks the decide permission and the pending state itself.
pub async fn guard_decision(
    pool: &sqlx::PgPool,
    ctx: &sutra_kernel::auth::AuthContext,
    subject: impl std::fmt::Display,
    what: &str,
) -> Result<(), KernelError> {
    guard_decision_by(
        pool,
        ctx.scope.tenant_id.as_uuid(),
        Some(&ctx.subject),
        subject,
        &[],
        what,
    )
    .await
}

/// [`guard_decision`] for an explicit decider (e.g. a repository function called with an
/// `Actor`), with `also` = further makers known to the caller (creator / requester columns).
pub async fn guard_decision_by(
    pool: &sqlx::PgPool,
    tenant_id: Uuid,
    decider: Option<&str>,
    subject: impl std::fmt::Display,
    also: &[Option<&str>],
    what: &str,
) -> Result<(), KernelError> {
    let subject = subject.to_string();
    // Audit C1: no recorded request and no maker column => no maker at all, which the kernel
    // check refuses (409) — never "anyone may decide".
    let mut makers = latest_requesters(pool, tenant_id, &subject)
        .await?
        .unwrap_or_default();
    makers.extend(also.iter().flatten().map(|s| (*s).to_owned()));
    sutra_kernel::approval::ensure_independent_decider(
        decider,
        makers.iter().map(String::as_str),
        what,
    )
}

/// Who requested the approval process `process_instance_id` (empty when not recorded).
pub async fn process_requesters(
    pool: &sqlx::PgPool,
    tenant_id: Uuid,
    process_instance_id: &str,
) -> Result<Vec<String>, KernelError> {
    let rows: Vec<Option<String>> = sqlx::query_scalar(
        "SELECT requested_by FROM platform.approval_request \
         WHERE tenant_id = $1 AND process_instance_id = $2",
    )
    .bind(tenant_id)
    .bind(process_instance_id)
    .fetch_all(pool)
    .await
    .map_err(map_db_error)?;
    Ok(rows
        .into_iter()
        .flatten()
        .filter(|r| !r.trim().is_empty())
        .collect())
}

/// [`guard_decision`] for an aggregate whose business key is not its own id: the pending
/// request is found through the `process_instance_id` stored on the aggregate's row
/// (`table` / `key_column` are static identifiers of the owning module, `key` the row key), and
/// the row's `maker_columns` (creator / requester columns, e.g. `raised_by`) are makers too.
pub async fn guard_decision_for_row(
    pool: &sqlx::PgPool,
    ctx: &sutra_kernel::auth::AuthContext,
    table: &'static str,
    key_column: &'static str,
    key: Uuid,
    maker_columns: &[&'static str],
    what: &str,
) -> Result<(), KernelError> {
    let tenant = ctx.scope.tenant_id.as_uuid();
    let makers_sql = if maker_columns.is_empty() {
        "ARRAY[]::text[]".to_owned()
    } else {
        format!(
            "array_remove(ARRAY[{}]::text[], NULL)",
            maker_columns
                .iter()
                .map(|c| format!("{c}::text"))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    let row: Option<(Option<String>, Vec<String>)> = sqlx::query_as(&format!(
        "SELECT process_instance_id, {makers_sql} FROM {table} \
         WHERE tenant_id = $1 AND {key_column} = $2 LIMIT 1"
    ))
    .bind(tenant)
    .bind(key)
    .fetch_optional(pool)
    .await
    .map_err(map_db_error)?;
    // Audit C1: a missing row / process yields no maker, which the kernel check refuses (409).
    let (pid, columns) = row.unwrap_or_default();
    let mut makers = match pid {
        Some(pid) => process_requesters(pool, tenant, &pid).await?,
        None => Vec::new(),
    };
    makers.extend(columns.into_iter().filter(|c| !c.trim().is_empty()));
    sutra_kernel::approval::ensure_independent_decider(
        Some(&ctx.subject),
        makers.iter().map(String::as_str),
        what,
    )
}

#[cfg(test)]
mod tests {
    use super::parse_business_key;

    #[test]
    fn business_keys_parse_into_subject_and_version() {
        assert_eq!(
            parse_business_key("refund:0192-ab:v3"),
            ("0192-ab".to_owned(), Some(3))
        );
        assert_eq!(
            parse_business_key("section_enrolment_override:s1:st2:v7"),
            ("s1:st2".to_owned(), Some(7))
        );
        assert_eq!(parse_business_key("odd"), ("odd".to_owned(), None));
        assert_eq!(
            parse_business_key("x:id:vnext"),
            ("id:vnext".to_owned(), None)
        );
    }
}
