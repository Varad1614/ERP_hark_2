//! Students as identity subjects (Part 2 · S4-idp, port
//! [`sutra_contracts::platform::identity::IdentitySubjectSource`]).
//!
//! Account state from the persona and its live StudentContext:
//!
//! | persona | live context | account |
//! |---|---|---|
//! | `activated` · `enrolled` · `academically_assigned` · `active` | `pending_activation` · `active` | **active** |
//! | any | `suspended`, or persona `suspended` | suspended |
//! | `created` | — | suspended (not activated yet; never auto-provisioned) |
//! | `archived` · `qualification_completed`, or no live context (exit / withdrawal / transfer out) | — | disabled |
//!
//! Reads and the `subject_ref` write stay inside Domain C's own schema.

use sqlx::{PgPool, Row};
use sutra_contracts::platform::identity::{
    AccountState, ErpSubjectType, IdentityFacts, IdentitySubjectSource, SubjectLink,
};
use sutra_contracts::student::events::{completion, record, STUDENT_STATUS_CHANGED};
use sutra_kernel::auth::Role;
use sutra_kernel::error::KernelError;
use sutra_persistence::map_db_error;
use uuid::Uuid;

/// Domain C identity source.
#[derive(Clone)]
pub struct PgStudentIdentitySource {
    pool: PgPool,
}

impl PgStudentIdentitySource {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

/// Events that can change a student's account (every persona / context status change is also
/// published as `student.status_changed`; profile updates may change the e-mail).
pub const EVENT_TYPES: &[&str] = &[
    STUDENT_STATUS_CHANGED,
    record::STUDENT_PROFILE_UPDATED,
    record::STUDENT_TRANSFERRED,
    completion::STUDENT_EXIT_COMPLETED,
];

/// Account state of a persona status + its best context status (pure).
pub fn account_state(persona: &str, context: Option<&str>) -> AccountState {
    match persona {
        "archived" | "qualification_completed" => return AccountState::Disabled,
        "suspended" => return AccountState::Suspended,
        "created" => return AccountState::Suspended,
        _ => {}
    }
    match context {
        Some("pending_activation" | "active") => AccountState::Active,
        Some("suspended") => AccountState::Suspended,
        _ => AccountState::Disabled,
    }
}

/// Student ids an event payload names (pure).
pub fn subject_ids(data: &serde_json::Value) -> Vec<Uuid> {
    let mut v: Vec<Uuid> = vec![];
    for k in ["studentId", "archivedStudentId", "survivingStudentId"] {
        if let Some(id) = data
            .get(k)
            .and_then(|x| x.as_str())
            .and_then(|s| Uuid::parse_str(s.trim()).ok())
        {
            if !v.contains(&id) {
                v.push(id);
            }
        }
    }
    v
}

#[async_trait::async_trait]
impl IdentitySubjectSource for PgStudentIdentitySource {
    fn subject_type(&self) -> ErpSubjectType {
        ErpSubjectType::Student
    }

    fn event_types(&self) -> &'static [&'static str] {
        EVENT_TYPES
    }

    fn subjects_of(
        &self,
        _event_type: &str,
        _subject: Option<&str>,
        data: &serde_json::Value,
    ) -> Vec<Uuid> {
        subject_ids(data)
    }

