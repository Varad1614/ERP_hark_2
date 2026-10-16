//! Governed finance configuration and financial rule versions (L3-F-084 / F-085, L2-F00
//! amendments 2026-09-01). Both share one lifecycle: draft → in review (WorkflowPort
//! `FINANCE_CONFIG_CHANGE` / `FINANCE_RULE_CHANGE`) → approved → effective (the previous effective
//! version becomes superseded — or rolled back when the new one is a rollback). A rollback
//! creates a **new** version based on an eligible prior one; historical versions are never
//! reactivated or rewritten.

use crate::charges::kit::{invalid, require_version, status_event, StatusEvent};
use crate::shared::approval::{business_key, process, request_approval, ApprovalOutcome};
use crate::shared::lifecycle::{record_history, HistoryEntry, StatusChange};
use crate::shared::publish::audit;
use crate::shared::{Actor, FinanceState};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::Serialize;
use serde_json::{json, Map, Value};
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
use std::collections::BTreeMap;
use std::str::FromStr;
use sutra_contracts::finance::events::charges::{
    FINANCE_CONFIG_EFFECTIVE, FINANCE_CONFIG_EFFECTIVE_SCHEMA, FINANCE_RULE_EFFECTIVE,
    FINANCE_RULE_EFFECTIVE_SCHEMA,
};
use sutra_contracts::finance::status::FinancePolicyStatus;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use utoipa::ToSchema;
use uuid::Uuid;

/// Which governed policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Config,
    Rule,
}

impl Kind {
    fn table(self) -> &'static str {
        match self {
            Self::Config => "finance.finance_config_version",
            Self::Rule => "finance.finance_rule",
        }
    }
    fn payload(self) -> &'static str {
        match self {
            Self::Config => "settings",
            Self::Rule => "parameters",
        }
    }
    fn aggregate(self) -> &'static str {
        match self {
            Self::Config => "finance_config",
            Self::Rule => "finance_rule",
        }
    }
    fn process(self) -> &'static str {
        match self {
            Self::Config => process::FINANCE_CONFIG_CHANGE,
            Self::Rule => process::FINANCE_RULE_CHANGE,
        }
    }
    fn event(self) -> (&'static str, &'static str) {
        match self {
            Self::Config => (FINANCE_CONFIG_EFFECTIVE, FINANCE_CONFIG_EFFECTIVE_SCHEMA),
            Self::Rule => (FINANCE_RULE_EFFECTIVE, FINANCE_RULE_EFFECTIVE_SCHEMA),
        }
    }
    fn entity(self) -> &'static str {
        match self {
            Self::Config => "finance configuration version",
            Self::Rule => "financial rule",
        }
    }
}

/// Rule keys (F-085).
pub const RULE_KEYS: &[&str] = &[
    "late_fee",
    "rounding",
    "instalment_policy",
    "refund_window",
    "write_off_threshold",
    "reminder_schedule",
];

/// A governed configuration / rule version.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PolicyVersionView {
    pub id: Uuid,
    pub institution_id: Uuid,
    /// Rules only (`late_fee`, `rounding`, `instalment_policy`, `refund_window`, `write_off_threshold`,
    /// `reminder_schedule`).
    pub rule_key: Option<String>,
    pub version_no: i32,
    /// Settings (configuration) or parameters (rule), camelCase JSON.
    pub values: Value,
    pub change_note: Option<String>,
    pub status: FinancePolicyStatus,
    pub status_reason: Option<String>,
    pub based_on_id: Option<Uuid>,
    pub rollback_reason: Option<String>,
    pub process_instance_id: Option<String>,
    pub approved_at: Option<DateTime<Utc>>,
    pub approved_by: Option<String>,
    pub effective_from: Option<DateTime<Utc>>,
    pub effective_until: Option<DateTime<Utc>>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
}

