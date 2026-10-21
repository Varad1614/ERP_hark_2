//! Audit C3 (M-2): **read scope** of staff reads in Domain C.
//!
//! * `student.read` (administrator, registrar, approver roles) — every student of the tenant.
//! * `student.read.assigned` (faculty) — only the students holding an `enrolled` seat in a
//!   section where the caller has an **active** faculty assignment (own sections), resolved
//!   through [`TaughtStudentsReader`](sutra_contracts::academic::ports::TaughtStudentsReader).
//!   Any other student is indistinguishable from a missing one (`404`), as a foreign tenant's.
//! * neither — `403` (`missing permission student.read`), checked **before** any row is read.
//!
//! Only the reads a teacher needs accept the assigned scope (directory, Student 360, status
//! history, the student's registrations and progression decisions, progression policies);
//! documents, ledgers, admissions, guardians, scholarships, transfers, completion and every
//! institution-wide list stay on `student.read`.

use crate::shared::scope::tenant_scope;
use crate::shared::StudentState;
use sutra_contracts::student::ids::StudentId;
use sutra_kernel::auth::{AuthContext, Permission};
use sutra_kernel::error::KernelError;
use sutra_web::{ApiResult, Authenticated};
use uuid::Uuid;

/// Which students the caller may read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StudentReadScope {
    /// `student.read`: every student of the tenant.
    Tenant,
    /// `student.read.assigned`: only these students (sorted, distinct).
    Taught(Vec<Uuid>),
}

impl StudentReadScope {
    /// `None` for the tenant-wide scope, else the readable student ids (bind as `uuid[]`).
    pub fn student_ids(&self) -> Option<&[Uuid]> {
        match self {
            StudentReadScope::Tenant => None,
            StudentReadScope::Taught(ids) => Some(ids),
        }
    }

    /// Whether `student` is inside the scope.
    pub fn allows(&self, student: StudentId) -> bool {
        match self {
            StudentReadScope::Tenant => true,
            StudentReadScope::Taught(ids) => ids.binary_search(&student.as_uuid()).is_ok(),
        }
    }
}

/// `403` unless the caller holds `student.read` or `student.read.assigned` (no row is read).
pub fn require_any_student_read(auth: &Authenticated) -> ApiResult<&AuthContext> {
    if auth.0.has_permission(Permission::StudentRead) {
        return auth.require(Permission::StudentRead);
    }
    if auth.0.has_permission(Permission::StudentReadAssigned) {
        return auth.require(Permission::StudentReadAssigned);
    }
    auth.require(Permission::StudentRead)
}

/// The caller's read scope (lists filter on [`StudentReadScope::student_ids`]).
pub async fn read_scope<'a>(
    st: &StudentState,
    auth: &'a Authenticated,
) -> ApiResult<(&'a AuthContext, StudentReadScope)> {
    let ctx = require_any_student_read(auth)?;
    if ctx.has_permission(Permission::StudentRead) {
        return Ok((ctx, StudentReadScope::Tenant));
    }
    let mut ids: Vec<Uuid> = st
        .taught
        .taught_students(&tenant_scope(ctx), &ctx.subject)
        .await?
        .into_iter()
        .map(|s| s.as_uuid())
        .collect();
    ids.sort_unstable();
    ids.dedup();
    Ok((ctx, StudentReadScope::Taught(ids)))
}

/// One student's record: `student.read` reads any; `student.read.assigned` only a student the
/// caller teaches (`404 student` otherwise, so existence never leaks); else `403`.
pub async fn require_student_read<'a>(
    st: &StudentState,
    auth: &'a Authenticated,
    student: StudentId,
) -> ApiResult<&'a AuthContext> {
    let ctx = require_any_student_read(auth)?;
    if ctx.has_permission(Permission::StudentRead) {
        return Ok(ctx);
    }
    if st
        .taught
        .teaches_student(&tenant_scope(ctx), &ctx.subject, student)
        .await?
    {
        Ok(ctx)
    } else {
        Err(KernelError::NotFound { entity: "student" }.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn taught_scope_allows_only_listed_students() {
        let a = Uuid::from_u128(1);
        let b = Uuid::from_u128(2);
        let s = StudentReadScope::Taught(vec![a]);
        assert!(s.allows(StudentId(a)));
        assert!(!s.allows(StudentId(b)));
        assert_eq!(s.student_ids(), Some(&[a][..]));
        assert!(StudentReadScope::Tenant.allows(StudentId(b)));
        assert_eq!(StudentReadScope::Tenant.student_ids(), None);
        assert!(!StudentReadScope::Taught(Vec::new()).allows(StudentId(a)));
    }
}
