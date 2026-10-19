//! Released event contracts published by Domain A (notices, to-dos) through the outbox.

use chrono::{DateTime, Utc};
use uuid::Uuid;

pub const SOURCE: &str = "/sutraerp/platform";

pub const NOTICE_PUBLISHED: &str = "sutraerp.platform.notice.published";
pub const NOTICE_PUBLISHED_SCHEMA: &str = "urn:sutraerp:schema:platform.notice.published:1";
pub const NOTICE_UPDATED: &str = "sutraerp.platform.notice.updated";
pub const NOTICE_UPDATED_SCHEMA: &str = "urn:sutraerp:schema:platform.notice.updated:1";
pub const NOTICE_ARCHIVED: &str = "sutraerp.platform.notice.archived";
pub const NOTICE_ARCHIVED_SCHEMA: &str = "urn:sutraerp:schema:platform.notice.archived:1";
pub const TODO_ASSIGNED: &str = "sutraerp.platform.todo.assigned";
pub const TODO_ASSIGNED_SCHEMA: &str = "urn:sutraerp:schema:platform.todo.assigned:1";

/// `notice.published` / `notice.updated` / `notice.archived`.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NoticeChangedV1 {
    pub notice_id: Uuid,
    pub title: String,
    /// Role names (`administrator`, `faculty`, `student`).
    pub audience_roles: Vec<String>,
    pub publish_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub pinned: bool,
    pub version: i64,
}

/// A to-do was assigned to someone other than its creator.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TodoAssignedV1 {
    pub todo_id: Uuid,
    pub assignee_subject: String,
    pub title: String,
    pub assigned_by: String,
    pub assigned_by_name: String,
    pub due_at: Option<DateTime<Utc>>,
}
