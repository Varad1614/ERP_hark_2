//! **3C** guardian persistence + commands (L3-C-051 … C-061).
//!
//! Relationship: `proposed → pending_verification → verified → active ⇄ suspended → terminated`.
//! Verification runs through the WorkflowPort (`GUARDIAN_VERIFICATION`; auto-approve in
//! Batch 3, `/verification-decision` otherwise). Responsibilities (consent, communication,
//! emergency contact, financial responsibility, pickup) need an **active** relationship;
//! suspension suspends them, reinstatement restores those it suspended, termination revokes
//! them and withdraws granted consents (history is kept: rows are never deleted).

use super::domain::*;
use crate::completion::kit::{self, StatusEvent};
use crate::shared::approval::{self, business_key, process, ApprovalOutcome};
use crate::shared::guard::require_student_writable;
use crate::shared::student::{load_student, StudentRow};
use crate::shared::time::business_today;
use crate::shared::{Actor, StudentState};
use serde_json::json;
use sqlx::PgConnection;
use std::collections::BTreeMap;
use sutra_contracts::student::events::guardian::*;
use sutra_contracts::student::ids::{
    EmergencyContactId, GuardianConsentId, GuardianId, GuardianRelationshipId,
    PickupAuthorisationId, StudentId,
};
use sutra_contracts::student::status::{
    AssignmentStatus, GuardianConsentStatus, GuardianRelationshipStatus,
};
use sutra_kernel::auth::AuthContext;
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use uuid::Uuid;

const GUARDIAN: &str = "guardian";
const REL: &str = "guardian_relationship";
const REL_ENTITY: &str = "guardian relationship";
const CONSENT: &str = "guardian_consent";
const COMM: &str = "guardian_communication";
const EMERGENCY: &str = "emergency_contact";
const FINANCIAL: &str = "financial_responsibility";
const PICKUP: &str = "pickup_authorisation";
const CUSTODY: &str = "guardian_custody_history";
const SUSPENDED_BY_RELATIONSHIP: &str = "guardian relationship suspended";

type RelStatus = GuardianRelationshipStatus;

macro_rules! tx {
    ($state:expr) => {
        $state.pool.begin().await.map_err(map_db_error)?
    };
}

fn trimmed(v: Option<String>) -> Option<String> {
    v.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty())
}

// ---- guardians ----------------------------------------------------------------------------------

pub async fn list_guardians(
    state: &StudentState,
    tenant: TenantId,
    institution_id: InstitutionId,
    q: Option<&str>,
) -> Result<Vec<Guardian>, KernelError> {
    let rows: Vec<serde_json::Value> = sqlx::query_scalar(
        "SELECT to_jsonb(g) FROM student.guardian g WHERE g.tenant_id = $1 AND g.institution_id = $2 \
         AND ($3::text IS NULL OR (g.given_name || ' ' || coalesce(g.family_name, '') || ' ' || \
              coalesce(g.email, '') || ' ' || coalesce(g.phone, '')) ILIKE '%' || $3 || '%') \
         ORDER BY g.given_name, g.family_name NULLS FIRST, g.id LIMIT 200",
    )
    .bind(tenant.as_uuid())
    .bind(institution_id.as_uuid())
    .bind(q.map(str::trim).filter(|s| !s.is_empty()))
    .fetch_all(&state.pool)
    .await
    .map_err(map_db_error)?;
    rows.into_iter().map(kit::decode).collect()
}

pub async fn get_guardian(
    state: &StudentState,
    tenant: TenantId,
    id: GuardianId,
) -> Result<Guardian, KernelError> {
    kit::fetch(&state.pool, GUARDIAN, "guardian", tenant, id.as_uuid()).await
}

