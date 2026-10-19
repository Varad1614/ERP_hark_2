//! Self-service person resolution (contract `docs/batch8-contracts.md` §4.4): the caller of a
//! `campus.self` endpoint is resolved from the session subject — a Domain C student
//! (`StudentDirectory::by_subject`) or else a Domain E staff record
//! (`WorkforceDirectory::staff_by_subject`) — never from a client-supplied id. Residents,
//! passengers, borrowers and requesters are stored as `(person_kind, person_id)`.

use crate::shared::CampusState;
use sutra_contracts::campus::status::PersonKind;
use sutra_contracts::people::ids::StaffMemberId;
use sutra_contracts::student::ids::StudentId;
use sutra_kernel::auth::{AuthContext, Permission};
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::ExecutionScope;
use uuid::Uuid;

/// A student or staff member a campus record belongs to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PersonRef {
    pub kind: PersonKind,
    /// Domain C student id or Domain E staff id.
    pub id: Uuid,
    /// Student number / staff number.
    pub number: String,
    pub display_name: String,
    /// Login subject, when linked (inbox rules notify it).
    pub subject_ref: Option<String>,
}

/// Resolves a person by kind + id (tenant from `scope`; `None` when missing / foreign).
pub async fn person(
    state: &CampusState,
    scope: &ExecutionScope,
    kind: PersonKind,
    id: Uuid,
) -> Result<Option<PersonRef>, KernelError> {
    Ok(match kind {
        PersonKind::Student => state
            .students
            .student(scope, StudentId(id))
            .await?
            .map(|s| PersonRef {
                kind,
                id,
                number: s.student_number,
                display_name: s.display_name,
                subject_ref: s.subject_ref,
            }),
        PersonKind::Staff => state
            .workforce
            .staff(scope, StaffMemberId(id))
            .await?
            .filter(|s| s.status.is_live())
            .map(|s| PersonRef {
                kind,
                id,
                number: s.staff_number,
                display_name: s.display_name,
                subject_ref: s.subject_ref,
            }),
    })
}

/// Like [`person`], 422 on the given field when the person is unknown.
pub async fn require_person(
    state: &CampusState,
    scope: &ExecutionScope,
    kind: PersonKind,
    id: Uuid,
    field: &str,
) -> Result<PersonRef, KernelError> {
    person(state, scope, kind, id).await?.ok_or_else(|| {
        KernelError::Validation(vec![sutra_kernel::error::FieldViolation::new(
            field,
            "unknown_person",
            format!("no {kind} with this id"),
        )])
    })
}

/// The caller as a campus person (`campus.self`): their student record, else their staff
/// record; 404 when the login has neither.
pub async fn my_person(state: &CampusState, ctx: &AuthContext) -> Result<PersonRef, KernelError> {
    ctx.require(Permission::CampusSelfService)?;
    let scope = ExecutionScope::tenant(ctx.scope.tenant_id);
    if let Some(s) = state.students.by_subject(&scope, &ctx.subject).await? {
        return Ok(PersonRef {
            kind: PersonKind::Student,
            id: s.id.as_uuid(),
            number: s.student_number,
            display_name: s.display_name,
            subject_ref: s.subject_ref.or_else(|| Some(ctx.subject.clone())),
        });
    }
    if let Some(s) = state
        .workforce
        .staff_by_subject(&scope, &ctx.subject)
        .await?
    {
        return Ok(PersonRef {
            kind: PersonKind::Staff,
            id: s.id.as_uuid(),
            number: s.staff_number,
            display_name: s.display_name,
            subject_ref: s.subject_ref.or_else(|| Some(ctx.subject.clone())),
        });
    }
    Err(KernelError::NotFound {
        entity: "campus person (student or staff record)",
    })
}
