//! Progression policies, decisions (+ revisions, conditions, probation, detention) and the
//! credit ledger. Owner-local transactions with audit + outbox in the same transaction.

use super::domain::*;
use crate::registration::support::{
    audit, check_version, conflict, emit, invalid, parse_enum, read_student, set_standing,
    standing_of, status_changed, write_student, StudentScope,
};
use crate::shared::approval::{business_key, process, request_approval, ApprovalOutcome};
use crate::shared::guard::require_institution_writable;
use crate::shared::student::{lock_context, set_context_progress, StudentContextRow};
use crate::shared::time::business_today;
use crate::shared::{Actor, StudentState};
use chrono::{DateTime, NaiveDate, Utc};
use serde_json::json;
use sqlx::{PgConnection, PgExecutor, PgPool};
use std::collections::BTreeMap;
use sutra_contracts::exams::StudentTermResultQuery;
use sutra_contracts::institution::status::PublicationStatus;
use sutra_contracts::student::events::progression::*;
use sutra_contracts::student::ids::StudentId;
use sutra_contracts::student::status::{
    AcademicStanding, CreditEntryKind, PolicyVersionStatus, ProbationStatus,
    ProgressionConditionStatus, ProgressionDecisionStatus, ProgressionOutcome,
};
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use uuid::Uuid;

const POLICY: KernelError = KernelError::NotFound {
    entity: "progression policy",
};
const DECISION: KernelError = KernelError::NotFound {
    entity: "progression decision",
};
const CONDITION: KernelError = KernelError::NotFound {
    entity: "progression condition",
};
const PROBATION: KernelError = KernelError::NotFound {
    entity: "probation case",
};
const DETENTION: KernelError = KernelError::NotFound {
    entity: "detention case",
};

pub(crate) fn text(
    v: Option<String>,
    field: &str,
    max: usize,
) -> Result<Option<String>, KernelError> {
    let v = v.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty());
    if v.as_ref().is_some_and(|s| s.chars().count() > max) {
        return Err(invalid(
            field,
            "too_long",
            format!("at most {max} characters"),
        ));
    }
    Ok(v)
}

pub(crate) fn required(v: Option<String>, field: &str, max: usize) -> Result<String, KernelError> {
    text(v, field, max)?.ok_or_else(|| invalid(field, "required", "required"))
}

fn check_repeat_unit(u: Option<&str>) -> Result<(), KernelError> {
    if u.is_some_and(|u| !matches!(u, "term" | "year")) {
        return Err(invalid("repeatUnit", "invalid", "`term` or `year`"));
    }
    Ok(())
}

// ================================================================== policies

const POLICY_COLUMNS: &str = "id, institution_id, programme_id, code, version_no, name, effective_from, \
    rules, status, status_reason, supersedes_policy_id, process_instance_id, effective_at, version, \
    created_at, updated_at";