    async fn facts(&self, tenant: Uuid, id: Uuid) -> Result<Option<IdentityFacts>, KernelError> {
        let row = sqlx::query(
            "SELECT s.id, s.student_number, s.given_name, s.family_name, s.primary_email, \
                    s.subject_ref, s.status, c.institution_id, c.status AS context_status \
             FROM student.student s \
             LEFT JOIN LATERAL ( \
                 SELECT sc.institution_id, sc.status FROM student.student_context sc \
                 WHERE sc.student_id = s.id \
                 ORDER BY (sc.status IN ('pending_activation', 'active', 'suspended')) DESC, \
                          sc.created_at DESC, sc.id DESC LIMIT 1) c ON true \
             WHERE s.tenant_id = $1 AND s.id = $2",
        )
        .bind(tenant)
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_db_error)?;
        let Some(r) = row else {
            return Ok(None);
        };
        let get = map_db_error;
        let status: String = r.try_get("status").map_err(get)?;
        let context: Option<String> = r.try_get("context_status").map_err(get)?;
        let email: Option<String> = r.try_get("primary_email").map_err(get)?;
        Ok(Some(IdentityFacts {
            subject_type: ErpSubjectType::Student,
            subject_id: id,
            tenant_id: tenant,
            institution_id: r.try_get("institution_id").map_err(get)?,
            number: r.try_get("student_number").map_err(get)?,
            email: email
                .map(|e| e.trim().to_ascii_lowercase())
                .filter(|e| e.contains('@')),
            given_name: r.try_get("given_name").map_err(get)?,
            family_name: r.try_get("family_name").map_err(get)?,
            state: account_state(&status, context.as_deref()),
            base_roles: vec![Role::Student],
            subject_ref: r.try_get("subject_ref").map_err(get)?,
        }))
    }

    async fn link_subject(
        &self,
        tenant: Uuid,
        id: Uuid,
        subject_ref: &str,
    ) -> Result<SubjectLink, KernelError> {
        let current: Option<Option<String>> = sqlx::query_scalar(
            "SELECT subject_ref FROM student.student WHERE tenant_id = $1 AND id = $2",
        )
        .bind(tenant)
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_db_error)?;
        match current {
            None => Ok(SubjectLink::Missing),
            Some(Some(s)) if s == subject_ref => Ok(SubjectLink::AlreadyLinked),
            Some(Some(_)) => Ok(SubjectLink::Conflict(
                "the student record is already linked to another login".into(),
            )),
            Some(None) => {
                let res = sqlx::query(
                    "UPDATE student.student SET subject_ref = $3, updated_at = now() \
                     WHERE tenant_id = $1 AND id = $2 AND subject_ref IS NULL",
                )
                .bind(tenant)
                .bind(id)
                .bind(subject_ref)
                .execute(&self.pool)
                .await;
                match res {
                    Ok(r) if r.rows_affected() == 1 => Ok(SubjectLink::Linked),
                    Ok(_) => Ok(SubjectLink::Conflict(
                        "the student record was linked concurrently".into(),
                    )),
                    Err(sqlx::Error::Database(d)) if d.is_unique_violation() => {
                        Ok(SubjectLink::Conflict(
                            "another student of the tenant is linked to this login".into(),
                        ))
                    }
                    Err(e) => Err(map_db_error(e)),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_state_follows_the_lifecycle() {
        assert_eq!(
            account_state("active", Some("active")),
            AccountState::Active
        );
        assert_eq!(
            account_state("activated", Some("pending_activation")),
            AccountState::Active
        );
        assert_eq!(
            account_state("suspended", Some("suspended")),
            AccountState::Suspended
        );
        assert_eq!(
            account_state("active", Some("suspended")),
            AccountState::Suspended
        );
        assert_eq!(account_state("created", None), AccountState::Suspended);
        assert_eq!(
            account_state("active", Some("withdrawn")),
            AccountState::Disabled
        );
        assert_eq!(account_state("active", None), AccountState::Disabled);
        assert_eq!(
            account_state("archived", Some("active")),
            AccountState::Disabled
        );
        assert_eq!(
            account_state("qualification_completed", None),
            AccountState::Disabled
        );
    }

    #[test]
    fn subjects_come_from_the_payload() {
        let id = Uuid::now_v7();
        let v = subject_ids(
            &serde_json::json!({"studentId": id.to_string(), "survivingStudentId": id.to_string()}),
        );
        assert_eq!(v, vec![id]);
        assert!(subject_ids(&serde_json::json!({})).is_empty());
    }
}
