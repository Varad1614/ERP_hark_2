//! **ERP_EXTENSIONS 2C** — the dated-lot projection (`resources.stock_lot`) kept in step with the
//! stock ledger. [`follow`] runs inside `shared::stock::post_movement` for every new entry of an
//! item that tracks expiry: lots are first re-sized to the on hand before the movement (stock
//! from before tracking becomes undated), then an incoming movement adds a lot (dated by the
//! receipt, or the dates a reversal gives back) and an outgoing one consumes first-expired-
//! first-out (a reversal of a receipt takes back the very lots it brought). Each entry records
//! the dated quantities it moved in `resources.stock_lot_movement`.

use super::lots::{self, Lot, NO_DATE};
use crate::shared::stock::NewMovement;
use chrono::NaiveDate;
use rust_decimal::prelude::{FromPrimitive, ToPrimitive};
use rust_decimal::Decimal;
use sqlx::{PgConnection, Row};
use sutra_contracts::resources::status::StockMovementKind;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use uuid::Uuid;

fn f(d: Decimal) -> f64 {
    d.to_f64().unwrap_or(0.0)
}

fn dec(x: f64) -> Decimal {
    Decimal::from_f64(x).unwrap_or_default().round_dp(3)
}

fn date_str(d: Option<NaiveDate>) -> String {
    d.map(|d| d.format("%Y-%m-%d").to_string()).unwrap_or_default()
}

fn parse(d: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(d, "%Y-%m-%d").ok()
}

/// The lots of an item at a location (FEFO order).
pub async fn load(
    conn: &mut PgConnection,
    tenant: TenantId,
    item: Uuid,
    location: Uuid,
) -> Result<Vec<Lot>, KernelError> {
    let rows = sqlx::query(
        "SELECT expiry_on, quantity FROM resources.stock_lot WHERE tenant_id = $1 AND item_id = $2 \
         AND location_id = $3",
    )
    .bind(tenant.as_uuid())
    .bind(item)
    .bind(location)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let list: Vec<Lot> = rows
        .iter()
        .map(|r| {
            Ok(Lot {
                date: date_str(r.try_get("expiry_on")?),
                qty: f(r.try_get("quantity")?),
            })
        })
        .collect::<Result<_, sqlx::Error>>()
        .map_err(map_db_error)?;
    Ok(lots::normalize_lots(&list))
}