#[derive(sqlx::FromRow)]
struct PolicyRow {
    id: Uuid,
    institution_id: Uuid,
    programme_id: Option<Uuid>,
    code: String,
    version_no: i32,
    name: String,
    effective_from: NaiveDate,
    rules: serde_json::Value,
    status: String,
    status_reason: Option<String>,
    supersedes_policy_id: Option<Uuid>,
    process_instance_id: Option<String>,
    effective_at: Option<DateTime<Utc>>,
    version: i64,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl PolicyRow {
    fn dto(self) -> Result<ProgressionPolicy, KernelError> {
        Ok(ProgressionPolicy {
            rules: serde_json::from_value(self.rules).map_err(KernelError::internal)?,
            status: parse_enum(&self.status, PolicyVersionStatus::parse, "policy status")?,
            id: self.id,
            institution_id: self.institution_id,
            programme_id: self.programme_id,
            code: self.code,
            version_no: self.version_no,
            name: self.name,
            effective_from: self.effective_from,
            status_reason: self.status_reason,
            supersedes_policy_id: self.supersedes_policy_id,
            process_instance_id: self.process_instance_id,
            effective_at: self.effective_at,
            version: self.version,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}

async fn policy_row<'e, E: PgExecutor<'e>>(
    ex: E,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<ProgressionPolicy, KernelError> {
    let sql = format!(
        "SELECT {POLICY_COLUMNS} FROM student.progression_policy WHERE id = $1 AND tenant_id = $2{}",
        if lock { " FOR UPDATE" } else { "" }
    );
    sqlx::query_as::<_, PolicyRow>(&sql)
        .bind(id)
        .bind(tenant.as_uuid())
        .fetch_optional(ex)
        .await
        .map_err(map_db_error)?
        .ok_or(POLICY)?
        .dto()
}

pub async fn get_policy(
    pool: &PgPool,
    tenant: TenantId,
    id: Uuid,
) -> Result<ProgressionPolicy, KernelError> {
    policy_row(pool, tenant, id, false).await
}

pub async fn list_policies(
    state: &StudentState,
    scope: &ExecutionScope,
    institution_id: InstitutionId,
    status: Option<PolicyVersionStatus>,
) -> Result<Vec<ProgressionPolicy>, KernelError> {
    state
        .references
        .institution(scope, institution_id)
        .await?
        .ok_or(KernelError::NotFound {
            entity: "institution",
        })?;
    let sql = format!(
        "SELECT {POLICY_COLUMNS} FROM student.progression_policy WHERE tenant_id = $1 AND institution_id = $2 \
         AND ($3::text IS NULL OR status = $3) ORDER BY code, version_no DESC"
    );
    sqlx::query_as::<_, PolicyRow>(&sql)
        .bind(scope.tenant_id.as_uuid())
        .bind(institution_id.as_uuid())
        .bind(status.map(|s| s.as_str()))
        .fetch_all(&state.pool)
        .await
        .map_err(map_db_error)?
        .into_iter()
        .map(PolicyRow::dto)
        .collect()
}

/// The effective policy governing a context on `on` (programme-specific first).
pub async fn effective_policy<'e, E: PgExecutor<'e>>(
    ex: E,
    tenant: TenantId,
    ctx: &StudentContextRow,
    on: NaiveDate,
) -> Result<Option<ProgressionPolicy>, KernelError> {
    let sql = format!(
        "SELECT {POLICY_COLUMNS} FROM student.progression_policy WHERE tenant_id = $1 AND institution_id = $2 \
         AND status = 'effective' AND effective_from <= $4 AND (programme_id IS NULL OR programme_id = $3) \
         ORDER BY (programme_id IS NOT NULL) DESC, effective_from DESC, version_no DESC LIMIT 1"
    );
    sqlx::query_as::<_, PolicyRow>(&sql)
        .bind(tenant.as_uuid())
        .bind(ctx.institution_id.as_uuid())
        .bind(ctx.programme_id)
        .bind(on)
        .fetch_optional(ex)
        .await
        .map_err(map_db_error)?
        .map(PolicyRow::dto)
        .transpose()
}

fn validate_rules(r: &PolicyRules) -> Result<(), KernelError> {
    let p = r.problems();
    if p.is_empty() {
        return Ok(());
    }
    Err(KernelError::Validation(
        p.into_iter()
            .map(|(f, m)| sutra_kernel::error::FieldViolation::new(f, "invalid", m))
            .collect(),
    ))
}

async fn insert_policy(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    institution_id: InstitutionId,
    p: (
        &str,
        &str,
        Option<Uuid>,
        NaiveDate,
        &PolicyRules,
        i32,
        Option<Uuid>,
    ),
) -> Result<ProgressionPolicy, KernelError> {
    let (code, name, programme_id, effective_from, rules, version_no, supersedes) = p;
    let sql = format!(
        "INSERT INTO student.progression_policy (tenant_id, institution_id, programme_id, code, version_no, \
         name, effective_from, rules, supersedes_policy_id) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) \
         RETURNING {POLICY_COLUMNS}"
    );
    sqlx::query_as::<_, PolicyRow>(&sql)
        .bind(scope.tenant_id.as_uuid())
        .bind(institution_id.as_uuid())
        .bind(programme_id)
        .bind(code)
        .bind(version_no)
        .bind(name)
        .bind(effective_from)
        .bind(serde_json::to_value(rules).map_err(KernelError::internal)?)
        .bind(supersedes)
        .fetch_one(&mut *conn)
        .await
        .map_err(|e| match map_db_error(e) {
            KernelError::Conflict(_) => {
                conflict("the policy code already has an open draft or this version exists")
            }
            other => other,
        })?
        .dto()
}

/// Defines a draft progression policy (L3-C-075).
pub async fn create_policy(
    state: &StudentState,
    scope: &ExecutionScope,
    institution_id: InstitutionId,
    cmd: CreateProgressionPolicy,
    actor: &Actor,
) -> Result<ProgressionPolicy, KernelError> {
    require_institution_writable(state, scope, institution_id).await?;
    let code = cmd.code.trim().to_owned();
    if code.is_empty()
        || code.len() > 40
        || !code
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c))
    {
        return Err(invalid(
            "code",
            "invalid",
            "1-40 letters, digits, `_`, `.` or `-`",
        ));
    }
    let name = required(Some(cmd.name), "name", 160)?;
    validate_rules(&cmd.rules)?;
    if let Some(p) = cmd.programme_id {
        state
            .references
            .programme(scope, p)
            .await?
            .filter(|x| x.institution_id == institution_id)
            .ok_or_else(|| {
                invalid(
                    "programmeId",
                    "unknown",
                    "no such programme in this institution",
                )
            })?;
    }
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let existing: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM student.progression_policy WHERE tenant_id = $1 AND institution_id = $2 AND code = $3",
    )
    .bind(scope.tenant_id.as_uuid())
    .bind(institution_id.as_uuid())
    .bind(&code)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    if existing > 0 {
        return Err(conflict(
            "a policy with this code exists; revise it to create a new version",
        ));
    }
    let p = insert_policy(
        &mut tx,
        scope,
        institution_id,
        (
            &code,
            &name,
            cmd.programme_id,
            cmd.effective_from,
            &cmd.rules,
            1,
            None,
        ),
    )
    .await?;
    audit(
        &mut tx,
        scope,
        "student.progression_policy.create",
        "progression_policy",
        p.id,
        json!({"code": code, "rules": cmd.rules}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(p)
}

/// Creates the next draft version of a policy (history and version continuity, L4-C-075-BH-004).
pub async fn revise_policy(
    state: &StudentState,
    tenant: TenantId,
    id: Uuid,
    actor: &Actor,
) -> Result<ProgressionPolicy, KernelError> {
    let cur = get_policy(&state.pool, tenant, id).await?;
    let inst = InstitutionId(cur.institution_id);
    let scope = ExecutionScope::institution(tenant, inst);
    require_institution_writable(state, &scope, inst).await?;
    if !matches!(
        cur.status,
        PolicyVersionStatus::Effective
            | PolicyVersionStatus::Retired
            | PolicyVersionStatus::Superseded
    ) {
        return Err(conflict(
            "only an effective or historical version can be revised",
        ));
    }
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let next: i32 = sqlx::query_scalar(
        "SELECT COALESCE(max(version_no), 0) + 1 FROM student.progression_policy WHERE tenant_id = $1 \
         AND institution_id = $2 AND code = $3",
    )
    .bind(tenant.as_uuid())
    .bind(cur.institution_id)
    .bind(&cur.code)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let p = insert_policy(
        &mut tx,
        &scope,
        inst,
        (
            &cur.code,
            &cur.name,
            cur.programme_id,
            cur.effective_from,
            &cur.rules,
            next,
            Some(cur.id),
        ),
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "student.progression_policy.revise",
        "progression_policy",
        p.id,
        json!({"from": cur.id, "versionNo": next}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(p)
}

pub async fn update_policy(
    state: &StudentState,
    tenant: TenantId,
    id: Uuid,
    expected: Version,
    cmd: UpdateProgressionPolicy,
    actor: &Actor,
) -> Result<ProgressionPolicy, KernelError> {
    let cur = get_policy(&state.pool, tenant, id).await?;
    let scope = ExecutionScope::institution(tenant, InstitutionId(cur.institution_id));
    require_institution_writable(state, &scope, InstitutionId(cur.institution_id)).await?;
    if let Some(r) = &cmd.rules {
        validate_rules(r)?;
    }
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let p = policy_row(&mut *tx, tenant, id, true).await?;
    check_version(p.version, expected)?;
    if p.status != PolicyVersionStatus::Draft {
        return Err(conflict(format!(
            "policy version is {}; only drafts can change",
            p.status
        )));
    }
    let name = text(cmd.name, "name", 160)?.unwrap_or(p.name.clone());
    let rules = cmd.rules.unwrap_or(p.rules.clone());
    let sql = format!(
        "UPDATE student.progression_policy SET name = $2, effective_from = $3, rules = $4, version = version + 1, \
         updated_at = now() WHERE id = $1 RETURNING {POLICY_COLUMNS}"
    );
    let p2 = sqlx::query_as::<_, PolicyRow>(&sql)
        .bind(id)
        .bind(&name)
        .bind(cmd.effective_from.unwrap_or(p.effective_from))
        .bind(serde_json::to_value(&rules).map_err(KernelError::internal)?)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?
        .dto()?;
    audit(
        &mut tx,
        &scope,
        "student.progression_policy.update",
        "progression_policy",
        id,
        json!({"rules": rules}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(p2)
}

async fn set_policy_status(
    conn: &mut PgConnection,
    p: &ProgressionPolicy,
    to: PolicyVersionStatus,
    reason: Option<&str>,
    pid: Option<&str>,
) -> Result<ProgressionPolicy, KernelError> {
    if !policy_can_transition(p.status, to) {
        return Err(conflict(format!(
            "policy version is {}; it cannot become {to}",
            p.status
        )));
    }
    let sql = format!(
        "UPDATE student.progression_policy SET status = $2, status_reason = $3, \
         process_instance_id = COALESCE($4, process_instance_id), \
         effective_at = CASE WHEN $2 = 'effective' THEN now() ELSE effective_at END, \
         version = version + 1, updated_at = now() WHERE id = $1 RETURNING {POLICY_COLUMNS}"
    );
    sqlx::query_as::<_, PolicyRow>(&sql)
        .bind(p.id)
        .bind(to.as_str())
        .bind(reason)
        .bind(pid)
        .fetch_one(&mut *conn)
        .await
        .map_err(map_db_error)?
        .dto()
}

/// in_review → effective; the previous effective version of the code is superseded.
async fn make_policy_effective(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    p: &ProgressionPolicy,
    pid: Option<&str>,
    actor: &Actor,
) -> Result<ProgressionPolicy, KernelError> {
    let prev: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM student.progression_policy WHERE tenant_id = $1 AND institution_id = $2 AND code = $3 \
         AND status = 'effective' FOR UPDATE",
    )
    .bind(scope.tenant_id.as_uuid())
    .bind(p.institution_id)
    .bind(&p.code)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    if let Some(prev) = prev {
        let old = policy_row(&mut *conn, scope.tenant_id, prev, false).await?;
        let old = set_policy_status(
            conn,
            &old,
            PolicyVersionStatus::Superseded,
            Some("superseded by a newer version"),
            None,
        )
        .await?;
        audit(
            conn,
            scope,
            "student.progression_policy.supersede",
            "progression_policy",
            old.id,
            json!({"by": p.id}),
            actor,
        )
        .await?;
    }
    let e = set_policy_status(conn, p, PolicyVersionStatus::Effective, None, pid).await?;
    emit(
        conn,
        scope,
        PROGRESSION_POLICY_EFFECTIVE,
        PROGRESSION_POLICY_EFFECTIVE_SCHEMA,
        e.id,
        &status_changed(
            e.id,
            scope,
            None,
            Some("in_review"),
            "effective",
            e.version,
            None,
            pid,
            None,
        ),
        actor,
    )
    .await?;
    audit(
        conn,
        scope,
        "student.progression_policy.activate",
        "progression_policy",
        e.id,
        json!({"processInstanceId": pid, "supersedes": prev}),
        actor,
    )
    .await?;
    Ok(e)
}

/// Submits a draft for institutional governance (WorkflowPort `student.progression_policy`).
pub async fn submit_policy(
    state: &StudentState,
    tenant: TenantId,
    id: Uuid,
    expected: Version,
    actor: &Actor,
) -> Result<ProgressionPolicy, KernelError> {
    let cur = get_policy(&state.pool, tenant, id).await?;
    let scope = ExecutionScope::institution(tenant, InstitutionId(cur.institution_id));
    require_institution_writable(state, &scope, InstitutionId(cur.institution_id)).await?;
    check_version(cur.version, expected)?;
    if cur.status != PolicyVersionStatus::Draft {
        return Err(conflict(format!(
            "policy version is {}; only drafts can be submitted",
            cur.status
        )));
    }
    let mut vars = BTreeMap::new();
    vars.insert("policyId".into(), json!(id));
    vars.insert("code".into(), json!(cur.code));
    let outcome = sutra_kernel::approval::with_maker(
        actor.id.as_deref(),
        request_approval(
            state,
            &scope,
            process::PROGRESSION_POLICY,
            business_key("progression_policy", id, cur.version),
            vars,
        ),
    )
    .await?;
    let pid = outcome.process_instance_id().to_owned();
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let p = policy_row(&mut *tx, tenant, id, true).await?;
    check_version(p.version, expected)?;
    let p = set_policy_status(&mut tx, &p, PolicyVersionStatus::InReview, None, Some(&pid)).await?;
    audit(
        &mut tx,
        &scope,
        "student.progression_policy.submit",
        "progression_policy",
        id,
        json!({"processInstanceId": pid}),
        actor,
    )
    .await?;
    let p = match outcome {
        ApprovalOutcome::Approved { .. } => {
            make_policy_effective(
                &mut tx,
                &scope,
                &p,
                Some(&pid),
                &Actor::system(actor.correlation_id.clone()),
            )
            .await?
        }
        ApprovalOutcome::Pending { .. } => p,
    };
    tx.commit().await.map_err(map_db_error)?;
    Ok(p)
}

/// Decision on a policy in review (approve → effective, reject → draft with reason).
pub async fn decide_policy(
    state: &StudentState,
    tenant: TenantId,
    id: Uuid,
    expected: Version,
    approve: bool,
    reason: Option<String>,
    actor: &Actor,
) -> Result<ProgressionPolicy, KernelError> {
    let cur = get_policy(&state.pool, tenant, id).await?;
    let scope = ExecutionScope::institution(tenant, InstitutionId(cur.institution_id));
    if !approve && reason.is_none() {
        return Err(invalid("reason", "required", "a rejection needs a reason"));
    }
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let p = policy_row(&mut *tx, tenant, id, true).await?;
    check_version(p.version, expected)?;
    if p.status != PolicyVersionStatus::InReview {
        return Err(conflict(format!(
            "policy version is {}; nothing to decide",
            p.status
        )));
    }
    let p = if approve {
        let pid = p.process_instance_id.clone();
        make_policy_effective(&mut tx, &scope, &p, pid.as_deref(), actor).await?
    } else {
        let p = set_policy_status(
            &mut tx,
            &p,
            PolicyVersionStatus::Draft,
            reason.as_deref(),
            None,
        )
        .await?;
        audit(
            &mut tx,
            &scope,
            "student.progression_policy.return",
            "progression_policy",
            id,
            json!({"reason": reason}),
            actor,
        )
        .await?;
        p
    };
    tx.commit().await.map_err(map_db_error)?;
    Ok(p)
}

/// Retires an effective version (or discards a draft).
pub async fn retire_policy(
    state: &StudentState,
    tenant: TenantId,
    id: Uuid,
    expected: Version,
    reason: Option<String>,
    actor: &Actor,
) -> Result<ProgressionPolicy, KernelError> {
    let cur = get_policy(&state.pool, tenant, id).await?;
    let scope = ExecutionScope::institution(tenant, InstitutionId(cur.institution_id));
    require_institution_writable(state, &scope, InstitutionId(cur.institution_id)).await?;
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let p = policy_row(&mut *tx, tenant, id, true).await?;
    check_version(p.version, expected)?;
    let from = p.status;
    let p = set_policy_status(
        &mut tx,
        &p,
        PolicyVersionStatus::Retired,
        reason.as_deref(),
        None,
    )
    .await?;
    if from == PolicyVersionStatus::Effective {
        emit(
            &mut tx,
            &scope,
            PROGRESSION_POLICY_RETIRED,
            PROGRESSION_POLICY_RETIRED_SCHEMA,
            id,
            &status_changed(
                id,
                &scope,
                None,
                Some(from.as_str()),
                "retired",
                p.version,
                reason.as_deref(),
                None,
                None,
            ),
            actor,
        )
        .await?;
    }
    audit(
        &mut tx,
        &scope,
        "student.progression_policy.retire",
        "progression_policy",
        id,
        json!({"from": from, "reason": reason}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(p)
}

// ================================================================== decisions

const DECISION_COLUMNS: &str = "id, tenant_id, institution_id, student_id, student_context_id, academic_term_id, \
    term_no, policy_id, outcome, recommended_outcome, evidence_source, credits_attempted::float8 AS credits_attempted, \
    credits_earned::float8 AS credits_earned, failed_component_ids, sgpa::float8 AS sgpa, cgpa::float8 AS cgpa, \
    evidence_note, repeat_unit, probation_review_due_on, status, status_reason, supersedes_decision_id, \
    resulting_term_no, process_instance_id, decided_by, effective_at, version, created_at, updated_at";

#[derive(sqlx::FromRow, Clone)]
struct DecisionRow {
    id: Uuid,
    tenant_id: Uuid,
    institution_id: Uuid,
    student_id: Uuid,
    student_context_id: Uuid,
    academic_term_id: Uuid,
    term_no: i32,
    policy_id: Option<Uuid>,
    outcome: String,
    recommended_outcome: Option<String>,
    evidence_source: String,
    credits_attempted: Option<f64>,
    credits_earned: f64,
    failed_component_ids: Vec<Uuid>,
    sgpa: Option<f64>,
    cgpa: Option<f64>,
    evidence_note: Option<String>,
    repeat_unit: Option<String>,
    probation_review_due_on: Option<NaiveDate>,
    status: String,
    status_reason: Option<String>,
    supersedes_decision_id: Option<Uuid>,
    resulting_term_no: Option<i32>,
    process_instance_id: Option<String>,
    decided_by: Option<String>,
    effective_at: Option<DateTime<Utc>>,
    version: i64,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl DecisionRow {
    fn status(&self) -> Result<ProgressionDecisionStatus, KernelError> {
        parse_enum(
            &self.status,
            ProgressionDecisionStatus::parse,
            "decision status",
        )
    }
    fn outcome(&self) -> Result<ProgressionOutcome, KernelError> {
        parse_enum(
            &self.outcome,
            ProgressionOutcome::parse,
            "progression outcome",
        )
    }
    fn dto(
        self,
        conditions: Vec<ProgressionCondition>,
    ) -> Result<ProgressionDecision, KernelError> {
        Ok(ProgressionDecision {
            status: self.status()?,
            outcome: self.outcome()?,
            recommended_outcome: self
                .recommended_outcome
                .as_deref()
                .map(|o| parse_enum(o, ProgressionOutcome::parse, "progression outcome"))
                .transpose()?,
            conditions,
            id: self.id,
            student_id: self.student_id,
            student_context_id: self.student_context_id,
            institution_id: self.institution_id,
            academic_term_id: self.academic_term_id,
            term_no: self.term_no,
            policy_id: self.policy_id,
            evidence_source: self.evidence_source,
            credits_attempted: self.credits_attempted,
            credits_earned: self.credits_earned,
            failed_component_ids: self.failed_component_ids,
            sgpa: self.sgpa,
            cgpa: self.cgpa,
            evidence_note: self.evidence_note,
            repeat_unit: self.repeat_unit,
            probation_review_due_on: self.probation_review_due_on,
            status_reason: self.status_reason,
            supersedes_decision_id: self.supersedes_decision_id,
            resulting_term_no: self.resulting_term_no,
            process_instance_id: self.process_instance_id,
            decided_by: self.decided_by,
            effective_at: self.effective_at,
            version: self.version,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}

async fn decision_row<'e, E: PgExecutor<'e>>(
    ex: E,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<DecisionRow, KernelError> {
    let sql = format!(
        "SELECT {DECISION_COLUMNS} FROM student.progression_decision WHERE id = $1 AND tenant_id = $2{}",
        if lock { " FOR UPDATE" } else { "" }
    );
    sqlx::query_as::<_, DecisionRow>(&sql)
        .bind(id)
        .bind(tenant.as_uuid())
        .fetch_optional(ex)
        .await
        .map_err(map_db_error)?
        .ok_or(DECISION)
}

const CONDITION_COLUMNS: &str = "id, decision_id, student_id, student_context_id, description, \
    curriculum_component_id, due_on, status, resolution_note, resolved_at, version";

#[derive(sqlx::FromRow)]
struct ConditionRow {
    id: Uuid,
    decision_id: Uuid,
    student_id: Uuid,
    student_context_id: Uuid,
    description: String,
    curriculum_component_id: Option<Uuid>,
    due_on: Option<NaiveDate>,
    status: String,
    resolution_note: Option<String>,
    resolved_at: Option<DateTime<Utc>>,
    version: i64,
}

impl ConditionRow {
    fn dto(self) -> Result<ProgressionCondition, KernelError> {
        Ok(ProgressionCondition {
            status: parse_enum(
                &self.status,
                ProgressionConditionStatus::parse,
                "condition status",
            )?,
            id: self.id,
            decision_id: self.decision_id,
            student_id: self.student_id,
            description: self.description,
            curriculum_component_id: self.curriculum_component_id,
            due_on: self.due_on,
            resolution_note: self.resolution_note,
            resolved_at: self.resolved_at,
            version: self.version,
        })
    }
}

async fn conditions_of<'e, E: PgExecutor<'e>>(
    ex: E,
    decision_id: Uuid,
) -> Result<Vec<ProgressionCondition>, KernelError> {
    let sql = format!(
        "SELECT {CONDITION_COLUMNS} FROM student.progression_condition WHERE decision_id = $1 ORDER BY created_at, id"
    );
    sqlx::query_as::<_, ConditionRow>(&sql)
        .bind(decision_id)
        .fetch_all(ex)
        .await
        .map_err(map_db_error)?
        .into_iter()
        .map(ConditionRow::dto)
        .collect()
}

async fn decision_dto(
    conn: &mut PgConnection,
    row: DecisionRow,
) -> Result<ProgressionDecision, KernelError> {
    let c = conditions_of(&mut *conn, row.id).await?;
    row.dto(c)
}

pub async fn get_decision(
    pool: &PgPool,
    tenant: TenantId,
    id: Uuid,
) -> Result<ProgressionDecision, KernelError> {
    let row = decision_row(pool, tenant, id, false).await?;
    let c = conditions_of(pool, id).await?;
    row.dto(c)
}

pub async fn list_decisions(
    pool: &PgPool,
    tenant: TenantId,
    student_id: StudentId,
    status: Option<ProgressionDecisionStatus>,
) -> Result<Vec<ProgressionDecision>, KernelError> {
    crate::shared::student::load_student(pool, tenant, student_id).await?;
    let sql = format!(
        "SELECT {DECISION_COLUMNS} FROM student.progression_decision WHERE tenant_id = $1 AND student_id = $2 \
         AND ($3::text IS NULL OR status = $3) ORDER BY created_at DESC, id DESC"
    );
    let rows = sqlx::query_as::<_, DecisionRow>(&sql)
        .bind(tenant.as_uuid())
        .bind(student_id.as_uuid())
        .bind(status.map(|s| s.as_str()))
        .fetch_all(pool)
        .await
        .map_err(map_db_error)?;
    let mut out = vec![];
    for r in rows {
        let c = conditions_of(pool, r.id).await?;
        out.push(r.dto(c)?);
    }
    Ok(out)
}

/// Evidence of a decision: Exams result when published, else entered by hand.
struct Evidence {
    source: &'static str,
    attempted: Option<f64>,
    earned: f64,
    failed: Vec<Uuid>,
    sgpa: Option<f64>,
    cgpa: Option<f64>,
}

fn check_credits(v: Option<f64>, field: &str) -> Result<(), KernelError> {
    if v.is_some_and(|c| !(0.0..=1000.0).contains(&c)) {
        return Err(invalid(field, "out_of_range", "0 … 1000"));
    }
    Ok(())
}

async fn term_count(
    state: &StudentState,
    scope: &ExecutionScope,
    ctx: &StudentContextRow,
) -> Result<i32, KernelError> {
    let Some(cv) = ctx.curriculum_version_id else {
        return Ok(i32::MAX);
    };
    Ok(state
        .references
        .curriculum_version(scope, cv)
        .await?
        .map_or(i32::MAX, |c| c.term_count))
}

async fn insert_conditions(
    conn: &mut PgConnection,
    ss: &StudentScope,
    decision_id: Uuid,
    conds: &[ConditionInput],
) -> Result<(), KernelError> {
    for (i, c) in conds.iter().enumerate() {
        let d = required(
            Some(c.description.clone()),
            &format!("conditions[{i}].description"),
            500,
        )?;
        sqlx::query(
            "INSERT INTO student.progression_condition (tenant_id, institution_id, student_id, student_context_id, \
             decision_id, description, curriculum_component_id, due_on) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(ss.scope.tenant_id.as_uuid())
        .bind(ss.context.institution_id.as_uuid())
        .bind(ss.student.id.as_uuid())
        .bind(ss.context.id.as_uuid())
        .bind(decision_id)
        .bind(d)
        .bind(c.curriculum_component_id)
        .bind(c.due_on)
        .execute(&mut *conn)
        .await
        .map_err(map_db_error)?;
    }
    Ok(())
}

fn check_outcome_inputs(
    outcome: ProgressionOutcome,
    conds: &[ConditionInput],
    term_no: i32,
    terms: i32,
) -> Result<(), KernelError> {
    if outcome == ProgressionOutcome::ConditionallyProgressed && conds.is_empty() {
        return Err(invalid(
            "conditions",
            "required",
            "a conditional promotion needs at least one condition",
        ));
    }
    if outcome != ProgressionOutcome::ConditionallyProgressed && !conds.is_empty() {
        return Err(invalid(
            "conditions",
            "not_applicable",
            "conditions apply to conditional promotions only",
        ));
    }
    if outcome == ProgressionOutcome::QualificationEligible && term_no < terms {
        return Err(invalid(
            "outcome",
            "not_final_term",
            format!("qualification_eligible needs the final term ({terms})"),
        ));
    }
    Ok(())
}

async fn active_probation(
    conn: &mut PgConnection,
    tenant: TenantId,
    ctx: Uuid,
) -> Result<Option<Uuid>, KernelError> {
    sqlx::query_scalar("SELECT id FROM student.probation_case WHERE tenant_id = $1 AND student_context_id = $2 AND status = 'active'")
        .bind(tenant.as_uuid())
        .bind(ctx)
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)
}

/// Drafts a progression decision (L3-C-027, C-031).
pub async fn create_decision(
    state: &StudentState,
    tenant: TenantId,
    student_id: StudentId,
    cmd: CreateDecision,
    actor: &Actor,
) -> Result<ProgressionDecision, KernelError> {
    let ss = write_student(state, tenant, student_id, true).await?;
    let term = state
        .references
        .academic_term(&ss.scope, cmd.academic_term_id)
        .await?
        .filter(|t| t.institution_id == ss.context.institution_id)
        .ok_or_else(|| {
            invalid(
                "academicTermId",
                "unknown",
                "no such term in the student's institution",
            )
        })?;
    check_credits(cmd.credits_attempted, "creditsAttempted")?;
    check_credits(cmd.credits_earned, "creditsEarned")?;
    check_repeat_unit(cmd.repeat_unit.as_deref())?;
    let published = state
        .results
        .term_result(
            &ss.scope,
            StudentTermResultQuery {
                institution_id: ss.context.institution_id,
                student_id: student_id.as_uuid(),
                academic_term_id: term.id,
            },
        )
        .await?;
    let ev = match published {
        Some(r) => Evidence {
            source: "results",
            attempted: Some(r.credits_attempted),
            earned: r.credits_earned,
            failed: r.failed_component_ids,
            sgpa: r.sgpa,
            cgpa: r.cgpa,
        },
        None => Evidence {
            source: "manual",
            attempted: cmd.credits_attempted,
            earned: cmd.credits_earned.ok_or_else(|| {
                invalid(
                    "creditsEarned",
                    "evidence_required",
                    "no published result: enter the credits earned",
                )
            })?,
            failed: cmd.failed_component_ids.clone(),
            sgpa: cmd.sgpa,
            cgpa: cmd.cgpa,
        },
    };
    let terms = term_count(state, &ss.scope, &ss.context).await?;
    let term_no = ss.context.current_term_no.unwrap_or(1);
    let standing = standing_of(&state.pool, tenant, ss.context.id.as_uuid()).await?;
    let today = business_today(state, &ss.scope, ss.context.institution_id).await?;
    let policy = effective_policy(&state.pool, tenant, &ss.context, today).await?;
    let recommended = policy.as_ref().map(|p| {
        recommend(
            &p.rules,
            ev.attempted,
            ev.earned,
            ev.failed.len(),
            standing == AcademicStanding::Probation,
            term_no >= terms,
        )
    });
    let outcome = cmd.outcome.or(recommended).ok_or_else(|| {
        invalid(
            "outcome",
            "required",
            "no effective progression policy: choose the outcome",
        )
    })?;
    check_outcome_inputs(outcome, &cmd.conditions, term_no, terms)?;
    let repeat_unit = match outcome {
        ProgressionOutcome::Detained | ProgressionOutcome::Repeat => {
            cmd.repeat_unit.clone().or_else(|| {
                policy
                    .as_ref()
                    .map(|p| p.rules.detention_repeat_unit.clone())
            })
        }
        _ => None,
    };
    let note = text(cmd.evidence_note.clone(), "evidenceNote", 2000)?;
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    lock_context(&mut tx, tenant, ss.context.id).await?;
    if outcome == ProgressionOutcome::Probation
        && active_probation(&mut tx, tenant, ss.context.id.as_uuid())
            .await?
            .is_some()
    {
        return Err(conflict(
            "the student is already on probation; resolve it or choose detention",
        ));
    }
    let effective: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM student.progression_decision WHERE tenant_id = $1 AND student_context_id = $2 \
         AND academic_term_id = $3 AND status = 'effective'",
    )
    .bind(tenant.as_uuid())
    .bind(ss.context.id.as_uuid())
    .bind(term.id)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    if effective > 0 {
        return Err(conflict(
            "the term already has an effective decision; revise it instead",
        ));
    }
    let row = insert_decision(
        &mut tx,
        &ss,
        term.id,
        term_no,
        policy.as_ref().map(|p| p.id),
        outcome,
        recommended,
        &ev,
        note,
        repeat_unit,
        cmd.probation_review_due_on,
        None,
        "draft",
        None,
    )
    .await?;
    insert_conditions(&mut tx, &ss, row.id, &cmd.conditions).await?;
    audit(&mut tx, &ss.scope, "student.progression_decision.create", "progression_decision", row.id,
        json!({"outcome": outcome, "recommended": recommended, "evidenceSource": ev.source, "creditsEarned": ev.earned}), actor).await?;
    let d = decision_dto(&mut tx, row).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(d)
}

#[allow(clippy::too_many_arguments)]
async fn insert_decision(
    conn: &mut PgConnection,
    ss: &StudentScope,
    term_id: Uuid,
    term_no: i32,
    policy_id: Option<Uuid>,
    outcome: ProgressionOutcome,
    recommended: Option<ProgressionOutcome>,
    ev: &Evidence,
    note: Option<String>,
    repeat_unit: Option<String>,
    review_due: Option<NaiveDate>,
    supersedes: Option<Uuid>,
    status: &str,
    pid: Option<&str>,
) -> Result<DecisionRow, KernelError> {
    let sql = format!(
        "INSERT INTO student.progression_decision (tenant_id, institution_id, student_id, student_context_id, \
         academic_term_id, term_no, policy_id, outcome, recommended_outcome, evidence_source, credits_attempted, \
         credits_earned, failed_component_ids, sgpa, cgpa, evidence_note, repeat_unit, probation_review_due_on, \
         supersedes_decision_id, status, process_instance_id) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20, $21) \
         RETURNING {DECISION_COLUMNS}"
    );
    sqlx::query_as::<_, DecisionRow>(&sql)
        .bind(ss.scope.tenant_id.as_uuid())
        .bind(ss.context.institution_id.as_uuid())
        .bind(ss.student.id.as_uuid())
        .bind(ss.context.id.as_uuid())
        .bind(term_id)
        .bind(term_no)
        .bind(policy_id)
        .bind(outcome.as_str())
        .bind(recommended.map(|o| o.as_str()))
        .bind(ev.source)
        .bind(ev.attempted)
        .bind(ev.earned)
        .bind(&ev.failed)
        .bind(ev.sgpa)
        .bind(ev.cgpa)
        .bind(note)
        .bind(repeat_unit)
        .bind(review_due)
        .bind(supersedes)
        .bind(status)
        .bind(pid)
        .fetch_one(&mut *conn)
        .await
        .map_err(|e| match map_db_error(e) {
            KernelError::Conflict(_) => {
                conflict("the term already has an open decision for this student")
            }
            other => other,
        })
}

fn decision_v1(d: &DecisionRow, subject: Option<String>) -> ProgressionDecisionV1 {
    ProgressionDecisionV1 {
        decision_id: d.id,
        student_id: d.student_id,
        student_context_id: d.student_context_id,
        institution_id: d.institution_id,
        academic_term_id: Some(d.academic_term_id),
        outcome: d.outcome.clone(),
        policy_id: d.policy_id,
        supersedes_decision_id: d.supersedes_decision_id,
        process_instance_id: d.process_instance_id.clone(),
        student_subject_ref: subject,
    }
}

/// Ledger total of a context.
pub async fn ledger_total<'e, E: PgExecutor<'e>>(
    ex: E,
    tenant: TenantId,
    ctx: Uuid,
) -> Result<f64, KernelError> {
    sqlx::query_scalar(
        "SELECT COALESCE(sum(CASE WHEN kind IN ('revoked', 'expired') THEN -credits ELSE credits END), 0)::float8 \
         FROM student.credit_ledger_entry WHERE tenant_id = $1 AND student_context_id = $2",
    )
    .bind(tenant.as_uuid())
    .bind(ctx)
    .fetch_one(ex)
    .await
    .map_err(map_db_error)
}

/// One new ledger entry.
pub(crate) struct NewEntry<'a> {
    pub kind: CreditEntryKind,
    pub credits: f64,
    pub component: Option<Uuid>,
    pub term: Option<Uuid>,
    pub source_type: &'a str,
    pub source_id: Option<Uuid>,
    pub reverses: Option<Uuid>,
    pub approved: bool,
    pub note: Option<&'a str>,
}

pub(crate) async fn append_entry(
    conn: &mut PgConnection,
    ctx: &StudentContextRow,
    e: &NewEntry<'_>,
    actor: &Actor,
) -> Result<Uuid, KernelError> {
    sqlx::query_scalar(
        "INSERT INTO student.credit_ledger_entry (tenant_id, institution_id, student_id, student_context_id, kind, \
         credits, curriculum_component_id, academic_term_id, source_type, source_id, reverses_entry_id, \
         institution_approved, note, recorded_by) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14) \
         RETURNING id",
    )
    .bind(ctx.tenant_id.as_uuid())
    .bind(ctx.institution_id.as_uuid())
    .bind(ctx.student_id.as_uuid())
    .bind(ctx.id.as_uuid())
    .bind(e.kind.as_str())
    .bind(e.credits)
    .bind(e.component)
    .bind(e.term)
    .bind(e.source_type)
    .bind(e.source_id)
    .bind(e.reverses)
    .bind(e.approved)
    .bind(e.note)
    .bind(actor.id.as_deref())
    .fetch_one(&mut *conn)
    .await
    .map_err(|e| match map_db_error(e) {
        KernelError::Conflict(_) => conflict("the entry has already been reversed"),
        other => other,
    })
}

/// Emits `credit_ledger.updated` for appended entries.
pub(crate) async fn ledger_updated(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    ctx: &StudentContextRow,
    entry_ids: Vec<Uuid>,
    actor: &Actor,
) -> Result<(), KernelError> {
    if entry_ids.is_empty() {
        return Ok(());
    }
    let total = ledger_total(&mut *conn, ctx.tenant_id, ctx.id.as_uuid()).await?;
    if total < -1e-9 {
        return Err(conflict("the credit ledger balance cannot become negative"));
    }
    emit(
        conn,
        scope,
        CREDIT_LEDGER_UPDATED,
        CREDIT_LEDGER_UPDATED_SCHEMA,
        ctx.student_id.as_uuid(),
        &CreditLedgerUpdatedV1 {
            student_id: ctx.student_id.as_uuid(),
            institution_id: ctx.institution_id.as_uuid(),
            entry_ids,
            total_credits: total,
        },
        actor,
    )
    .await
}

/// Reverses every not-yet-reversed earned entry a decision produced.
async fn reverse_decision_entries(
    conn: &mut PgConnection,
    ctx: &StudentContextRow,
    decision_id: Uuid,
    actor: &Actor,
) -> Result<Vec<Uuid>, KernelError> {
    let rows: Vec<(Uuid, f64, Option<Uuid>, Option<Uuid>)> = sqlx::query_as(
        "SELECT e.id, e.credits::float8, e.curriculum_component_id, e.academic_term_id FROM student.credit_ledger_entry e \
         WHERE e.tenant_id = $1 AND e.source_type = 'progression_decision' AND e.source_id = $2 AND e.kind = 'earned' \
         AND NOT EXISTS (SELECT 1 FROM student.credit_ledger_entry r WHERE r.reverses_entry_id = e.id)",
    )
    .bind(ctx.tenant_id.as_uuid())
    .bind(decision_id)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let mut ids = vec![];
    for (id, credits, comp, term) in rows {
        ids.push(
            append_entry(
                conn,
                ctx,
                &NewEntry {
                    kind: CreditEntryKind::Revoked,
                    credits,
                    component: comp,
                    term,
                    source_type: "progression_decision",
                    source_id: Some(decision_id),
                    reverses: Some(id),
                    approved: true,
                    note: Some("superseded by a progression decision revision"),
                },
                actor,
            )
            .await?,
        );
    }
    Ok(ids)
}

/// Earned entries for an effective decision: per registered component of the term's finalised
/// registration that did not fail; without a registration one entry with the credits earned.
async fn earn_entries(
    conn: &mut PgConnection,
    ctx: &StudentContextRow,
    d: &DecisionRow,
    actor: &Actor,
) -> Result<Vec<Uuid>, KernelError> {
    let lines: Vec<(Uuid, f64)> = sqlx::query_as(
        "SELECT l.curriculum_component_id, l.credits::float8 FROM student.registration_line l \
         JOIN student.registration r ON r.id = l.registration_id WHERE r.tenant_id = $1 AND r.student_context_id = $2 \
         AND r.academic_term_id = $3 AND r.status = 'finalised' AND l.status = 'registered' AND l.kind <> 'audit' \
         ORDER BY l.component_term_no, l.slot_code",
    )
    .bind(ctx.tenant_id.as_uuid())
    .bind(ctx.id.as_uuid())
    .bind(d.academic_term_id)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let mut ids = vec![];
    let entry = |comp: Option<Uuid>, credits: f64| NewEntry {
        kind: CreditEntryKind::Earned,
        credits,
        component: comp,
        term: Some(d.academic_term_id),
        source_type: "progression_decision",
        source_id: Some(d.id),
        reverses: None,
        approved: true,
        note: None,
    };
    if lines.is_empty() {
        if d.credits_earned > 0.0 {
            ids.push(append_entry(conn, ctx, &entry(None, d.credits_earned), actor).await?);
        }
    } else {
        for (comp, credits) in lines {
            if d.failed_component_ids.contains(&comp) || credits <= 0.0 {
                continue;
            }
            ids.push(append_entry(conn, ctx, &entry(Some(comp), credits), actor).await?);
        }
    }
    Ok(ids)
}

/// Makes a decision effective inside the tx (superseding its predecessor for a revision).
async fn apply_decision(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    row: DecisionRow,
    terms: i32,
    subject: Option<String>,
    actor: &Actor,
) -> Result<DecisionRow, KernelError> {
    let tenant = TenantId(row.tenant_id);
    let ctx = lock_context(
        conn,
        tenant,
        sutra_contracts::student::ids::StudentContextId(row.student_context_id),
    )
    .await?;
    crate::shared::guard::require_live_context(&ctx)?;
    let mut ledger_ids = vec![];
    let expected_term = if let Some(prev_id) = row.supersedes_decision_id {
        let prev = decision_row(&mut *conn, tenant, prev_id, true).await?;
        if prev.status()? != ProgressionDecisionStatus::Effective {
            return Err(conflict(
                "the decision being revised is no longer effective",
            ));
        }
        let expected = prev.resulting_term_no.unwrap_or(prev.term_no);
        sqlx::query(
            "UPDATE student.progression_decision SET status = 'superseded', status_reason = 'superseded by a revision', \
             version = version + 1, updated_at = now() WHERE id = $1",
        )
        .bind(prev.id)
        .execute(&mut *conn)
        .await
        .map_err(map_db_error)?;
        ledger_ids.extend(reverse_decision_entries(conn, &ctx, prev.id, actor).await?);
        sqlx::query(
            "UPDATE student.probation_case SET status = 'cleared', resolution_note = 'superseded by a decision revision', \
             resolved_at = now(), resolved_by = $2, version = version + 1, updated_at = now() \
             WHERE decision_id = $1 AND status = 'active'",
        )
        .bind(prev.id)
        .bind(actor.id.as_deref())
        .execute(&mut *conn)
        .await
        .map_err(map_db_error)?;
        sqlx::query(
            "UPDATE student.detention_case SET released_at = now(), release_note = 'superseded by a decision revision', \
             version = version + 1, updated_at = now() WHERE decision_id = $1 AND released_at IS NULL",
        )
        .bind(prev.id)
        .execute(&mut *conn)
        .await
        .map_err(map_db_error)?;
        expected
    } else {
        row.term_no
    };
    let current = ctx.current_term_no.unwrap_or(1);
    if current != expected_term {
        return Err(conflict(format!(
            "the student is in term {current}; the decision applies to term {expected_term}"
        )));
    }
    let outcome = row.outcome()?;
    let new_term = next_term_no(row.term_no, outcome, row.repeat_unit.as_deref(), terms);
    let ctx = if new_term != current {
        set_context_progress(
            conn,
            &ctx,
            Some(new_term),
            ctx.programme_id,
            ctx.curriculum_version_id,
        )
        .await?
    } else {
        ctx
    };
    ledger_ids.extend(earn_entries(conn, &ctx, &row, actor).await?);
    set_standing(
        conn,
        &ctx,
        standing_for(outcome),
        "progression_decision",
        row.id,
    )
    .await?;
    match outcome {
        ProgressionOutcome::Probation => {
            if active_probation(conn, tenant, ctx.id.as_uuid())
                .await?
                .is_some()
            {
                return Err(conflict("the student is already on probation"));
            }
            sqlx::query(
                "INSERT INTO student.probation_case (tenant_id, institution_id, student_id, student_context_id, \
                 decision_id, review_due_on) VALUES ($1, $2, $3, $4, $5, $6)",
            )
            .bind(row.tenant_id)
            .bind(row.institution_id)
            .bind(row.student_id)
            .bind(row.student_context_id)
            .bind(row.id)
            .bind(row.probation_review_due_on)
            .execute(&mut *conn)
            .await
            .map_err(map_db_error)?;
        }
        ProgressionOutcome::Detained | ProgressionOutcome::Repeat => {
            sqlx::query(
                "INSERT INTO student.detention_case (tenant_id, institution_id, student_id, student_context_id, \
                 decision_id, repeat_unit, repeat_term_no, determined_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, CASE WHEN $6::text IS NULL THEN NULL ELSE now() END)",
            )
            .bind(row.tenant_id)
            .bind(row.institution_id)
            .bind(row.student_id)
            .bind(row.student_context_id)
            .bind(row.id)
            .bind(row.repeat_unit.as_deref())
            .bind(row.repeat_unit.as_ref().map(|_| new_term))
            .execute(&mut *conn)
            .await
            .map_err(map_db_error)?;
        }
        _ => {}
    }
    let sql = format!(
        "UPDATE student.progression_decision SET status = 'effective', resulting_term_no = $2, effective_at = now(), \
         decided_by = $3, version = version + 1, updated_at = now() WHERE id = $1 RETURNING {DECISION_COLUMNS}"
    );
    let d = sqlx::query_as::<_, DecisionRow>(&sql)
        .bind(row.id)
        .bind(new_term)
        .bind(actor.id.as_deref())
        .fetch_one(&mut *conn)
        .await
        .map_err(map_db_error)?;
    ledger_updated(conn, scope, &ctx, ledger_ids, actor).await?;
    let payload = decision_v1(&d, subject);
    emit(
        conn,
        scope,
        PROGRESSION_EVALUATED,
        PROGRESSION_EVALUATED_SCHEMA,
        d.id,
        &payload,
        actor,
    )
    .await?;
    let specific = match outcome {
        ProgressionOutcome::Progressed => Some((
            PROGRESSION_STUDENT_PROGRESSED,
            PROGRESSION_STUDENT_PROGRESSED_SCHEMA,
        )),
        ProgressionOutcome::ConditionallyProgressed => Some((
            PROGRESSION_CONDITIONAL_PROMOTION_GRANTED,
            PROGRESSION_CONDITIONAL_PROMOTION_GRANTED_SCHEMA,
        )),
        ProgressionOutcome::Probation => Some((
            PROGRESSION_ACADEMIC_PROBATION_ASSIGNED,
            PROGRESSION_ACADEMIC_PROBATION_ASSIGNED_SCHEMA,
        )),
        ProgressionOutcome::Detained | ProgressionOutcome::Repeat => Some((
            PROGRESSION_ACADEMIC_DETENTION_APPLIED,
            PROGRESSION_ACADEMIC_DETENTION_APPLIED_SCHEMA,
        )),
        ProgressionOutcome::QualificationEligible => None,
    };
    if let Some((t, s)) = specific {
        emit(conn, scope, t, s, d.id, &payload, actor).await?;
    }
    if d.supersedes_decision_id.is_some() {
        emit(
            conn,
            scope,
            PROGRESSION_DECISION_REVISED,
            PROGRESSION_DECISION_REVISED_SCHEMA,
            d.id,
            &payload,
            actor,
        )
        .await?;
    }
    audit(conn, scope, "student.progression_decision.apply", "progression_decision", d.id,
        json!({"outcome": outcome, "resultingTermNo": new_term, "supersedes": d.supersedes_decision_id}), actor).await?;
    Ok(d)
}

async fn decision_scope(
    state: &StudentState,
    tenant: TenantId,
    d: &DecisionRow,
) -> Result<(StudentScope, i32), KernelError> {
    let ss = write_student(state, tenant, StudentId(d.student_id), false).await?;
    if ss.context.id.as_uuid() != d.student_context_id {
        return Err(conflict("the decision belongs to a closed student context"));
    }
    let terms = term_count(state, &ss.scope, &ss.context).await?;
    Ok((ss, terms))
}

/// Submits a draft for approval (WorkflowPort `student.progression_decision`).
pub async fn submit_decision(
    state: &StudentState,
    tenant: TenantId,
    id: Uuid,
    expected: Version,
    actor: &Actor,
) -> Result<ProgressionDecision, KernelError> {
    let cur = decision_row(&state.pool, tenant, id, false).await?;
    let (ss, terms) = decision_scope(state, tenant, &cur).await?;
    check_version(cur.version, expected)?;
    if cur.status()? != ProgressionDecisionStatus::Draft {
        return Err(conflict(format!(
            "decision is {}; only drafts can be submitted",
            cur.status
        )));
    }
    let mut vars = BTreeMap::new();
    vars.insert("decisionId".into(), json!(id));
    vars.insert("outcome".into(), json!(cur.outcome));
    let outcome = sutra_kernel::approval::with_maker(
        actor.id.as_deref(),
        request_approval(
            state,
            &ss.scope,
            process::PROGRESSION_DECISION,
            business_key("progression_decision", id, cur.version),
            vars,
        ),
    )
    .await?;
    let pid = outcome.process_instance_id().to_owned();
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let row = decision_row(&mut *tx, tenant, id, true).await?;
    check_version(row.version, expected)?;
    let sql = format!(
        "UPDATE student.progression_decision SET status = 'pending_approval', process_instance_id = $2, \
         version = version + 1, updated_at = now() WHERE id = $1 AND status = 'draft' RETURNING {DECISION_COLUMNS}"
    );
    let row = sqlx::query_as::<_, DecisionRow>(&sql)
        .bind(id)
        .bind(&pid)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_db_error)?
        .ok_or_else(|| conflict("the decision is no longer a draft"))?;
    audit(
        &mut tx,
        &ss.scope,
        "student.progression_decision.submit",
        "progression_decision",
        id,
        json!({"processInstanceId": pid}),
        actor,
    )
    .await?;
    let row = match outcome {
        ApprovalOutcome::Approved { .. } => {
            apply_decision(
                &mut tx,
                &ss.scope,
                row,
                terms,
                ss.subject_ref(),
                &Actor::system(actor.correlation_id.clone()),
            )
            .await?
        }
        ApprovalOutcome::Pending { .. } => row,
    };
    let d = decision_dto(&mut tx, row).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(d)
}

/// Explicit decision on a pending progression decision (approve → effective, reject → rejected).
#[allow(clippy::too_many_arguments)]
pub async fn decide_decision(
    state: &StudentState,
    tenant: TenantId,
    id: Uuid,
    expected: Version,
    approve: bool,
    reason: Option<String>,
    actor: &Actor,
) -> Result<ProgressionDecision, KernelError> {
    let cur = decision_row(&state.pool, tenant, id, false).await?;
    let (ss, terms) = decision_scope(state, tenant, &cur).await?;
    if !approve && reason.is_none() {
        return Err(invalid("reason", "required", "a rejection needs a reason"));
    }
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let row = decision_row(&mut *tx, tenant, id, true).await?;
    check_version(row.version, expected)?;
    let st = row.status()?;
    let to = if approve {
        ProgressionDecisionStatus::Effective
    } else {
        ProgressionDecisionStatus::Rejected
    };
    if st != ProgressionDecisionStatus::PendingApproval || !decision_can_transition(st, to) {
        return Err(conflict(format!(
            "decision is {st}; there is no pending approval"
        )));
    }
    let row = if approve {
        apply_decision(&mut tx, &ss.scope, row, terms, ss.subject_ref(), actor).await?
    } else {
        let sql = format!(
            "UPDATE student.progression_decision SET status = 'rejected', status_reason = $2, decided_by = $3, \
             version = version + 1, updated_at = now() WHERE id = $1 RETURNING {DECISION_COLUMNS}"
        );
        let r = sqlx::query_as::<_, DecisionRow>(&sql)
            .bind(id)
            .bind(reason.as_deref())
            .bind(actor.id.as_deref())
            .fetch_one(&mut *tx)
            .await
            .map_err(map_db_error)?;
        audit(
            &mut tx,
            &ss.scope,
            "student.progression_decision.reject",
            "progression_decision",
            id,
            json!({"reason": reason}),
            actor,
        )
        .await?;
        r
    };
    let d = decision_dto(&mut tx, row).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(d)
}

/// Discards a draft decision.
pub async fn discard_decision(
    state: &StudentState,
    tenant: TenantId,
    id: Uuid,
    expected: Version,
    reason: Option<String>,
    actor: &Actor,
) -> Result<ProgressionDecision, KernelError> {
    let cur = decision_row(&state.pool, tenant, id, false).await?;
    let (ss, _) = decision_scope(state, tenant, &cur).await?;
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let row = decision_row(&mut *tx, tenant, id, true).await?;
    check_version(row.version, expected)?;
    if row.status()? != ProgressionDecisionStatus::Draft {
        return Err(conflict(format!(
            "decision is {}; only drafts can be discarded",
            row.status
        )));
    }
    let sql = format!(
        "UPDATE student.progression_decision SET status = 'rejected', status_reason = $2, version = version + 1, \
         updated_at = now() WHERE id = $1 RETURNING {DECISION_COLUMNS}"
    );
    let r = sqlx::query_as::<_, DecisionRow>(&sql)
        .bind(id)
        .bind(reason.as_deref().unwrap_or("discarded"))
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?;
    audit(
        &mut tx,
        &ss.scope,
        "student.progression_decision.discard",
        "progression_decision",
        id,
        json!({"reason": reason}),
        actor,
    )
    .await?;
    let d = decision_dto(&mut tx, r).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(d)
}

// ================================================================== revisions (L3-C-038)

const REVISION_COLUMNS: &str =
    "id, original_decision_id, revised_decision_id, trigger, same_outcome, evaluated_outcome, \
    reason, evidence_reference, actor, created_at";

#[derive(sqlx::FromRow)]
struct RevisionRow {
    id: Uuid,
    original_decision_id: Uuid,
    revised_decision_id: Option<Uuid>,
    trigger: String,
    same_outcome: bool,
    evaluated_outcome: String,
    reason: String,
    evidence_reference: Option<String>,
    actor: Option<String>,
    created_at: DateTime<Utc>,
}

impl RevisionRow {
    fn dto(self) -> Result<ProgressionRevision, KernelError> {
        Ok(ProgressionRevision {
            evaluated_outcome: parse_enum(
                &self.evaluated_outcome,
                ProgressionOutcome::parse,
                "outcome",
            )?,
            id: self.id,
            original_decision_id: self.original_decision_id,
            revised_decision_id: self.revised_decision_id,
            trigger: self.trigger,
            same_outcome: self.same_outcome,
            reason: self.reason,
            evidence_reference: self.evidence_reference,
            actor: self.actor,
            created_at: self.created_at,
        })
    }
}

pub async fn list_revisions(
    pool: &PgPool,
    tenant: TenantId,
    decision_id: Uuid,
) -> Result<Vec<ProgressionRevision>, KernelError> {
    decision_row(pool, tenant, decision_id, false).await?;
    let sql = format!(
        "SELECT {REVISION_COLUMNS} FROM student.progression_revision WHERE tenant_id = $1 \
         AND (original_decision_id = $2 OR revised_decision_id = $2) ORDER BY created_at, id"
    );
    sqlx::query_as::<_, RevisionRow>(&sql)
        .bind(tenant.as_uuid())
        .bind(decision_id)
        .fetch_all(pool)
        .await
        .map_err(map_db_error)?
        .into_iter()
        .map(RevisionRow::dto)
        .collect()
}

/// Fresh evaluation of an effective decision with changed evidence (L3-C-038). The prior
/// decision is never mutated: a different outcome creates a superseding decision (approval
/// `student.progression_revision`); the same outcome is recorded as a revision only.
pub async fn revise_decision(
    state: &StudentState,
    tenant: TenantId,
    id: Uuid,
    cmd: CreateRevision,
    actor: &Actor,
) -> Result<RevisionResult, KernelError> {
    let old = decision_row(&state.pool, tenant, id, false).await?;
    let (ss, terms) = decision_scope(state, tenant, &old).await?;
    if old.status()? != ProgressionDecisionStatus::Effective {
        return Err(conflict(format!(
            "decision is {}; only an effective decision can be re-evaluated",
            old.status
        )));
    }
    let reason = required(Some(cmd.reason.clone()), "reason", 1000)?;
    let evidence_reference = text(cmd.evidence_reference.clone(), "evidenceReference", 200)?;
    check_credits(cmd.credits_attempted, "creditsAttempted")?;
    check_credits(cmd.credits_earned, "creditsEarned")?;
    check_repeat_unit(cmd.repeat_unit.as_deref())?;
    let policy = match old.policy_id {
        Some(p) => Some(get_policy(&state.pool, tenant, p).await?),
        None => None,
    };
    if policy.as_ref().is_some_and(|p| !p.rules.allow_revision) {
        return Err(conflict(
            "the governing progression policy does not permit fresh evaluation",
        ));
    }
    if cmd.credits_earned.is_none()
        && cmd.credits_attempted.is_none()
        && cmd.failed_component_ids.is_none()
        && cmd.outcome.is_none()
    {
        return Err(invalid(
            "creditsEarned",
            "evidence_required",
            "give the changed evidence",
        ));
    }
    let ev = Evidence {
        source: "manual",
        attempted: cmd.credits_attempted.or(old.credits_attempted),
        earned: cmd.credits_earned.unwrap_or(old.credits_earned),
        failed: cmd
            .failed_component_ids
            .clone()
            .unwrap_or_else(|| old.failed_component_ids.clone()),
        sgpa: old.sgpa,
        cgpa: old.cgpa,
    };
    // Standing before the decision being revised (probation escalation must not see its own outcome).
    let prior_probation = sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM student.probation_case WHERE tenant_id = $1 AND student_context_id = $2 \
         AND decision_id <> $3 AND status IN ('active', 'escalated')",
    )
    .bind(tenant.as_uuid())
    .bind(old.student_context_id)
    .bind(old.id)
    .fetch_one(&state.pool)
    .await
    .map_err(map_db_error)?
        > 0;
    let recommended = policy.as_ref().map(|p| {
        recommend(
            &p.rules,
            ev.attempted,
            ev.earned,
            ev.failed.len(),
            prior_probation,
            old.term_no >= terms,
        )
    });
    let old_outcome = old.outcome()?;
    let outcome = cmd.outcome.or(recommended).unwrap_or(old_outcome);
    let same_evidence = (ev.earned - old.credits_earned).abs() < 1e-9 && {
        let mut a = ev.failed.clone();
        let mut b = old.failed_component_ids.clone();
        a.sort();
        b.sort();
        a == b
    };
    let same = outcome == old_outcome && same_evidence;
    let mut tx_conds = cmd.conditions.clone();
    if same {
        let mut tx = state.pool.begin().await.map_err(map_db_error)?;
        let rev = insert_revision(
            &mut tx,
            &ss,
            old.id,
            None,
            cmd.trigger,
            true,
            outcome,
            &reason,
            evidence_reference.as_deref(),
            actor,
        )
        .await?;
        let payload = decision_v1(&old, ss.subject_ref());
        emit(
            &mut tx,
            &ss.scope,
            PROGRESSION_EVALUATED,
            PROGRESSION_EVALUATED_SCHEMA,
            old.id,
            &payload,
            actor,
        )
        .await?;
        audit(
            &mut tx,
            &ss.scope,
            "student.progression_decision.reevaluate",
            "progression_decision",
            old.id,
            json!({"trigger": cmd.trigger, "sameOutcome": true, "revisionId": rev.id}),
            actor,
        )
        .await?;
        let d = decision_dto(&mut tx, old).await?;
        tx.commit().await.map_err(map_db_error)?;
        return Ok(RevisionResult {
            revision: rev,
            decision: d,
        });
    }
    if outcome != ProgressionOutcome::ConditionallyProgressed {
        tx_conds.clear();
    }
    check_outcome_inputs(outcome, &tx_conds, old.term_no, terms)?;
    let mut vars = BTreeMap::new();
    vars.insert("decisionId".into(), json!(id));
    vars.insert("outcome".into(), json!(outcome));
    let approval = request_approval(
        state,
        &ss.scope,
        process::PROGRESSION_REVISION,
        business_key("progression_revision", id, old.version),
        vars,
    )
    .await?;
    let pid = approval.process_instance_id().to_owned();
    let repeat_unit = match outcome {
        ProgressionOutcome::Detained | ProgressionOutcome::Repeat => {
            cmd.repeat_unit.clone().or_else(|| {
                policy
                    .as_ref()
                    .map(|p| p.rules.detention_repeat_unit.clone())
            })
        }
        _ => None,
    };
    let note = text(cmd.evidence_note.clone(), "evidenceNote", 2000)?;
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    lock_context(&mut tx, tenant, ss.context.id).await?;
    let new_row = insert_decision(
        &mut tx,
        &ss,
        old.academic_term_id,
        old.term_no,
        old.policy_id,
        outcome,
        recommended,
        &ev,
        note,
        repeat_unit,
        None,
        Some(old.id),
        "pending_approval",
        Some(&pid),
    )
    .await?;
    insert_conditions(&mut tx, &ss, new_row.id, &tx_conds).await?;
    let rev = insert_revision(
        &mut tx,
        &ss,
        old.id,
        Some(new_row.id),
        cmd.trigger,
        false,
        outcome,
        &reason,
        evidence_reference.as_deref(),
        actor,
    )
    .await?;
    audit(&mut tx, &ss.scope, "student.progression_decision.revise", "progression_decision", new_row.id,
        json!({"trigger": cmd.trigger, "supersedes": old.id, "outcome": outcome, "processInstanceId": pid}), actor).await?;
    let row = match approval {
        ApprovalOutcome::Approved { .. } => {
            apply_decision(
                &mut tx,
                &ss.scope,
                new_row,
                terms,
                ss.subject_ref(),
                &Actor::system(actor.correlation_id.clone()),
            )
            .await?
        }
        ApprovalOutcome::Pending { .. } => new_row,
    };
    let d = decision_dto(&mut tx, row).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(RevisionResult {
        revision: rev,
        decision: d,
    })
}

#[allow(clippy::too_many_arguments)]
async fn insert_revision(
    conn: &mut PgConnection,
    ss: &StudentScope,
    original: Uuid,
    revised: Option<Uuid>,
    trigger: RevisionTrigger,
    same: bool,
    outcome: ProgressionOutcome,
    reason: &str,
    evidence_reference: Option<&str>,
    actor: &Actor,
) -> Result<ProgressionRevision, KernelError> {
    let sql = format!(
        "INSERT INTO student.progression_revision (tenant_id, institution_id, student_id, original_decision_id, \
         revised_decision_id, trigger, same_outcome, evaluated_outcome, reason, evidence_reference, actor) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) RETURNING {REVISION_COLUMNS}"
    );
    sqlx::query_as::<_, RevisionRow>(&sql)
        .bind(ss.scope.tenant_id.as_uuid())
        .bind(ss.context.institution_id.as_uuid())
        .bind(ss.student.id.as_uuid())
        .bind(original)
        .bind(revised)
        .bind(trigger.as_str())
        .bind(same)
        .bind(outcome.as_str())
        .bind(reason)
        .bind(evidence_reference)
        .bind(actor.id.as_deref())
        .fetch_one(&mut *conn)
        .await
        .map_err(map_db_error)?
        .dto()
}

// ================================================================== conditions (L3-C-029)

async fn condition_row<'e, E: PgExecutor<'e>>(
    ex: E,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<ConditionRow, KernelError> {
    let sql = format!(
        "SELECT {CONDITION_COLUMNS} FROM student.progression_condition WHERE id = $1 AND tenant_id = $2{}",
        if lock { " FOR UPDATE" } else { "" }
    );
    sqlx::query_as::<_, ConditionRow>(&sql)
        .bind(id)
        .bind(tenant.as_uuid())
        .fetch_optional(ex)
        .await
        .map_err(map_db_error)?
        .ok_or(CONDITION)
}

pub async fn get_condition(
    pool: &PgPool,
    tenant: TenantId,
    id: Uuid,
) -> Result<ProgressionCondition, KernelError> {
    condition_row(pool, tenant, id, false).await?.dto()
}

/// Resolves a condition of an effective conditional promotion. All fulfilled/waived → good
/// standing; a failed condition places the student on probation (L3-C-029 failure path).
pub async fn resolve_condition(
    state: &StudentState,
    tenant: TenantId,
    id: Uuid,
    expected: Version,
    to: ProgressionConditionStatus,
    note: Option<String>,
    actor: &Actor,
) -> Result<ProgressionCondition, KernelError> {
    let cur = condition_row(&state.pool, tenant, id, false).await?;
    let ss = write_student(state, tenant, StudentId(cur.student_id), false).await?;
    if ss.context.id.as_uuid() != cur.student_context_id {
        return Err(conflict(
            "the condition belongs to a closed student context",
        ));
    }
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let ctx = lock_context(&mut tx, tenant, ss.context.id).await?;
    let c = condition_row(&mut *tx, tenant, id, true).await?;
    check_version(c.version, expected)?;
    let from = parse_enum(
        &c.status,
        ProgressionConditionStatus::parse,
        "condition status",
    )?;
    if !condition_can_transition(from, to) {
        return Err(conflict(format!(
            "condition is {from}; it cannot become {to}"
        )));
    }
    let d = decision_row(&mut *tx, tenant, c.decision_id, false).await?;
    if d.status()? != ProgressionDecisionStatus::Effective {
        return Err(conflict("the conditional promotion is not effective"));
    }
    let sql = format!(
        "UPDATE student.progression_condition SET status = $2, resolution_note = $3, resolved_at = now(), resolved_by = $4, \
         version = version + 1, updated_at = now() WHERE id = $1 RETURNING {CONDITION_COLUMNS}"
    );
    let c2 = sqlx::query_as::<_, ConditionRow>(&sql)
        .bind(id)
        .bind(to.as_str())
        .bind(note.as_deref())
        .bind(actor.id.as_deref())
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?
        .dto()?;
    let payload = status_changed(
        id,
        &ss.scope,
        Some(c2.student_id),
        Some(from.as_str()),
        to.as_str(),
        c2.version,
        note.as_deref(),
        None,
        ss.subject_ref(),
    );
    match to {
        ProgressionConditionStatus::Fulfilled => {
            emit(
                &mut tx,
                &ss.scope,
                PROGRESSION_CONDITION_FULFILLED,
                PROGRESSION_CONDITION_FULFILLED_SCHEMA,
                id,
                &payload,
                actor,
            )
            .await?
        }
        ProgressionConditionStatus::Failed => {
            emit(
                &mut tx,
                &ss.scope,
                PROGRESSION_CONDITION_FAILED,
                PROGRESSION_CONDITION_FAILED_SCHEMA,
                id,
                &payload,
                actor,
            )
            .await?
        }
        _ => {}
    }
    if to == ProgressionConditionStatus::Failed {
        if active_probation(&mut tx, tenant, ctx.id.as_uuid())
            .await?
            .is_none()
        {
            sqlx::query(
                "INSERT INTO student.probation_case (tenant_id, institution_id, student_id, student_context_id, decision_id) \
                 VALUES ($1, $2, $3, $4, $5)",
            )
            .bind(tenant.as_uuid())
            .bind(ctx.institution_id.as_uuid())
            .bind(ctx.student_id.as_uuid())
            .bind(ctx.id.as_uuid())
            .bind(d.id)
            .execute(&mut *tx)
            .await
            .map_err(map_db_error)?;
        }
        set_standing(
            &mut tx,
            &ctx,
            AcademicStanding::Probation,
            "progression_condition",
            id,
        )
        .await?;
        emit(
            &mut tx,
            &ss.scope,
            PROGRESSION_ACADEMIC_PROBATION_ASSIGNED,
            PROGRESSION_ACADEMIC_PROBATION_ASSIGNED_SCHEMA,
            d.id,
            &decision_v1(&d, ss.subject_ref()),
            actor,
        )
        .await?;
    } else {
        let open: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM student.progression_condition WHERE decision_id = $1 AND status IN ('pending', 'failed')",
        )
        .bind(d.id)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?;
        let standing = standing_of(&mut *tx, tenant, ctx.id.as_uuid()).await?;
        if open == 0 && standing == AcademicStanding::Conditional {
            set_standing(
                &mut tx,
                &ctx,
                AcademicStanding::GoodStanding,
                "progression_condition",
                id,
            )
            .await?;
        }
    }
    audit(
        &mut tx,
        &ss.scope,
        &format!(
            "student.progression_condition.{}",
            match to {
                ProgressionConditionStatus::Fulfilled => "fulfil",
                ProgressionConditionStatus::Failed => "fail",
                _ => "waive",
            }
        ),
        "progression_condition",
        id,
        json!({"note": note}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(c2)
}

// ================================================================== probation (L3-C-030)

const PROBATION_COLUMNS: &str =
    "id, student_id, student_context_id, decision_id, review_due_on, status, resolution_note, \
    resolved_at, version, created_at";

#[derive(sqlx::FromRow)]
struct ProbationRow {
    id: Uuid,
    student_id: Uuid,
    student_context_id: Uuid,
    decision_id: Uuid,
    review_due_on: Option<NaiveDate>,
    status: String,
    resolution_note: Option<String>,
    resolved_at: Option<DateTime<Utc>>,
    version: i64,
    created_at: DateTime<Utc>,
}

impl ProbationRow {
    fn dto(self) -> Result<ProbationCase, KernelError> {
        Ok(ProbationCase {
            status: parse_enum(&self.status, ProbationStatus::parse, "probation status")?,
            id: self.id,
            student_id: self.student_id,
            decision_id: self.decision_id,
            review_due_on: self.review_due_on,
            resolution_note: self.resolution_note,
            resolved_at: self.resolved_at,
            version: self.version,
            created_at: self.created_at,
        })
    }
}

async fn probation_row<'e, E: PgExecutor<'e>>(
    ex: E,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<ProbationRow, KernelError> {
    let sql = format!(
        "SELECT {PROBATION_COLUMNS} FROM student.probation_case WHERE id = $1 AND tenant_id = $2{}",
        if lock { " FOR UPDATE" } else { "" }
    );
    sqlx::query_as::<_, ProbationRow>(&sql)
        .bind(id)
        .bind(tenant.as_uuid())
        .fetch_optional(ex)
        .await
        .map_err(map_db_error)?
        .ok_or(PROBATION)
}

pub async fn get_probation(
    pool: &PgPool,
    tenant: TenantId,
    id: Uuid,
) -> Result<ProbationCase, KernelError> {
    probation_row(pool, tenant, id, false).await?.dto()
}

/// Clears (→ good standing) or escalates (standing stays probation; the policy's next
/// evaluation escalates to detention) an active probation.
pub async fn resolve_probation(
    state: &StudentState,
    tenant: TenantId,
    id: Uuid,
    expected: Version,
    to: ProbationStatus,
    note: String,
    actor: &Actor,
) -> Result<ProbationCase, KernelError> {
    let cur = probation_row(&state.pool, tenant, id, false).await?;
    let ss = write_student(state, tenant, StudentId(cur.student_id), false).await?;
    if ss.context.id.as_uuid() != cur.student_context_id {
        return Err(conflict(
            "the probation belongs to a closed student context",
        ));
    }
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let ctx = lock_context(&mut tx, tenant, ss.context.id).await?;
    let p = probation_row(&mut *tx, tenant, id, true).await?;
    check_version(p.version, expected)?;
    let from = parse_enum(&p.status, ProbationStatus::parse, "probation status")?;
    if !probation_can_transition(from, to) {
        return Err(conflict(format!(
            "probation is {from}; it cannot become {to}"
        )));
    }
    let sql = format!(
        "UPDATE student.probation_case SET status = $2, resolution_note = $3, resolved_at = now(), resolved_by = $4, \
         version = version + 1, updated_at = now() WHERE id = $1 RETURNING {PROBATION_COLUMNS}"
    );
    let p2 = sqlx::query_as::<_, ProbationRow>(&sql)
        .bind(id)
        .bind(to.as_str())
        .bind(&note)
        .bind(actor.id.as_deref())
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?
        .dto()?;
    if to == ProbationStatus::Cleared
        && standing_of(&mut *tx, tenant, ctx.id.as_uuid()).await? == AcademicStanding::Probation
    {
        set_standing(
            &mut tx,
            &ctx,
            AcademicStanding::GoodStanding,
            "probation_case",
            id,
        )
        .await?;
    }
    emit(
        &mut tx,
        &ss.scope,
        PROBATION_RESOLVED,
        PROBATION_RESOLVED_SCHEMA,
        id,
        &status_changed(
            id,
            &ss.scope,
            Some(p2.student_id),
            Some(from.as_str()),
            to.as_str(),
            p2.version,
            Some(&note),
            None,
            ss.subject_ref(),
        ),
        actor,
    )
    .await?;
    audit(
        &mut tx,
        &ss.scope,
        &format!(
            "student.probation_case.{}",
            if to == ProbationStatus::Cleared {
                "clear"
            } else {
                "escalate"
            }
        ),
        "probation_case",
        id,
        json!({"note": note}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(p2)
}

// ================================================================== detention (L3-C-031)

const DETENTION_COLUMNS: &str =
    "id, student_id, student_context_id, decision_id, repeat_unit, repeat_term_no, determined_at, \
    released_at, release_note, version, created_at";

#[derive(sqlx::FromRow)]
struct DetentionRow {
    id: Uuid,
    student_id: Uuid,
    student_context_id: Uuid,
    decision_id: Uuid,
    repeat_unit: Option<String>,
    repeat_term_no: Option<i32>,
    determined_at: Option<DateTime<Utc>>,
    released_at: Option<DateTime<Utc>>,
    release_note: Option<String>,
    version: i64,
    created_at: DateTime<Utc>,
}

impl DetentionRow {
    fn dto(self) -> DetentionCase {
        DetentionCase {
            id: self.id,
            student_id: self.student_id,
            decision_id: self.decision_id,
            repeat_unit: self.repeat_unit,
            repeat_term_no: self.repeat_term_no,
            determined_at: self.determined_at,
            released_at: self.released_at,
            release_note: self.release_note,
            version: self.version,
            created_at: self.created_at,
        }
    }
}

async fn detention_row<'e, E: PgExecutor<'e>>(
    ex: E,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<DetentionRow, KernelError> {
    let sql = format!(
        "SELECT {DETENTION_COLUMNS} FROM student.detention_case WHERE id = $1 AND tenant_id = $2{}",
        if lock { " FOR UPDATE" } else { "" }
    );
    sqlx::query_as::<_, DetentionRow>(&sql)
        .bind(id)
        .bind(tenant.as_uuid())
        .fetch_optional(ex)
        .await
        .map_err(map_db_error)?
        .ok_or(DETENTION)
}

pub async fn get_detention(
    pool: &PgPool,
    tenant: TenantId,
    id: Uuid,
) -> Result<DetentionCase, KernelError> {
    Ok(detention_row(pool, tenant, id, false).await?.dto())
}

/// Determines a pending repeat consequence (L4-C-031 continuation: detention stays valid while
/// the repeat consequence is unresolved).
pub async fn determine_repeat(
    state: &StudentState,
    tenant: TenantId,
    id: Uuid,
    expected: Version,
    cmd: DetermineRepeat,
    actor: &Actor,
) -> Result<DetentionCase, KernelError> {
    check_repeat_unit(Some(&cmd.repeat_unit))?;
    let cur = detention_row(&state.pool, tenant, id, false).await?;
    let ss = write_student(state, tenant, StudentId(cur.student_id), false).await?;
    if ss.context.id.as_uuid() != cur.student_context_id {
        return Err(conflict(
            "the detention belongs to a closed student context",
        ));
    }
    let terms = term_count(state, &ss.scope, &ss.context).await?;
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let ctx = lock_context(&mut tx, tenant, ss.context.id).await?;
    let d = detention_row(&mut *tx, tenant, id, true).await?;
    check_version(d.version, expected)?;
    if d.repeat_unit.is_some() || d.released_at.is_some() {
        return Err(conflict("the repeat consequence is already determined"));
    }
    let dec = decision_row(&mut *tx, tenant, d.decision_id, true).await?;
    let new_term = next_term_no(dec.term_no, dec.outcome()?, Some(&cmd.repeat_unit), terms);
    let current = ctx.current_term_no.unwrap_or(1);
    if Some(current) != dec.resulting_term_no {
        return Err(conflict("the student's term changed since the detention"));
    }
    if new_term != current {
        set_context_progress(
            &mut tx,
            &ctx,
            Some(new_term),
            ctx.programme_id,
            ctx.curriculum_version_id,
        )
        .await?;
    }
    sqlx::query("UPDATE student.progression_decision SET repeat_unit = $2, resulting_term_no = $3, version = version + 1, updated_at = now() WHERE id = $1")
        .bind(dec.id)
        .bind(&cmd.repeat_unit)
        .bind(new_term)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    let sql = format!(
        "UPDATE student.detention_case SET repeat_unit = $2, repeat_term_no = $3, determined_at = now(), version = version + 1, \
         updated_at = now() WHERE id = $1 RETURNING {DETENTION_COLUMNS}"
    );
    let d2 = sqlx::query_as::<_, DetentionRow>(&sql)
        .bind(id)
        .bind(&cmd.repeat_unit)
        .bind(new_term)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?
        .dto();
    audit(
        &mut tx,
        &ss.scope,
        "student.detention_case.determine",
        "detention_case",
        id,
        json!({"repeatUnit": cmd.repeat_unit, "repeatTermNo": new_term}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(d2)
}

// ================================================================== ledger (L3-C-028)

const ENTRY_COLUMNS: &str = "id, kind, credits::float8 AS credits, curriculum_component_id, academic_term_id, source_type, \
    source_id, reverses_entry_id, institution_approved, note, recorded_by, recorded_at";

#[derive(sqlx::FromRow)]
struct EntryRow {
    id: Uuid,
    kind: String,
    credits: f64,
    curriculum_component_id: Option<Uuid>,
    academic_term_id: Option<Uuid>,
    source_type: String,
    source_id: Option<Uuid>,
    reverses_entry_id: Option<Uuid>,
    institution_approved: bool,
    note: Option<String>,
    recorded_by: Option<String>,
    recorded_at: DateTime<Utc>,
}

impl EntryRow {
    fn dto(self) -> Result<CreditLedgerEntry, KernelError> {
        Ok(CreditLedgerEntry {
            kind: parse_enum(&self.kind, CreditEntryKind::parse, "credit entry kind")?,
            id: self.id,
            credits: self.credits,
            curriculum_component_id: self.curriculum_component_id,
            academic_term_id: self.academic_term_id,
            source_type: self.source_type,
            source_id: self.source_id,
            reverses_entry_id: self.reverses_entry_id,
            institution_approved: self.institution_approved,
            note: self.note,
            recorded_by: self.recorded_by,
            recorded_at: self.recorded_at,
        })
    }
}

/// The credit ledger of the student's current (or most recent) context.
pub async fn ledger(
    state: &StudentState,
    tenant: TenantId,
    student_id: StudentId,
) -> Result<CreditLedger, KernelError> {
    let (_s, ctx) = read_student(state, tenant, student_id).await?;
    let ctx_id = match ctx {
        Some(c) => Some(c.id.as_uuid()),
        None => crate::shared::student::contexts_of(&state.pool, tenant, student_id)
            .await?
            .first()
            .map(|c| c.id.as_uuid()),
    };
    let Some(ctx_id) = ctx_id else {
        return Ok(CreditLedger {
            student_id: student_id.as_uuid(),
            student_context_id: None,
            total_credits: 0.0,
            entries: vec![],
        });
    };
    let sql = format!(
        "SELECT {ENTRY_COLUMNS} FROM student.credit_ledger_entry WHERE tenant_id = $1 AND student_context_id = $2 \
         ORDER BY recorded_at, id"
    );
    let entries = sqlx::query_as::<_, EntryRow>(&sql)
        .bind(tenant.as_uuid())
        .bind(ctx_id)
        .fetch_all(&state.pool)
        .await
        .map_err(map_db_error)?
        .into_iter()
        .map(EntryRow::dto)
        .collect::<Result<Vec<_>, _>>()?;
    let total = entries
        .iter()
        .map(|e| signed(e.kind, e.credits))
        .sum::<f64>();
    Ok(CreditLedger {
        student_id: student_id.as_uuid(),
        student_context_id: Some(ctx_id),
        total_credits: total,
        entries,
    })
}

/// Appends a manual ledger entry.
pub async fn append_manual(
    state: &StudentState,
    tenant: TenantId,
    student_id: StudentId,
    cmd: AppendLedgerEntry,
    actor: &Actor,
) -> Result<CreditLedger, KernelError> {
    let ss = write_student(state, tenant, student_id, false).await?;
    let note = required(Some(cmd.note.clone()), "note", 1000)?;
    match cmd.kind {
        CreditEntryKind::Adjusted => {
            if cmd.credits == 0.0 || cmd.credits.abs() > 1000.0 {
                return Err(invalid(
                    "credits",
                    "out_of_range",
                    "a non-zero adjustment of at most ±1000",
                ));
            }
        }
        _ => {
            if !(cmd.credits > 0.0 && cmd.credits <= 1000.0) {
                return Err(invalid(
                    "credits",
                    "out_of_range",
                    "greater than 0, at most 1000",
                ));
            }
        }
    }
    if matches!(
        cmd.kind,
        CreditEntryKind::TransferredIn | CreditEntryKind::Recognised
    ) {
        return Err(invalid(
            "kind",
            "use_credit_transfer",
            "transferred and recognised credits come from an approved credit transfer",
        ));
    }
    if matches!(
        cmd.kind,
        CreditEntryKind::Revoked | CreditEntryKind::Expired
    ) && cmd.reverses_entry_id.is_none()
    {
        return Err(invalid(
            "reversesEntryId",
            "required",
            "revoked/expired entries reverse an existing entry",
        ));
    }
    if let Some(c) = cmd.curriculum_component_id {
        let cv = ss
            .context
            .curriculum_version_id
            .ok_or_else(|| conflict("the student context has no curriculum"))?;
        if !state
            .references
            .curriculum_components(&ss.scope, cv)
            .await?
            .iter()
            .any(|x| x.id == c)
        {
            return Err(invalid(
                "curriculumComponentId",
                "not_in_curriculum",
                "not a component of the student's curriculum",
            ));
        }
    }
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let ctx = lock_context(&mut tx, tenant, ss.context.id).await?;
    let (mut component, mut term) = (cmd.curriculum_component_id, cmd.academic_term_id);
    if let Some(rev) = cmd.reverses_entry_id {
        let orig: Option<(String, f64, Option<Uuid>, Option<Uuid>)> = sqlx::query_as(
            "SELECT kind, credits::float8, curriculum_component_id, academic_term_id FROM student.credit_ledger_entry \
             WHERE id = $1 AND tenant_id = $2 AND student_context_id = $3",
        )
        .bind(rev)
        .bind(tenant.as_uuid())
        .bind(ctx.id.as_uuid())
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_db_error)?;
        let (kind, credits, c, t) = orig.ok_or_else(|| {
            invalid(
                "reversesEntryId",
                "unknown",
                "no such entry in the student's ledger",
            )
        })?;
        if matches!(kind.as_str(), "revoked" | "expired") {
            return Err(invalid(
                "reversesEntryId",
                "not_reversible",
                "a reversal cannot be reversed",
            ));
        }
        if cmd.credits > credits + 1e-9 {
            return Err(invalid(
                "credits",
                "exceeds_entry",
                format!("the entry carries {credits} credits"),
            ));
        }
        component = component.or(c);
        term = term.or(t);
    }
    let id = append_entry(
        &mut tx,
        &ctx,
        &NewEntry {
            kind: cmd.kind,
            credits: cmd.credits,
            component,
            term,
            source_type: "manual",
            source_id: None,
            reverses: cmd.reverses_entry_id,
            approved: cmd.institution_approved.unwrap_or(true),
            note: Some(&note),
        },
        actor,
    )
    .await?;
    ledger_updated(&mut tx, &ss.scope, &ctx, vec![id], actor).await?;
    audit(
        &mut tx,
        &ss.scope,
        "student.credit_ledger.append",
        "credit_ledger_entry",
        id,
        json!({"kind": cmd.kind, "credits": cmd.credits, "note": note}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    ledger(state, tenant, student_id).await
}

// ================================================================== summary

pub async fn summary(
    state: &StudentState,
    tenant: TenantId,
    student_id: StudentId,
) -> Result<ProgressionSummary, KernelError> {
    let (s, ctx) = read_student(state, tenant, student_id).await?;
    let Some(ctx) = ctx else {
        return Ok(ProgressionSummary {
            student_id: s.id.as_uuid(),
            student_context_id: None,
            programme_id: None,
            curriculum_version_id: None,
            current_term_no: None,
            standing: AcademicStanding::GoodStanding,
            total_credits: 0.0,
            effective_policy: None,
            probation_cases: vec![],
            detention_cases: vec![],
            pending_conditions: vec![],
            eligibility: super::eligibility::current(&state.pool, tenant, student_id).await?,
        });
    };
    let scope = ExecutionScope::institution(tenant, ctx.institution_id);
    let today = business_today(state, &scope, ctx.institution_id).await?;
    let c = ctx.id.as_uuid();
    let probation_cases = sqlx::query_as::<_, ProbationRow>(&format!(
        "SELECT {PROBATION_COLUMNS} FROM student.probation_case WHERE tenant_id = $1 AND student_context_id = $2 ORDER BY created_at DESC"
    ))
    .bind(tenant.as_uuid())
    .bind(c)
    .fetch_all(&state.pool)
    .await
    .map_err(map_db_error)?
    .into_iter()
    .map(ProbationRow::dto)
    .collect::<Result<Vec<_>, _>>()?;
    let detention_cases = sqlx::query_as::<_, DetentionRow>(&format!(
        "SELECT {DETENTION_COLUMNS} FROM student.detention_case WHERE tenant_id = $1 AND student_context_id = $2 ORDER BY created_at DESC"
    ))
    .bind(tenant.as_uuid())
    .bind(c)
    .fetch_all(&state.pool)
    .await
    .map_err(map_db_error)?
    .into_iter()
    .map(DetentionRow::dto)
    .collect();
    let pending_conditions = sqlx::query_as::<_, ConditionRow>(&format!(
        "SELECT {CONDITION_COLUMNS} FROM student.progression_condition pc WHERE tenant_id = $1 AND student_context_id = $2 \
         AND status = 'pending' AND EXISTS (SELECT 1 FROM student.progression_decision d WHERE d.id = pc.decision_id AND d.status = 'effective') \
         ORDER BY created_at"
    ))
    .bind(tenant.as_uuid())
    .bind(c)
    .fetch_all(&state.pool)
    .await
    .map_err(map_db_error)?
    .into_iter()
    .map(ConditionRow::dto)
    .collect::<Result<Vec<_>, _>>()?;
    Ok(ProgressionSummary {
        student_id: s.id.as_uuid(),
        student_context_id: Some(c),
        programme_id: ctx.programme_id,
        curriculum_version_id: ctx.curriculum_version_id,
        current_term_no: ctx.current_term_no,
        standing: standing_of(&state.pool, tenant, c).await?,
        total_credits: ledger_total(&state.pool, tenant, c).await?,
        effective_policy: effective_policy(&state.pool, tenant, &ctx, today).await?,
        probation_cases,
        detention_cases,
        pending_conditions,
        eligibility: super::eligibility::current(&state.pool, tenant, student_id).await?,
    })
}

/// Validates a curriculum version for a programme (published, same institution).
pub(crate) async fn published_curriculum(
    state: &StudentState,
    scope: &ExecutionScope,
    programme_id: Uuid,
    cv: Option<Uuid>,
    on: NaiveDate,
    field: &str,
) -> Result<sutra_contracts::institution::reference::CurriculumVersionRef, KernelError> {
    let v = match cv {
        Some(id) => state.references.curriculum_version(scope, id).await?,
        None => {
            state
                .references
                .current_curriculum_version(scope, programme_id, on)
                .await?
        }
    }
    .filter(|v| v.programme_id == programme_id)
    .ok_or_else(|| {
        invalid(
            field,
            "unknown",
            "no published curriculum version of the programme",
        )
    })?;
    if v.status != PublicationStatus::Published {
        return Err(invalid(
            field,
            "not_published",
            "the curriculum version is not published",
        ));
    }
    Ok(v)
}
