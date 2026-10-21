//! Durable Publication Intent writer (ADR-L5-0007).
//!
//! Call [`enqueue`] with the *same transaction* that mutates owner state:
//!
//! ```ignore
//! let mut tx = pool.begin().await?;
//! sqlx::query("UPDATE institution.institution SET ...").execute(&mut *tx).await?;
//! outbox::enqueue(&mut *tx, &envelope).await?;
//! tx.commit().await?;
//! ```

use sqlx::PgExecutor;
use sutra_kernel::events::EventEnvelope;

/// Audit C2 (finding M1): approval-ledger rows queued by the request's `request_approval`
/// (see [`crate::approval::record_request`]) are written in the **same statement** as the
/// event, so they commit or roll back with the owning transaction.
pub async fn enqueue<'e, E: PgExecutor<'e>>(
    executor: E,
    env: &EventEnvelope,
) -> Result<(), sqlx::Error> {
    let envelope = serde_json::to_value(env).map_err(|e| sqlx::Error::Encode(Box::new(e)))?;
    let pending = crate::approval::take_pending();
    let insert = "INSERT INTO platform.outbox (id, tenant_id, institution_id, source, event_type, dataschema, subject, envelope) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)";
    let sql = if pending.is_empty() {
        insert.to_owned()
    } else {
        format!(
            "WITH approval_ledger AS ({}) {insert}",
            crate::approval::ledger_insert_sql(9)
        )
    };
    let mut q = sqlx::query(&sql)
        .bind(env.id)
        .bind(env.tenantid)
        .bind(env.institutionid)
        .bind(&env.source)
        .bind(&env.event_type)
        .bind(&env.dataschema)
        .bind(&env.subject)
        .bind(envelope);
    if !pending.is_empty() {
        let (a, b, c, d, e, f, g) = crate::approval::ledger_columns(&pending);
        q = q.bind(a).bind(b).bind(c).bind(d).bind(e).bind(f).bind(g);
    }
    q.execute(executor).await?;
    Ok(())
}