fn from_row(kind: Kind, r: &PgRow) -> Result<PolicyVersionView, sqlx::Error> {
    let st: String = r.try_get("status")?;
    Ok(PolicyVersionView {
        id: r.try_get("id")?,
        institution_id: r.try_get("institution_id")?,
        rule_key: match kind {
            Kind::Rule => Some(r.try_get("rule_key")?),
            Kind::Config => None,
        },
        version_no: r.try_get("version_no")?,
        values: r.try_get(kind.payload())?,
        change_note: r.try_get("change_note")?,
        status: FinancePolicyStatus::parse(&st)
            .ok_or_else(|| sqlx::Error::Decode(st.clone().into()))?,
        status_reason: r.try_get("status_reason")?,
        based_on_id: r.try_get("based_on_id")?,
        rollback_reason: r.try_get("rollback_reason")?,
        process_instance_id: r.try_get("process_instance_id")?,
        approved_at: r.try_get("approved_at")?,
        approved_by: r.try_get("approved_by")?,
        effective_from: r.try_get("effective_from")?,
        effective_until: r.try_get("effective_until")?,
        version: r.try_get("version")?,
        created_at: r.try_get("created_at")?,
    })
}

fn int_in(v: &Value, field: &str, lo: i64, hi: i64) -> Result<(), KernelError> {
    match v.as_i64() {
        Some(n) if (lo..=hi).contains(&n) => Ok(()),
        _ => Err(invalid(
            field,
            "invalid_value",
            &format!("must be an integer between {lo} and {hi}"),
        )),
    }
}

fn decimal_in(v: &Value, field: &str, lo: Decimal, hi: Decimal) -> Result<(), KernelError> {
    let d = v
        .as_str()
        .and_then(|s| Decimal::from_str(s).ok())
        .filter(|d| *d >= lo && *d <= hi && d.scale() <= 2);
    d.map(|_| ())
        .ok_or_else(|| invalid(field, "invalid_value", "must be a decimal string in range"))
}

/// Validates a configuration / rule payload (known camelCase keys and types; unknown keys are
/// refused so a typo never silently changes nothing).
pub fn validate_values(
    kind: Kind,
    rule_key: Option<&str>,
    values: &Value,
) -> Result<(), KernelError> {
    let obj: &Map<String, Value> = values
        .as_object()
        .ok_or_else(|| invalid("values", "invalid", "must be a JSON object"))?;
    if obj.is_empty() {
        return Err(invalid("values", "required", "at least one value"));
    }
    // ERP_EXTENSIONS 1D: the late fee and the reminder schedule are validated as a whole.
    if kind == Kind::Rule && rule_key == Some("late_fee") {
        return crate::receivables::latefee::LateFeePolicy::from_values(values)
            .map(|_| ())
            .map_err(|m| invalid("values", "invalid_value", &m));
    }
    if kind == Kind::Rule && rule_key == Some("reminder_schedule") {
        return crate::receivables::latefee::ReminderSchedule::from_values(values)
            .map(|_| ())
            .map_err(|m| invalid("values", "invalid_value", &m));
    }
    let max = Decimal::from(1_000_000_000_i64);
    for (k, v) in obj {
        let field = format!("values.{k}");
        match (kind, rule_key, k.as_str()) {
            (Kind::Config, _, "defaultDueDays") => int_in(v, &field, 0, 365)?,
            (Kind::Config, _, "maxInstalments") => int_in(v, &field, 1, 24)?,
            (Kind::Config, _, "lateFeeGraceDays") => int_in(v, &field, 0, 90)?,
            (Kind::Config, _, "allowPartialPayments" | "studentDisputesEnabled") => {
                if !v.is_boolean() {
                    return Err(invalid(&field, "invalid_value", "must be true or false"));
                }
            }
            (Kind::Config, _, "receiptFooter") => {
                if !v.as_str().is_some_and(|s| s.len() <= 200) {
                    return Err(invalid(
                        &field,
                        "invalid_value",
                        "text of at most 200 characters",
                    ));
                }
            }
            (Kind::Rule, Some("rounding"), "mode") => {
                if !matches!(v.as_str(), Some("half_away_from_zero" | "half_even")) {
                    return Err(invalid(
                        &field,
                        "invalid_value",
                        "half_away_from_zero or half_even",
                    ));
                }
            }
            (Kind::Rule, Some("instalment_policy"), "maxInstalments") => int_in(v, &field, 1, 24)?,
            (Kind::Rule, Some("instalment_policy"), "minAmount") => {
                decimal_in(v, &field, Decimal::ZERO, max)?
            }
            (Kind::Rule, Some("refund_window"), "days") => int_in(v, &field, 0, 3650)?,
            (Kind::Rule, Some("write_off_threshold"), "amount") => {
                decimal_in(v, &field, Decimal::ZERO, max)?
            }
            _ => return Err(invalid(&field, "unknown_key", "is not a known setting")),
        }
    }
    Ok(())
}

