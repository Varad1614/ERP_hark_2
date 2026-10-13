//! Payroll configuration versions (L3-E-074), salary structures, effective-dated salary
//! assignments and payroll periods.

use super::common::{
    cause, ensure_version, history, illegal, invalid, opt_reason, payload, Change, Cmd,
};
use super::domain::{
    config_can_transition, month_bounds, validate_config, ComponentDef, Rounding, RuleDef,
    SalaryAssignmentStatus, StructureLine,
};
use super::rows::*;
use crate::shared::approval::{business_key, process, request_approval, ApprovalOutcome};
use crate::shared::guard::{require_institution_writable, require_live_engagement};
use crate::shared::publish::{audit, event};
use crate::shared::rows::lock_engagement;
use crate::shared::{Actor, PeopleState};
use chrono::NaiveDate;
use serde_json::json;
use sqlx::PgConnection;
use std::collections::BTreeMap;
use sutra_contracts::people::events::payroll::{
    PAYROLL_CONFIG_ACTIVATED, PAYROLL_CONFIG_ACTIVATED_SCHEMA,
};
use sutra_contracts::people::ids::StaffEngagementId;
use sutra_contracts::people::status::{CalculationBasis, PayrollConfigStatus};
use sutra_kernel::error::{FieldViolation, KernelError};
use sutra_kernel::scope::{InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use uuid::Uuid;

const CONFIG: &str = "payroll_config";

/// A configuration with its components and statutory rules.
#[derive(Clone, Debug)]
pub struct ConfigDetail {
    pub config: ConfigRow,
    pub components: Vec<ComponentRow>,
    pub rules: Vec<RuleRow>,
}

pub async fn config_detail(
    st: &PeopleState,
    tenant: TenantId,
    id: Uuid,
) -> Result<ConfigDetail, KernelError> {
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let config = load_config(&mut conn, tenant, id, false).await?;
    let components = components_of(&mut conn, id).await?;
    let rules = rules_of(&mut conn, id).await?;
    Ok(ConfigDetail {
        config,
        components,
        rules,
    })
}

pub async fn list_configs(
    st: &PeopleState,
    tenant: TenantId,
    institution: InstitutionId,
    status: Option<PayrollConfigStatus>,
) -> Result<Vec<ConfigRow>, KernelError> {
    let sql = format!(
        "SELECT {CONFIG_COLUMNS} FROM people.payroll_config WHERE tenant_id = $1 \
         AND institution_id = $2 AND ($3::text IS NULL OR status = $3) ORDER BY version_no DESC"
    );
    sqlx::query(&sql)
        .bind(tenant.as_uuid())
        .bind(institution.as_uuid())
        .bind(status.map(|s| s.as_str()))
        .fetch_all(&st.pool)
        .await
        .map_err(map_db_error)?
        .iter()
        .map(config_from_row)
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_db_error)
}

/// Configuration versions with their pay components (the list screen shows the codes).
pub async fn list_configs_with_components(
    st: &PeopleState,
    tenant: TenantId,
    institution: InstitutionId,
    status: Option<PayrollConfigStatus>,
) -> Result<Vec<(ConfigRow, Vec<ComponentRow>)>, KernelError> {
    let configs = list_configs(st, tenant, institution, status).await?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let mut out = Vec::with_capacity(configs.len());
    for c in configs {
        let comps = components_of(&mut conn, c.id).await?;
        out.push((c, comps));
    }
    Ok(out)
}

/// Input of a new configuration version.
pub struct NewConfig {
    pub name: String,
    pub effective_from: NaiveDate,
    pub rounding: Rounding,
    pub encashment_divisor: i32,
    /// Copy components and statutory rules from this version.
    pub copy_from: Option<Uuid>,
}

