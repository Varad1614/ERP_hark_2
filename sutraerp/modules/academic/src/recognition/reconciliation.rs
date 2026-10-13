//! External learning record reconciliation (L3-D-049): a run holds both evidence sets — the
//! provider's records as received and the institutional snapshot — and the discrepancies
//! found (registration, progress hours, completion, identity). D06 records are authoritative
//! for D06 facts; provider records are external evidence and are never overwritten. A run
//! without discrepancies is reconciled at once; otherwise it is resolved by upholding the
//! institutional record (provider-side correction requested, notes required) or escalated to a
//! flexible-learning exception case — never by asserting false consistency.

use crate::flexible::exception::{self, NewFlexibleExceptionCase};
use crate::flexible::kit::{self, begin, check_version, commit, invalid, require, row_scope};
use crate::flexible::opportunity::{self, LearningOpportunity};
use crate::shared::guard::require_institution_writable;
use crate::shared::scope::institution_scope;
use crate::shared::{AcademicState, Actor};
use chrono::{DateTime, Utc};
use serde_json::json;
use std::collections::HashMap;
use sutra_kernel::auth::AuthContext;
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use uuid::Uuid;

pub const TABLE: &str = "reconciliation_run";
const ENTITY: &str = "reconciliation run";

/// One learner record as reported by the provider.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProviderRecord {
    pub student_number: String,
    pub learner_name: Option<String>,
    pub provider_reference: Option<String>,
    #[serde(default)]
    pub completed: bool,
    pub hours_completed: Option<f64>,
}

/// The institutional facts compared (snapshot at run time).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct InstitutionalRecord {
    pub enrolment_id: Uuid,
    pub student_number: String,
    pub display_name: String,
    pub status: String,
    pub provider_reference: Option<String>,
    pub completed: bool,
    pub hours_completed: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReconciliationDiscrepancy {
    pub student_number: String,
    /// `unknown_learner` | `missing_at_provider` | `registration_reference_mismatch` |
    /// `completion_unrecorded` | `completion_unconfirmed` | `hours_mismatch` | `identity_mismatch`.
    pub kind: String,
    pub institutional: Option<String>,
    pub provider: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReconciliationRun {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub opportunity_id: Uuid,
    pub provider_id: Option<Uuid>,
    pub provider_records: Vec<ProviderRecord>,
    pub institutional_records: Vec<InstitutionalRecord>,
    pub discrepancies: Vec<ReconciliationDiscrepancy>,
    /// `open` | `discrepancies` | `reconciled` | `escalated`.
    pub status: String,
    pub resolution: Option<String>,
    pub resolution_notes: Option<String>,
    pub exception_case_id: Option<Uuid>,
    pub run_by: Option<String>,
    pub resolved_by: Option<String>,
    pub resolved_at: Option<DateTime<Utc>>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct ReconciliationRunList {
    pub items: Vec<ReconciliationRun>,
}

#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NewReconciliationRun {
    pub opportunity_id: Uuid,
    pub provider_records: Vec<ProviderRecord>,
}

#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReconciliationResolution {
    /// `institutional_record_upheld` | `escalated`.
    pub resolution: String,
    pub notes: Option<String>,
}

/// Pure comparison of both evidence sets.
pub fn compare(
    institutional: &[InstitutionalRecord],
    provider: &[ProviderRecord],
) -> Vec<ReconciliationDiscrepancy> {
    let mut out = Vec::new();
    let by_number: HashMap<String, &InstitutionalRecord> = institutional
        .iter()
        .map(|r| (r.student_number.to_lowercase(), r))
        .collect();
    let mut seen = std::collections::HashSet::new();
    for p in provider {
        let key = p.student_number.trim().to_lowercase();
        let Some(i) = by_number.get(&key) else {
            out.push(ReconciliationDiscrepancy {
                student_number: p.student_number.clone(),
                kind: "unknown_learner".into(),
                institutional: None,
                provider: p.provider_reference.clone(),
            });
            continue;
        };
        seen.insert(key);
        let d = |kind: &str, a: Option<String>, b: Option<String>| ReconciliationDiscrepancy {
            student_number: i.student_number.clone(),
            kind: kind.into(),
            institutional: a,
            provider: b,
        };
        if let Some(name) = p.learner_name.as_deref().filter(|n| !n.trim().is_empty()) {
            if !name.trim().eq_ignore_ascii_case(i.display_name.trim()) {
                out.push(d(
                    "identity_mismatch",
                    Some(i.display_name.clone()),
                    Some(name.to_owned()),
                ));
            }
        }
        if p.provider_reference.is_some()
            && i.provider_reference.is_some()
            && p.provider_reference != i.provider_reference
        {
            out.push(d(
                "registration_reference_mismatch",
                i.provider_reference.clone(),
                p.provider_reference.clone(),
            ));
        }
        if p.completed && !i.completed {
            out.push(d(
                "completion_unrecorded",
                Some(i.status.clone()),
                Some("completed".into()),
            ));
        }
        if !p.completed && i.completed {
            out.push(d(
                "completion_unconfirmed",
                Some("completed".into()),
                Some("not completed".into()),
            ));
        }
        if let (Some(a), Some(b)) = (i.hours_completed, p.hours_completed) {
            if (a - b).abs() > 0.5 {
                out.push(d(
                    "hours_mismatch",
                    Some(a.to_string()),
                    Some(b.to_string()),
                ));
            }
        }
    }
    for i in institutional {
        if i.provider_reference.is_some() && !seen.contains(&i.student_number.to_lowercase()) {
            out.push(ReconciliationDiscrepancy {
                student_number: i.student_number.clone(),
                kind: "missing_at_provider".into(),
                institutional: i.provider_reference.clone(),
                provider: None,
            });
        }
    }
    out
}

