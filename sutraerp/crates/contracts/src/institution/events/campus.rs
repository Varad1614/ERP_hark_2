//! **2B** — institutional campuses (L2-B03).

use super::event_type;
use uuid::Uuid;

event_type!(
    /// [`CampusRegisteredV1`]. Read by 2C readiness consumers.
    CAMPUS_REGISTERED, CAMPUS_REGISTERED_SCHEMA, "campus.registered");
event_type!(
    /// → Active after the activation decision (StatusChangedV1). Read by 2C readiness.
    CAMPUS_ACTIVATED, CAMPUS_ACTIVATED_SCHEMA, "campus.activated");
event_type!(
    /// Governed modification (ModifiedV1).
    CAMPUS_MODIFIED, CAMPUS_MODIFIED_SCHEMA, "campus.modified");
event_type!(
    /// Active → Suspended (StatusChangedV1). Read by 2C readiness.
    CAMPUS_SUSPENDED, CAMPUS_SUSPENDED_SCHEMA, "campus.suspended");
event_type!(
    /// Suspended → Active (StatusChangedV1). Read by 2C readiness.
    CAMPUS_RESTORED, CAMPUS_RESTORED_SCHEMA, "campus.restored");
event_type!(
    /// → Decommissioned (StatusChangedV1). Read by 2C readiness.
    CAMPUS_DECOMMISSIONED, CAMPUS_DECOMMISSIONED_SCHEMA, "campus.decommissioned");
event_type!(
    /// Decommissioned → Archived (StatusChangedV1).
    CAMPUS_ARCHIVED, CAMPUS_ARCHIVED_SCHEMA, "campus.archived");

pub const EVENT_TYPES: &[&str] = &[
    CAMPUS_REGISTERED,
    CAMPUS_ACTIVATED,
    CAMPUS_MODIFIED,
    CAMPUS_SUSPENDED,
    CAMPUS_RESTORED,
    CAMPUS_DECOMMISSIONED,
    CAMPUS_ARCHIVED,
];

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CampusRegisteredV1 {
    pub campus_id: Uuid,
    pub institution_id: Uuid,
    pub code: String,
    pub name: String,
    /// IANA zone when the campus overrides the institution's business timezone.
    pub timezone: Option<String>,
    pub is_main: bool,
}
