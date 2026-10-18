//! Staff members as identity subjects (Part 2 · S4-idp, port
//! [`sutra_contracts::platform::identity::IdentitySubjectSource`]).
//!
//! Account state from the staff record and its current engagement:
//!
//! | staff record | engagement | account |
//! |---|---|---|
//! | `active` | `active` · `long_leave` · `deputed` | **active** (joined) |
//! | `active` | `suspended` · `pending` | suspended |
//! | `suspended`, or `draft` · `pending_verification` · `verified` (not joined yet) | — | suspended |
//! | `merged` · `archived`, or engagement `closed` · `cancelled` / none (separation) | — | disabled |
//!
//! Teaching staff get the `faculty` role from the record; every other role is granted through
//! the identity admin API. Reads and the `subject_ref` write stay inside Domain E's schema.

use sqlx::{PgPool, Row};
use sutra_contracts::people::events::workforce;
use sutra_contracts::platform::identity::{
    AccountState, ErpSubjectType, IdentityFacts, IdentitySubjectSource, SubjectLink,
};
use sutra_kernel::auth::Role;
use sutra_kernel::error::KernelError;
use sutra_persistence::map_db_error;
use uuid::Uuid;

/// Domain E identity source.
#[derive(Clone)]
pub struct PgStaffIdentitySource {
    pool: PgPool,
}

impl PgStaffIdentitySource {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

/// Events that can change a staff member's account.
pub const EVENT_TYPES: &[&str] = &[
    workforce::STAFF_ACTIVATED,
    workforce::STAFF_SUSPENDED,
    workforce::STAFF_RESTORED,
    workforce::STAFF_MERGED,
    workforce::STAFF_ARCHIVED,
    workforce::STAFF_MODIFIED,
    workforce::ENGAGEMENT_ACTIVATED,
    workforce::ENGAGEMENT_SUSPENDED,
    workforce::ENGAGEMENT_RESTORED,
    workforce::ENGAGEMENT_CLOSED,
    workforce::ENGAGEMENT_TRANSFERRED,
    workforce::ONBOARDING_COMPLETED,
    workforce::SEPARATION_COMPLETED,
];

/// Account state of a staff record status + its current engagement status (pure).
pub fn account_state(staff: &str, engagement: Option<&str>) -> AccountState {
    match staff {
        "merged" | "archived" => AccountState::Disabled,
        "active" => match engagement {
            Some("active" | "long_leave" | "deputed") => AccountState::Active,
            Some("suspended" | "pending") => AccountState::Suspended,
            _ => AccountState::Disabled,
        },
        _ => AccountState::Suspended,
    }
}

/// Staff ids an event names (pure): `staffId`, or the aggregate of a `staff_member.*` event.
pub fn subject_ids(event_type: &str, data: &serde_json::Value) -> Vec<Uuid> {
    let uuid = |k: &str| {
        data.get(k)
            .and_then(|x| x.as_str())
            .and_then(|s| Uuid::parse_str(s.trim()).ok())
    };
    let id = uuid("staffId").or_else(|| {
        event_type
            .contains(".staff_member.")
            .then(|| uuid("aggregateId"))
            .flatten()
    });
    id.into_iter().collect()
}

#[async_trait::async_trait]
impl IdentitySubjectSource for PgStaffIdentitySource {
    fn subject_type(&self) -> ErpSubjectType {
        ErpSubjectType::Staff
    }

    fn event_types(&self) -> &'static [&'static str] {
        EVENT_TYPES
    }

    fn subjects_of(
        &self,
        event_type: &str,
        _subject: Option<&str>,
        data: &serde_json::Value,
    ) -> Vec<Uuid> {
        subject_ids(event_type, data)
    }

    async fn facts(&self, tenant: Uuid, id: Uuid) -> Result<Option<IdentityFacts>, KernelError> {
        let row = sqlx::query(
            "SELECT m.institution_id, m.staff_number, m.given_name, m.family_name, m.work_email, \
                    m.subject_ref, m.status, m.staff_category, e.status AS engagement_status \
             FROM people.staff_member m \
             LEFT JOIN LATERAL ( \
                 SELECT se.status FROM people.staff_engagement se WHERE se.staff_id = m.id \
                 ORDER BY (se.status NOT IN ('closed', 'cancelled')) DESC, se.start_date DESC, \
                          se.created_at DESC LIMIT 1) e ON true \
             WHERE m.tenant_id = $1 AND m.id = $2",
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
        let engagement: Option<String> = r.try_get("engagement_status").map_err(get)?;
        let category: String = r.try_get("staff_category").map_err(get)?;
        let email: Option<String> = r.try_get("work_email").map_err(get)?;
        Ok(Some(IdentityFacts {
            subject_type: ErpSubjectType::Staff,
            subject_id: id,
            tenant_id: tenant,
            institution_id: Some(r.try_get("institution_id").map_err(get)?),
            number: r.try_get("staff_number").map_err(get)?,
            email: email
                .map(|e| e.trim().to_ascii_lowercase())
                .filter(|e| e.contains('@')),
            given_name: r.try_get("given_name").map_err(get)?,
            family_name: r.try_get("family_name").map_err(get)?,
            state: account_state(&status, engagement.as_deref()),
            base_roles: if category == "teaching" {
                vec![Role::Faculty]
            } else {
                vec![]
            },
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
            "SELECT subject_ref FROM people.staff_member WHERE tenant_id = $1 AND id = $2",
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
                "the staff record is already linked to another login".into(),
            )),
            Some(None) => {
                let res = sqlx::query(
                    "UPDATE people.staff_member SET subject_ref = $3, updated_at = now() \
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
                        "the staff record was linked concurrently".into(),
                    )),
                    Err(sqlx::Error::Database(d)) if d.is_unique_violation() => {
                        Ok(SubjectLink::Conflict(
                            "another live staff record of the tenant is linked to this login"
                                .into(),
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
    fn account_state_follows_joining_and_separation() {
        assert_eq!(
            account_state("active", Some("active")),
            AccountState::Active
        );
        assert_eq!(
            account_state("active", Some("long_leave")),
            AccountState::Active
        );
        assert_eq!(
            account_state("active", Some("suspended")),
            AccountState::Suspended
        );
        assert_eq!(
            account_state("active", Some("pending")),
            AccountState::Suspended
        );
        assert_eq!(account_state("verified", None), AccountState::Suspended);
        assert_eq!(
            account_state("suspended", Some("active")),
            AccountState::Suspended
        );
        assert_eq!(
            account_state("active", Some("closed")),
            AccountState::Disabled
        );
        assert_eq!(
            account_state("archived", Some("active")),
            AccountState::Disabled
        );
        assert_eq!(account_state("merged", None), AccountState::Disabled);
    }

    #[test]
    fn subjects_come_from_the_payload() {
        let id = Uuid::now_v7();
        assert_eq!(
            subject_ids(
                workforce::ENGAGEMENT_CLOSED,
                &serde_json::json!({"staffId": id})
            ),
            vec![id]
        );
        assert_eq!(
            subject_ids(
                workforce::STAFF_SUSPENDED,
                &serde_json::json!({"aggregateId": id})
            ),
            vec![id]
        );
        assert!(subject_ids(
            workforce::ENGAGEMENT_CLOSED,
            &serde_json::json!({"aggregateId": id})
        )
        .is_empty());
    }
}
