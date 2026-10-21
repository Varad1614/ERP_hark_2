//! Academic mobility: credit transfer / recognition (L3-C-032), programme migration (C-033),
//! academic break and rejoining (C-034/035).

use super::domain::*;
use super::repo::{append_entry, ledger_updated, published_curriculum, required, text, NewEntry};
use crate::registration::support::{
    audit, check_version, conflict, emit, invalid, parse_enum, set_standing, standing_of,
    status_changed, write_student, StudentScope,
};
use crate::shared::approval::{business_key, process, request_approval, ApprovalOutcome};
use crate::shared::student::{lock_context, set_context_progress, StudentContextRow};
use crate::shared::time::business_today;
use crate::shared::{Actor, StudentState};
use chrono::{DateTime, NaiveDate, Utc};
use serde_json::json;
use sqlx::{PgConnection, PgExecutor, PgPool};
use std::collections::BTreeMap;
use sutra_contracts::institution::status::PublicationStatus;
use sutra_contracts::student::events::progression::*;
use sutra_contracts::student::ids::StudentId;
use sutra_contracts::student::status::{
    AcademicBreakStatus, AcademicStanding, CaseStatus, CreditEntryKind,
};
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::TenantId;
use sutra_persistence::map_db_error;
use uuid::Uuid;

const TRANSFER: KernelError = KernelError::NotFound {
    entity: "credit transfer",
};
const MIGRATION: KernelError = KernelError::NotFound {
    entity: "programme migration",
};
const BREAK: KernelError = KernelError::NotFound {
    entity: "academic break",
};

/// A registration still in progress blocks migration / break (finalise or cancel it first).
async fn open_registration(
    conn: &mut PgConnection,
    ctx: &StudentContextRow,
) -> Result<bool, KernelError> {
    let n: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM student.registration WHERE tenant_id = $1 AND student_context_id = $2 \
         AND status IN ('draft', 'submitted', 'returned', 'approved', 'registered')",
    )
    .bind(ctx.tenant_id.as_uuid())
    .bind(ctx.id.as_uuid())
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(n > 0)
}

fn case_status(raw: &str) -> Result<CaseStatus, KernelError> {
    parse_enum(raw, CaseStatus::parse, "case status")
}

fn require_case(from: CaseStatus, to: CaseStatus, what: &str) -> Result<(), KernelError> {
    if !case_can_transition(from, to) {
        return Err(conflict(format!("{what} is {from}; it cannot become {to}")));
    }
    Ok(())
}

async fn same_context(ss: &StudentScope, ctx_id: Uuid, what: &str) -> Result<(), KernelError> {
    if ss.context.id.as_uuid() != ctx_id {
        return Err(conflict(format!(
            "the {what} belongs to a closed student context"
        )));
    }
    Ok(())
}

// ================================================================== credit transfer

const TRANSFER_COLUMNS: &str = "id, student_id, student_context_id, institution_id, kind, source_institution, source_programme, \
    external_reference, evidence_note, items, credits_requested::float8 AS credits_requested, \
    credits_recognised::float8 AS credits_recognised, status, status_reason, revision_requested, process_instance_id, \
    version, created_at, updated_at";

