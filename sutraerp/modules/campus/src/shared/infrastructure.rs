//! The **only** writers of `campus.infrastructure_node` (contract `docs/batch8-contracts.md`
//! §3.3): [`insert_node`] and [`change_node_status`]. 8A calls them from its API; 8B / 8C never
//! write nodes (their tests seed nodes through `support::b8`, which calls [`insert_node`]).
//!
//! Inside the caller's owner-local transaction:
//! * [`insert_node`] checks the hierarchy rule (`InfrastructureNodeType::allowed_parents`, same
//!   institution and campus as the parent, parent not decommissioned / archived), that exactly
//!   `space` nodes carry a space type, and that a node starts `planned` or `active`; a duplicate
//!   code in the institution is `409`; appends `campus.status_history`.
//! * [`change_node_status`] checks `InfrastructureStatus::can_transition` (`409` otherwise) on a
//!   node the caller locked with `shared::rows::lock_node`, bumps `version` and appends history.
//!
//! The slice still emits its own business event and audit entry in the same transaction.

use crate::shared::lifecycle::{illegal, record_history, HistoryEntry, StatusChange};
use crate::shared::rows::{node_from_row, InfrastructureNode, NODE_COLUMNS};
use crate::shared::Actor;
use chrono::NaiveDate;
use rust_decimal::Decimal;
use sqlx::PgConnection;
use sutra_contracts::campus::status::{InfrastructureNodeType, InfrastructureStatus, SpaceType};
use sutra_kernel::error::{FieldViolation, KernelError};
use sutra_kernel::scope::{InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use uuid::Uuid;

/// A node to create.
#[derive(Clone, Debug)]
pub struct NewNode<'a> {
    pub tenant: TenantId,
    pub institution: InstitutionId,
    /// Domain B campus (validated by the caller through `InstitutionReferencePort`).
    pub campus_id: Uuid,
    /// `None` only for a `site`.
    pub parent: Option<&'a InfrastructureNode>,
    pub node_type: InfrastructureNodeType,
    pub code: &'a str,
    pub name: &'a str,
    pub space_type: Option<SpaceType>,
    pub capacity: Option<i32>,
    pub floor_number: Option<i32>,
    pub area_sq_m: Option<Decimal>,
    pub attributes: serde_json::Value,
    /// `planned` or `active`.
    pub status: InfrastructureStatus,
    pub effective_from: Option<NaiveDate>,
}

fn invalid(field: &str, code: &str, message: &str) -> KernelError {
    KernelError::Validation(vec![FieldViolation::new(field, code, message)])
}

/// Pure checks of [`insert_node`] (unit-tested).
pub fn validate_new_node(n: &NewNode<'_>) -> Result<(), KernelError> {
    let code_ok = {
        let b = n.code.as_bytes();
        (1..=32).contains(&b.len())
            && (b[0].is_ascii_uppercase() || b[0].is_ascii_digit())
            && b.iter().all(|c| {
                c.is_ascii_uppercase() || c.is_ascii_digit() || matches!(c, b'_' | b'.' | b'-')
            })
    };
    if !code_ok {
        return Err(invalid(
            "code",
            "invalid_code",
            "must match ^[A-Z0-9][A-Z0-9_.-]{0,31}$",
        ));
    }
    if n.name.trim().is_empty() || n.name.len() > 200 {
        return Err(invalid("name", "invalid", "1–200 characters"));
    }
    if (n.node_type == InfrastructureNodeType::Space) != n.space_type.is_some() {
        return Err(invalid(
            "spaceType",
            "space_type_mismatch",
            "exactly the `space` nodes carry a space type",
        ));
    }
    if n.capacity.is_some_and(|c| c < 0) {
        return Err(invalid(
            "capacity",
            "negative",
            "capacity cannot be negative",
        ));
    }
    if !matches!(
        n.status,
        InfrastructureStatus::Planned | InfrastructureStatus::Active
    ) {
        return Err(invalid(
            "status",
            "invalid_initial_status",
            "a node starts planned or active",
        ));
    }
    let allowed = n.node_type.allowed_parents();
    match n.parent {
        None if allowed.is_empty() => Ok(()),
        None => Err(invalid(
            "parentId",
            "parent_required",
            "only a site has no parent",
        )),
        Some(_) if allowed.is_empty() => Err(invalid(
            "parentId",
            "parent_not_allowed",
            "a site has no parent",
        )),
        Some(p) => {
            if !allowed.contains(&p.node_type) {
                return Err(invalid(
                    "parentId",
                    "invalid_parent_type",
                    &format!("a {} cannot sit under a {}", n.node_type, p.node_type),
                ));
            }
            if p.institution_id != n.institution || p.campus_id != n.campus_id {
                return Err(invalid(
                    "parentId",
                    "parent_elsewhere",
                    "the parent belongs to another institution or campus",
                ));
            }
            if matches!(
                p.status,
                InfrastructureStatus::Decommissioned | InfrastructureStatus::Archived
            ) {
                return Err(KernelError::Conflict(format!(
                    "parent {} is {}; nothing can be added under it",
                    p.code, p.status
                )));
            }
            Ok(())
        }
    }
}

