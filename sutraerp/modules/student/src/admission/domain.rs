//! Admission state machines (L2-C01 §6.9.1 + expiry amendment 2026-08-22) and wire types.

use chrono::{DateTime, NaiveDate, Utc};
use sutra_contracts::student::ids::{
    AdmissionApplicationId, AdmissionDocumentId, AdmissionId, StudentContextId, StudentId,
};
use sutra_contracts::student::status::{
    AdmissionApplicationStatus, AdmissionStatus, DocumentStatus,
};
use sutra_kernel::concurrency::Version;
use sutra_kernel::scope::InstitutionId;
use uuid::Uuid;

/// Legal application transitions.
pub fn application_can_transition(
    from: AdmissionApplicationStatus,
    to: AdmissionApplicationStatus,
) -> bool {
    use AdmissionApplicationStatus::*;
    matches!(
        (from, to),
        (Draft, Submitted)
            | (Submitted, UnderReview)
            | (Submitted | UnderReview, Offered | Rejected)
            | (Draft | Submitted | UnderReview, Withdrawn)
    )
}

/// Legal Admission Record transitions (L2-C01 §6.9.1; `approval_pending` sits between
/// `verification_completed` and `approved`; a rejected approval → `cancelled`; `initiated →
/// expired` per the expiry amendment). `student_created → completed` happens on enrolment.
/// Revocation after student creation is policy-controlled (`student_created | completed →
/// revoked`).
pub fn admission_can_transition(from: AdmissionStatus, to: AdmissionStatus) -> bool {
    use AdmissionStatus::*;
    matches!(
        (from, to),
        (Initiated, DocumentsSubmitted)
            | (DocumentsSubmitted, VerificationCompleted)
            | (VerificationCompleted, ApprovalPending)
            | (ApprovalPending, Approved)
            | (Approved, FinanciallyCleared)
            | (FinanciallyCleared, Confirmed)
            | (Confirmed, StudentCreated)
            | (StudentCreated, Completed)
            | (
                Initiated
                    | DocumentsSubmitted
                    | VerificationCompleted
                    | ApprovalPending
                    | Approved
                    | FinanciallyCleared,
                Cancelled
            )
            | (Confirmed, Withdrawn)
            | (Confirmed | StudentCreated | Completed, Revoked)
            | (Initiated, Expired)
    )
}

/// Terminal admissions (no further transition).
pub fn admission_is_terminal(s: AdmissionStatus) -> bool {
    use AdmissionStatus::*;
    matches!(s, Completed | Cancelled | Withdrawn | Revoked | Expired)
}

/// Closed admissions a readmission may start from (L2-C01 §6.9.1 "Readmission (New Admission
/// Record)" from withdrawn / cancelled / revoked; expired offers and completed admissions whose
/// student no longer has a live context are re-entry candidates too).
pub fn readmission_source(s: AdmissionStatus) -> bool {
    use AdmissionStatus::*;
    matches!(s, Cancelled | Withdrawn | Revoked | Expired | Completed)
}

/// Documents count for verification when they are not superseded.
pub fn document_counts(s: DocumentStatus) -> bool {
    s != DocumentStatus::Superseded
}

/// Default documentary requirements when the offer does not name any.
pub const DEFAULT_REQUIRED_DOCUMENTS: &[&str] = &["identity_proof", "qualifying_marksheet"];

/// Verification outcome of the documents of an admission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerificationState {
    /// A required type has no live document.
    Missing(Vec<String>),
    /// Some documents still await verification.
    Pending,
    /// A required type only has rejected documents.
    Rejected(Vec<String>),
    /// Every required type has a verified or waived document and nothing is pending.
    Complete,
}