/// Creates a configuration version in `draft` (L4-E-074-BH-001), optionally copying the
/// components and rules of an earlier version.
pub async fn create_config(
    c: &Cmd<'_>,
    institution: InstitutionId,
    n: NewConfig,
) -> Result<ConfigRow, KernelError> {
    let scope = c.scope(institution);
    require_institution_writable(c.st, &scope, institution).await?;
    if n.name.trim().is_empty() || n.name.chars().count() > 200 {
        return Err(invalid(
            "name",
            "required",
            "a name of at most 200 characters",
        ));
    }
    if !(1..=31).contains(&n.encashment_divisor) {
        return Err(invalid(
            "encashmentDivisor",
            "out_of_range",
            "between 1 and 31",
        ));
    }
    let mut tx = c.st.pool.begin().await.map_err(map_db_error)?;
    if let Some(src) = n.copy_from {
        let s = load_config(&mut tx, c.tenant, src, false).await?;
        if s.institution_id != institution {
            return Err(invalid(
                "copyFrom",
                "foreign",
                "copy from a configuration of the same institution",
            ));
        }
    }
    // Serialise version numbering per institution.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(format!("payroll_config:{}", institution.as_uuid()))
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    let next: i32 = sqlx::query_scalar(
        "SELECT COALESCE(max(version_no), 0) + 1 FROM people.payroll_config \
         WHERE tenant_id = $1 AND institution_id = $2",
    )
    .bind(c.tenant.as_uuid())
    .bind(institution.as_uuid())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let sql = format!(
        "INSERT INTO people.payroll_config (tenant_id, institution_id, version_no, name, \
         effective_from, rounding, encashment_divisor, copied_from_id) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING {CONFIG_COLUMNS}"
    );
    let row = sqlx::query(&sql)
        .bind(c.tenant.as_uuid())
        .bind(institution.as_uuid())
        .bind(next)
        .bind(n.name.trim())
        .bind(n.effective_from)
        .bind(n.rounding.as_str())
        .bind(n.encashment_divisor)
        .bind(n.copy_from)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?;
    let cfg = config_from_row(&row).map_err(map_db_error)?;
    if let Some(src) = n.copy_from {
        sqlx::query(
            "INSERT INTO people.pay_component (tenant_id, institution_id, config_id, code, name, \
             kind, basis, amount, rate, sequence) SELECT tenant_id, institution_id, $2, code, name, \
             kind, basis, amount, rate, sequence FROM people.pay_component WHERE config_id = $1",
        )
        .bind(src)
        .bind(cfg.id)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
        sqlx::query(
            "INSERT INTO people.statutory_rule (tenant_id, institution_id, config_id, kind, \
             employee_rate, employer_rate, wage_ceiling, threshold, slabs, fixed_amount, \
             employee_component_code, employer_component_code) SELECT tenant_id, institution_id, \
             $2, kind, employee_rate, employer_rate, wage_ceiling, threshold, slabs, fixed_amount, \
             employee_component_code, employer_component_code FROM people.statutory_rule \
             WHERE config_id = $1",
        )
        .bind(src)
        .bind(cfg.id)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    }
    history(
        &mut tx,
        c.tenant,
        institution,
        CONFIG,
        cfg.id,
        None,
        cfg.status.as_str(),
        &cause("people.payroll_config.created", None, None),
        c.actor,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "people.payroll_config.create",
        CONFIG,
        cfg.id.to_string(),
        json!({"versionNo": cfg.version_no, "copiedFrom": n.copy_from}),
        c.actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(cfg)
}

/// Locks a draft configuration for an edit (409 unless `draft`, 412 on a stale version) and
/// bumps its version.
async fn edit_draft(
    conn: &mut PgConnection,
    c: &Cmd<'_>,
    id: Uuid,
    expected: i64,
) -> Result<ConfigRow, KernelError> {
    let cfg = load_config(conn, c.tenant, id, true).await?;
    ensure_version(expected, cfg.version)?;
    if cfg.status != PayrollConfigStatus::Draft {
        return Err(KernelError::Conflict(format!(
            "configuration v{} is {}; only a draft can change",
            cfg.version_no, cfg.status
        )));
    }
    let sql = format!(
        "UPDATE people.payroll_config SET version = version + 1, updated_at = now() \
         WHERE id = $1 RETURNING {CONFIG_COLUMNS}"
    );
    let row = sqlx::query(&sql)
        .bind(id)
        .fetch_one(&mut *conn)
        .await
        .map_err(map_db_error)?;
    config_from_row(&row).map_err(map_db_error)
}

/// Adds (or replaces, by code) a pay component on a draft configuration.
pub async fn put_component(
    c: &Cmd<'_>,
    config: Uuid,
    expected: i64,
    d: ComponentDef,
) -> Result<ConfigRow, KernelError> {
    if !valid_code(&d.code) {
        return Err(invalid(
            "code",
            "invalid_code",
            "upper-case letters, digits and `_`, starting with a letter (at most 16)",
        ));
    }
    if d.name.trim().is_empty() {
        return Err(invalid("name", "required", "a component name is required"));
    }
    if matches!(
        d.basis,
        CalculationBasis::PercentOfBasic | CalculationBasis::PercentOfGross
    ) && d.amount.is_some()
    {
        return Err(invalid(
            "amount",
            "not_applicable",
            "percentage components take a rate",
        ));
    }
    if d.basis == CalculationBasis::Fixed && d.rate.is_some() {
        return Err(invalid(
            "rate",
            "not_applicable",
            "fixed components take an amount",
        ));
    }
    if d.amount.is_some_and(|a| a.is_sign_negative()) {
        return Err(invalid("amount", "negative", "must not be negative"));
    }
    let mut tx = c.st.pool.begin().await.map_err(map_db_error)?;
    let cfg = edit_draft(&mut tx, c, config, expected).await?;
    sqlx::query(
        "INSERT INTO people.pay_component (tenant_id, institution_id, config_id, code, name, kind, \
         basis, amount, rate, sequence) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) \
         ON CONFLICT (config_id, code) DO UPDATE SET name = EXCLUDED.name, kind = EXCLUDED.kind, \
         basis = EXCLUDED.basis, amount = EXCLUDED.amount, rate = EXCLUDED.rate, \
         sequence = EXCLUDED.sequence",
    )
    .bind(c.tenant.as_uuid())
    .bind(cfg.institution_id.as_uuid())
    .bind(config)
    .bind(&d.code)
    .bind(d.name.trim())
    .bind(d.kind.as_str())
    .bind(d.basis.as_str())
    .bind(d.amount)
    .bind(d.rate)
    .bind(d.sequence)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    audit(
        &mut tx,
        &c.scope(cfg.institution_id),
        "people.payroll_config.component",
        CONFIG,
        config.to_string(),
        json!({"code": d.code, "kind": d.kind.as_str(), "basis": d.basis.as_str()}),
        c.actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(cfg)
}

/// Adds (or replaces, by kind) a statutory rule on a draft configuration.
pub async fn put_rule(
    c: &Cmd<'_>,
    config: Uuid,
    expected: i64,
    d: RuleDef,
) -> Result<ConfigRow, KernelError> {
    if !valid_code(&d.employee_code) || d.employer_code.as_deref().is_some_and(|x| !valid_code(x)) {
        return Err(invalid(
            "employeeComponentCode",
            "invalid_code",
            "component codes are upper-case letters, digits and `_`",
        ));
    }
    let mut last: Option<rust_decimal::Decimal> = None;
    for (i, s) in d.slabs.iter().enumerate() {
        if s.amount.is_sign_negative() {
            return Err(invalid(
                "slabs",
                "negative",
                "slab amounts must not be negative",
            ));
        }
        match (last, s.up_to) {
            (_, None) if i + 1 != d.slabs.len() => {
                return Err(invalid(
                    "slabs",
                    "unordered",
                    "only the last slab may be open-ended",
                ));
            }
            (Some(prev), Some(up)) if up <= prev => {
                return Err(invalid("slabs", "unordered", "slabs must ascend"));
            }
            _ => {}
        }
        last = s.up_to.or(last);
    }
    let slabs = serde_json::to_value(&d.slabs).map_err(KernelError::internal)?;
    let mut tx = c.st.pool.begin().await.map_err(map_db_error)?;
    let cfg = edit_draft(&mut tx, c, config, expected).await?;
    sqlx::query(
        "INSERT INTO people.statutory_rule (tenant_id, institution_id, config_id, kind, \
         employee_rate, employer_rate, wage_ceiling, threshold, slabs, fixed_amount, \
         employee_component_code, employer_component_code) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12) \
         ON CONFLICT (config_id, kind) DO UPDATE SET employee_rate = EXCLUDED.employee_rate, \
         employer_rate = EXCLUDED.employer_rate, wage_ceiling = EXCLUDED.wage_ceiling, \
         threshold = EXCLUDED.threshold, slabs = EXCLUDED.slabs, \
         fixed_amount = EXCLUDED.fixed_amount, \
         employee_component_code = EXCLUDED.employee_component_code, \
         employer_component_code = EXCLUDED.employer_component_code",
    )
    .bind(c.tenant.as_uuid())
    .bind(cfg.institution_id.as_uuid())
    .bind(config)
    .bind(d.kind.as_str())
    .bind(d.employee_rate)
    .bind(d.employer_rate)
    .bind(d.wage_ceiling)
    .bind(d.threshold)
    .bind(slabs)
    .bind(d.fixed_amount)
    .bind(&d.employee_code)
    .bind(d.employer_code.as_deref())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    audit(
        &mut tx,
        &c.scope(cfg.institution_id),
        "people.payroll_config.statutory_rule",
        CONFIG,
        config.to_string(),
        json!({"kind": d.kind.as_str()}),
        c.actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(cfg)
}

/// `^[A-Z][A-Z0-9_]{0,15}$`.
pub fn valid_code(code: &str) -> bool {
    let b = code.as_bytes();
    !b.is_empty()
        && b.len() <= 16
        && b[0].is_ascii_uppercase()
        && b.iter()
            .all(|x| x.is_ascii_uppercase() || x.is_ascii_digit() || *x == b'_')
}

/// Moves a locked configuration and records history.
async fn move_config(
    conn: &mut PgConnection,
    c: &Cmd<'_>,
    cfg: &ConfigRow,
    to: PayrollConfigStatus,
    reason: Option<&str>,
    process_instance_id: Option<&str>,
    actor: &Actor,
) -> Result<ConfigRow, KernelError> {
    if !config_can_transition(cfg.status, to) {
        return Err(illegal(
            "configuration",
            &format!("v{}", cfg.version_no),
            cfg.status.as_str(),
            to.as_str(),
        ));
    }
    let sql = format!(
        "UPDATE people.payroll_config SET status = $2, status_reason = $3, \
         process_instance_id = COALESCE($4, process_instance_id), \
         approved_at = CASE WHEN $2 = 'approved' THEN now() ELSE approved_at END, \
         activated_at = CASE WHEN $2 = 'active' THEN now() ELSE activated_at END, \
         superseded_at = CASE WHEN $2 = 'superseded' THEN now() ELSE superseded_at END, \
         version = version + 1, updated_at = now() WHERE id = $1 RETURNING {CONFIG_COLUMNS}"
    );
    let row = sqlx::query(&sql)
        .bind(cfg.id)
        .bind(to.as_str())
        .bind(reason)
        .bind(process_instance_id)
        .fetch_one(&mut *conn)
        .await
        .map_err(map_db_error)?;
    history(
        conn,
        c.tenant,
        cfg.institution_id,
        CONFIG,
        cfg.id,
        Some(cfg.status.as_str()),
        to.as_str(),
        &cause("people.payroll_config.status", reason, process_instance_id),
        actor,
    )
    .await?;
    config_from_row(&row).map_err(map_db_error)
}

/// Submits a draft for review (L4-E-074-BH-002/003): validates the components and rules
/// (422 with every finding), starts `PAYROLL_CONFIG_APPROVAL`; auto-approval moves it on to
/// `approved` in the same transaction.
pub async fn submit_config(
    c: &Cmd<'_>,
    id: Uuid,
    expected: i64,
    reason: Option<&str>,
) -> Result<ConfigRow, KernelError> {
    let d = config_detail(c.st, c.tenant, id).await?;
    ensure_version(expected, d.config.version)?;
    if d.config.status != PayrollConfigStatus::Draft {
        return Err(illegal(
            "configuration",
            &format!("v{}", d.config.version_no),
            d.config.status.as_str(),
            "review",
        ));
    }
    let comps: Vec<ComponentDef> = d.components.iter().map(|x| x.def.clone()).collect();
    let rules: Vec<RuleDef> = d.rules.iter().map(|x| x.def.clone()).collect();
    let findings = validate_config(&comps, &rules);
    if !findings.is_empty() {
        return Err(KernelError::Validation(
            findings
                .into_iter()
                .map(|(f, code, m)| FieldViolation::new(f, code, m))
                .collect(),
        ));
    }
    let scope = c.scope(d.config.institution_id);
    require_institution_writable(c.st, &scope, d.config.institution_id).await?;
    let outcome = request_approval(
        c.st,
        &scope,
        process::PAYROLL_CONFIG_APPROVAL,
        business_key(CONFIG, id, d.config.version),
        BTreeMap::from([("versionNo".to_owned(), json!(d.config.version_no))]),
    )
    .await?;
    let pid = outcome.process_instance_id().to_owned();
    let reason = opt_reason(reason);
    let mut tx = c.st.pool.begin().await.map_err(map_db_error)?;
    let cfg = load_config(&mut tx, c.tenant, id, true).await?;
    ensure_version(expected, cfg.version)?;
    let mut cfg = move_config(
        &mut tx,
        c,
        &cfg,
        PayrollConfigStatus::Review,
        reason.as_deref(),
        Some(&pid),
        c.actor,
    )
    .await?;
    if matches!(outcome, ApprovalOutcome::Approved { .. }) {
        cfg = move_config(
            &mut tx,
            c,
            &cfg,
            PayrollConfigStatus::Approved,
            Some("auto-approved"),
            Some(&pid),
            &c.auto(),
        )
        .await?;
    }
    audit(
        &mut tx,
        &scope,
        "people.payroll_config.submit",
        CONFIG,
        id.to_string(),
        json!({"status": cfg.status.as_str(), "processInstanceId": pid}),
        c.actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(cfg)
}

/// Decides a configuration under review: approve → `approved`, reject → back to `draft`.
pub async fn decide_config(
    c: &Cmd<'_>,
    id: Uuid,
    expected: i64,
    approve: bool,
    reason: Option<&str>,
) -> Result<ConfigRow, KernelError> {
    let reason = opt_reason(reason);
    if !approve && reason.is_none() {
        return Err(invalid("reason", "required", "a rejection needs a reason"));
    }
    let mut tx = c.st.pool.begin().await.map_err(map_db_error)?;
    let cfg = load_config(&mut tx, c.tenant, id, true).await?;
    ensure_version(expected, cfg.version)?;
    if cfg.status != PayrollConfigStatus::Review {
        return Err(KernelError::Conflict(format!(
            "configuration v{} is {}; only a configuration under review can be decided",
            cfg.version_no, cfg.status
        )));
    }
    let to = if approve {
        PayrollConfigStatus::Approved
    } else {
        PayrollConfigStatus::Draft
    };
    let pid = cfg.process_instance_id.clone();
    let cfg = move_config(
        &mut tx,
        c,
        &cfg,
        to,
        reason.as_deref(),
        pid.as_deref(),
        c.actor,
    )
    .await?;
    audit(
        &mut tx,
        &c.scope(cfg.institution_id),
        "people.payroll_config.decide",
        CONFIG,
        id.to_string(),
        json!({"decision": if approve { "approve" } else { "reject" }, "reason": reason}),
        c.actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(cfg)
}

/// Activates an approved configuration (L4-E-074-BH-004/005): the previously active version
/// becomes `superseded` in the same transaction; `payroll_config.activated`.
pub async fn activate_config(
    c: &Cmd<'_>,
    id: Uuid,
    expected: i64,
) -> Result<ConfigRow, KernelError> {
    let mut tx = c.st.pool.begin().await.map_err(map_db_error)?;
    let cfg = load_config(&mut tx, c.tenant, id, true).await?;
    ensure_version(expected, cfg.version)?;
    let scope = c.scope(cfg.institution_id);
    require_institution_writable(c.st, &scope, cfg.institution_id).await?;
    if !config_can_transition(cfg.status, PayrollConfigStatus::Active) {
        return Err(illegal(
            "configuration",
            &format!("v{}", cfg.version_no),
            cfg.status.as_str(),
            "active",
        ));
    }
    let previous = active_config(&mut tx, c.tenant, cfg.institution_id).await?;
    let mut superseded = None;
    if let Some(prev) = previous {
        let locked = load_config(&mut tx, c.tenant, prev.id, true).await?;
        let reason = format!("superseded by v{}", cfg.version_no);
        move_config(
            &mut tx,
            c,
            &locked,
            PayrollConfigStatus::Superseded,
            Some(&reason),
            None,
            c.actor,
        )
        .await?;
        superseded = Some(prev.id);
    }
    let from = cfg.status;
    let pid = cfg.process_instance_id.clone();
    let cfg = move_config(
        &mut tx,
        c,
        &cfg,
        PayrollConfigStatus::Active,
        None,
        pid.as_deref(),
        c.actor,
    )
    .await?;
    event(
        &mut tx,
        &scope,
        PAYROLL_CONFIG_ACTIVATED,
        PAYROLL_CONFIG_ACTIVATED_SCHEMA,
        cfg.id.to_string(),
        &payload(&Change {
            aggregate_id: cfg.id,
            institution: Some(cfg.institution_id),
            from: Some(from.as_str()),
            to: cfg.status.as_str(),
            version: cfg.version,
            process_instance_id: cfg.process_instance_id.as_deref(),
            ..Change::default()
        }),
        c.actor,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "people.payroll_config.activate",
        CONFIG,
        id.to_string(),
        json!({"versionNo": cfg.version_no, "superseded": superseded}),
        c.actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(cfg)
}

/// Retires an active (or approved, never activated) configuration.
pub async fn supersede_config(
    c: &Cmd<'_>,
    id: Uuid,
    expected: i64,
    reason: Option<&str>,
) -> Result<ConfigRow, KernelError> {
    let reason = super::common::required_reason(reason)?;
    let mut tx = c.st.pool.begin().await.map_err(map_db_error)?;
    let cfg = load_config(&mut tx, c.tenant, id, true).await?;
    ensure_version(expected, cfg.version)?;
    let cfg = move_config(
        &mut tx,
        c,
        &cfg,
        PayrollConfigStatus::Superseded,
        Some(&reason),
        None,
        c.actor,
    )
    .await?;
    audit(
        &mut tx,
        &c.scope(cfg.institution_id),
        "people.payroll_config.supersede",
        CONFIG,
        id.to_string(),
        json!({"reason": reason}),
        c.actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(cfg)
}

// ------------------------------------------------------------------ salary structures

/// Input of a salary structure.
pub struct StructureInput {
    pub code: String,
    pub name: String,
    pub grade: Option<String>,
    pub lines: Vec<StructureLine>,
}

fn valid_structure_code(code: &str) -> bool {
    let b = code.as_bytes();
    !b.is_empty()
        && b.len() <= 32
        && (b[0].is_ascii_uppercase() || b[0].is_ascii_digit())
        && b.iter()
            .all(|x| x.is_ascii_uppercase() || x.is_ascii_digit() || *x == b'_' || *x == b'-')
}

fn check_structure(s: &StructureInput) -> Result<(), KernelError> {
    if !valid_structure_code(&s.code) {
        return Err(invalid(
            "code",
            "invalid_code",
            "upper-case letters, digits, `_` and `-` (at most 32)",
        ));
    }
    if s.name.trim().is_empty() {
        return Err(invalid("name", "required", "a name is required"));
    }
    if s.lines.is_empty() {
        return Err(invalid("lines", "required", "at least one line (BASIC)"));
    }
    let mut seen = std::collections::BTreeSet::new();
    for l in &s.lines {
        if !valid_code(&l.code) {
            return Err(invalid(
                "lines",
                "invalid_code",
                format!("{} is not a component code", l.code),
            ));
        }
        if !seen.insert(l.code.clone()) {
            return Err(invalid(
                "lines",
                "duplicate",
                format!("{} appears twice", l.code),
            ));
        }
        if l.amount.is_none() && l.rate.is_none() {
            return Err(invalid(
                "lines",
                "required",
                format!("{} needs an amount or a rate", l.code),
            ));
        }
        if l.amount.is_some_and(|a| a.is_sign_negative()) {
            return Err(invalid("lines", "negative", "amounts must not be negative"));
        }
    }
    if !s
        .lines
        .iter()
        .any(|l| l.code == super::domain::BASIC && l.amount.is_some())
    {
        return Err(invalid(
            "lines",
            "basic_required",
            "a BASIC amount is required",
        ));
    }
    Ok(())
}

async fn write_lines(
    conn: &mut PgConnection,
    tenant: TenantId,
    structure: Uuid,
    lines: &[StructureLine],
) -> Result<(), KernelError> {
    sqlx::query("DELETE FROM people.salary_structure_line WHERE structure_id = $1")
        .bind(structure)
        .execute(&mut *conn)
        .await
        .map_err(map_db_error)?;
    for l in lines {
        sqlx::query(
            "INSERT INTO people.salary_structure_line (tenant_id, structure_id, component_code, \
             amount, rate) VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(tenant.as_uuid())
        .bind(structure)
        .bind(&l.code)
        .bind(l.amount)
        .bind(l.rate)
        .execute(&mut *conn)
        .await
        .map_err(map_db_error)?;
    }
    Ok(())
}

pub async fn create_structure(
    c: &Cmd<'_>,
    institution: InstitutionId,
    s: StructureInput,
) -> Result<StructureRow, KernelError> {
    check_structure(&s)?;
    let scope = c.scope(institution);
    require_institution_writable(c.st, &scope, institution).await?;
    let mut tx = c.st.pool.begin().await.map_err(map_db_error)?;
    let sql = format!(
        "INSERT INTO people.salary_structure (tenant_id, institution_id, code, name, grade) \
         VALUES ($1, $2, $3, $4, $5) RETURNING {STRUCTURE_COLUMNS}"
    );
    let row = sqlx::query(&sql)
        .bind(c.tenant.as_uuid())
        .bind(institution.as_uuid())
        .bind(&s.code)
        .bind(s.name.trim())
        .bind(s.grade.as_deref().map(str::trim))
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| match map_db_error(e) {
            KernelError::Conflict(_) => KernelError::Conflict(
                "a salary structure with this code (or an active one for this grade) exists".into(),
            ),
            other => other,
        })?;
    let mut out = structure_from_row(&row).map_err(map_db_error)?;
    write_lines(&mut tx, c.tenant, out.id, &s.lines).await?;
    out.lines = structure_lines(&mut tx, out.id).await?;
    audit(
        &mut tx,
        &scope,
        "people.salary_structure.create",
        "salary_structure",
        out.id.to_string(),
        json!({"code": out.code, "grade": out.grade}),
        c.actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(out)
}

/// Replaces a structure's name, grade and lines (`If-Match`).
pub async fn update_structure(
    c: &Cmd<'_>,
    id: Uuid,
    expected: i64,
    s: StructureInput,
) -> Result<StructureRow, KernelError> {
    let mut tx = c.st.pool.begin().await.map_err(map_db_error)?;
    let cur = load_structure(&mut tx, c.tenant, id, true).await?;
    ensure_version(expected, cur.version)?;
    let s = StructureInput {
        code: cur.code.clone(),
        ..s
    };
    check_structure(&s)?;
    let sql = format!(
        "UPDATE people.salary_structure SET name = $2, grade = $3, version = version + 1, \
         updated_at = now() WHERE id = $1 RETURNING {STRUCTURE_COLUMNS}"
    );
    let row = sqlx::query(&sql)
        .bind(id)
        .bind(s.name.trim())
        .bind(s.grade.as_deref().map(str::trim))
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?;
    let mut out = structure_from_row(&row).map_err(map_db_error)?;
    write_lines(&mut tx, c.tenant, id, &s.lines).await?;
    out.lines = structure_lines(&mut tx, id).await?;
    audit(
        &mut tx,
        &c.scope(out.institution_id),
        "people.salary_structure.update",
        "salary_structure",
        id.to_string(),
        json!({"lines": out.lines.len()}),
        c.actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(out)
}

pub async fn list_structures(
    st: &PeopleState,
    tenant: TenantId,
    institution: InstitutionId,
) -> Result<Vec<StructureRow>, KernelError> {
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let sql = format!(
        "SELECT {STRUCTURE_COLUMNS} FROM people.salary_structure WHERE tenant_id = $1 \
         AND institution_id = $2 ORDER BY code"
    );
    let rows = sqlx::query(&sql)
        .bind(tenant.as_uuid())
        .bind(institution.as_uuid())
        .fetch_all(&mut *conn)
        .await
        .map_err(map_db_error)?;
    let mut out = Vec::with_capacity(rows.len());
    for r in &rows {
        let mut s = structure_from_row(r).map_err(map_db_error)?;
        s.lines = structure_lines(&mut conn, s.id).await?;
        out.push(s);
    }
    Ok(out)
}

// ------------------------------------------------------------------ salary assignments

/// Input of a salary assignment.
pub struct AssignmentInput {
    /// `None` = the active structure of the engagement's grade.
    pub structure_id: Option<Uuid>,
    pub effective_from: NaiveDate,
    pub effective_to: Option<NaiveDate>,
    pub reason: Option<String>,
}

/// Assigns a salary structure to an engagement from a date (effective dated; overlapping an
/// existing assignment → 409). The engagement row is locked to serialise assignments.
pub async fn create_assignment(
    c: &Cmd<'_>,
    engagement: Uuid,
    a: AssignmentInput,
) -> Result<AssignmentRow, KernelError> {
    if a.effective_to.is_some_and(|t| t < a.effective_from) {
        return Err(invalid(
            "effectiveTo",
            "before_start",
            "must not be before effectiveFrom",
        ));
    }
    let mut tx = c.st.pool.begin().await.map_err(map_db_error)?;
    let e = lock_engagement(&mut tx, c.tenant, StaffEngagementId(engagement)).await?;
    require_live_engagement(&e)?;
    let scope = c.scope(e.institution_id);
    require_institution_writable(c.st, &scope, e.institution_id).await?;
    if a.effective_from < e.start_date {
        return Err(invalid(
            "effectiveFrom",
            "before_engagement",
            format!("the engagement starts on {}", e.start_date),
        ));
    }
    let structure_id = match a.structure_id {
        Some(s) => s,
        None => {
            let grade = e.grade.clone().ok_or_else(|| {
                invalid(
                    "structureId",
                    "required",
                    "the engagement has no grade; name a structure",
                )
            })?;
            sqlx::query_scalar::<_, Uuid>(
                "SELECT id FROM people.salary_structure WHERE tenant_id = $1 \
                 AND institution_id = $2 AND grade = $3 AND status = 'active'",
            )
            .bind(c.tenant.as_uuid())
            .bind(e.institution_id.as_uuid())
            .bind(&grade)
            .fetch_optional(&mut *tx)
            .await
            .map_err(map_db_error)?
            .ok_or_else(|| {
                invalid(
                    "structureId",
                    "no_structure",
                    format!("no active salary structure for grade {grade}"),
                )
            })?
        }
    };
    let s = match load_structure(&mut tx, c.tenant, structure_id, false).await {
        Ok(s) => s,
        Err(KernelError::NotFound { .. }) => {
            return Err(invalid(
                "structureId",
                "not_found",
                "unknown salary structure",
            ))
        }
        Err(other) => return Err(other),
    };
    if s.institution_id != e.institution_id || s.status != "active" {
        return Err(invalid(
            "structureId",
            "not_applicable",
            "an active structure of the engagement's institution",
        ));
    }
    let overlap: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM people.salary_assignment WHERE engagement_id = $1 \
         AND effective_from <= COALESCE($3, DATE '9999-12-31') \
         AND COALESCE(effective_to, DATE '9999-12-31') >= $2)",
    )
    .bind(engagement)
    .bind(a.effective_from)
    .bind(a.effective_to)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    if overlap {
        return Err(KernelError::Conflict(
            "the engagement already has a salary assignment in this window; end it first".into(),
        ));
    }
    let status = if a.effective_to.is_some() {
        SalaryAssignmentStatus::Ended
    } else {
        SalaryAssignmentStatus::Active
    };
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO people.salary_assignment (tenant_id, institution_id, staff_id, engagement_id, \
         structure_id, effective_from, effective_to, status, reason) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) RETURNING id",
    )
    .bind(c.tenant.as_uuid())
    .bind(e.institution_id.as_uuid())
    .bind(e.staff_id.as_uuid())
    .bind(engagement)
    .bind(structure_id)
    .bind(a.effective_from)
    .bind(a.effective_to)
    .bind(status.as_str())
    .bind(a.reason.as_deref())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    audit(
        &mut tx,
        &scope,
        "people.salary_assignment.create",
        "salary_assignment",
        id.to_string(),
        json!({"engagementId": engagement, "structureId": structure_id,
               "effectiveFrom": a.effective_from, "effectiveTo": a.effective_to}),
        c.actor,
    )
    .await?;
    let out = load_assignment(&mut tx, c.tenant, id).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(out)
}

pub async fn load_assignment(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
) -> Result<AssignmentRow, KernelError> {
    let sql = format!("{ASSIGNMENT_SELECT} WHERE a.id = $1 AND a.tenant_id = $2");
    let row = sqlx::query(&sql)
        .bind(id)
        .bind(tenant.as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound {
            entity: "salary assignment",
        })?;
    assignment_from_row(&row).map_err(map_db_error)
}

pub async fn list_assignments(
    st: &PeopleState,
    tenant: TenantId,
    engagement: Uuid,
) -> Result<Vec<AssignmentRow>, KernelError> {
    let sql = format!(
        "{ASSIGNMENT_SELECT} WHERE a.tenant_id = $1 AND a.engagement_id = $2 \
         ORDER BY a.effective_from DESC"
    );
    sqlx::query(&sql)
        .bind(tenant.as_uuid())
        .bind(engagement)
        .fetch_all(&st.pool)
        .await
        .map_err(map_db_error)?
        .iter()
        .map(assignment_from_row)
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_db_error)
}

/// Ends an assignment on `end_date` (may lie in a finalised period: a retro run then pays the
/// difference, L3-E-055).
pub async fn end_assignment(
    c: &Cmd<'_>,
    id: Uuid,
    expected: i64,
    end_date: NaiveDate,
    reason: Option<&str>,
) -> Result<AssignmentRow, KernelError> {
    let mut tx = c.st.pool.begin().await.map_err(map_db_error)?;
    let cur = load_assignment(&mut tx, c.tenant, id).await?;
    lock_engagement(&mut tx, c.tenant, StaffEngagementId(cur.engagement_id)).await?;
    let cur = load_assignment(&mut tx, c.tenant, id).await?;
    ensure_version(expected, cur.version)?;
    if end_date < cur.effective_from {
        return Err(invalid(
            "endDate",
            "before_start",
            "must not be before effectiveFrom",
        ));
    }
    if cur.effective_to.is_some_and(|t| end_date > t) {
        return Err(invalid(
            "endDate",
            "after_end",
            "an assignment can only be shortened",
        ));
    }
    let reason = opt_reason(reason);
    sqlx::query(
        "UPDATE people.salary_assignment SET effective_to = $2, status = 'ended', \
         reason = COALESCE($3, reason), version = version + 1, updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(end_date)
    .bind(reason.as_deref())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    audit(
        &mut tx,
        &c.scope(cur.institution_id),
        "people.salary_assignment.end",
        "salary_assignment",
        id.to_string(),
        json!({"endDate": end_date, "reason": reason}),
        c.actor,
    )
    .await?;
    let out = load_assignment(&mut tx, c.tenant, id).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(out)
}

// ------------------------------------------------------------------ periods

/// Inserts (or finds) the period of `year`-`month` in the caller's transaction.
pub async fn ensure_period(
    conn: &mut PgConnection,
    tenant: TenantId,
    institution: InstitutionId,
    year: i32,
    month: u32,
) -> Result<(PeriodRow, bool), KernelError> {
    let (start, end) = month_bounds(year, month)
        .ok_or_else(|| invalid("month", "invalid", "not a calendar month"))?;
    let month = i32::try_from(month).unwrap_or(1);
    let sql = format!(
        "INSERT INTO people.payroll_period (tenant_id, institution_id, year, month, period_start, \
         period_end) VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT (tenant_id, institution_id, year, \
         month) DO NOTHING RETURNING {PERIOD_COLUMNS}"
    );
    let inserted = sqlx::query(&sql)
        .bind(tenant.as_uuid())
        .bind(institution.as_uuid())
        .bind(year)
        .bind(month)
        .bind(start)
        .bind(end)
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?;
    if let Some(r) = inserted {
        return Ok((period_from_row(&r).map_err(map_db_error)?, true));
    }
    let sql = format!(
        "SELECT {PERIOD_COLUMNS} FROM people.payroll_period WHERE tenant_id = $1 \
         AND institution_id = $2 AND year = $3 AND month = $4"
    );
    let r = sqlx::query(&sql)
        .bind(tenant.as_uuid())
        .bind(institution.as_uuid())
        .bind(year)
        .bind(month)
        .fetch_one(&mut *conn)
        .await
        .map_err(map_db_error)?;
    Ok((period_from_row(&r).map_err(map_db_error)?, false))
}

/// Opens the payroll period of a month (409 when it exists).
pub async fn create_period(
    c: &Cmd<'_>,
    institution: InstitutionId,
    year: i32,
    month: u32,
) -> Result<PeriodRow, KernelError> {
    if !(2000..=2100).contains(&year) {
        return Err(invalid("year", "out_of_range", "between 2000 and 2100"));
    }
    if !(1..=12).contains(&month) {
        return Err(invalid("month", "out_of_range", "between 1 and 12"));
    }
    let scope = c.scope(institution);
    require_institution_writable(c.st, &scope, institution).await?;
    let mut tx = c.st.pool.begin().await.map_err(map_db_error)?;
    let (p, created) = ensure_period(&mut tx, c.tenant, institution, year, month).await?;
    if !created {
        return Err(KernelError::Conflict(format!(
            "the payroll period {year}-{month:02} exists"
        )));
    }
    audit(
        &mut tx,
        &scope,
        "people.payroll_period.create",
        "payroll_period",
        p.id.to_string(),
        json!({"year": year, "month": month}),
        c.actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(p)
}

pub async fn list_periods(
    st: &PeopleState,
    tenant: TenantId,
    institution: InstitutionId,
) -> Result<Vec<PeriodRow>, KernelError> {
    let sql = format!(
        "SELECT {PERIOD_COLUMNS} FROM people.payroll_period WHERE tenant_id = $1 \
         AND institution_id = $2 ORDER BY period_start DESC"
    );
    sqlx::query(&sql)
        .bind(tenant.as_uuid())
        .bind(institution.as_uuid())
        .fetch_all(&st.pool)
        .await
        .map_err(map_db_error)?
        .iter()
        .map(period_from_row)
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_db_error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn component_and_structure_codes() {
        assert!(valid_code("BASIC"));
        assert!(valid_code("PF_ER"));
        assert!(!valid_code("basic"));
        assert!(!valid_code("1PF"));
        assert!(!valid_code(""));
        assert!(!valid_code("ABCDEFGHIJKLMNOPQ"));
        assert!(valid_structure_code("AP-1"));
        assert!(!valid_structure_code("ap-1"));
        let s = StructureInput {
            code: "AP-1".into(),
            name: "Assistant Professor level 1".into(),
            grade: Some("AP-1".into()),
            lines: vec![StructureLine {
                code: "BASIC".into(),
                amount: Some(rust_decimal::Decimal::from(30000)),
                rate: None,
            }],
        };
        assert!(check_structure(&s).is_ok());
        let none = StructureInput {
            lines: vec![],
            code: "AP-1".into(),
            name: "x".into(),
            grade: None,
        };
        assert!(check_structure(&none).is_err());
    }
}
