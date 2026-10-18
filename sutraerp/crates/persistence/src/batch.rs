//! Part 2 · S4-import: one owner-local transaction per batch, one savepoint per item.
//!
//! [`run`] is the shared skeleton of every module's `ImportTargetPort::apply`: it opens one
//! transaction, runs each item through the module's [`ItemWriter`] inside its own savepoint (a
//! failing item is rolled back to its savepoint, so later items still get validated and
//! reported), and then
//!
//! * **dry run** → rolls the transaction back (nothing persists, no outbox row is published);
//! * **commit** → commits only when every item succeeded; one failure rolls the **whole** batch
//!   back (all or nothing per batch).
//!
//! Errors of an item are returned per item. Database rule violations (integrity / data
//! exceptions, SQLSTATE classes 22 and 23) that a writer surfaced as an internal error become an
//! item error ([`KernelError::Validation`] on the constraint); anything else internal (lost
//! connection, …) aborts the batch with `Err`.

use crate::map_db_error;
use sqlx::{PgConnection, PgPool};
use sutra_kernel::error::{FieldViolation, KernelError};

/// Writes one item inside the batch transaction (the module's own domain code).
#[async_trait::async_trait]
pub trait ItemWriter<T: Sync>: Send {
    type Output: Send;
    async fn write(
        &mut self,
        conn: &mut PgConnection,
        item: &T,
    ) -> Result<Self::Output, KernelError>;
}

/// Result of [`run`].
pub struct BatchRun<O> {
    pub results: Vec<Result<O, KernelError>>,
    /// `true` when the transaction was committed.
    pub committed: bool,
}

/// An internal error caused by a database rule (constraint / invalid data) of one item.
fn rule_violation(e: &KernelError) -> Option<KernelError> {
    let KernelError::Internal(source) = e else {
        return None;
    };
    let db = source.downcast_ref::<sqlx::Error>()?;
    let sqlx::Error::Database(d) = db else {
        return None;
    };
    let code = d.code()?;
    if !(code.starts_with("22") || code.starts_with("23")) {
        return None;
    }
    let field = d.constraint().unwrap_or("").to_owned();
    Some(KernelError::Validation(vec![FieldViolation::new(
        field,
        "database_rule",
        format!("rejected by a database rule: {}", d.message()),
    )]))
}

/// Runs `items` through `writer` in one transaction (see the module docs).
pub async fn run<T: Sync, W: ItemWriter<T>>(
    pool: &PgPool,
    items: &[T],
    dry_run: bool,
    writer: &mut W,
) -> Result<BatchRun<W::Output>, KernelError> {
    let mut tx = pool.begin().await.map_err(map_db_error)?;
    let mut results = Vec::with_capacity(items.len());
    let mut failed = false;
    for item in items {
        sqlx::query("SAVEPOINT sutra_batch_item")
            .execute(&mut *tx)
            .await
            .map_err(map_db_error)?;
        match writer.write(&mut tx, item).await {
            Ok(out) => {
                sqlx::query("RELEASE SAVEPOINT sutra_batch_item")
                    .execute(&mut *tx)
                    .await
                    .map_err(map_db_error)?;
                results.push(Ok(out));
            }
            Err(e) => {
                sqlx::query("ROLLBACK TO SAVEPOINT sutra_batch_item")
                    .execute(&mut *tx)
                    .await
                    .map_err(map_db_error)?;
                sqlx::query("RELEASE SAVEPOINT sutra_batch_item")
                    .execute(&mut *tx)
                    .await
                    .map_err(map_db_error)?;
                let e = match rule_violation(&e) {
                    Some(v) => v,
                    None => match e {
                        KernelError::Internal(_) | KernelError::Unavailable(_) => {
                            // Infrastructure failure: abort the whole batch.
                            tx.rollback().await.map_err(map_db_error)?;
                            return Err(e);
                        }
                        other => other,
                    },
                };
                failed = true;
                results.push(Err(e));
            }
        }
    }
    let committed = !dry_run && !failed;
    if committed {
        tx.commit().await.map_err(map_db_error)?;
    } else {
        tx.rollback().await.map_err(map_db_error)?;
    }
    Ok(BatchRun { results, committed })
}