/// Evaluates `docs` (type, status) against `required` types.
pub fn verification_state(
    required: &[String],
    docs: &[(String, DocumentStatus)],
) -> VerificationState {
    let live: Vec<_> = docs.iter().filter(|(_, s)| document_counts(*s)).collect();
    let missing: Vec<String> = required
        .iter()
        .filter(|t| {
            !live
                .iter()
                .any(|(dt, s)| dt == *t && *s != DocumentStatus::Rejected)
        })
        .filter(|t| !live.iter().any(|(dt, _)| dt == *t))
        .cloned()
        .collect();
    if !missing.is_empty() {
        return VerificationState::Missing(missing);
    }
    if live.iter().any(|(_, s)| *s == DocumentStatus::Submitted) {
        return VerificationState::Pending;
    }
    let rejected: Vec<String> = required
        .iter()
        .filter(|t| {
            !live.iter().any(|(dt, s)| {
                dt == *t && matches!(s, DocumentStatus::Verified | DocumentStatus::Waived)
            })
        })
        .cloned()
        .collect();
    if !rejected.is_empty() {
        return VerificationState::Rejected(rejected);
    }
    VerificationState::Complete
}

/// Admission application (wire form).
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AdmissionApplication {
    pub id: AdmissionApplicationId,
    pub institution_id: InstitutionId,
    pub programme_id: Uuid,
    pub curriculum_version_id: Option<Uuid>,
    pub campus_id: Option<Uuid>,
    pub admission_term_id: Option<Uuid>,
    pub admission_year_id: Option<Uuid>,
    pub given_name: String,
    pub middle_name: Option<String>,
    pub family_name: Option<String>,
    pub display_name: String,
    pub date_of_birth: Option<NaiveDate>,
    pub gender: Option<String>,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub subject_ref: Option<String>,
    pub prior_qualification: Option<String>,
    pub notes: Option<String>,
    pub status: AdmissionApplicationStatus,
    pub status_reason: Option<String>,
    pub submitted_at: Option<DateTime<Utc>>,
    pub decided_at: Option<DateTime<Utc>>,
    /// Admission Record created by the offer.
    pub admission_id: Option<AdmissionId>,
    #[schema(value_type = i64)]
    pub version: Version,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Admission Record (wire form; contract §5.3 minimum fields included).
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Admission {
    pub id: AdmissionId,
    pub institution_id: InstitutionId,
    pub application_id: Option<AdmissionApplicationId>,
    pub programme_id: Uuid,
    pub curriculum_version_id: Uuid,
    pub campus_id: Option<Uuid>,
    pub admission_term_id: Option<Uuid>,
    pub admission_year_id: Option<Uuid>,
    pub given_name: String,
    pub middle_name: Option<String>,
    pub family_name: Option<String>,
    pub display_name: String,
    pub date_of_birth: Option<NaiveDate>,
    pub gender: Option<String>,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub subject_ref: Option<String>,
    /// Business-local date after which an un-actioned offer expires.
    pub offer_expires_on: Option<NaiveDate>,
    pub offer_accepted_at: Option<DateTime<Utc>>,
    pub required_documents: Vec<String>,
    /// `regular` | `lateral_entry` | `readmission` | `transfer_in` | `re_entry`.
    pub enrolment_type: String,
    pub status: AdmissionStatus,
    pub status_reason: Option<String>,
    pub process_instance_id: Option<String>,
    /// `approve` | `reject` once decided.
    pub approval_decision: Option<String>,
    pub approval_reason: Option<String>,
    pub approved_by: Option<String>,
    /// Finance-owned evidence the clearance decision used.
    pub finance_evidence: Option<serde_json::Value>,
    pub student_id: Option<StudentId>,
    pub student_context_id: Option<StudentContextId>,
    pub previous_admission_id: Option<AdmissionId>,
    #[schema(value_type = i64)]
    pub version: Version,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Document submitted for an admission.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AdmissionDocument {
    pub id: AdmissionDocumentId,
    pub admission_id: AdmissionId,
    pub document_type: String,
    pub title: String,
    pub file_ref: Option<String>,
    pub status: DocumentStatus,
    pub note: Option<String>,
    pub verified_by: Option<String>,
    pub verified_at: Option<DateTime<Utc>>,
    /// Part 2 · S4-docs: the stored file (platform document service), when one was uploaded
    /// (`PUT …/documents/{documentId}/file`).
    pub document_object_id: Option<Uuid>,
    #[schema(value_type = i64)]
    pub version: Version,
    pub created_at: DateTime<Utc>,
}

/// One admission lifecycle history row.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AdmissionHistoryEntry {
    pub from_status: Option<String>,
    pub to_status: String,
    pub reason: Option<String>,
    pub process_instance_id: Option<String>,
    pub actor: Option<String>,
    pub occurred_at: DateTime<Utc>,
}

/// "Given Middle Family".
pub fn display_name(given: &str, middle: Option<&str>, family: Option<&str>) -> String {
    [Some(given), middle, family]
        .into_iter()
        .flatten()
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use AdmissionStatus as A;

    #[test]
    fn application_machine() {
        use AdmissionApplicationStatus as P;
        assert!(application_can_transition(P::Draft, P::Submitted));
        assert!(application_can_transition(P::UnderReview, P::Offered));
        assert!(application_can_transition(P::Submitted, P::Rejected));
        assert!(!application_can_transition(P::Draft, P::Offered));
        assert!(!application_can_transition(P::Offered, P::Withdrawn));
        assert!(!application_can_transition(P::Rejected, P::Submitted));
    }

    #[test]
    fn admission_machine_follows_l2() {
        let happy = [
            A::Initiated,
            A::DocumentsSubmitted,
            A::VerificationCompleted,
            A::ApprovalPending,
            A::Approved,
            A::FinanciallyCleared,
            A::Confirmed,
            A::StudentCreated,
            A::Completed,
        ];
        for w in happy.windows(2) {
            assert!(admission_can_transition(w[0], w[1]), "{} → {}", w[0], w[1]);
        }
        // No skipping verification or approval.
        assert!(!admission_can_transition(
            A::DocumentsSubmitted,
            A::Approved
        ));
        assert!(!admission_can_transition(
            A::VerificationCompleted,
            A::Approved
        ));
        // Cancellation not after confirmation; withdrawal only from confirmed.
        assert!(admission_can_transition(
            A::FinanciallyCleared,
            A::Cancelled
        ));
        assert!(!admission_can_transition(A::Confirmed, A::Cancelled));
        assert!(!admission_can_transition(A::StudentCreated, A::Cancelled));
        assert!(admission_can_transition(A::Confirmed, A::Withdrawn));
        assert!(!admission_can_transition(A::StudentCreated, A::Withdrawn));
        // Expiry only from initiated; terminal outcomes stay.
        assert!(admission_can_transition(A::Initiated, A::Expired));
        assert!(!admission_can_transition(A::DocumentsSubmitted, A::Expired));
        for t in [
            A::Cancelled,
            A::Withdrawn,
            A::Revoked,
            A::Expired,
            A::Completed,
        ] {
            assert!(admission_is_terminal(t));
            for to in A::ALL {
                if t == A::Completed && *to == A::Revoked {
                    continue;
                }
                assert!(!admission_can_transition(t, *to), "{t} → {to}");
            }
        }
        assert!(admission_can_transition(A::StudentCreated, A::Revoked));
        assert!(readmission_source(A::Withdrawn) && !readmission_source(A::Confirmed));
    }

    #[test]
    fn verification_needs_every_required_type() {
        use DocumentStatus as D;
        let req = vec!["identity_proof".to_owned(), "marks".to_owned()];
        let d = |t: &str, s| (t.to_owned(), s);
        assert_eq!(
            verification_state(&req, &[d("identity_proof", D::Verified)]),
            VerificationState::Missing(vec!["marks".into()])
        );
        assert_eq!(
            verification_state(
                &req,
                &[d("identity_proof", D::Verified), d("marks", D::Submitted)]
            ),
            VerificationState::Pending
        );
        assert_eq!(
            verification_state(
                &req,
                &[d("identity_proof", D::Verified), d("marks", D::Rejected)]
            ),
            VerificationState::Rejected(vec!["marks".into()])
        );
        assert_eq!(
            verification_state(
                &req,
                &[
                    d("identity_proof", D::Waived),
                    d("marks", D::Rejected),
                    d("marks", D::Verified),
                    d("marks", D::Superseded)
                ]
            ),
            VerificationState::Complete
        );
        assert_eq!(display_name("A", Some(" "), Some("B")), "A B");
    }
}