async fn fetch(
    conn: &mut PgConnection,
    kind: Kind,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<PolicyVersionView, KernelError> {
    let r = sqlx::query(&format!(
        "SELECT * FROM {} WHERE tenant_id = $1 AND id = $2{}",
        kind.table(),
        if lock { " FOR UPDATE" } else { "" }
    ))
    .bind(tenant.as_uuid())
    .bind(id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: kind.entity(),
    })?;
    from_row(kind, &r).map_err(map_db_error)
}

pub async fn get(
    state: &FinanceState,
    kind: Kind,
    tenant: TenantId,
    id: Uuid,
) -> Result<PolicyVersionView, KernelError> {
    let mut conn = state.pool.acquire().await.map_err(map_db_error)?;
    fetch(&mut conn, kind, tenant, id, false).await
}

pub async fn list(
    state: &FinanceState,
    kind: Kind,
    tenant: TenantId,
    institution: InstitutionId,
    rule_key: Option<&str>,
) -> Result<Vec<PolicyVersionView>, KernelError> {
    let filter = match kind {
        Kind::Rule => " AND ($3::text IS NULL OR rule_key = $3) ORDER BY rule_key, version_no DESC",
        Kind::Config => " AND $3::text IS NULL ORDER BY version_no DESC",
    };
    sqlx::query(&format!(
        "SELECT * FROM {} WHERE tenant_id = $1 AND institution_id = $2{filter}",
        kind.table()
    ))
    .bind(tenant.as_uuid())
    .bind(institution.as_uuid())
    .bind(rule_key)
    .fetch_all(&state.pool)
    .await
    .map_err(map_db_error)?
    .iter()
    .map(|r| from_row(kind, r))
    .collect::<Result<_, _>>()
    .map_err(map_db_error)
}

#[allow(clippy::too_many_arguments)]
async fn insert(
    conn: &mut PgConnection,
    kind: Kind,
    scope: &ExecutionScope,
    institution: InstitutionId,
    rule_key: Option<&str>,
    values: &Value,
    note: Option<&str>,
    based_on: Option<(Uuid, &str)>,
    actor: &Actor,
) -> Result<PolicyVersionView, KernelError> {
    let tenant = scope.tenant_id;
    let version_no: i32 = sqlx::query_scalar(&format!(
        "SELECT COALESCE(max(version_no), 0) + 1 FROM {} WHERE tenant_id = $1 AND institution_id = $2{}",
        kind.table(),
        if kind == Kind::Rule { " AND rule_key = $3" } else { " AND $3::text IS NULL" }
    ))
    .bind(tenant.as_uuid())
    .bind(institution.as_uuid())
    .bind(rule_key)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let (cols, vals) = match kind {
        Kind::Rule => (", rule_key", ", $9"),
        Kind::Config => ("", ""),
    };
    let sql = format!(
        "INSERT INTO {} (tenant_id, institution_id, version_no, {}, change_note, based_on_id, \
         rollback_reason, created_by{cols}) VALUES ($1, $2, $3, $4, $5, $6, $7, $8{vals}) RETURNING id",
        kind.table(),
        kind.payload()
    );
    let mut q = sqlx::query_scalar::<_, Uuid>(&sql)
        .bind(tenant.as_uuid())
        .bind(institution.as_uuid())
        .bind(version_no)
        .bind(values)
        .bind(note)
        .bind(based_on.map(|b| b.0))
        .bind(based_on.map(|b| b.1))
        .bind(actor.id.as_deref());
    if kind == Kind::Rule {
        q = q.bind(rule_key);
    }
    let id = q.fetch_one(&mut *conn).await.map_err(map_db_error)?;
    audit(
        conn,
        scope,
        &format!("finance.{}.create", kind.aggregate()),
        kind.aggregate(),
        id.to_string(),
        json!({"versionNo": version_no, "ruleKey": rule_key, "basedOn": based_on.map(|b| b.0), "rollbackReason": based_on.map(|b| b.1)}),
        actor,
    )
    .await?;
    fetch(conn, kind, tenant, id, false).await
}