#[derive(sqlx::FromRow)]
struct TransferRow {
    id: Uuid,
    student_id: Uuid,
    student_context_id: Uuid,
    institution_id: Uuid,
    kind: String,
    source_institution: String,
    source_programme: Option<String>,
    external_reference: Option<String>,
    evidence_note: Option<String>,
    items: serde_json::Value,
    credits_requested: f64,
    credits_recognised: Option<f64>,
    status: String,
    status_reason: Option<String>,
    revision_requested: bool,
    process_instance_id: Option<String>,
    version: i64,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl TransferRow {
    fn dto(self) -> Result<(CreditTransfer, Uuid), KernelError> {
        let ctx = self.student_context_id;
        Ok((
            CreditTransfer {
                items: serde_json::from_value(self.items).map_err(KernelError::internal)?,
                status: case_status(&self.status)?,
                id: self.id,
                student_id: self.student_id,
                institution_id: self.institution_id,
                kind: self.kind,
                source_institution: self.source_institution,
                source_programme: self.source_programme,
                external_reference: self.external_reference,
                evidence_note: self.evidence_note,
                credits_requested: self.credits_requested,
                credits_recognised: self.credits_recognised,
                status_reason: self.status_reason,
                revision_requested: self.revision_requested,
                process_instance_id: self.process_instance_id,
                version: self.version,
                created_at: self.created_at,
                updated_at: self.updated_at,
            },
            ctx,
        ))
    }
}

async fn transfer_row<'e, E: PgExecutor<'e>>(
    ex: E,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<(CreditTransfer, Uuid), KernelError> {
    let sql = format!(
        "SELECT {TRANSFER_COLUMNS} FROM student.credit_transfer WHERE id = $1 AND tenant_id = $2{}",
        if lock { " FOR UPDATE" } else { "" }
    );
    sqlx::query_as::<_, TransferRow>(&sql)
        .bind(id)
        .bind(tenant.as_uuid())
        .fetch_optional(ex)
        .await
        .map_err(map_db_error)?
        .ok_or(TRANSFER)?
        .dto()
}

pub async fn get_transfer(
    pool: &PgPool,
    tenant: TenantId,
    id: Uuid,
) -> Result<CreditTransfer, KernelError> {
    Ok(transfer_row(pool, tenant, id, false).await?.0)
}

pub async fn list_transfers(
    pool: &PgPool,
    tenant: TenantId,
    student_id: StudentId,
) -> Result<Vec<CreditTransfer>, KernelError> {
    crate::shared::student::load_student(pool, tenant, student_id).await?;
    let sql = format!(
        "SELECT {TRANSFER_COLUMNS} FROM student.credit_transfer WHERE tenant_id = $1 AND student_id = $2 ORDER BY created_at DESC, id DESC"
    );
    sqlx::query_as::<_, TransferRow>(&sql)
        .bind(tenant.as_uuid())
        .bind(student_id.as_uuid())
        .fetch_all(pool)
        .await
        .map_err(map_db_error)?
        .into_iter()
        .map(|r| r.dto().map(|x| x.0))
        .collect()
}

/// Validates the items against the curriculum (equivalency: a mapped component must belong to
/// the student's curriculum and not already be earned).
async fn validate_items(
    state: &StudentState,
    ss: &StudentScope,
    items: &[CreditTransferItem],
) -> Result<f64, KernelError> {
    if items.is_empty() {
        return Err(invalid(
            "items",
            "required",
            "list at least one course to recognise",
        ));
    }
    let comps = match ss.context.curriculum_version_id {
        Some(cv) => {
            state
                .references
                .curriculum_components(&ss.scope, cv)
                .await?
        }
        None => vec![],
    };
    let earned: Vec<Uuid> = sqlx::query_scalar(
        "SELECT curriculum_component_id FROM student.credit_ledger_entry WHERE tenant_id = $1 AND student_context_id = $2 \
         AND curriculum_component_id IS NOT NULL GROUP BY 1 \
         HAVING sum(CASE WHEN kind IN ('revoked', 'expired') THEN -credits ELSE credits END) > 0",
    )
    .bind(ss.scope.tenant_id.as_uuid())
    .bind(ss.context.id.as_uuid())
    .fetch_all(&state.pool)
    .await
    .map_err(map_db_error)?;
    let mut total = 0.0;
    for (i, it) in items.iter().enumerate() {
        if it.title.trim().is_empty() || it.title.chars().count() > 200 {
            return Err(invalid(
                &format!("items[{i}].title"),
                "required",
                "1-200 characters",
            ));
        }
        if !(it.credits > 0.0 && it.credits <= 100.0) {
            return Err(invalid(
                &format!("items[{i}].credits"),
                "out_of_range",
                "greater than 0, at most 100",
            ));
        }
        if let Some(c) = it.curriculum_component_id {
            if !comps.iter().any(|x| x.id == c) {
                return Err(invalid(
                    &format!("items[{i}].curriculumComponentId"),
                    "not_in_curriculum",
                    "not a component of the student's curriculum",
                ));
            }
            if earned.contains(&c) {
                return Err(invalid(
                    &format!("items[{i}].curriculumComponentId"),
                    "already_earned",
                    "the component's credits are already earned",
                ));
            }
        }
        total += it.credits;
    }
    Ok(total)
}

pub async fn create_transfer(
    state: &StudentState,
    tenant: TenantId,
    student_id: StudentId,
    cmd: CreateCreditTransfer,
    actor: &Actor,
) -> Result<CreditTransfer, KernelError> {
    let ss = write_student(state, tenant, student_id, true).await?;
    let kind = cmd.kind.unwrap_or_else(|| "transferred_in".into());
    if !matches!(kind.as_str(), "transferred_in" | "recognised") {
        return Err(invalid(
            "kind",
            "invalid",
            "`transferred_in` or `recognised`",
        ));
    }
    let source = required(Some(cmd.source_institution), "sourceInstitution", 200)?;
    let total = validate_items(state, &ss, &cmd.items).await?;
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let sql = format!(
        "INSERT INTO student.credit_transfer (tenant_id, institution_id, student_id, student_context_id, kind, source_institution, \
         source_programme, external_reference, evidence_note, items, credits_requested) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) RETURNING {TRANSFER_COLUMNS}"
    );
    let (t, _) = sqlx::query_as::<_, TransferRow>(&sql)
        .bind(tenant.as_uuid())
        .bind(ss.context.institution_id.as_uuid())
        .bind(student_id.as_uuid())
        .bind(ss.context.id.as_uuid())
        .bind(&kind)
        .bind(&source)
        .bind(text(cmd.source_programme, "sourceProgramme", 200)?)
        .bind(text(cmd.external_reference, "externalReference", 200)?)
        .bind(text(cmd.evidence_note, "evidenceNote", 2000)?)
        .bind(serde_json::to_value(&cmd.items).map_err(KernelError::internal)?)
        .bind(total)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?
        .dto()?;
    audit(
        &mut tx,
        &ss.scope,
        "student.credit_transfer.create",
        "credit_transfer",
        t.id,
        json!({"kind": kind, "creditsRequested": total}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(t)
}

pub async fn update_transfer(
    state: &StudentState,
    tenant: TenantId,
    id: Uuid,
    expected: Version,
    cmd: UpdateCreditTransfer,
    actor: &Actor,
) -> Result<CreditTransfer, KernelError> {
    let (cur, ctx_id) = transfer_row(&state.pool, tenant, id, false).await?;
    let ss = write_student(state, tenant, StudentId(cur.student_id), false).await?;
    same_context(&ss, ctx_id, "credit transfer").await?;
    let items = cmd.items.unwrap_or(cur.items.clone());
    let total = validate_items(state, &ss, &items).await?;
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let (t, _) = transfer_row(&mut *tx, tenant, id, true).await?;
    check_version(t.version, expected)?;
    if t.status != CaseStatus::Requested {
        return Err(conflict(format!(
            "credit transfer is {}; only requested cases can change",
            t.status
        )));
    }
    let sql = format!(
        "UPDATE student.credit_transfer SET source_programme = COALESCE($2, source_programme), \
         external_reference = COALESCE($3, external_reference), evidence_note = COALESCE($4, evidence_note), items = $5, \
         credits_requested = $6, version = version + 1, updated_at = now() WHERE id = $1 RETURNING {TRANSFER_COLUMNS}"
    );
    let (t2, _) = sqlx::query_as::<_, TransferRow>(&sql)
        .bind(id)
        .bind(text(cmd.source_programme, "sourceProgramme", 200)?)
        .bind(text(cmd.external_reference, "externalReference", 200)?)
        .bind(text(cmd.evidence_note, "evidenceNote", 2000)?)
        .bind(serde_json::to_value(&items).map_err(KernelError::internal)?)
        .bind(total)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?
        .dto()?;
    audit(
        &mut tx,
        &ss.scope,
        "student.credit_transfer.update",
        "credit_transfer",
        id,
        json!({"creditsRequested": total}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(t2)
}

async fn set_transfer_status(
    conn: &mut PgConnection,
    id: Uuid,
    to: CaseStatus,
    reason: Option<&str>,
    pid: Option<&str>,
    revision: bool,
    recognised: Option<f64>,
) -> Result<CreditTransfer, KernelError> {
    let sql = format!(
        "UPDATE student.credit_transfer SET status = $2, status_reason = $3, process_instance_id = COALESCE($4, process_instance_id), \
         revision_requested = $5, credits_recognised = COALESCE($6, credits_recognised), version = version + 1, \
         updated_at = now() WHERE id = $1 RETURNING {TRANSFER_COLUMNS}"
    );
    Ok(sqlx::query_as::<_, TransferRow>(&sql)
        .bind(id)
        .bind(to.as_str())
        .bind(reason)
        .bind(pid)
        .bind(revision)
        .bind(recognised)
        .fetch_one(&mut *conn)
        .await
        .map_err(map_db_error)?
        .dto()?
        .0)
}

/// Approved → recognised credits appended to the ledger → completed.
async fn approve_transfer(
    conn: &mut PgConnection,
    ss: &StudentScope,
    t: &CreditTransfer,
    actor: &Actor,
) -> Result<CreditTransfer, KernelError> {
    let ctx = lock_context(conn, ss.scope.tenant_id, ss.context.id).await?;
    let approved = set_transfer_status(
        conn,
        t.id,
        CaseStatus::Approved,
        None,
        None,
        false,
        Some(t.credits_requested),
    )
    .await?;
    let kind = if t.kind == "recognised" {
        CreditEntryKind::Recognised
    } else {
        CreditEntryKind::TransferredIn
    };
    let mut ids = vec![];
    for it in &t.items {
        ids.push(
            append_entry(
                conn,
                &ctx,
                &NewEntry {
                    kind,
                    credits: it.credits,
                    component: it.curriculum_component_id,
                    term: None,
                    source_type: "credit_transfer",
                    source_id: Some(t.id),
                    reverses: None,
                    approved: true,
                    note: Some(&it.title),
                },
                actor,
            )
            .await?,
        );
    }
    emit(
        conn,
        &ss.scope,
        CREDIT_TRANSFER_APPROVED,
        CREDIT_TRANSFER_APPROVED_SCHEMA,
        t.id,
        &status_changed(
            t.id,
            &ss.scope,
            Some(t.student_id),
            Some("under_review"),
            "approved",
            approved.version,
            None,
            t.process_instance_id.as_deref(),
            ss.subject_ref(),
        ),
        actor,
    )
    .await?;
    ledger_updated(conn, &ss.scope, &ctx, ids, actor).await?;
    let done =
        set_transfer_status(conn, t.id, CaseStatus::Completed, None, None, false, None).await?;
    audit(
        conn,
        &ss.scope,
        "student.credit_transfer.approve",
        "credit_transfer",
        t.id,
        json!({"creditsRecognised": t.credits_requested}),
        actor,
    )
    .await?;
    Ok(done)
}

/// Submits for academic recognition (WorkflowPort `student.credit_transfer`). Evidence is
/// required (incomplete evidence → 422, the case stays `requested`).
pub async fn submit_transfer(
    state: &StudentState,
    tenant: TenantId,
    id: Uuid,
    expected: Version,
    actor: &Actor,
) -> Result<CreditTransfer, KernelError> {
    let (cur, ctx_id) = transfer_row(&state.pool, tenant, id, false).await?;
    let ss = write_student(state, tenant, StudentId(cur.student_id), true).await?;
    same_context(&ss, ctx_id, "credit transfer").await?;
    check_version(cur.version, expected)?;
    require_case(cur.status, CaseStatus::UnderReview, "credit transfer")?;
    if cur.evidence_note.is_none() && cur.external_reference.is_none() {
        return Err(invalid(
            "evidenceNote",
            "evidence_required",
            "supporting evidence (note or external reference) is required",
        ));
    }
    validate_items(state, &ss, &cur.items).await?;
    let mut vars = BTreeMap::new();
    vars.insert("creditTransferId".into(), json!(id));
    vars.insert("credits".into(), json!(cur.credits_requested));
    let outcome = request_approval(
        state,
        &ss.scope,
        process::CREDIT_TRANSFER,
        business_key("credit_transfer", id, cur.version),
        vars,
    )
    .await?;
    let pid = outcome.process_instance_id().to_owned();
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let (t, _) = transfer_row(&mut *tx, tenant, id, true).await?;
    check_version(t.version, expected)?;
    require_case(t.status, CaseStatus::UnderReview, "credit transfer")?;
    let t = set_transfer_status(
        &mut tx,
        id,
        CaseStatus::UnderReview,
        None,
        Some(&pid),
        false,
        None,
    )
    .await?;
    audit(
        &mut tx,
        &ss.scope,
        "student.credit_transfer.submit",
        "credit_transfer",
        id,
        json!({"processInstanceId": pid}),
        actor,
    )
    .await?;
    let t = match outcome {
        ApprovalOutcome::Approved { .. } => {
            approve_transfer(
                &mut tx,
                &ss,
                &t,
                &Actor::system(actor.correlation_id.clone()),
            )
            .await?
        }
        ApprovalOutcome::Pending { .. } => t,
    };
    tx.commit().await.map_err(map_db_error)?;
    Ok(t)
}

/// Recognition decision: approve (ledger), reject (no ledger entry) or revise (back to requested).
pub async fn decide_transfer(
    state: &StudentState,
    tenant: TenantId,
    id: Uuid,
    expected: Version,
    decision: RecognitionDecision,
    reason: Option<String>,
    actor: &Actor,
) -> Result<CreditTransfer, KernelError> {
    let (cur, ctx_id) = transfer_row(&state.pool, tenant, id, false).await?;
    let ss = write_student(state, tenant, StudentId(cur.student_id), false).await?;
    same_context(&ss, ctx_id, "credit transfer").await?;
    if decision != RecognitionDecision::Approve && reason.is_none() {
        return Err(invalid(
            "reason",
            "required",
            "a rejection or revision request needs a reason",
        ));
    }
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let (t, _) = transfer_row(&mut *tx, tenant, id, true).await?;
    check_version(t.version, expected)?;
    if t.status != CaseStatus::UnderReview {
        return Err(conflict(format!(
            "credit transfer is {}; nothing to decide",
            t.status
        )));
    }
    let t = match decision {
        RecognitionDecision::Approve => approve_transfer(&mut tx, &ss, &t, actor).await?,
        RecognitionDecision::Reject => {
            let r = set_transfer_status(
                &mut tx,
                id,
                CaseStatus::Rejected,
                reason.as_deref(),
                None,
                false,
                None,
            )
            .await?;
            audit(
                &mut tx,
                &ss.scope,
                "student.credit_transfer.reject",
                "credit_transfer",
                id,
                json!({"reason": reason}),
                actor,
            )
            .await?;
            r
        }
        RecognitionDecision::Revise => {
            let r = set_transfer_status(
                &mut tx,
                id,
                CaseStatus::Requested,
                reason.as_deref(),
                None,
                true,
                None,
            )
            .await?;
            audit(
                &mut tx,
                &ss.scope,
                "student.credit_transfer.request_revision",
                "credit_transfer",
                id,
                json!({"reason": reason}),
                actor,
            )
            .await?;
            r
        }
    };
    tx.commit().await.map_err(map_db_error)?;
    Ok(t)
}

pub async fn withdraw_transfer(
    state: &StudentState,
    tenant: TenantId,
    id: Uuid,
    expected: Version,
    reason: Option<String>,
    actor: &Actor,
) -> Result<CreditTransfer, KernelError> {
    let (cur, _) = transfer_row(&state.pool, tenant, id, false).await?;
    let ss = write_student(state, tenant, StudentId(cur.student_id), false).await?;
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let (t, _) = transfer_row(&mut *tx, tenant, id, true).await?;
    check_version(t.version, expected)?;
    require_case(t.status, CaseStatus::Withdrawn, "credit transfer")?;
    let t = set_transfer_status(
        &mut tx,
        id,
        CaseStatus::Withdrawn,
        reason.as_deref(),
        None,
        false,
        None,
    )
    .await?;
    audit(
        &mut tx,
        &ss.scope,
        "student.credit_transfer.withdraw",
        "credit_transfer",
        id,
        json!({"reason": reason}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(t)
}

// ================================================================== programme migration

const MIGRATION_COLUMNS: &str = "id, student_id, student_context_id, institution_id, from_programme_id, from_curriculum_version_id, \
    from_term_no, to_programme_id, to_curriculum_version_id, to_term_no, credit_mapping, reason, effective_on, status, \
    status_reason, process_instance_id, version, created_at, updated_at";

#[derive(sqlx::FromRow)]
struct MigrationRow {
    id: Uuid,
    student_id: Uuid,
    student_context_id: Uuid,
    institution_id: Uuid,
    from_programme_id: Uuid,
    from_curriculum_version_id: Option<Uuid>,
    from_term_no: Option<i32>,
    to_programme_id: Uuid,
    to_curriculum_version_id: Uuid,
    to_term_no: i32,
    credit_mapping: serde_json::Value,
    reason: String,
    effective_on: Option<NaiveDate>,
    status: String,
    status_reason: Option<String>,
    process_instance_id: Option<String>,
    version: i64,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl MigrationRow {
    fn dto(self) -> Result<(ProgrammeMigration, Uuid), KernelError> {
        let ctx = self.student_context_id;
        Ok((
            ProgrammeMigration {
                credit_mapping: serde_json::from_value(self.credit_mapping)
                    .map_err(KernelError::internal)?,
                status: case_status(&self.status)?,
                id: self.id,
                student_id: self.student_id,
                institution_id: self.institution_id,
                from_programme_id: self.from_programme_id,
                from_curriculum_version_id: self.from_curriculum_version_id,
                from_term_no: self.from_term_no,
                to_programme_id: self.to_programme_id,
                to_curriculum_version_id: self.to_curriculum_version_id,
                to_term_no: self.to_term_no,
                reason: self.reason,
                effective_on: self.effective_on,
                status_reason: self.status_reason,
                process_instance_id: self.process_instance_id,
                version: self.version,
                created_at: self.created_at,
                updated_at: self.updated_at,
            },
            ctx,
        ))
    }
}

async fn migration_row<'e, E: PgExecutor<'e>>(
    ex: E,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<(ProgrammeMigration, Uuid), KernelError> {
    let sql = format!(
        "SELECT {MIGRATION_COLUMNS} FROM student.programme_migration WHERE id = $1 AND tenant_id = $2{}",
        if lock { " FOR UPDATE" } else { "" }
    );
    sqlx::query_as::<_, MigrationRow>(&sql)
        .bind(id)
        .bind(tenant.as_uuid())
        .fetch_optional(ex)
        .await
        .map_err(map_db_error)?
        .ok_or(MIGRATION)?
        .dto()
}

pub async fn get_migration(
    pool: &PgPool,
    tenant: TenantId,
    id: Uuid,
) -> Result<ProgrammeMigration, KernelError> {
    Ok(migration_row(pool, tenant, id, false).await?.0)
}

pub async fn list_migrations(
    pool: &PgPool,
    tenant: TenantId,
    student_id: StudentId,
) -> Result<Vec<ProgrammeMigration>, KernelError> {
    crate::shared::student::load_student(pool, tenant, student_id).await?;
    let sql = format!(
        "SELECT {MIGRATION_COLUMNS} FROM student.programme_migration WHERE tenant_id = $1 AND student_id = $2 ORDER BY created_at DESC, id DESC"
    );
    sqlx::query_as::<_, MigrationRow>(&sql)
        .bind(tenant.as_uuid())
        .bind(student_id.as_uuid())
        .fetch_all(pool)
        .await
        .map_err(map_db_error)?
        .into_iter()
        .map(|r| r.dto().map(|x| x.0))
        .collect()
}

/// Requests a programme migration (eligibility + destination + curriculum mapping, L3-C-033).
pub async fn create_migration(
    state: &StudentState,
    tenant: TenantId,
    student_id: StudentId,
    cmd: CreateMigration,
    actor: &Actor,
) -> Result<ProgrammeMigration, KernelError> {
    let ss = write_student(state, tenant, student_id, true).await?;
    let reason = required(Some(cmd.reason), "reason", 1000)?;
    let from = ss
        .context
        .programme_id
        .ok_or_else(|| conflict("the student context has no programme"))?;
    if from == cmd.to_programme_id {
        return Err(invalid(
            "toProgrammeId",
            "same_programme",
            "the student is already in this programme",
        ));
    }
    let prog = state
        .references
        .programme(&ss.scope, cmd.to_programme_id)
        .await?
        .filter(|p| p.institution_id == ss.context.institution_id)
        .ok_or_else(|| {
            invalid(
                "toProgrammeId",
                "unknown",
                "no such programme in the student's institution",
            )
        })?;
    if prog.status != PublicationStatus::Published {
        return Err(invalid(
            "toProgrammeId",
            "not_published",
            "the destination programme is not published",
        ));
    }
    let today = business_today(state, &ss.scope, ss.context.institution_id).await?;
    let cv = published_curriculum(
        state,
        &ss.scope,
        prog.id,
        cmd.to_curriculum_version_id,
        today,
        "toCurriculumVersionId",
    )
    .await?;
    if !(1..=cv.term_count).contains(&cmd.to_term_no) {
        return Err(invalid(
            "toTermNo",
            "out_of_range",
            format!("1 … {}", cv.term_count),
        ));
    }
    let comps = state
        .references
        .curriculum_components(&ss.scope, cv.id)
        .await?;
    for (i, m) in cmd.credit_mapping.iter().enumerate() {
        if let Some(c) = m.to_component_id {
            if !comps.iter().any(|x| x.id == c) {
                return Err(invalid(
                    &format!("creditMapping[{i}].toComponentId"),
                    "not_in_curriculum",
                    "not a component of the destination curriculum",
                ));
            }
        }
        if !(m.credits >= 0.0 && m.credits <= 100.0) {
            return Err(invalid(
                &format!("creditMapping[{i}].credits"),
                "out_of_range",
                "0 … 100",
            ));
        }
    }
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let sql = format!(
        "INSERT INTO student.programme_migration (tenant_id, institution_id, student_id, student_context_id, from_programme_id, \
         from_curriculum_version_id, from_term_no, to_programme_id, to_curriculum_version_id, to_term_no, credit_mapping, reason) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12) RETURNING {MIGRATION_COLUMNS}"
    );
    let (m, _) = sqlx::query_as::<_, MigrationRow>(&sql)
        .bind(tenant.as_uuid())
        .bind(ss.context.institution_id.as_uuid())
        .bind(student_id.as_uuid())
        .bind(ss.context.id.as_uuid())
        .bind(from)
        .bind(ss.context.curriculum_version_id)
        .bind(ss.context.current_term_no)
        .bind(prog.id)
        .bind(cv.id)
        .bind(cmd.to_term_no)
        .bind(serde_json::to_value(&cmd.credit_mapping).map_err(KernelError::internal)?)
        .bind(&reason)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| match map_db_error(e) {
            KernelError::Conflict(_) => {
                conflict("the student already has an open programme migration")
            }
            other => other,
        })?
        .dto()?;
    audit(&mut tx, &ss.scope, "student.programme_migration.create", "programme_migration", m.id,
        json!({"from": from, "to": prog.id, "toCurriculumVersionId": cv.id, "toTermNo": cmd.to_term_no}), actor).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(m)
}

async fn set_migration_status(
    conn: &mut PgConnection,
    id: Uuid,
    to: CaseStatus,
    reason: Option<&str>,
    pid: Option<&str>,
    effective: Option<NaiveDate>,
) -> Result<ProgrammeMigration, KernelError> {
    let sql = format!(
        "UPDATE student.programme_migration SET status = $2, status_reason = $3, process_instance_id = COALESCE($4, process_instance_id), \
         effective_on = COALESCE($5, effective_on), version = version + 1, updated_at = now() WHERE id = $1 RETURNING {MIGRATION_COLUMNS}"
    );
    Ok(sqlx::query_as::<_, MigrationRow>(&sql)
        .bind(id)
        .bind(to.as_str())
        .bind(reason)
        .bind(pid)
        .bind(effective)
        .fetch_one(&mut *conn)
        .await
        .map_err(map_db_error)?
        .dto()?
        .0)
}

/// Approved → the context's programme association is updated (Student Lifecycle applies it via
/// `set_context_progress`) → completed.
async fn apply_migration(
    conn: &mut PgConnection,
    ss: &StudentScope,
    m: &ProgrammeMigration,
    today: NaiveDate,
    actor: &Actor,
) -> Result<ProgrammeMigration, KernelError> {
    let ctx = lock_context(conn, ss.scope.tenant_id, ss.context.id).await?;
    if ctx.programme_id != Some(m.from_programme_id) {
        return Err(conflict(
            "the student's programme changed since the migration was requested",
        ));
    }
    if open_registration(conn, &ctx).await? {
        return Err(conflict(
            "finalise or cancel the open registration before migrating",
        ));
    }
    set_migration_status(conn, m.id, CaseStatus::Approved, None, None, None).await?;
    set_context_progress(
        conn,
        &ctx,
        Some(m.to_term_no),
        Some(m.to_programme_id),
        Some(m.to_curriculum_version_id),
    )
    .await?;
    let done =
        set_migration_status(conn, m.id, CaseStatus::Completed, None, None, Some(today)).await?;
    emit(
        conn,
        &ss.scope,
        PROGRAMME_MIGRATION_COMPLETED,
        PROGRAMME_MIGRATION_COMPLETED_SCHEMA,
        m.id,
        &ProgrammeMigratedV1 {
            migration_id: m.id,
            student_id: m.student_id,
            student_context_id: ctx.id.as_uuid(),
            institution_id: ctx.institution_id.as_uuid(),
            from_programme_id: m.from_programme_id,
            to_programme_id: m.to_programme_id,
            to_curriculum_version_id: Some(m.to_curriculum_version_id),
            effective_on: today,
        },
        actor,
    )
    .await?;
    audit(
        conn,
        &ss.scope,
        "student.programme_migration.approve",
        "programme_migration",
        m.id,
        json!({"toProgrammeId": m.to_programme_id, "toTermNo": m.to_term_no, "effectiveOn": today}),
        actor,
    )
    .await?;
    Ok(done)
}

pub async fn submit_migration(
    state: &StudentState,
    tenant: TenantId,
    id: Uuid,
    expected: Version,
    actor: &Actor,
) -> Result<ProgrammeMigration, KernelError> {
    let (cur, ctx_id) = migration_row(&state.pool, tenant, id, false).await?;
    let ss = write_student(state, tenant, StudentId(cur.student_id), true).await?;
    same_context(&ss, ctx_id, "programme migration").await?;
    check_version(cur.version, expected)?;
    require_case(cur.status, CaseStatus::UnderReview, "programme migration")?;
    let today = business_today(state, &ss.scope, ss.context.institution_id).await?;
    let mut vars = BTreeMap::new();
    vars.insert("migrationId".into(), json!(id));
    vars.insert("toProgrammeId".into(), json!(cur.to_programme_id));
    let outcome = request_approval(
        state,
        &ss.scope,
        process::PROGRAMME_MIGRATION,
        business_key("programme_migration", id, cur.version),
        vars,
    )
    .await?;
    let pid = outcome.process_instance_id().to_owned();
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let (m, _) = migration_row(&mut *tx, tenant, id, true).await?;
    check_version(m.version, expected)?;
    require_case(m.status, CaseStatus::UnderReview, "programme migration")?;
    let m =
        set_migration_status(&mut tx, id, CaseStatus::UnderReview, None, Some(&pid), None).await?;
    audit(
        &mut tx,
        &ss.scope,
        "student.programme_migration.submit",
        "programme_migration",
        id,
        json!({"processInstanceId": pid}),
        actor,
    )
    .await?;
    let m = match outcome {
        ApprovalOutcome::Approved { .. } => {
            apply_migration(
                &mut tx,
                &ss,
                &m,
                today,
                &Actor::system(actor.correlation_id.clone()),
            )
            .await?
        }
        ApprovalOutcome::Pending { .. } => m,
    };
    tx.commit().await.map_err(map_db_error)?;
    Ok(m)
}

#[allow(clippy::too_many_arguments)]
pub async fn decide_migration(
    state: &StudentState,
    tenant: TenantId,
    id: Uuid,
    expected: Version,
    decision: RecognitionDecision,
    reason: Option<String>,
    actor: &Actor,
) -> Result<ProgrammeMigration, KernelError> {
    let (cur, ctx_id) = migration_row(&state.pool, tenant, id, false).await?;
    let ss = write_student(state, tenant, StudentId(cur.student_id), false).await?;
    same_context(&ss, ctx_id, "programme migration").await?;
    if decision != RecognitionDecision::Approve && reason.is_none() {
        return Err(invalid(
            "reason",
            "required",
            "a rejection or revision request needs a reason",
        ));
    }
    let today = business_today(state, &ss.scope, ss.context.institution_id).await?;
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let (m, _) = migration_row(&mut *tx, tenant, id, true).await?;
    check_version(m.version, expected)?;
    if m.status != CaseStatus::UnderReview {
        return Err(conflict(format!(
            "programme migration is {}; nothing to decide",
            m.status
        )));
    }
    let m = match decision {
        RecognitionDecision::Approve => apply_migration(&mut tx, &ss, &m, today, actor).await?,
        RecognitionDecision::Reject => {
            set_migration_status(
                &mut tx,
                id,
                CaseStatus::Rejected,
                reason.as_deref(),
                None,
                None,
            )
            .await?
        }
        RecognitionDecision::Revise => {
            set_migration_status(
                &mut tx,
                id,
                CaseStatus::Requested,
                reason.as_deref(),
                None,
                None,
            )
            .await?
        }
    };
    audit(
        &mut tx,
        &ss.scope,
        "student.programme_migration.decide",
        "programme_migration",
        id,
        json!({"decision": format!("{decision:?}"), "reason": reason}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(m)
}

pub async fn withdraw_migration(
    state: &StudentState,
    tenant: TenantId,
    id: Uuid,
    expected: Version,
    reason: Option<String>,
    actor: &Actor,
) -> Result<ProgrammeMigration, KernelError> {
    let (cur, _) = migration_row(&state.pool, tenant, id, false).await?;
    let ss = write_student(state, tenant, StudentId(cur.student_id), false).await?;
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let (m, _) = migration_row(&mut *tx, tenant, id, true).await?;
    check_version(m.version, expected)?;
    require_case(m.status, CaseStatus::Withdrawn, "programme migration")?;
    let m = set_migration_status(
        &mut tx,
        id,
        CaseStatus::Withdrawn,
        reason.as_deref(),
        None,
        None,
    )
    .await?;
    audit(
        &mut tx,
        &ss.scope,
        "student.programme_migration.withdraw",
        "programme_migration",
        id,
        json!({"reason": reason}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(m)
}

// ================================================================== academic break

const BREAK_COLUMNS: &str = "id, student_id, student_context_id, institution_id, reason, starts_on, expected_return_on, \
    term_no_at_start, status, status_reason, process_instance_id, rejoin_term_no, rejoin_curriculum_version_id, \
    rejoin_mapping_note, rejoined_on, standing_before, version, created_at, updated_at";

#[derive(sqlx::FromRow)]
struct BreakRow {
    id: Uuid,
    student_id: Uuid,
    student_context_id: Uuid,
    institution_id: Uuid,
    reason: String,
    starts_on: NaiveDate,
    expected_return_on: NaiveDate,
    term_no_at_start: Option<i32>,
    status: String,
    status_reason: Option<String>,
    process_instance_id: Option<String>,
    rejoin_term_no: Option<i32>,
    rejoin_curriculum_version_id: Option<Uuid>,
    rejoin_mapping_note: Option<String>,
    rejoined_on: Option<NaiveDate>,
    standing_before: Option<String>,
    version: i64,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

struct BreakRec {
    dto: AcademicBreak,
    context_id: Uuid,
    standing_before: Option<String>,
}

impl BreakRow {
    fn rec(self) -> Result<BreakRec, KernelError> {
        Ok(BreakRec {
            context_id: self.student_context_id,
            standing_before: self.standing_before,
            dto: AcademicBreak {
                status: parse_enum(&self.status, AcademicBreakStatus::parse, "break status")?,
                id: self.id,
                student_id: self.student_id,
                institution_id: self.institution_id,
                reason: self.reason,
                starts_on: self.starts_on,
                expected_return_on: self.expected_return_on,
                term_no_at_start: self.term_no_at_start,
                status_reason: self.status_reason,
                process_instance_id: self.process_instance_id,
                rejoin_term_no: self.rejoin_term_no,
                rejoin_curriculum_version_id: self.rejoin_curriculum_version_id,
                rejoin_mapping_note: self.rejoin_mapping_note,
                rejoined_on: self.rejoined_on,
                version: self.version,
                created_at: self.created_at,
                updated_at: self.updated_at,
            },
        })
    }
}

async fn break_row<'e, E: PgExecutor<'e>>(
    ex: E,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<BreakRec, KernelError> {
    let sql = format!(
        "SELECT {BREAK_COLUMNS} FROM student.academic_break WHERE id = $1 AND tenant_id = $2{}",
        if lock { " FOR UPDATE" } else { "" }
    );
    sqlx::query_as::<_, BreakRow>(&sql)
        .bind(id)
        .bind(tenant.as_uuid())
        .fetch_optional(ex)
        .await
        .map_err(map_db_error)?
        .ok_or(BREAK)?
        .rec()
}

pub async fn get_break(
    pool: &PgPool,
    tenant: TenantId,
    id: Uuid,
) -> Result<AcademicBreak, KernelError> {
    Ok(break_row(pool, tenant, id, false).await?.dto)
}

pub async fn list_breaks(
    pool: &PgPool,
    tenant: TenantId,
    student_id: StudentId,
) -> Result<Vec<AcademicBreak>, KernelError> {
    crate::shared::student::load_student(pool, tenant, student_id).await?;
    let sql = format!(
        "SELECT {BREAK_COLUMNS} FROM student.academic_break WHERE tenant_id = $1 AND student_id = $2 ORDER BY created_at DESC, id DESC"
    );
    sqlx::query_as::<_, BreakRow>(&sql)
        .bind(tenant.as_uuid())
        .bind(student_id.as_uuid())
        .fetch_all(pool)
        .await
        .map_err(map_db_error)?
        .into_iter()
        .map(|r| r.rec().map(|x| x.dto))
        .collect()
}

#[allow(clippy::too_many_arguments)]
async fn set_break(
    conn: &mut PgConnection,
    id: Uuid,
    from: AcademicBreakStatus,
    to: AcademicBreakStatus,
    reason: Option<&str>,
    pid: Option<&str>,
    standing_before: Option<&str>,
) -> Result<AcademicBreak, KernelError> {
    if !break_can_transition(from, to) {
        return Err(conflict(format!(
            "academic break is {from}; it cannot become {to}"
        )));
    }
    let sql = format!(
        "UPDATE student.academic_break SET status = $2, status_reason = COALESCE($3, status_reason), \
         process_instance_id = COALESCE($4, process_instance_id), standing_before = COALESCE($5, standing_before), \
         started_at = CASE WHEN $2 = 'in_effect' THEN now() ELSE started_at END, version = version + 1, updated_at = now() \
         WHERE id = $1 RETURNING {BREAK_COLUMNS}"
    );
    Ok(sqlx::query_as::<_, BreakRow>(&sql)
        .bind(id)
        .bind(to.as_str())
        .bind(reason)
        .bind(pid)
        .bind(standing_before)
        .fetch_one(&mut *conn)
        .await
        .map_err(map_db_error)?
        .rec()?
        .dto)
}

/// approved → in_effect: the student is on academic break (standing preserved for rejoining).
async fn start_locked(
    conn: &mut PgConnection,
    ss: &StudentScope,
    b: &AcademicBreak,
    actor: &Actor,
) -> Result<AcademicBreak, KernelError> {
    let ctx = lock_context(conn, ss.scope.tenant_id, ss.context.id).await?;
    if open_registration(conn, &ctx).await? {
        return Err(conflict(
            "finalise or cancel the open registration before the break starts",
        ));
    }
    let before = standing_of(&mut *conn, ss.scope.tenant_id, ctx.id.as_uuid()).await?;
    let started = set_break(
        conn,
        b.id,
        b.status,
        AcademicBreakStatus::InEffect,
        None,
        None,
        Some(before.as_str()),
    )
    .await?;
    set_standing(
        conn,
        &ctx,
        AcademicStanding::AcademicBreak,
        "academic_break",
        b.id,
    )
    .await?;
    emit(
        conn,
        &ss.scope,
        ACADEMIC_BREAK_STARTED,
        ACADEMIC_BREAK_STARTED_SCHEMA,
        b.id,
        &status_changed(
            b.id,
            &ss.scope,
            Some(b.student_id),
            Some(b.status.as_str()),
            "in_effect",
            started.version,
            Some(&b.reason),
            b.process_instance_id.as_deref(),
            ss.subject_ref(),
        ),
        actor,
    )
    .await?;
    audit(
        conn,
        &ss.scope,
        "student.academic_break.start",
        "academic_break",
        b.id,
        json!({"standingBefore": before}),
        actor,
    )
    .await?;
    Ok(started)
}

/// Applies an approval: approved, and in effect at once when the break has started.
async fn approve_break(
    conn: &mut PgConnection,
    ss: &StudentScope,
    b: &AcademicBreak,
    today: NaiveDate,
    actor: &Actor,
) -> Result<AcademicBreak, KernelError> {
    let approved = set_break(
        conn,
        b.id,
        b.status,
        AcademicBreakStatus::Approved,
        None,
        None,
        None,
    )
    .await?;
    audit(
        conn,
        &ss.scope,
        "student.academic_break.approve",
        "academic_break",
        b.id,
        json!({}),
        actor,
    )
    .await?;
    if approved.starts_on <= today {
        start_locked(conn, ss, &approved, actor).await
    } else {
        Ok(approved)
    }
}

/// Requests an academic break; the approval step (WorkflowPort `student.academic_break`)
/// starts at once (auto-approved in Batch 3; in effect when `startsOn` ≤ today).
pub async fn create_break(
    state: &StudentState,
    tenant: TenantId,
    student_id: StudentId,
    cmd: CreateBreak,
    actor: &Actor,
) -> Result<AcademicBreak, KernelError> {
    let ss = write_student(state, tenant, student_id, true).await?;
    let reason = required(Some(cmd.reason), "reason", 1000)?;
    if cmd.starts_on > cmd.expected_return_on {
        return Err(invalid(
            "expectedReturnOn",
            "before_start",
            "expectedReturnOn is before startsOn",
        ));
    }
    let standing = standing_of(&state.pool, tenant, ss.context.id.as_uuid()).await?;
    if standing == AcademicStanding::AcademicBreak {
        return Err(conflict("the student is already on an academic break"));
    }
    let today = business_today(state, &ss.scope, ss.context.institution_id).await?;
    // The id is allocated up front so the approval's business key is deterministic and the
    // request + approval land in one owner-local transaction.
    let id = Uuid::now_v7();
    let mut vars = BTreeMap::new();
    vars.insert("academicBreakId".into(), json!(id));
    let outcome = sutra_kernel::approval::with_maker(
        actor.id.as_deref(),
        request_approval(
            state,
            &ss.scope,
            process::ACADEMIC_BREAK,
            business_key("academic_break", id, 1),
            vars,
        ),
    )
    .await?;
    let pid = outcome.process_instance_id().to_owned();
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    lock_context(&mut tx, tenant, ss.context.id).await?;
    let sql = format!(
        "INSERT INTO student.academic_break (id, tenant_id, institution_id, student_id, student_context_id, reason, starts_on, \
         expected_return_on, term_no_at_start, process_instance_id) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) RETURNING {BREAK_COLUMNS}"
    );
    let b = sqlx::query_as::<_, BreakRow>(&sql)
        .bind(id)
        .bind(tenant.as_uuid())
        .bind(ss.context.institution_id.as_uuid())
        .bind(student_id.as_uuid())
        .bind(ss.context.id.as_uuid())
        .bind(&reason)
        .bind(cmd.starts_on)
        .bind(cmd.expected_return_on)
        .bind(ss.context.current_term_no)
        .bind(&pid)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| match map_db_error(e) {
            KernelError::Conflict(_) => {
                conflict("the student already has an open academic break request")
            }
            other => other,
        })?
        .rec()?
        .dto;
    audit(&mut tx, &ss.scope, "student.academic_break.request", "academic_break", b.id, json!({"startsOn": b.starts_on, "expectedReturnOn": b.expected_return_on, "processInstanceId": pid}), actor).await?;
    let out = match outcome {
        ApprovalOutcome::Approved { .. } => {
            approve_break(
                &mut tx,
                &ss,
                &b,
                today,
                &Actor::system(actor.correlation_id.clone()),
            )
            .await?
        }
        ApprovalOutcome::Pending { .. } => b,
    };
    tx.commit().await.map_err(map_db_error)?;
    Ok(out)
}

async fn break_scope(
    state: &StudentState,
    tenant: TenantId,
    id: Uuid,
) -> Result<(BreakRec, StudentScope), KernelError> {
    let rec = break_row(&state.pool, tenant, id, false).await?;
    let ss = write_student(state, tenant, StudentId(rec.dto.student_id), false).await?;
    same_context(&ss, rec.context_id, "academic break").await?;
    Ok((rec, ss))
}

#[allow(clippy::too_many_arguments)]
pub async fn decide_break(
    state: &StudentState,
    tenant: TenantId,
    id: Uuid,
    expected: Version,
    approve: bool,
    reason: Option<String>,
    actor: &Actor,
) -> Result<AcademicBreak, KernelError> {
    let (_, ss) = break_scope(state, tenant, id).await?;
    if !approve && reason.is_none() {
        return Err(invalid("reason", "required", "a rejection needs a reason"));
    }
    let today = business_today(state, &ss.scope, ss.context.institution_id).await?;
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let b = break_row(&mut *tx, tenant, id, true).await?.dto;
    check_version(b.version, expected)?;
    if b.status != AcademicBreakStatus::Requested {
        return Err(conflict(format!(
            "academic break is {}; nothing to decide",
            b.status
        )));
    }
    let b = if approve {
        approve_break(&mut tx, &ss, &b, today, actor).await?
    } else {
        let r = set_break(
            &mut tx,
            id,
            b.status,
            AcademicBreakStatus::Rejected,
            reason.as_deref(),
            None,
            None,
        )
        .await?;
        audit(
            &mut tx,
            &ss.scope,
            "student.academic_break.reject",
            "academic_break",
            id,
            json!({"reason": reason}),
            actor,
        )
        .await?;
        r
    };
    tx.commit().await.map_err(map_db_error)?;
    Ok(b)
}

/// Puts an approved break into effect once its start date is reached.
pub async fn start_break(
    state: &StudentState,
    tenant: TenantId,
    id: Uuid,
    expected: Version,
    actor: &Actor,
) -> Result<AcademicBreak, KernelError> {
    let (_, ss) = break_scope(state, tenant, id).await?;
    let today = business_today(state, &ss.scope, ss.context.institution_id).await?;
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let b = break_row(&mut *tx, tenant, id, true).await?.dto;
    check_version(b.version, expected)?;
    if b.status != AcademicBreakStatus::Approved {
        return Err(conflict(format!(
            "academic break is {}; only an approved break can start",
            b.status
        )));
    }
    if b.starts_on > today {
        return Err(conflict(format!("the break starts on {}", b.starts_on)));
    }
    let b = start_locked(&mut tx, &ss, &b, actor).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(b)
}

/// Rejoining after an academic break (L3-C-035): curriculum change + placement, standing restored.
pub async fn rejoin(
    state: &StudentState,
    tenant: TenantId,
    id: Uuid,
    expected: Version,
    cmd: RejoinBody,
    actor: &Actor,
) -> Result<AcademicBreak, KernelError> {
    let (rec, ss) = break_scope(state, tenant, id).await?;
    if rec.dto.status != AcademicBreakStatus::InEffect {
        return Err(conflict(format!(
            "academic break is {}; only a break in effect can end in rejoining",
            rec.dto.status
        )));
    }
    let today = business_today(state, &ss.scope, ss.context.institution_id).await?;
    let programme = ss
        .context
        .programme_id
        .ok_or_else(|| conflict("the student context has no programme"))?;
    let cv = match cmd.curriculum_version_id {
        Some(v) => {
            published_curriculum(
                state,
                &ss.scope,
                programme,
                Some(v),
                today,
                "curriculumVersionId",
            )
            .await?
        }
        None => {
            let cur = ss
                .context
                .curriculum_version_id
                .ok_or_else(|| conflict("the student context has no curriculum"))?;
            state
                .references
                .curriculum_version(&ss.scope, cur)
                .await?
                .ok_or_else(|| conflict("the curriculum version no longer exists"))?
        }
    };
    let term_no = cmd.term_no.or(rec.dto.term_no_at_start).unwrap_or(1);
    if !(1..=cv.term_count).contains(&term_no) {
        return Err(invalid(
            "termNo",
            "out_of_range",
            format!("1 … {}", cv.term_count),
        ));
    }
    let changed_curriculum = Some(cv.id) != ss.context.curriculum_version_id;
    let mapping = text(cmd.mapping_note, "mappingNote", 2000)?;
    if changed_curriculum && mapping.is_none() {
        return Err(invalid(
            "mappingNote",
            "required",
            "the curriculum changed: describe how preserved credits map",
        ));
    }
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let ctx = lock_context(&mut tx, tenant, ss.context.id).await?;
    let b = break_row(&mut *tx, tenant, id, true).await?;
    check_version(b.dto.version, expected)?;
    if !break_can_transition(b.dto.status, AcademicBreakStatus::Rejoined) {
        return Err(conflict(format!("academic break is {}", b.dto.status)));
    }
    if ctx.current_term_no != Some(term_no) || changed_curriculum {
        set_context_progress(&mut tx, &ctx, Some(term_no), ctx.programme_id, Some(cv.id)).await?;
    }
    let sql = format!(
        "UPDATE student.academic_break SET status = 'rejoined', rejoin_term_no = $2, rejoin_curriculum_version_id = $3, \
         rejoin_mapping_note = $4, rejoined_on = $5, version = version + 1, updated_at = now() WHERE id = $1 RETURNING {BREAK_COLUMNS}"
    );
    let out = sqlx::query_as::<_, BreakRow>(&sql)
        .bind(id)
        .bind(term_no)
        .bind(cv.id)
        .bind(mapping.as_deref())
        .bind(today)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?
        .rec()?
        .dto;
    let restored = b
        .standing_before
        .as_deref()
        .and_then(AcademicStanding::parse)
        .filter(|s| *s != AcademicStanding::AcademicBreak)
        .unwrap_or(AcademicStanding::GoodStanding);
    set_standing(&mut tx, &ctx, restored, "academic_break", id).await?;
    emit(
        &mut tx,
        &ss.scope,
        ACADEMIC_BREAK_REJOINED,
        ACADEMIC_BREAK_REJOINED_SCHEMA,
        id,
        &status_changed(
            id,
            &ss.scope,
            Some(out.student_id),
            Some("in_effect"),
            "rejoined",
            out.version,
            None,
            out.process_instance_id.as_deref(),
            ss.subject_ref(),
        ),
        actor,
    )
    .await?;
    audit(
        &mut tx,
        &ss.scope,
        "student.academic_break.rejoin",
        "academic_break",
        id,
        json!({"termNo": term_no, "curriculumVersionId": cv.id, "restoredStanding": restored}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(out)
}

pub async fn withdraw_break(
    state: &StudentState,
    tenant: TenantId,
    id: Uuid,
    expected: Version,
    reason: Option<String>,
    actor: &Actor,
) -> Result<AcademicBreak, KernelError> {
    let (_, ss) = break_scope(state, tenant, id).await?;
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let b = break_row(&mut *tx, tenant, id, true).await?.dto;
    check_version(b.version, expected)?;
    let out = set_break(
        &mut tx,
        id,
        b.status,
        AcademicBreakStatus::Withdrawn,
        reason.as_deref(),
        None,
        None,
    )
    .await?;
    audit(
        &mut tx,
        &ss.scope,
        "student.academic_break.withdraw",
        "academic_break",
        id,
        json!({"reason": reason}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(out)
}