pub async fn list(
    st: &AcademicState,
    tenant: TenantId,
    institution: Uuid,
    status: Option<&str>,
) -> Result<Vec<ReconciliationRun>, KernelError> {
    kit::list_by(
        &st.pool,
        TABLE,
        tenant,
        "institution_id",
        institution,
        status,
        "t.created_at DESC",
        200,
    )
    .await
}

pub async fn get(
    st: &AcademicState,
    tenant: TenantId,
    id: Uuid,
) -> Result<ReconciliationRun, KernelError> {
    kit::fetch(&st.pool, TABLE, ENTITY, tenant, id).await
}

/// One institutional enrolment row read for a reconciliation (id, student number, name, status,
/// accepted provider reference, completed, hours completed).
type InstitutionalRow = (
    Uuid,
    String,
    String,
    String,
    Option<String>,
    bool,
    Option<f64>,
);

pub async fn run(
    st: &AcademicState,
    ctx: &AuthContext,
    institution: Uuid,
    cmd: NewReconciliationRun,
    actor: &Actor,
) -> Result<ReconciliationRun, KernelError> {
    let scope = institution_scope(ctx, InstitutionId(institution));
    require_institution_writable(st, &scope, InstitutionId(institution)).await?;
    let tenant = ctx.scope.tenant_id;
    let o: LearningOpportunity = kit::fetch(
        &st.pool,
        opportunity::TABLE,
        opportunity::ENTITY,
        tenant,
        cmd.opportunity_id,
    )
    .await
    .map_err(|_| {
        invalid(
            "opportunityId",
            "not_found",
            "learning opportunity not found",
        )
    })?;
    if o.institution_id != institution {
        return Err(invalid(
            "opportunityId",
            "not_found",
            "learning opportunity not found in this institution",
        ));
    }
    if cmd
        .provider_records
        .iter()
        .any(|r| r.student_number.trim().is_empty())
    {
        return Err(invalid(
            "providerRecords",
            "invalid",
            "every provider record needs a studentNumber",
        ));
    }
    let rows: Vec<InstitutionalRow> = sqlx::query_as(
        "SELECT e.id, e.student_number, e.display_name, e.status, \
                (SELECT r.provider_reference FROM academic.external_registration r WHERE r.enrolment_id = e.id AND r.status = 'accepted' LIMIT 1), \
                e.status = 'completed', e.hours_completed::float8 \
         FROM academic.flexible_enrolment e WHERE e.tenant_id = $1 AND e.opportunity_id = $2 \
           AND e.status IN ('registered', 'in_progress', 'completed') ORDER BY e.student_number",
    )
    .bind(tenant.as_uuid())
    .bind(o.id)
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    let institutional: Vec<InstitutionalRecord> = rows
        .into_iter()
        .map(
            |(
                enrolment_id,
                student_number,
                display_name,
                status,
                provider_reference,
                completed,
                hours_completed,
            )| InstitutionalRecord {
                enrolment_id,
                student_number,
                display_name,
                status,
                provider_reference,
                completed,
                hours_completed,
            },
        )
        .collect();
    let discrepancies = compare(&institutional, &cmd.provider_records);
    let clean = discrepancies.is_empty();
    let mut tx = begin(st).await?;
    let r: ReconciliationRun = kit::insert(&mut tx, TABLE, json!({
        "tenant_id": tenant.as_uuid(), "institution_id": institution, "opportunity_id": o.id, "provider_id": o.provider_id,
        "provider_records": cmd.provider_records, "institutional_records": institutional, "discrepancies": discrepancies,
        "status": if clean { "reconciled" } else { "discrepancies" },
        "resolution": if clean { Some("consistent") } else { None }, "run_by": actor.id,
        "resolved_by": if clean { actor.id.clone() } else { None }, "resolved_at": if clean { Some(kit::now()) } else { None },
    })).await?;
    kit::audit(
        &mut tx,
        &scope,
        TABLE,
        "run",
        r.id,
        json!({"opportunityId": o.id, "discrepancies": r.discrepancies.len()}),
        actor,
    )
    .await?;
    commit(tx).await?;
    get(st, tenant, r.id).await
}

