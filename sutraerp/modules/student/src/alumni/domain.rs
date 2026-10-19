//! **3C** alumni transition DTOs and machine (L2-C08 §12.4).

use chrono::{DateTime, Utc};
use sutra_contracts::student::ids::{
    AlumniTransitionId, StudentContextId, StudentExitId, StudentId,
};
use sutra_contracts::student::status::AlumniTransitionStatus;
use uuid::Uuid;

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum AlumniBasis {
    /// A current conferred qualification of the context.
    Qualification,
    /// A completed withdrawal or transfer-out exit.
    EligibleExit,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AlumniEligibility {
    pub eligible: bool,
    pub basis: Option<AlumniBasis>,
    #[serde(default)]
    pub unmet: Vec<String>,
    pub evaluated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AlumniCorrection {
    pub id: Uuid,
    pub transition_version: i64,
    pub changed_fields: Vec<String>,
    #[schema(value_type = Object)]
    pub previous: serde_json::Value,
    #[schema(value_type = Object)]
    pub corrected: serde_json::Value,
    pub reason: String,
    pub actor: Option<String>,
    pub recorded_at: DateTime<Utc>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AlumniTransition {
    pub id: AlumniTransitionId,
    pub institution_id: Uuid,
    pub student_id: StudentId,
    pub student_context_id: StudentContextId,
    pub basis: Option<AlumniBasis>,
    pub conferred_qualification_id: Option<Uuid>,
    pub exit_id: Option<StudentExitId>,
    pub status: AlumniTransitionStatus,
    pub graduation_year: Option<i32>,
    pub alumni_email: Option<String>,
    pub directory_consent: bool,
    pub eligibility_result: Option<AlumniEligibility>,
    pub process_instance_id: Option<String>,
    pub decision_reason: Option<String>,
    pub activated_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub archived_at: Option<DateTime<Utc>>,
    pub correction_count: i32,
    pub status_reason: Option<String>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Correction history, oldest first (filled on reads).
    #[serde(default)]
    pub corrections: Vec<AlumniCorrection>,
}

#[derive(Clone, Debug, serde::Serialize, utoipa::ToSchema)]
pub struct AlumniTransitionList {
    pub items: Vec<AlumniTransition>,
}

#[derive(Clone, Debug, Default, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NewAlumniTransition {
    /// The terminal StudentContext (default: the student's most recent eligible terminal context).
    pub student_context_id: Option<StudentContextId>,
}

/// Institutional transition policy facts (L2-C08: the policy definition stays B01-owned).
#[derive(Clone, Debug, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ValidateAlumniPolicy {
    pub graduation_year: i32,
    pub alumni_email: Option<String>,
    #[serde(default)]
    pub directory_consent: bool,
}

#[derive(Clone, Debug, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CorrectAlumniTransition {
    pub graduation_year: Option<i32>,
    pub alumni_email: Option<String>,
    pub directory_consent: Option<bool>,
    pub reason: String,
}

/// Alumni transition machine (L2-C08 §12.4).
pub fn can_transition(from: AlumniTransitionStatus, to: AlumniTransitionStatus) -> bool {
    use AlumniTransitionStatus::*;
    matches!(
        (from, to),
        (Created, PendingVerification)
            | (PendingVerification, Verified)
            | (Verified, PendingApproval)
            | (PendingApproval, Approved | Rejected)
            | (Approved, Transitioning)
            | (Transitioning, Activated)
            | (Activated, Completed)
            | (Completed, Archived)
            | (Created | PendingVerification | Verified, Rejected)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alumni_machine() {
        use AlumniTransitionStatus::*;
        assert!(can_transition(Created, PendingVerification));
        assert!(!can_transition(PendingVerification, Approved));
        assert!(can_transition(Transitioning, Activated));
        assert!(!can_transition(Completed, Activated));
        assert!(!can_transition(Archived, Completed));
    }
}