/// Inserts one node (see the module docs).
pub async fn insert_node(
    conn: &mut PgConnection,
    n: NewNode<'_>,
    actor: &Actor,
) -> Result<InfrastructureNode, KernelError> {
    validate_new_node(&n)?;
    let sql = format!(
        "INSERT INTO campus.infrastructure_node (tenant_id, institution_id, campus_id, parent_id, \
         node_type, code, name, space_type, capacity, floor_number, area_sq_m, attributes, status, \
         effective_from) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14) \
         RETURNING {NODE_COLUMNS}"
    );
    let row = sqlx::query(&sql)
        .bind(n.tenant.as_uuid())
        .bind(n.institution.as_uuid())
        .bind(n.campus_id)
        .bind(n.parent.map(|p| p.id.as_uuid()))
        .bind(n.node_type.as_str())
        .bind(n.code)
        .bind(n.name.trim())
        .bind(n.space_type.map(|s| s.as_str()))
        .bind(n.capacity)
        .bind(n.floor_number)
        .bind(n.area_sq_m)
        .bind(&n.attributes)
        .bind(n.status.as_str())
        .bind(n.effective_from)
        .fetch_one(&mut *conn)
        .await
        .map_err(|e| match &e {
            sqlx::Error::Database(d) if d.is_unique_violation() => KernelError::Conflict(format!(
                "infrastructure code {} is already used in this institution",
                n.code
            )),
            _ => map_db_error(e),
        })?;
    let node = node_from_row(&row).map_err(map_db_error)?;
    record_history(
        conn,
        HistoryEntry {
            tenant: n.tenant,
            institution: n.institution,
            aggregate_type: "infrastructure_node",
            aggregate_id: node.id.as_uuid(),
            from: None,
            to: node.status.as_str(),
        },
        &StatusChange {
            cause_event_type: "",
            reason: None,
            process_instance_id: None,
        },
        actor,
    )
    .await?;
    Ok(node)
}

/// Moves a locked node to `to` (see the module docs).
pub async fn change_node_status(
    conn: &mut PgConnection,
    tenant: TenantId,
    node: &InfrastructureNode,
    to: InfrastructureStatus,
    change: &StatusChange<'_>,
    actor: &Actor,
) -> Result<InfrastructureNode, KernelError> {
    if !node.status.can_transition(to) {
        return Err(illegal(
            "infrastructure node",
            &node.code,
            node.status.as_str(),
            to.as_str(),
        ));
    }
    let sql = format!(
        "UPDATE campus.infrastructure_node SET status = $3, status_reason = $4, \
         status_changed_at = now(), process_instance_id = COALESCE($5, process_instance_id), \
         version = version + 1, updated_at = now() \
         WHERE id = $1 AND tenant_id = $2 AND version = $6 RETURNING {NODE_COLUMNS}"
    );
    let row = sqlx::query(&sql)
        .bind(node.id.as_uuid())
        .bind(tenant.as_uuid())
        .bind(to.as_str())
        .bind(change.reason)
        .bind(change.process_instance_id)
        .bind(node.version)
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .ok_or_else(|| KernelError::Conflict("infrastructure node changed concurrently".into()))?;
    record_history(
        conn,
        HistoryEntry {
            tenant,
            institution: node.institution_id,
            aggregate_type: "infrastructure_node",
            aggregate_id: node.id.as_uuid(),
            from: Some(node.status.as_str()),
            to: to.as_str(),
        },
        change,
        actor,
    )
    .await?;
    node_from_row(&row).map_err(map_db_error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sutra_contracts::campus::ids::InfrastructureNodeId;

    fn node(t: InfrastructureNodeType, status: InfrastructureStatus) -> InfrastructureNode {
        InfrastructureNode {
            id: InfrastructureNodeId(Uuid::from_u128(1)),
            institution_id: InstitutionId(Uuid::from_u128(9)),
            campus_id: Uuid::from_u128(5),
            parent_id: None,
            node_type: t,
            code: "MAIN".into(),
            name: "Main".into(),
            space_type: None,
            capacity: None,
            floor_number: None,
            area_sq_m: None,
            attributes: serde_json::json!({}),
            status,
            status_reason: None,
            effective_from: None,
            process_instance_id: None,
            version: 1,
        }
    }

    fn new(
        t: InfrastructureNodeType,
        parent: Option<&InfrastructureNode>,
        space: Option<SpaceType>,
    ) -> NewNode<'_> {
        NewNode {
            tenant: TenantId(Uuid::from_u128(7)),
            institution: InstitutionId(Uuid::from_u128(9)),
            campus_id: Uuid::from_u128(5),
            parent,
            node_type: t,
            code: "B1-101",
            name: "Room 101",
            space_type: space,
            capacity: Some(60),
            floor_number: None,
            area_sq_m: None,
            attributes: serde_json::json!({}),
            status: InfrastructureStatus::Active,
            effective_from: None,
        }
    }

    #[test]
    fn hierarchy_rules() {
        use InfrastructureNodeType::*;
        let site = node(Site, InfrastructureStatus::Active);
        let floor = node(Floor, InfrastructureStatus::Active);
        assert!(validate_new_node(&new(Site, None, None)).is_ok());
        assert!(validate_new_node(&new(Building, Some(&site), None)).is_ok());
        assert!(validate_new_node(&new(Space, Some(&floor), Some(SpaceType::Classroom))).is_ok());
        assert!(validate_new_node(&new(Space, Some(&floor), None)).is_err());
        assert!(validate_new_node(&new(Floor, Some(&site), None)).is_err());
        assert!(validate_new_node(&new(Building, None, None)).is_err());
        let gone = node(Floor, InfrastructureStatus::Decommissioned);
        assert!(matches!(
            validate_new_node(&new(Space, Some(&gone), Some(SpaceType::Lab))),
            Err(KernelError::Conflict(_))
        ));
        let mut bad = new(Site, None, None);
        bad.code = "lower";
        assert!(validate_new_node(&bad).is_err());
    }
}