/// Resolves a run with discrepancies: uphold the institutional record (notes required — the
/// provider is asked to correct its side) or escalate into an exception case.
pub async fn resolve(
    st: &AcademicState,
    ctx: &AuthContext,
    id: Uuid,
    expected: Version,
    cmd: ReconciliationResolution,
    actor: &Actor,
) -> Result<ReconciliationRun, KernelError> {
    let tenant = ctx.scope.tenant_id;
    let r: ReconciliationRun = kit::fetch(&st.pool, TABLE, ENTITY, tenant, id).await?;
    check_version(r.version, expected)?;
    let scope = row_scope(ctx, r.institution_id);
    require_institution_writable(st, &scope, InstitutionId(r.institution_id)).await?;
    require(
        r.status == "discrepancies",
        format!("the reconciliation run is {}; nothing to resolve", r.status),
    )?;
    let notes = kit::opt_text(cmd.notes.as_deref());
    let mut tx = begin(st).await?;
    let locked: ReconciliationRun = kit::lock(&mut tx, TABLE, ENTITY, tenant, id).await?;
    check_version(locked.version, expected)?;
    let changes = match cmd.resolution.as_str() {
        "institutional_record_upheld" => {
            let notes = notes.ok_or_else(|| invalid("notes", "required", "record why the institutional record stands and the provider correction requested"))?;
            json!({"status": "reconciled", "resolution": cmd.resolution, "resolution_notes": notes, "resolved_by": actor.id, "resolved_at": kit::now()})
        }
        "escalated" => {
            let case = NewFlexibleExceptionCase {
                category: "reconciliation".into(),
                title: format!(
                    "Unresolved provider record discrepancies ({})",
                    r.discrepancies.len()
                ),
                description: notes.clone(),
                root_cause: Some(
                    r.discrepancies
                        .iter()
                        .map(|d| format!("{}: {}", d.student_number, d.kind))
                        .collect::<Vec<_>>()
                        .join("; "),
                ),
                owner_subject: None,
                opportunity_id: Some(r.opportunity_id),
                enrolment_id: None,
                source_key: Some(format!("reconciliation:{id}")),
            };
            let (case_id, _) = exception::raise_in(
                &mut tx,
                &scope,
                tenant,
                r.institution_id,
                &case,
                &ctx.subject,
                actor,
            )
            .await?;
            json!({"status": "escalated", "resolution": cmd.resolution, "resolution_notes": notes, "exception_case_id": case_id, "resolved_by": actor.id, "resolved_at": kit::now()})
        }
        _ => {
            return Err(invalid(
                "resolution",
                "invalid",
                "resolution must be institutional_record_upheld or escalated",
            ))
        }
    };
    kit::patch::<ReconciliationRun>(&mut tx, TABLE, tenant, id, changes).await?;
    kit::audit(
        &mut tx,
        &scope,
        TABLE,
        "resolve",
        id,
        json!({"resolution": cmd.resolution}),
        actor,
    )
    .await?;
    commit(tx).await?;
    get(st, tenant, id).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inst(
        n: &str,
        reference: Option<&str>,
        completed: bool,
        hours: Option<f64>,
    ) -> InstitutionalRecord {
        InstitutionalRecord {
            enrolment_id: Uuid::nil(),
            student_number: n.into(),
            display_name: format!("Student {n}"),
            status: if completed {
                "completed".into()
            } else {
                "registered".into()
            },
            provider_reference: reference.map(str::to_owned),
            completed,
            hours_completed: hours,
        }
    }

    fn prov(
        n: &str,
        reference: Option<&str>,
        completed: bool,
        hours: Option<f64>,
    ) -> ProviderRecord {
        ProviderRecord {
            student_number: n.into(),
            learner_name: None,
            provider_reference: reference.map(str::to_owned),
            completed,
            hours_completed: hours,
        }
    }

    #[test]
    fn consistent_records_have_no_discrepancy() {
        let i = vec![inst("S1", Some("P-1"), true, Some(30.0))];
        let p = vec![prov("s1", Some("P-1"), true, Some(30.2))];
        assert!(compare(&i, &p).is_empty());
    }

    #[test]
    fn every_kind_of_discrepancy_is_found() {
        let i = vec![
            inst("S1", Some("P-1"), false, Some(10.0)),
            inst("S2", Some("P-2"), true, None),
            inst("S3", Some("P-3"), false, None),
        ];
        let p = vec![
            prov("S1", Some("P-9"), true, Some(20.0)),
            prov("S2", None, false, None),
            prov("S9", None, true, None),
        ];
        let kinds: Vec<String> = compare(&i, &p).into_iter().map(|d| d.kind).collect();
        for k in [
            "registration_reference_mismatch",
            "completion_unrecorded",
            "hours_mismatch",
            "completion_unconfirmed",
            "unknown_learner",
            "missing_at_provider",
        ] {
            assert!(kinds.iter().any(|x| x == k), "{k} in {kinds:?}");
        }
    }
}