/// Creates a draft version.
#[allow(clippy::too_many_arguments)]
pub async fn create(
    state: &FinanceState,
    kind: Kind,
    scope: &ExecutionScope,
    institution: InstitutionId,
    rule_key: Option<&str>,
    values: Value,
    note: Option<String>,
    actor: &Actor,
) -> Result<PolicyVersionView, KernelError> {
    if kind == Kind::Rule && !rule_key.is_some_and(|k| RULE_KEYS.contains(&k)) {
        return Err(invalid("ruleKey", "invalid", "unknown rule key"));
    }
    validate_values(kind, rule_key, &values)?;
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let v = insert(
        &mut tx,
        kind,
        scope,
        institution,
        rule_key,
        &values,
        note.as_deref(),
        None,
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(v)
}

#[allow(clippy::too_many_arguments)]
async fn set_status(
    conn: &mut PgConnection,
    kind: Kind,
    tenant: TenantId,
    v: &PolicyVersionView,
    to: FinancePolicyStatus,
    reason: Option<&str>,
    pid: Option<&str>,
    extra_sql: &str,
    actor: &Actor,
) -> Result<(), KernelError> {
    let n = sqlx::query(&format!(
        "UPDATE {} SET status = $3, status_reason = COALESCE($4, status_reason), \
         process_instance_id = COALESCE($5, process_instance_id), version = version + 1, \
         updated_at = now(){extra_sql} WHERE tenant_id = $1 AND id = $2 AND version = $6",
        kind.table()
    ))
    .bind(tenant.as_uuid())
    .bind(v.id)
    .bind(to.as_str())
    .bind(reason)
    .bind(pid)
    .bind(v.version)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?
    .rows_affected();
    if n != 1 {
        return Err(KernelError::Conflict(format!(
            "{} changed concurrently",
            kind.entity()
        )));
    }
    if v.status != to {
        record_history(
            conn,
            HistoryEntry {
                tenant,
                institution: InstitutionId(v.institution_id),
                aggregate_type: kind.aggregate(),
                aggregate_id: v.id,
                from: Some(v.status.as_str()),
                to: to.as_str(),
            },
            &StatusChange {
                cause_event_type: kind.event().0,
                reason,
                process_instance_id: pid,
            },
            actor,
        )
        .await?;
    }
    Ok(())
}

/// Submits a draft for approval; auto-approval records the approval at once (activation stays a
/// separate, explicit step).
pub async fn submit(
    state: &FinanceState,
    kind: Kind,
    tenant: TenantId,
    id: Uuid,
    expected: i64,
    actor: &Actor,
) -> Result<PolicyVersionView, KernelError> {
    let cur = get(state, kind, tenant, id).await?;
    require_version(cur.version, expected)?;
    if cur.status != FinancePolicyStatus::Draft {
        return Err(KernelError::Conflict(format!(
            "version {} is {}; only drafts are submitted",
            cur.version_no, cur.status
        )));
    }
    let scope = ExecutionScope::institution(tenant, InstitutionId(cur.institution_id));
    let outcome = request_approval(
        state,
        &scope,
        kind.process(),
        business_key(kind.aggregate(), id, cur.version),
        BTreeMap::from([("versionNo".to_owned(), json!(cur.version_no))]),
    )
    .await?;
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let v = fetch(&mut tx, kind, tenant, id, true).await?;
    require_version(v.version, expected)?;
    let extra = if let ApprovalOutcome::Approved { .. } = outcome {
        ", approved_at = now(), approved_by = 'system:auto-approval'"
    } else {
        ""
    };
    set_status(
        &mut tx,
        kind,
        tenant,
        &v,
        FinancePolicyStatus::InReview,
        None,
        Some(outcome.process_instance_id()),
        extra,
        actor,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        &format!("finance.{}.submit", kind.aggregate()),
        kind.aggregate(),
        id.to_string(),
        json!({"processInstanceId": outcome.process_instance_id(), "approved": !extra.is_empty()}),
        actor,
    )
    .await?;
    let out = fetch(&mut tx, kind, tenant, id, false).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(out)
}

/// Approval decision on a version in review (`finance.approve`): approve records the approval;
/// reject returns it to draft (the effective version is untouched either way).
#[allow(clippy::too_many_arguments)]
pub async fn decide(
    state: &FinanceState,
    kind: Kind,
    tenant: TenantId,
    id: Uuid,
    expected: i64,
    approve: bool,
    reason: Option<String>,
    actor: &Actor,
) -> Result<PolicyVersionView, KernelError> {
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let v = fetch(&mut tx, kind, tenant, id, true).await?;
    require_version(v.version, expected)?;
    if v.status != FinancePolicyStatus::InReview || v.approved_at.is_some() {
        return Err(KernelError::Conflict(format!(
            "version {} has no pending decision",
            v.version_no
        )));
    }
    if approve {
        set_status(
            &mut tx,
            kind,
            tenant,
            &v,
            FinancePolicyStatus::InReview,
            reason.as_deref(),
            None,
            ", approved_at = now()",
            actor,
        )
        .await?;
        sqlx::query(&format!(
            "UPDATE {} SET approved_by = $3 WHERE tenant_id = $1 AND id = $2",
            kind.table()
        ))
        .bind(tenant.as_uuid())
        .bind(id)
        .bind(actor.id.as_deref())
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    } else {
        let r = crate::charges::kit::require_text("reason", reason.as_deref())?;
        set_status(
            &mut tx,
            kind,
            tenant,
            &v,
            FinancePolicyStatus::Draft,
            Some(&r),
            None,
            "",
            actor,
        )
        .await?;
    }
    let scope = ExecutionScope::institution(tenant, InstitutionId(v.institution_id));
    audit(
        &mut tx,
        &scope,
        &format!("finance.{}.decide", kind.aggregate()),
        kind.aggregate(),
        id.to_string(),
        json!({"approve": approve, "reason": reason}),
        actor,
    )
    .await?;
    let out = fetch(&mut tx, kind, tenant, id, false).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(out)
}

/// Activates an approved version: it becomes the only effective one; the previous effective
/// version becomes `superseded` (or `rolled_back` when this version is a rollback).
pub async fn activate(
    state: &FinanceState,
    kind: Kind,
    tenant: TenantId,
    id: Uuid,
    expected: i64,
    actor: &Actor,
) -> Result<PolicyVersionView, KernelError> {
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let v = fetch(&mut tx, kind, tenant, id, true).await?;
    require_version(v.version, expected)?;
    if v.status != FinancePolicyStatus::InReview || v.approved_at.is_none() {
        return Err(KernelError::Conflict(format!(
            "version {} is {}{}; only an approved version is activated",
            v.version_no,
            v.status,
            if v.status == FinancePolicyStatus::InReview {
                " (awaiting approval)"
            } else {
                ""
            }
        )));
    }
    let institution = InstitutionId(v.institution_id);
    let scope = ExecutionScope::institution(tenant, institution);
    let previous: Option<Uuid> = sqlx::query_scalar(&format!(
        "SELECT id FROM {} WHERE tenant_id = $1 AND institution_id = $2 AND status = 'effective'{} FOR UPDATE",
        kind.table(),
        if kind == Kind::Rule { " AND rule_key = $3" } else { " AND $3::text IS NULL" }
    ))
    .bind(tenant.as_uuid())
    .bind(institution.as_uuid())
    .bind(v.rule_key.as_deref())
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_db_error)?;
    if let Some(prev) = previous {
        let p = fetch(&mut tx, kind, tenant, prev, true).await?;
        let to = if v.rollback_reason.is_some() {
            FinancePolicyStatus::RolledBack
        } else {
            FinancePolicyStatus::Superseded
        };
        set_status(
            &mut tx,
            kind,
            tenant,
            &p,
            to,
            Some(&format!("replaced by version {}", v.version_no)),
            None,
            ", effective_until = now()",
            actor,
        )
        .await?;
    }
    set_status(
        &mut tx,
        kind,
        tenant,
        &v,
        FinancePolicyStatus::Effective,
        None,
        None,
        ", effective_from = now()",
        actor,
    )
    .await?;
    let after = fetch(&mut tx, kind, tenant, id, false).await?;
    status_event(
        &mut tx,
        &scope,
        kind.event(),
        StatusEvent {
            aggregate_id: id,
            from: Some(v.status.as_str().into()),
            to: after.status.as_str().into(),
            version: after.version,
            reason: Some(match &after.rule_key {
                Some(k) => format!("{k} v{}", after.version_no),
                None => format!("configuration v{}", after.version_no),
            }),
            process_instance_id: after.process_instance_id.clone(),
            ..StatusEvent::default()
        },
        actor,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        &format!("finance.{}.activate", kind.aggregate()),
        kind.aggregate(),
        id.to_string(),
        json!({"replaces": previous, "rollback": after.rollback_reason.is_some()}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(after)
}

/// Rollback authorisation (`finance.approve`): creates a new draft version from the eligible
/// prior version `basis` (a superseded / rolled-back version that was once approved). The new
/// version goes through the normal forward lifecycle.
pub async fn rollback(
    state: &FinanceState,
    kind: Kind,
    tenant: TenantId,
    basis: Uuid,
    reason: &str,
    actor: &Actor,
) -> Result<PolicyVersionView, KernelError> {
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let b = fetch(&mut tx, kind, tenant, basis, true).await?;
    if !matches!(
        b.status,
        FinancePolicyStatus::Superseded | FinancePolicyStatus::RolledBack
    ) || b.approved_at.is_none()
    {
        return Err(KernelError::Conflict(format!(
            "version {} is {}; only a previously effective version can be the basis of a rollback",
            b.version_no, b.status
        )));
    }
    let scope = ExecutionScope::institution(tenant, InstitutionId(b.institution_id));
    let v = insert(
        &mut tx,
        kind,
        &scope,
        InstitutionId(b.institution_id),
        b.rule_key.as_deref(),
        &b.values,
        Some(&format!("Rollback to version {}", b.version_no)),
        Some((basis, reason)),
        actor,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        &format!("finance.{}.rollback_authorize", kind.aggregate()),
        kind.aggregate(),
        v.id.to_string(),
        json!({"basisVersionId": basis, "basisVersionNo": b.version_no, "reason": reason}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payloads_validate() {
        assert!(validate_values(
            Kind::Config,
            None,
            &json!({"defaultDueDays": 30, "allowPartialPayments": true})
        )
        .is_ok());
        assert!(validate_values(Kind::Config, None, &json!({"defaultDueDays": 400})).is_err());
        assert!(validate_values(Kind::Config, None, &json!({"typo": 1})).is_err());
        assert!(validate_values(Kind::Config, None, &json!({})).is_err());
        assert!(validate_values(
            Kind::Rule,
            Some("late_fee"),
            &json!({"percent": "2.50", "graceDays": 7})
        )
        .is_ok());
        assert!(validate_values(Kind::Rule, Some("late_fee"), &json!({"percent": "250"})).is_err());
        assert!(
            validate_values(Kind::Rule, Some("late_fee"), &json!({"percent": 2.5})).is_err(),
            "no floats"
        );
        assert!(
            validate_values(Kind::Rule, Some("rounding"), &json!({"mode": "half_even"})).is_ok()
        );
        assert!(validate_values(Kind::Rule, Some("rounding"), &json!({"percent": "1"})).is_err());
    }
}