pub async fn create_guardian(
    state: &StudentState,
    ctx: &AuthContext,
    institution_id: InstitutionId,
    cmd: NewGuardian,
    actor: &Actor,
) -> Result<Guardian, KernelError> {
    let given = kit::required_text("givenName", Some(&cmd.given_name))?;
    let (email, phone) = (trimmed(cmd.email), trimmed(cmd.phone));
    if email.is_none() && phone.is_none() {
        return Err(kit::invalid(
            "phone",
            "required",
            "a guardian needs a phone number or an e-mail address",
        ));
    }
    let scope = kit::writable_institution(state, ctx, institution_id).await?;
    let mut tx = tx!(state);
    let g: Guardian = kit::insert(
        &mut tx,
        GUARDIAN,
        json!({
            "id": Uuid::now_v7(),
            "tenant_id": ctx.scope.tenant_id.as_uuid(),
            "institution_id": institution_id.as_uuid(),
            "given_name": given,
            "family_name": trimmed(cmd.family_name),
            "email": email,
            "phone": phone,
            "address": trimmed(cmd.address),
            "occupation": trimmed(cmd.occupation),
        }),
    )
    .await?;
    kit::audit(
        &mut tx,
        &scope,
        GUARDIAN,
        "create",
        g.id.as_uuid(),
        json!({"givenName": g.given_name}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(g)
}

pub async fn update_guardian(
    state: &StudentState,
    ctx: &AuthContext,
    id: GuardianId,
    expected: Version,
    cmd: GuardianUpdate,
    actor: &Actor,
) -> Result<Guardian, KernelError> {
    let tenant = ctx.scope.tenant_id;
    let g = get_guardian(state, tenant, id).await?;
    let scope = kit::writable_institution(state, ctx, InstitutionId(g.institution_id)).await?;
    let mut changes = serde_json::Map::new();
    let mut fields = Vec::new();
    if let Some(n) = cmd.given_name {
        changes.insert(
            "given_name".into(),
            json!(kit::required_text("givenName", Some(&n))?),
        );
        fields.push("givenName");
    }
    for (key, api, v) in [
        ("family_name", "familyName", cmd.family_name),
        ("email", "email", cmd.email),
        ("phone", "phone", cmd.phone),
        ("address", "address", cmd.address),
        ("occupation", "occupation", cmd.occupation),
    ] {
        if let Some(v) = v {
            changes.insert(key.into(), json!(trimmed(Some(v))));
            fields.push(api);
        }
    }
    if changes.is_empty() {
        return Err(kit::invalid("body", "no_change", "nothing to update"));
    }
    let mut tx = tx!(state);
    let locked: Guardian = kit::lock(&mut tx, GUARDIAN, "guardian", tenant, id.as_uuid()).await?;
    kit::check_version(locked.version, expected)?;
    let g: Guardian = kit::patch(
        &mut tx,
        GUARDIAN,
        tenant,
        id.as_uuid(),
        serde_json::Value::Object(changes),
    )
    .await?;
    if g.email.is_none() && g.phone.is_none() {
        return Err(kit::invalid(
            "phone",
            "required",
            "a guardian needs a phone number or an e-mail address",
        ));
    }
    kit::audit(
        &mut tx,
        &scope,
        GUARDIAN,
        "update",
        id.as_uuid(),
        json!({"changedFields": fields}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(g)
}

// ---- relationships ------------------------------------------------------------------------------

async fn hydrate(
    state: &StudentState,
    tenant: TenantId,
    mut r: GuardianRelationship,
) -> Result<GuardianRelationship, KernelError> {
    r.guardian = Some(get_guardian(state, tenant, r.guardian_id).await?);
    r.consents = kit::list_by(
        &state.pool,
        CONSENT,
        tenant,
        "relationship_id",
        r.id.as_uuid(),
        None,
    )
    .await?;
    r.communications = kit::list_by(
        &state.pool,
        COMM,
        tenant,
        "relationship_id",
        r.id.as_uuid(),
        None,
    )
    .await?;
    Ok(r)
}

pub async fn get_relationship(
    state: &StudentState,
    tenant: TenantId,
    id: GuardianRelationshipId,
) -> Result<GuardianRelationship, KernelError> {
    let r = kit::fetch(&state.pool, REL, REL_ENTITY, tenant, id.as_uuid()).await?;
    hydrate(state, tenant, r).await
}

pub async fn list_relationships(
    state: &StudentState,
    tenant: TenantId,
    column: &str,
    id: Uuid,
    status: Option<RelStatus>,
) -> Result<Vec<GuardianRelationship>, KernelError> {
    let list: Vec<GuardianRelationship> = kit::list_by(
        &state.pool,
        REL,
        tenant,
        column,
        id,
        status.map(|s| s.as_str()),
    )
    .await?;
    let mut out = Vec::with_capacity(list.len());
    for r in list {
        out.push(hydrate(state, tenant, r).await?);
    }
    Ok(out)
}

fn rel_payload(r: &GuardianRelationship) -> GuardianRelationshipV1 {
    GuardianRelationshipV1 {
        relationship_id: r.id.as_uuid(),
        guardian_id: r.guardian_id.as_uuid(),
        student_id: r.student_id.as_uuid(),
        institution_id: r.institution_id,
        relationship_type: r.relationship_type.as_str().into(),
        has_legal_custody: r.has_legal_custody,
        status: r.status.as_str().into(),
        version: r.version,
    }
}

fn rel_event<'a>(
    (event_type, schema): (&'static str, &'static str),
    r: &'a GuardianRelationship,
    student: &'a StudentRow,
    from: RelStatus,
    reason: Option<&'a str>,
) -> StatusEvent<'a> {
    StatusEvent {
        event_type,
        schema,
        aggregate_id: r.id.as_uuid(),
        institution_id: r.institution_id,
        student: Some(student),
        from: Some(from.as_str()),
        to: r.status.as_str(),
        version: r.version,
        reason,
        process_instance_id: r.process_instance_id.as_deref(),
    }
}

fn unique_conflict(e: KernelError, msg: &str) -> KernelError {
    match e {
        KernelError::Conflict(_) => KernelError::Conflict(msg.into()),
        other => other,
    }
}

/// Proposes a relationship between a guardian of the institution and the student (L3-C-051).
pub async fn propose(
    state: &StudentState,
    ctx: &AuthContext,
    student_id: StudentId,
    cmd: NewGuardianRelationship,
    actor: &Actor,
) -> Result<GuardianRelationship, KernelError> {
    let target = kit::writable_student(state, ctx, student_id).await?;
    let tenant = ctx.scope.tenant_id;
    let g = get_guardian(state, tenant, cmd.guardian_id)
        .await
        .map_err(|_| kit::invalid("guardianId", "not_found", "unknown guardian"))?;
    if g.institution_id != target.institution_id().as_uuid() {
        return Err(kit::invalid(
            "guardianId",
            "other_institution",
            "the guardian is registered at another institution",
        ));
    }
    let mut tx = tx!(state);
    let r: GuardianRelationship = kit::insert(&mut tx, REL, json!({
        "id": Uuid::now_v7(),
        "tenant_id": tenant.as_uuid(),
        "institution_id": target.institution_id().as_uuid(),
        "guardian_id": g.id.as_uuid(),
        "student_id": student_id.as_uuid(),
        "relationship_type": cmd.relationship_type.as_str(),
        "is_primary": cmd.is_primary,
        "status": RelStatus::Proposed.as_str(),
        "notes": trimmed(cmd.notes),
    })).await.map_err(|e| unique_conflict(e, "the guardian already has a current relationship with this student, or a primary guardian exists"))?;
    crate::shared::publish::event(
        &mut tx,
        &target.scope,
        GUARDIAN_RELATIONSHIP_PROPOSED,
        GUARDIAN_RELATIONSHIP_PROPOSED_SCHEMA,
        r.id.to_string(),
        &rel_payload(&r),
        actor,
    )
    .await?;
    kit::audit(
        &mut tx,
        &target.scope,
        REL,
        "propose",
        r.id.as_uuid(),
        json!({"guardianId": g.id, "relationshipType": cmd.relationship_type}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    get_relationship(state, tenant, r.id).await
}

struct RelCmd {
    r: GuardianRelationship,
    student: StudentRow,
    scope: ExecutionScope,
}

async fn load_rel(
    state: &StudentState,
    ctx: &AuthContext,
    id: GuardianRelationshipId,
) -> Result<RelCmd, KernelError> {
    let tenant = ctx.scope.tenant_id;
    let r: GuardianRelationship =
        kit::fetch(&state.pool, REL, REL_ENTITY, tenant, id.as_uuid()).await?;
    let student = load_student(&state.pool, tenant, r.student_id).await?;
    require_student_writable(&student)?;
    let scope = kit::writable_institution(state, ctx, InstitutionId(r.institution_id)).await?;
    Ok(RelCmd { r, student, scope })
}

async fn lock_rel(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: GuardianRelationshipId,
    expected: Option<Version>,
) -> Result<GuardianRelationship, KernelError> {
    let r: GuardianRelationship = kit::lock(conn, REL, REL_ENTITY, tenant, id.as_uuid()).await?;
    if let Some(v) = expected {
        kit::check_version(r.version, v)?;
    }
    Ok(r)
}

async fn move_rel(
    conn: &mut PgConnection,
    tenant: TenantId,
    r: &GuardianRelationship,
    to: RelStatus,
    extra: serde_json::Value,
) -> Result<GuardianRelationship, KernelError> {
    if !relationship_can_transition(r.status, to) {
        return Err(kit::illegal("the guardian relationship", r.status, to));
    }
    let mut changes = if extra.is_object() { extra } else { json!({}) };
    changes["status"] = json!(to.as_str());
    kit::patch(conn, REL, tenant, r.id.as_uuid(), changes).await
}

fn require_active(r: &GuardianRelationship) -> Result<(), KernelError> {
    if r.status != RelStatus::Active {
        return Err(KernelError::Conflict(format!(
            "the guardian relationship is {}; guardian responsibilities need an active relationship",
            r.status
        )));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn apply_verification(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    tenant: TenantId,
    r: &GuardianRelationship,
    student: &StudentRow,
    approve: bool,
    reason: Option<&str>,
    actor: &Actor,
) -> Result<GuardianRelationship, KernelError> {
    if approve {
        let v = move_rel(
            conn,
            tenant,
            r,
            RelStatus::Verified,
            json!({
                "verified_at": chrono::Utc::now(), "verified_by": actor.id, "status_reason": null,
            }),
        )
        .await?;
        rel_event(
            (
                GUARDIAN_RELATIONSHIP_VERIFIED,
                GUARDIAN_RELATIONSHIP_VERIFIED_SCHEMA,
            ),
            &v,
            student,
            r.status,
            None,
        )
        .emit(conn, scope, actor)
        .await?;
        kit::audit(
            conn,
            scope,
            REL,
            "verify",
            r.id.as_uuid(),
            json!({"processInstanceId": r.process_instance_id}),
            actor,
        )
        .await?;
        Ok(v)
    } else {
        // A rejected verification stays auditable; the relationship returns to `proposed`.
        let v = move_rel(
            conn,
            tenant,
            r,
            RelStatus::Proposed,
            json!({"status_reason": reason}),
        )
        .await?;
        kit::audit(conn, scope, REL, "verification_reject", r.id.as_uuid(), json!({"reason": reason, "method": r.verification_method, "evidence": r.verification_evidence}), actor).await?;
        Ok(v)
    }
}

/// Submits the relationship for verification (WorkflowPort `GUARDIAN_VERIFICATION`).
pub async fn submit_verification(
    state: &StudentState,
    ctx: &AuthContext,
    id: GuardianRelationshipId,
    expected: Version,
    cmd: SubmitVerification,
    actor: &Actor,
) -> Result<GuardianRelationship, KernelError> {
    let method = kit::required_text("method", Some(&cmd.method))?;
    let evidence = kit::required_text("evidenceRef", Some(&cmd.evidence_ref))?;
    let RelCmd { r, student, scope } = load_rel(state, ctx, id).await?;
    if r.status != RelStatus::Proposed {
        return Err(kit::illegal(
            "the guardian relationship",
            r.status,
            RelStatus::PendingVerification,
        ));
    }
    let tenant = ctx.scope.tenant_id;
    let mut vars = BTreeMap::new();
    vars.insert("method".to_owned(), json!(method));
    let outcome = approval::request_approval(
        state,
        &scope,
        process::GUARDIAN_VERIFICATION,
        business_key(REL, id, r.version),
        vars,
    )
    .await?;
    let mut tx = tx!(state);
    let locked = lock_rel(&mut tx, tenant, id, Some(expected)).await?;
    let pending = move_rel(
        &mut tx,
        tenant,
        &locked,
        RelStatus::PendingVerification,
        json!({
            "verification_method": method, "verification_evidence": evidence,
            "process_instance_id": outcome.process_instance_id(),
        }),
    )
    .await?;
    kit::audit(
        &mut tx,
        &scope,
        REL,
        "submit_verification",
        id.as_uuid(),
        json!({"method": method, "evidenceRef": evidence}),
        actor,
    )
    .await?;
    if let ApprovalOutcome::Approved { .. } = outcome {
        let system = Actor::system(actor.correlation_id.clone());
        apply_verification(
            &mut tx, &scope, tenant, &pending, &student, true, None, &system,
        )
        .await?;
    }
    tx.commit().await.map_err(map_db_error)?;
    get_relationship(state, tenant, id).await
}

/// Verification decision (`AwaitDecision`).
pub async fn verification_decision(
    state: &StudentState,
    ctx: &AuthContext,
    id: GuardianRelationshipId,
    expected: Version,
    cmd: kit::LifecycleDecision,
    actor: &Actor,
) -> Result<GuardianRelationship, KernelError> {
    let approve = cmd.decision == kit::LifecycleDecisionValue::Approve;
    if !approve {
        kit::required_text("reason", cmd.reason.as_deref())?;
    }
    let RelCmd { student, scope, .. } = load_rel(state, ctx, id).await?;
    let tenant = ctx.scope.tenant_id;
    let mut tx = tx!(state);
    let locked = lock_rel(&mut tx, tenant, id, Some(expected)).await?;
    if locked.status != RelStatus::PendingVerification {
        return Err(KernelError::Conflict(format!(
            "the guardian relationship is {}; no verification is pending",
            locked.status
        )));
    }
    apply_verification(
        &mut tx,
        &scope,
        tenant,
        &locked,
        &student,
        approve,
        cmd.reason.as_deref(),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    get_relationship(state, tenant, id).await
}

/// Activates a verified relationship.
pub async fn activate(
    state: &StudentState,
    ctx: &AuthContext,
    id: GuardianRelationshipId,
    expected: Version,
    actor: &Actor,
) -> Result<GuardianRelationship, KernelError> {
    simple_move(
        state,
        ctx,
        id,
        expected,
        RelStatus::Active,
        None,
        "activate",
        (
            GUARDIAN_RELATIONSHIP_ACTIVATED,
            GUARDIAN_RELATIONSHIP_ACTIVATED_SCHEMA,
        ),
        actor,
    )
    .await
}

/// Cascades a relationship state change to its dependent assignments and consents.
async fn cascade(
    conn: &mut PgConnection,
    tenant: TenantId,
    rel_id: Uuid,
    to: RelStatus,
) -> Result<serde_json::Value, KernelError> {
    let mut counts = serde_json::Map::new();
    for table in [COMM, EMERGENCY, FINANCIAL, PICKUP] {
        let sql = match to {
            RelStatus::Suspended => format!(
                "UPDATE student.{table} SET status = 'suspended', status_reason = $3, version = version + 1, updated_at = now() \
                 WHERE tenant_id = $1 AND relationship_id = $2 AND status = 'active'"
            ),
            RelStatus::Active => format!(
                "UPDATE student.{table} SET status = 'active', status_reason = NULL, version = version + 1, updated_at = now() \
                 WHERE tenant_id = $1 AND relationship_id = $2 AND status = 'suspended' AND status_reason = $3"
            ),
            _ => format!(
                "UPDATE student.{table} SET status = 'revoked', status_reason = 'guardian relationship terminated', \
                 version = version + 1, updated_at = now() \
                 WHERE tenant_id = $1 AND relationship_id = $2 AND status IN ('active', 'suspended') AND $3::text IS NOT NULL"
            ),
        };
        let n = sqlx::query(&sql)
            .bind(tenant.as_uuid())
            .bind(rel_id)
            .bind(SUSPENDED_BY_RELATIONSHIP)
            .execute(&mut *conn)
            .await
            .map_err(map_db_error)?
            .rows_affected();
        counts.insert(table.into(), json!(n));
    }
    if to == RelStatus::Terminated {
        let n = sqlx::query(
            "UPDATE student.guardian_consent SET status = 'withdrawn', status_reason = 'guardian relationship terminated', \
             withdrawn_at = now(), version = version + 1, updated_at = now() \
             WHERE tenant_id = $1 AND relationship_id = $2 AND status = 'granted'",
        )
        .bind(tenant.as_uuid())
        .bind(rel_id)
        .execute(&mut *conn)
        .await
        .map_err(map_db_error)?
        .rows_affected();
        counts.insert(CONSENT.into(), json!(n));
    }
    Ok(serde_json::Value::Object(counts))
}

#[allow(clippy::too_many_arguments)]
async fn simple_move(
    state: &StudentState,
    ctx: &AuthContext,
    id: GuardianRelationshipId,
    expected: Version,
    to: RelStatus,
    reason: Option<String>,
    command: &str,
    event: (&'static str, &'static str),
    actor: &Actor,
) -> Result<GuardianRelationship, KernelError> {
    let RelCmd { student, scope, .. } = load_rel(state, ctx, id).await?;
    let tenant = ctx.scope.tenant_id;
    let mut tx = tx!(state);
    let locked = lock_rel(&mut tx, tenant, id, Some(expected)).await?;
    if to == RelStatus::Active
        && locked.status == RelStatus::Suspended
        && locked.has_legal_custody
        && reason.is_none()
    {
        // Reinstating a custodial guardian is documented explicitly.
        return Err(kit::invalid(
            "reason",
            "required",
            "reinstating a guardian with legal custody needs a reason",
        ));
    }
    let updated = move_rel(
        &mut tx,
        tenant,
        &locked,
        to,
        json!({"status_reason": reason}),
    )
    .await?;
    let cascaded = if matches!(to, RelStatus::Suspended | RelStatus::Terminated)
        || (to == RelStatus::Active && locked.status == RelStatus::Suspended)
    {
        cascade(&mut tx, tenant, id.as_uuid(), to).await?
    } else {
        json!({})
    };
    if to == RelStatus::Terminated && locked.is_primary {
        let _: GuardianRelationship = kit::patch(
            &mut tx,
            REL,
            tenant,
            id.as_uuid(),
            json!({"is_primary": false}),
        )
        .await?;
    }
    rel_event(event, &updated, &student, locked.status, reason.as_deref())
        .emit(&mut tx, &scope, actor)
        .await?;
    kit::audit(&mut tx, &scope, REL, command, id.as_uuid(), json!({"fromStatus": locked.status, "toStatus": to, "reason": reason, "cascaded": cascaded}), actor).await?;
    tx.commit().await.map_err(map_db_error)?;
    get_relationship(state, tenant, id).await
}

/// Suspends an active relationship (L3-C-059); its responsibilities are suspended.
pub async fn suspend(
    state: &StudentState,
    ctx: &AuthContext,
    id: GuardianRelationshipId,
    expected: Version,
    reason: Option<String>,
    actor: &Actor,
) -> Result<GuardianRelationship, KernelError> {
    let reason = kit::required_text("reason", reason.as_deref())?;
    simple_move(
        state,
        ctx,
        id,
        expected,
        RelStatus::Suspended,
        Some(reason),
        "suspend",
        (
            GUARDIAN_RELATIONSHIP_SUSPENDED,
            GUARDIAN_RELATIONSHIP_SUSPENDED_SCHEMA,
        ),
        actor,
    )
    .await
}

/// Reinstates a suspended relationship (L3-C-060); the responsibilities it suspended return.
pub async fn reinstate(
    state: &StudentState,
    ctx: &AuthContext,
    id: GuardianRelationshipId,
    expected: Version,
    reason: Option<String>,
    actor: &Actor,
) -> Result<GuardianRelationship, KernelError> {
    let RelCmd { r, .. } = load_rel(state, ctx, id).await?;
    if r.status != RelStatus::Suspended {
        return Err(kit::illegal(
            "the guardian relationship",
            r.status,
            "reinstated",
        ));
    }
    simple_move(
        state,
        ctx,
        id,
        expected,
        RelStatus::Active,
        reason,
        "reinstate",
        (
            GUARDIAN_RELATIONSHIP_REINSTATED,
            GUARDIAN_RELATIONSHIP_REINSTATED_SCHEMA,
        ),
        actor,
    )
    .await
}

/// Terminates a relationship (L3-C-061); responsibilities are revoked, consents withdrawn.
pub async fn terminate(
    state: &StudentState,
    ctx: &AuthContext,
    id: GuardianRelationshipId,
    expected: Version,
    reason: Option<String>,
    actor: &Actor,
) -> Result<GuardianRelationship, KernelError> {
    let reason = kit::required_text("reason", reason.as_deref())?;
    simple_move(
        state,
        ctx,
        id,
        expected,
        RelStatus::Terminated,
        Some(reason),
        "terminate",
        (
            GUARDIAN_RELATIONSHIP_TERMINATED,
            GUARDIAN_RELATIONSHIP_TERMINATED_SCHEMA,
        ),
        actor,
    )
    .await
}

/// Governed modification (L3-C-052): type, primary flag, notes.
pub async fn modify(
    state: &StudentState,
    ctx: &AuthContext,
    id: GuardianRelationshipId,
    expected: Version,
    cmd: GuardianRelationshipUpdate,
    actor: &Actor,
) -> Result<GuardianRelationship, KernelError> {
    let RelCmd { r, student, scope } = load_rel(state, ctx, id).await?;
    if matches!(r.status, RelStatus::Terminated | RelStatus::Historical) {
        return Err(KernelError::Conflict(format!(
            "the guardian relationship is {} and read-only",
            r.status
        )));
    }
    let mut changes = serde_json::Map::new();
    let mut fields = Vec::new();
    if let Some(t) = cmd.relationship_type.filter(|t| *t != r.relationship_type) {
        changes.insert("relationship_type".into(), json!(t.as_str()));
        fields.push("relationshipType".to_owned());
    }
    if let Some(p) = cmd.is_primary.filter(|p| *p != r.is_primary) {
        changes.insert("is_primary".into(), json!(p));
        fields.push("isPrimary".to_owned());
    }
    if let Some(n) = cmd.notes {
        changes.insert("notes".into(), json!(trimmed(Some(n))));
        fields.push("notes".to_owned());
    }
    if fields.is_empty() {
        return Err(kit::invalid(
            "body",
            "no_change",
            "the modification changes nothing",
        ));
    }
    let tenant = ctx.scope.tenant_id;
    let mut tx = tx!(state);
    lock_rel(&mut tx, tenant, id, Some(expected)).await?;
    let updated: GuardianRelationship = kit::patch(
        &mut tx,
        REL,
        tenant,
        id.as_uuid(),
        serde_json::Value::Object(changes),
    )
    .await
    .map_err(|e| unique_conflict(e, "the student already has a primary guardian"))?;
    kit::modified_event(
        &mut tx,
        &scope,
        (
            GUARDIAN_RELATIONSHIP_MODIFIED,
            GUARDIAN_RELATIONSHIP_MODIFIED_SCHEMA,
        ),
        id.as_uuid(),
        Some(&student),
        updated.version,
        fields.clone(),
        actor,
    )
    .await?;
    kit::audit(
        &mut tx,
        &scope,
        REL,
        "modify",
        id.as_uuid(),
        json!({"changedFields": fields}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    get_relationship(state, tenant, id).await
}

/// Legal custody designation / change (L3-C-053). History is appended, never overwritten.
pub async fn change_custody(
    state: &StudentState,
    ctx: &AuthContext,
    id: GuardianRelationshipId,
    expected: Version,
    cmd: CustodyChange,
    actor: &Actor,
) -> Result<GuardianRelationship, KernelError> {
    let reason = kit::required_text("reason", Some(&cmd.reason))?;
    let basis = trimmed(cmd.custody_basis);
    if cmd.has_legal_custody && basis.is_none() {
        return Err(kit::invalid(
            "custodyBasis",
            "required",
            "legal custody needs its legal basis (court order, deed, …)",
        ));
    }
    let RelCmd { r, student, scope } = load_rel(state, ctx, id).await?;
    if !matches!(r.status, RelStatus::Verified | RelStatus::Active) {
        return Err(KernelError::Conflict(format!(
            "the guardian relationship is {}; custody needs a verified or active relationship",
            r.status
        )));
    }
    if r.has_legal_custody == cmd.has_legal_custody && r.custody_basis == basis {
        return Err(kit::invalid(
            "hasLegalCustody",
            "no_change",
            "the custody designation is unchanged",
        ));
    }
    let tenant = ctx.scope.tenant_id;
    let effective = match cmd.effective_from {
        Some(d) => d,
        None => business_today(state, &scope, InstitutionId(r.institution_id)).await?,
    };
    let mut tx = tx!(state);
    lock_rel(&mut tx, tenant, id, Some(expected)).await?;
    let updated: GuardianRelationship = kit::patch(&mut tx, REL, tenant, id.as_uuid(), json!({
        "has_legal_custody": cmd.has_legal_custody, "custody_basis": basis, "custody_effective_from": effective,
    })).await?;
    kit::append(&mut tx, CUSTODY, json!({
        "tenant_id": tenant.as_uuid(), "relationship_id": id.as_uuid(), "student_id": r.student_id.as_uuid(),
        "has_legal_custody": cmd.has_legal_custody, "custody_basis": basis, "effective_from": effective,
        "reason": reason, "actor": actor.id,
    })).await?;
    crate::shared::publish::event(
        &mut tx,
        &scope,
        GUARDIAN_CUSTODY_CHANGED,
        GUARDIAN_CUSTODY_CHANGED_SCHEMA,
        id.to_string(),
        &rel_payload(&updated),
        actor,
    )
    .await?;
    kit::audit(
        &mut tx,
        &scope,
        REL,
        "custody",
        id.as_uuid(),
        json!({"hasLegalCustody": cmd.has_legal_custody, "basis": basis, "reason": reason}),
        actor,
    )
    .await?;
    let _ = student;
    tx.commit().await.map_err(map_db_error)?;
    get_relationship(state, tenant, id).await
}

pub async fn custody_history(
    state: &StudentState,
    tenant: TenantId,
    id: GuardianRelationshipId,
) -> Result<Vec<CustodyRecord>, KernelError> {
    let rows: Vec<serde_json::Value> = sqlx::query_scalar(
        "SELECT to_jsonb(h) FROM student.guardian_custody_history h WHERE h.tenant_id = $1 AND h.relationship_id = $2 ORDER BY h.recorded_at, h.id",
    )
    .bind(tenant.as_uuid())
    .bind(id.as_uuid())
    .fetch_all(&state.pool)
    .await
    .map_err(map_db_error)?;
    rows.into_iter().map(kit::decode).collect()
}

// ---- consent + communication (nested under the relationship) ----------------------------------

pub async fn grant_consent(
    state: &StudentState,
    ctx: &AuthContext,
    id: GuardianRelationshipId,
    cmd: NewGuardianConsent,
    actor: &Actor,
) -> Result<GuardianConsent, KernelError> {
    let RelCmd { r, student, scope } = load_rel(state, ctx, id).await?;
    require_active(&r)?;
    let tenant = ctx.scope.tenant_id;
    let today = business_today(state, &scope, InstitutionId(r.institution_id)).await?;
    if cmd.valid_until.is_some_and(|d| d < today) {
        return Err(kit::invalid(
            "validUntil",
            "in_past",
            "a consent cannot end before today",
        ));
    }
    let mut tx = tx!(state);
    let locked = lock_rel(&mut tx, tenant, id, None).await?;
    require_active(&locked)?;
    let c: GuardianConsent = kit::insert(&mut tx, CONSENT, json!({
        "id": Uuid::now_v7(), "tenant_id": tenant.as_uuid(), "institution_id": r.institution_id,
        "relationship_id": id.as_uuid(), "student_id": r.student_id.as_uuid(),
        "consent_type": cmd.consent_type.as_str(), "scope_note": trimmed(cmd.scope_note),
        "evidence_ref": trimmed(cmd.evidence_ref), "granted_on": today, "valid_until": cmd.valid_until,
        "status": GuardianConsentStatus::Granted.as_str(),
    })).await.map_err(|e| unique_conflict(e, "this consent is already granted; withdraw it first"))?;
    StatusEvent {
        event_type: GUARDIAN_CONSENT_GRANTED,
        schema: GUARDIAN_CONSENT_GRANTED_SCHEMA,
        aggregate_id: c.id.as_uuid(),
        institution_id: r.institution_id,
        student: Some(&student),
        from: None,
        to: c.status.as_str(),
        version: c.version,
        reason: None,
        process_instance_id: None,
    }
    .emit(&mut tx, &scope, actor)
    .await?;
    kit::audit(
        &mut tx,
        &scope,
        CONSENT,
        "grant",
        c.id.as_uuid(),
        json!({"relationshipId": id, "consentType": cmd.consent_type}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(c)
}

pub async fn withdraw_consent(
    state: &StudentState,
    ctx: &AuthContext,
    id: GuardianRelationshipId,
    consent_id: GuardianConsentId,
    expected: Version,
    reason: Option<String>,
    actor: &Actor,
) -> Result<GuardianConsent, KernelError> {
    let RelCmd { r, student, scope } = load_rel(state, ctx, id).await?;
    let tenant = ctx.scope.tenant_id;
    let mut tx = tx!(state);
    let c: GuardianConsent = kit::lock(
        &mut tx,
        CONSENT,
        "guardian consent",
        tenant,
        consent_id.as_uuid(),
    )
    .await?;
    if c.relationship_id != id {
        return Err(KernelError::NotFound {
            entity: "guardian consent",
        });
    }
    kit::check_version(c.version, expected)?;
    if !consent_can_transition(c.status, GuardianConsentStatus::Withdrawn) {
        return Err(kit::illegal(
            "the consent",
            c.status,
            GuardianConsentStatus::Withdrawn,
        ));
    }
    let updated: GuardianConsent = kit::patch(&mut tx, CONSENT, tenant, consent_id.as_uuid(), json!({
        "status": GuardianConsentStatus::Withdrawn.as_str(), "status_reason": reason, "withdrawn_at": chrono::Utc::now(),
    })).await?;
    StatusEvent {
        event_type: GUARDIAN_CONSENT_WITHDRAWN,
        schema: GUARDIAN_CONSENT_WITHDRAWN_SCHEMA,
        aggregate_id: c.id.as_uuid(),
        institution_id: r.institution_id,
        student: Some(&student),
        from: Some(c.status.as_str()),
        to: updated.status.as_str(),
        version: updated.version,
        reason: reason.as_deref(),
        process_instance_id: None,
    }
    .emit(&mut tx, &scope, actor)
    .await?;
    kit::audit(
        &mut tx,
        &scope,
        CONSENT,
        "withdraw",
        c.id.as_uuid(),
        json!({"reason": reason}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(updated)
}

/// Topics beyond emergency contact need a granted `data_sharing` consent (L4-C-055).
pub fn communication_needs_consent(topics: &[CommunicationTopic]) -> bool {
    topics.iter().any(|t| *t != CommunicationTopic::Emergency)
}

pub async fn authorise_communication(
    state: &StudentState,
    ctx: &AuthContext,
    id: GuardianRelationshipId,
    cmd: NewGuardianCommunication,
    actor: &Actor,
) -> Result<GuardianCommunication, KernelError> {
    if cmd.channels.is_empty() {
        return Err(kit::invalid(
            "channels",
            "required",
            "at least one channel is required",
        ));
    }
    if cmd.topics.is_empty() {
        return Err(kit::invalid(
            "topics",
            "required",
            "at least one topic is required",
        ));
    }
    let RelCmd { r, student, scope } = load_rel(state, ctx, id).await?;
    require_active(&r)?;
    let tenant = ctx.scope.tenant_id;
    // Consent validity is a business-local date (ADR-L5-0036 §3.4), not the database's UTC date.
    let today = business_today(state, &scope, InstitutionId(r.institution_id)).await?;
    let mut tx = tx!(state);
    let locked = lock_rel(&mut tx, tenant, id, None).await?;
    require_active(&locked)?;
    if communication_needs_consent(&cmd.topics) {
        let consent: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM student.guardian_consent WHERE tenant_id = $1 AND relationship_id = $2 \
             AND consent_type = 'data_sharing' AND status = 'granted' AND (valid_until IS NULL OR valid_until >= $3)",
        )
        .bind(tenant.as_uuid())
        .bind(id.as_uuid())
        .bind(today)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?;
        if consent == 0 {
            return Err(KernelError::Conflict("sharing student information with this guardian needs a granted data_sharing consent".into()));
        }
    }
    let mut channels: Vec<&str> = cmd.channels.iter().map(|c| c.as_str()).collect();
    channels.dedup();
    let mut topics: Vec<&str> = cmd.topics.iter().map(|t| t.as_str()).collect();
    topics.dedup();
    let c: GuardianCommunication = kit::insert(
        &mut tx,
        COMM,
        json!({
            "id": Uuid::now_v7(), "tenant_id": tenant.as_uuid(), "institution_id": r.institution_id,
            "relationship_id": id.as_uuid(), "student_id": r.student_id.as_uuid(),
            "channels": channels, "topics": topics, "status": AssignmentStatus::Active.as_str(),
        }),
    )
    .await
    .map_err(|e| {
        unique_conflict(
            e,
            "a communication authorisation is already current; revoke it first",
        )
    })?;
    StatusEvent {
        event_type: GUARDIAN_COMMUNICATION_AUTHORISED,
        schema: GUARDIAN_COMMUNICATION_AUTHORISED_SCHEMA,
        aggregate_id: c.id,
        institution_id: r.institution_id,
        student: Some(&student),
        from: None,
        to: c.status.as_str(),
        version: c.version,
        reason: None,
        process_instance_id: None,
    }
    .emit(&mut tx, &scope, actor)
    .await?;
    kit::audit(
        &mut tx,
        &scope,
        COMM,
        "authorise",
        c.id,
        json!({"relationshipId": id, "channels": channels, "topics": topics}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(c)
}

pub async fn revoke_communication(
    state: &StudentState,
    ctx: &AuthContext,
    id: GuardianRelationshipId,
    communication_id: Uuid,
    expected: Version,
    reason: Option<String>,
    actor: &Actor,
) -> Result<GuardianCommunication, KernelError> {
    let RelCmd { r, student, scope } = load_rel(state, ctx, id).await?;
    let tenant = ctx.scope.tenant_id;
    let mut tx = tx!(state);
    let c: GuardianCommunication = kit::lock(
        &mut tx,
        COMM,
        "guardian communication",
        tenant,
        communication_id,
    )
    .await?;
    if c.relationship_id != id {
        return Err(KernelError::NotFound {
            entity: "guardian communication",
        });
    }
    kit::check_version(c.version, expected)?;
    if !assignment_can_transition(c.status, AssignmentStatus::Revoked) {
        return Err(kit::illegal(
            "the communication authorisation",
            c.status,
            AssignmentStatus::Revoked,
        ));
    }
    let updated: GuardianCommunication = kit::patch(
        &mut tx,
        COMM,
        tenant,
        communication_id,
        json!({
            "status": AssignmentStatus::Revoked.as_str(), "status_reason": reason,
        }),
    )
    .await?;
    StatusEvent {
        event_type: GUARDIAN_COMMUNICATION_REVOKED,
        schema: GUARDIAN_COMMUNICATION_REVOKED_SCHEMA,
        aggregate_id: c.id,
        institution_id: r.institution_id,
        student: Some(&student),
        from: Some(c.status.as_str()),
        to: updated.status.as_str(),
        version: updated.version,
        reason: reason.as_deref(),
        process_instance_id: None,
    }
    .emit(&mut tx, &scope, actor)
    .await?;
    kit::audit(
        &mut tx,
        &scope,
        COMM,
        "revoke",
        c.id,
        json!({"reason": reason}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(updated)
}

// ---- student-level assignments: emergency contacts, financial responsibility, pickup ----------

/// Resolves an optional relationship reference of the student: it must be active.
async fn active_relationship_of(
    state: &StudentState,
    tenant: TenantId,
    student_id: StudentId,
    id: Option<GuardianRelationshipId>,
    field: &str,
) -> Result<Option<(GuardianRelationship, Guardian)>, KernelError> {
    let Some(id) = id else { return Ok(None) };
    let r: GuardianRelationship = kit::fetch(&state.pool, REL, REL_ENTITY, tenant, id.as_uuid())
        .await
        .map_err(|_| kit::invalid(field, "not_found", "unknown guardian relationship"))?;
    if r.student_id != student_id {
        return Err(kit::invalid(
            field,
            "other_student",
            "the relationship belongs to another student",
        ));
    }
    require_active(&r)?;
    let g = get_guardian(state, tenant, r.guardian_id).await?;
    Ok(Some((r, g)))
}

#[allow(clippy::too_many_arguments)]
async fn assignment_event(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    (event_type, schema): (&'static str, &'static str),
    id: Uuid,
    student: &StudentRow,
    from: Option<AssignmentStatus>,
    to: AssignmentStatus,
    version: i64,
    reason: Option<&str>,
    actor: &Actor,
) -> Result<(), KernelError> {
    StatusEvent {
        event_type,
        schema,
        aggregate_id: id,
        institution_id: scope
            .institution_id
            .map(|i| i.as_uuid())
            .unwrap_or_default(),
        student: Some(student),
        from: from.map(|f| f.as_str()),
        to: to.as_str(),
        version,
        reason,
        process_instance_id: None,
    }
    .emit(conn, scope, actor)
    .await
}

pub async fn add_emergency_contact(
    state: &StudentState,
    ctx: &AuthContext,
    student_id: StudentId,
    cmd: NewEmergencyContact,
    actor: &Actor,
) -> Result<EmergencyContact, KernelError> {
    if !(1..=9).contains(&cmd.priority) {
        return Err(kit::invalid("priority", "range", "priority is 1 … 9"));
    }
    let target = kit::writable_student(state, ctx, student_id).await?;
    let tenant = ctx.scope.tenant_id;
    let rel = active_relationship_of(
        state,
        tenant,
        student_id,
        cmd.guardian_relationship_id,
        "guardianRelationshipId",
    )
    .await?;
    let (name, relation, phone) = match &rel {
        Some((r, g)) => (
            [Some(g.given_name.clone()), g.family_name.clone()]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" "),
            r.relationship_type.as_str().to_owned(),
            trimmed(cmd.phone.clone()).or_else(|| g.phone.clone()),
        ),
        None => (
            kit::required_text("name", cmd.name.as_deref())?,
            kit::required_text("relation", cmd.relation.as_deref())?,
            trimmed(cmd.phone.clone()),
        ),
    };
    let phone = phone.ok_or_else(|| {
        kit::invalid(
            "phone",
            "required",
            "an emergency contact needs a phone number",
        )
    })?;
    let mut tx = tx!(state);
    let c: EmergencyContact = kit::insert(&mut tx, EMERGENCY, json!({
        "id": Uuid::now_v7(), "tenant_id": tenant.as_uuid(), "institution_id": target.institution_id().as_uuid(),
        "student_id": student_id.as_uuid(), "relationship_id": rel.as_ref().map(|(r, _)| r.id),
        "name": name, "relation": relation, "phone": phone, "alternate_phone": trimmed(cmd.alternate_phone),
        "priority": cmd.priority, "status": AssignmentStatus::Active.as_str(),
    })).await.map_err(|e| unique_conflict(e, "another current emergency contact has this priority"))?;
    assignment_event(
        &mut tx,
        &target.scope,
        (
            EMERGENCY_CONTACT_ASSIGNED,
            EMERGENCY_CONTACT_ASSIGNED_SCHEMA,
        ),
        c.id.as_uuid(),
        &target.student,
        None,
        c.status,
        c.version,
        None,
        actor,
    )
    .await?;
    kit::audit(
        &mut tx,
        &target.scope,
        EMERGENCY,
        "assign",
        c.id.as_uuid(),
        json!({"priority": c.priority, "relationshipId": c.relationship_id}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(c)
}

struct Assigned<T> {
    row: T,
    student: StudentRow,
    scope: ExecutionScope,
    institution_id: InstitutionId,
}

async fn load_assigned<T: serde::de::DeserializeOwned + Clone>(
    state: &StudentState,
    ctx: &AuthContext,
    table: &str,
    entity: &'static str,
    id: Uuid,
    student_of: fn(&T) -> StudentId,
) -> Result<Assigned<T>, KernelError> {
    let tenant = ctx.scope.tenant_id;
    let row: T = kit::fetch(&state.pool, table, entity, tenant, id).await?;
    let student = load_student(&state.pool, tenant, student_of(&row)).await?;
    require_student_writable(&student)?;
    let inst: Uuid = sqlx::query_scalar(&format!(
        "SELECT institution_id FROM student.{table} WHERE id = $1"
    ))
    .bind(id)
    .fetch_one(&state.pool)
    .await
    .map_err(map_db_error)?;
    let scope = kit::writable_institution(state, ctx, InstitutionId(inst)).await?;
    Ok(Assigned {
        row,
        student,
        scope,
        institution_id: InstitutionId(inst),
    })
}

pub async fn update_emergency_contact(
    state: &StudentState,
    ctx: &AuthContext,
    id: EmergencyContactId,
    expected: Version,
    cmd: EmergencyContactUpdate,
    actor: &Actor,
) -> Result<EmergencyContact, KernelError> {
    let Assigned { row, scope, .. } = load_assigned::<EmergencyContact>(
        state,
        ctx,
        EMERGENCY,
        "emergency contact",
        id.as_uuid(),
        |c| c.student_id,
    )
    .await?;
    if row.status != AssignmentStatus::Active {
        return Err(KernelError::Conflict(format!(
            "the emergency contact is {}",
            row.status
        )));
    }
    let mut changes = serde_json::Map::new();
    if let Some(p) = cmd.phone {
        changes.insert(
            "phone".into(),
            json!(kit::required_text("phone", Some(&p))?),
        );
    }
    if let Some(p) = cmd.alternate_phone {
        changes.insert("alternate_phone".into(), json!(trimmed(Some(p))));
    }
    if let Some(p) = cmd.priority {
        if !(1..=9).contains(&p) {
            return Err(kit::invalid("priority", "range", "priority is 1 … 9"));
        }
        changes.insert("priority".into(), json!(p));
    }
    if changes.is_empty() {
        return Err(kit::invalid("body", "no_change", "nothing to update"));
    }
    let tenant = ctx.scope.tenant_id;
    let mut tx = tx!(state);
    let locked: EmergencyContact = kit::lock(
        &mut tx,
        EMERGENCY,
        "emergency contact",
        tenant,
        id.as_uuid(),
    )
    .await?;
    kit::check_version(locked.version, expected)?;
    let fields: Vec<String> = changes.keys().map(|k| kit::camel_key(k)).collect();
    let c: EmergencyContact = kit::patch(
        &mut tx,
        EMERGENCY,
        tenant,
        id.as_uuid(),
        serde_json::Value::Object(changes),
    )
    .await
    .map_err(|e| unique_conflict(e, "another current emergency contact has this priority"))?;
    kit::audit(
        &mut tx,
        &scope,
        EMERGENCY,
        "update",
        id.as_uuid(),
        json!({"changedFields": fields}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(c)
}

/// Ends an assignment (emergency contact removal, responsibility end, pickup revocation).
#[allow(clippy::too_many_arguments)]
async fn end_assignment<T: serde::de::DeserializeOwned + Clone>(
    state: &StudentState,
    ctx: &AuthContext,
    table: &'static str,
    entity: &'static str,
    id: Uuid,
    expected: Version,
    reason: Option<String>,
    student_of: fn(&T) -> StudentId,
    status_of: fn(&T) -> AssignmentStatus,
    version_of: fn(&T) -> i64,
    event: (&'static str, &'static str),
    ends_effective: bool,
    actor: &Actor,
) -> Result<T, KernelError> {
    let Assigned {
        student,
        scope,
        institution_id,
        ..
    } = load_assigned::<T>(state, ctx, table, entity, id, student_of).await?;
    let tenant = ctx.scope.tenant_id;
    // The end date is a business-local date (ADR-L5-0036 §3.4), never the UTC date: between
    // midnight IST and 05:30 IST the UTC date is still "yesterday", before `effective_from`.
    let effective_to = if ends_effective {
        Some(business_today(state, &scope, institution_id).await?)
    } else {
        None
    };
    let mut tx = tx!(state);
    let locked: T = kit::lock(&mut tx, table, entity, tenant, id).await?;
    kit::check_version(version_of(&locked), expected)?;
    let from = status_of(&locked);
    if !assignment_can_transition(from, AssignmentStatus::Revoked) {
        return Err(kit::illegal(entity, from, AssignmentStatus::Revoked));
    }
    let mut changes = json!({});
    if let Some(to) = effective_to {
        changes["effective_to"] = json!(to);
    }
    changes["status"] = json!(AssignmentStatus::Revoked.as_str());
    changes["status_reason"] = json!(reason);
    let updated: T = kit::patch(&mut tx, table, tenant, id, changes).await?;
    assignment_event(
        &mut tx,
        &scope,
        event,
        id,
        &student,
        Some(from),
        AssignmentStatus::Revoked,
        version_of(&updated),
        reason.as_deref(),
        actor,
    )
    .await?;
    kit::audit(
        &mut tx,
        &scope,
        table,
        "end",
        id,
        json!({"fromStatus": from, "reason": reason}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(updated)
}

pub async fn remove_emergency_contact(
    state: &StudentState,
    ctx: &AuthContext,
    id: EmergencyContactId,
    expected: Version,
    reason: Option<String>,
    actor: &Actor,
) -> Result<EmergencyContact, KernelError> {
    end_assignment::<EmergencyContact>(
        state,
        ctx,
        EMERGENCY,
        "emergency contact",
        id.as_uuid(),
        expected,
        reason,
        |c| c.student_id,
        |c| c.status,
        |c| c.version,
        (EMERGENCY_CONTACT_REMOVED, EMERGENCY_CONTACT_REMOVED_SCHEMA),
        false,
        actor,
    )
    .await
}

pub async fn assign_financial_responsibility(
    state: &StudentState,
    ctx: &AuthContext,
    student_id: StudentId,
    cmd: NewFinancialResponsibility,
    actor: &Actor,
) -> Result<FinancialResponsibility, KernelError> {
    if !(cmd.share_percent > 0.0 && cmd.share_percent <= 100.0) {
        return Err(kit::invalid(
            "sharePercent",
            "range",
            "the share is more than 0 and at most 100 percent",
        ));
    }
    let target = kit::writable_student(state, ctx, student_id).await?;
    let tenant = ctx.scope.tenant_id;
    let (r, _) = active_relationship_of(
        state,
        tenant,
        student_id,
        Some(cmd.guardian_relationship_id),
        "guardianRelationshipId",
    )
    .await?
    .ok_or_else(|| {
        kit::invalid(
            "guardianRelationshipId",
            "required",
            "a guardian relationship is required",
        )
    })?;
    let today = business_today(state, &target.scope, target.institution_id()).await?;
    let from = cmd.effective_from.unwrap_or(today);
    if cmd.effective_to.is_some_and(|t| t < from) {
        return Err(kit::invalid(
            "effectiveTo",
            "before_start",
            "the end date is before the start date",
        ));
    }
    let mut tx = tx!(state);
    crate::shared::student::lock_student(&mut tx, tenant, student_id).await?;
    let active: Vec<FinancialResponsibility> = kit::list_by(
        &mut *tx,
        FINANCIAL,
        tenant,
        "student_id",
        student_id.as_uuid(),
        Some("active"),
    )
    .await?;
    let shares: Vec<(ResponsibilityType, f64)> = active
        .iter()
        .map(|f| (f.responsibility_type, f.share_percent))
        .collect();
    if max_share_with(&shares, (cmd.responsibility_type, cmd.share_percent)) > 100.0 + 1e-9 {
        return Err(KernelError::Conflict(
            "the assignment would exceed 100 percent of the student's financial responsibility"
                .into(),
        ));
    }
    let f: FinancialResponsibility = kit::insert(&mut tx, FINANCIAL, json!({
        "id": Uuid::now_v7(), "tenant_id": tenant.as_uuid(), "institution_id": target.institution_id().as_uuid(),
        "student_id": student_id.as_uuid(), "relationship_id": r.id.as_uuid(),
        "responsibility_type": cmd.responsibility_type.as_str(), "share_percent": cmd.share_percent,
        "effective_from": from, "effective_to": cmd.effective_to, "status": AssignmentStatus::Active.as_str(),
    })).await?;
    assignment_event(
        &mut tx,
        &target.scope,
        (
            FINANCIAL_RESPONSIBILITY_ASSIGNED,
            FINANCIAL_RESPONSIBILITY_ASSIGNED_SCHEMA,
        ),
        f.id,
        &target.student,
        None,
        f.status,
        f.version,
        None,
        actor,
    )
    .await?;
    kit::audit(&mut tx, &target.scope, FINANCIAL, "assign", f.id, json!({"relationshipId": r.id, "type": cmd.responsibility_type, "sharePercent": cmd.share_percent}), actor).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(f)
}

pub async fn end_financial_responsibility(
    state: &StudentState,
    ctx: &AuthContext,
    id: Uuid,
    expected: Version,
    reason: Option<String>,
    actor: &Actor,
) -> Result<FinancialResponsibility, KernelError> {
    end_assignment::<FinancialResponsibility>(
        state,
        ctx,
        FINANCIAL,
        "financial responsibility",
        id,
        expected,
        reason,
        |c| c.student_id,
        |c| c.status,
        |c| c.version,
        (
            FINANCIAL_RESPONSIBILITY_ENDED,
            FINANCIAL_RESPONSIBILITY_ENDED_SCHEMA,
        ),
        true,
        actor,
    )
    .await
}

pub async fn grant_pickup(
    state: &StudentState,
    ctx: &AuthContext,
    student_id: StudentId,
    cmd: NewPickupAuthorisation,
    actor: &Actor,
) -> Result<PickupAuthorisation, KernelError> {
    let target = kit::writable_student(state, ctx, student_id).await?;
    let tenant = ctx.scope.tenant_id;
    let rel = active_relationship_of(
        state,
        tenant,
        student_id,
        cmd.guardian_relationship_id,
        "guardianRelationshipId",
    )
    .await?;
    let (name, phone, id_doc) = match &rel {
        Some((_, g)) => (
            [Some(g.given_name.clone()), g.family_name.clone()]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" "),
            trimmed(cmd.phone.clone()).or_else(|| g.phone.clone()),
            trimmed(cmd.id_document_ref.clone()),
        ),
        None => (
            kit::required_text("personName", cmd.person_name.as_deref())?,
            trimmed(cmd.phone.clone()),
            Some(
                kit::required_text("idDocumentRef", cmd.id_document_ref.as_deref()).map_err(
                    |_| {
                        kit::invalid(
                            "idDocumentRef",
                            "required",
                            "a person who is not a guardian needs an identity document reference",
                        )
                    },
                )?,
            ),
        ),
    };
    let today = business_today(state, &target.scope, target.institution_id()).await?;
    let from = cmd.valid_from.unwrap_or(today);
    if cmd.valid_until.is_some_and(|u| u < from) {
        return Err(kit::invalid(
            "validUntil",
            "before_start",
            "the end date is before the start date",
        ));
    }
    let mut tx = tx!(state);
    let p: PickupAuthorisation = kit::insert(&mut tx, PICKUP, json!({
        "id": Uuid::now_v7(), "tenant_id": tenant.as_uuid(), "institution_id": target.institution_id().as_uuid(),
        "student_id": student_id.as_uuid(), "relationship_id": rel.as_ref().map(|(r, _)| r.id),
        "person_name": name, "phone": phone, "id_document_ref": id_doc,
        "valid_from": from, "valid_until": cmd.valid_until, "status": AssignmentStatus::Active.as_str(),
    })).await?;
    assignment_event(
        &mut tx,
        &target.scope,
        (
            PICKUP_AUTHORISATION_GRANTED,
            PICKUP_AUTHORISATION_GRANTED_SCHEMA,
        ),
        p.id.as_uuid(),
        &target.student,
        None,
        p.status,
        p.version,
        None,
        actor,
    )
    .await?;
    kit::audit(
        &mut tx,
        &target.scope,
        PICKUP,
        "grant",
        p.id.as_uuid(),
        json!({"relationshipId": p.relationship_id, "personName": p.person_name}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(p)
}

pub async fn revoke_pickup(
    state: &StudentState,
    ctx: &AuthContext,
    id: PickupAuthorisationId,
    expected: Version,
    reason: Option<String>,
    actor: &Actor,
) -> Result<PickupAuthorisation, KernelError> {
    end_assignment::<PickupAuthorisation>(
        state,
        ctx,
        PICKUP,
        "pickup authorisation",
        id.as_uuid(),
        expected,
        reason,
        |c| c.student_id,
        |c| c.status,
        |c| c.version,
        (
            PICKUP_AUTHORISATION_REVOKED,
            PICKUP_AUTHORISATION_REVOKED_SCHEMA,
        ),
        false,
        actor,
    )
    .await
}

pub async fn list_for_student<T: serde::de::DeserializeOwned>(
    state: &StudentState,
    tenant: TenantId,
    table: &'static str,
    student_id: StudentId,
    status: Option<AssignmentStatus>,
) -> Result<Vec<T>, KernelError> {
    kit::list_by(
        &state.pool,
        table,
        tenant,
        "student_id",
        student_id.as_uuid(),
        status.map(|s| s.as_str()),
    )
    .await
}

pub async fn get_row<T: serde::de::DeserializeOwned>(
    state: &StudentState,
    tenant: TenantId,
    table: &'static str,
    entity: &'static str,
    id: Uuid,
) -> Result<T, KernelError> {
    kit::fetch(&state.pool, table, entity, tenant, id).await
}

pub const TABLE_EMERGENCY: &str = EMERGENCY;
pub const TABLE_FINANCIAL: &str = FINANCIAL;
pub const TABLE_PICKUP: &str = PICKUP;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emergency_only_needs_no_consent() {
        assert!(!communication_needs_consent(&[
            CommunicationTopic::Emergency
        ]));
        assert!(communication_needs_consent(&[
            CommunicationTopic::Emergency,
            CommunicationTopic::Academic
        ]));
    }
}