/// Replaces the lots of an item at a location.
pub async fn store(
    conn: &mut PgConnection,
    tenant: TenantId,
    institution: InstitutionId,
    item: Uuid,
    location: Uuid,
    list: &[Lot],
) -> Result<(), KernelError> {
    sqlx::query("DELETE FROM resources.stock_lot WHERE tenant_id = $1 AND item_id = $2 AND location_id = $3")
        .bind(tenant.as_uuid())
        .bind(item)
        .bind(location)
        .execute(&mut *conn)
        .await
        .map_err(map_db_error)?;
    for l in lots::normalize_lots(list) {
        sqlx::query(
            "INSERT INTO resources.stock_lot (tenant_id, institution_id, item_id, location_id, \
             expiry_on, quantity) VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(tenant.as_uuid())
        .bind(institution.as_uuid())
        .bind(item)
        .bind(location)
        .bind(parse(&l.date))
        .bind(dec(l.qty))
        .execute(&mut *conn)
        .await
        .map_err(map_db_error)?;
    }
    Ok(())
}

async fn moved_by(conn: &mut PgConnection, tenant: TenantId, entry: Uuid) -> Result<Vec<Lot>, KernelError> {
    let rows = sqlx::query(
        "SELECT expiry_on, quantity FROM resources.stock_lot_movement WHERE tenant_id = $1 \
         AND ledger_entry_id = $2 ORDER BY created_at, id",
    )
    .bind(tenant.as_uuid())
    .bind(entry)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    rows.iter()
        .map(|r| {
            Ok(Lot {
                date: date_str(r.try_get("expiry_on")?),
                qty: f(r.try_get("quantity")?),
            })
        })
        .collect::<Result<_, sqlx::Error>>()
        .map_err(map_db_error)
}

/// Pure: what an entry does to the lots. `given` are the dated quantities a reversal gives
/// back or takes back (positive); returns the new lots and the signed dated quantities moved.
pub fn plan(current: &[Lot], before: f64, quantity: f64, expiry: Option<&str>, given: &[Lot]) -> (Vec<Lot>, Vec<Lot>) {
    let mut lots_now = lots::resize_lots(current, before);
    let mut moved = Vec::new();
    if quantity > 0.0 {
        let mut left = quantity;
        for g in given {
            let q = g.qty.min(left);
            if q <= 0.0 {
                continue;
            }
            lots_now = lots::add_lot(&lots_now, &g.date, q);
            moved.push(Lot { date: g.date.clone(), qty: q });
            left = ((left - q) * 1000.0).round() / 1000.0;
        }
        if left > 0.0 {
            let d = expiry.unwrap_or(NO_DATE);
            lots_now = lots::add_lot(&lots_now, d, left);
            moved.push(Lot { date: lots::clean_date(d), qty: left });
        }
    } else {
        let mut need = -quantity;
        for g in given {
            let q = g.qty.min(need);
            if q <= 0.0 {
                continue;
            }
            let (rest, taken) = lots::consume_lot_at(&lots_now, &g.date, q);
            lots_now = rest;
            for t in taken {
                need = ((need - t.qty) * 1000.0).round() / 1000.0;
                moved.push(Lot { date: t.date, qty: -t.qty });
            }
        }
        if need > 0.0 {
            let (rest, taken) = lots::consume_lots(&lots_now, need);
            lots_now = rest;
            moved.extend(taken.into_iter().map(|t| Lot { date: t.date, qty: -t.qty }));
        }
    }
    (lots_now, moved)
}

/// Keeps the lots of a tracked item in step with a new ledger entry (see the module docs).
pub async fn follow(
    conn: &mut PgConnection,
    m: &NewMovement<'_>,
    entry: Uuid,
    before: Decimal,
) -> Result<(), KernelError> {
    let track: bool = sqlx::query_scalar("SELECT track_expiry FROM resources.item WHERE id = $1 AND tenant_id = $2")
        .bind(m.item_id.as_uuid())
        .bind(m.tenant.as_uuid())
        .fetch_one(&mut *conn)
        .await
        .map_err(map_db_error)?;
    if !track {
        return Ok(());
    }
    let item = m.item_id.as_uuid();
    let location = m.location_id.as_uuid();
    let current = load(conn, m.tenant, item, location).await?;
    // A reversal moves the same dated quantities back.
    let given: Vec<Lot> = match (&m.kind, m.reverses_entry_id.as_ref()) {
        (StockMovementKind::Reversal, Some(rev)) => moved_by(conn, m.tenant, rev.as_uuid())
            .await?
            .into_iter()
            .map(|l| Lot { date: l.date, qty: l.qty.abs() })
            .collect(),
        _ => Vec::new(),
    };
    let expiry = m.expiry_on.map(|d| d.format("%Y-%m-%d").to_string());
    let (next, moved) = plan(&current, f(before), f(m.quantity), expiry.as_deref(), &given);
    store(conn, m.tenant, m.institution, item, location, &next).await?;
    for l in moved {
        sqlx::query(
            "INSERT INTO resources.stock_lot_movement (tenant_id, institution_id, ledger_entry_id, \
             expiry_on, quantity) VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(m.tenant.as_uuid())
        .bind(m.institution.as_uuid())
        .bind(entry)
        .bind(parse(&l.date))
        .bind(dec(l.qty))
        .execute(&mut *conn)
        .await
        .map_err(map_db_error)?;
    }
    Ok(())
}

/// Brings the lots of a tracked item in line with on hand at every location (tracking just
/// switched on, or a check): growth undated, shrinkage first-expired-first-out.
pub async fn sync_item(
    conn: &mut PgConnection,
    tenant: TenantId,
    institution: InstitutionId,
    item: Uuid,
) -> Result<(), KernelError> {
    let rows: Vec<(Uuid, Decimal)> = sqlx::query_as(
        "SELECT location_id, sum(quantity) FROM resources.stock_ledger_entry WHERE tenant_id = $1 \
         AND item_id = $2 GROUP BY location_id",
    )
    .bind(tenant.as_uuid())
    .bind(item)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    for (loc, qty) in rows {
        let current = load(conn, tenant, item, loc).await?;
        let next = lots::resize_lots(&current, f(qty));
        if next != current {
            store(conn, tenant, institution, item, loc, &next).await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn l(d: &str, q: f64) -> Lot {
        Lot { date: d.into(), qty: q }
    }

    #[test]
    fn receipts_issues_and_reversals() {
        // Untracked stock of 4 becomes undated; a dated receipt of 10.
        let (lots_now, moved) = plan(&[], 4.0, 10.0, Some("2027-02-01"), &[]);
        assert_eq!(lots_now, vec![l("2027-02-01", 10.0), l("", 4.0)]);
        assert_eq!(moved, vec![l("2027-02-01", 10.0)]);
        // An issue of 12 takes the dated lot first.
        let (after_issue, moved) = plan(&lots_now, 14.0, -12.0, None, &[]);
        assert_eq!(after_issue, vec![l("", 2.0)]);
        assert_eq!(moved, vec![l("2027-02-01", -10.0), l("", -2.0)]);
        // Reversing the issue gives the same dates back.
        let back: Vec<Lot> = moved.iter().map(|x| l(&x.date, x.qty.abs())).collect();
        let (restored, _) = plan(&after_issue, 2.0, 12.0, None, &back);
        assert_eq!(restored, vec![l("2027-02-01", 10.0), l("", 4.0)]);
        // Reversing the receipt takes back that very lot.
        let (gone, moved) = plan(&restored, 14.0, -10.0, None, &[l("2027-02-01", 10.0)]);
        assert_eq!(gone, vec![l("", 4.0)]);
        assert_eq!(moved, vec![l("2027-02-01", -10.0)]);
    }
}
