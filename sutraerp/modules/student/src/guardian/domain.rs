//! **3C** guardian domain: DTOs, commands and the slice-owned machines (L2-C06 §6, L3-C-051 … C-061).

use chrono::{DateTime, NaiveDate, Utc};
use sutra_contracts::student::ids::{
    EmergencyContactId, GuardianConsentId, GuardianId, GuardianRelationshipId,
    PickupAuthorisationId, StudentId,
};
use sutra_contracts::student::status::{
    AssignmentStatus, GuardianConsentStatus, GuardianRelationshipStatus,
};
use uuid::Uuid;

macro_rules! wire_enum {
    ($(#[$m:meta])* $name:ident { $($v:ident => $w:literal),+ $(,)? }) => {
        $(#[$m])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
        #[serde(rename_all = "snake_case")]
        pub enum $name { $($v),+ }
        impl $name {
            pub fn as_str(self) -> &'static str { match self { $(Self::$v => $w),+ } }
        }
    };
}

wire_enum!(
    /// Relationship of the guardian to the student.
    RelationshipType {
        Mother => "mother", Father => "father", LegalGuardian => "legal_guardian",
        Grandparent => "grandparent", Sibling => "sibling", Relative => "relative",
        Sponsor => "sponsor", Other => "other",
    }
);
wire_enum!(
    /// Kind of guardian consent (L3-C-054).
    ConsentType {
        DataSharing => "data_sharing", MedicalTreatment => "medical_treatment",
        FieldTrip => "field_trip", Photography => "photography",
        Extracurricular => "extracurricular", Research => "research", Other => "other",
    }
);
wire_enum!(
    /// Channel a guardian may be contacted on (L3-C-055).
    CommunicationChannel { Email => "email", Sms => "sms", Phone => "phone", Portal => "portal" }
);
wire_enum!(
    /// Topic a guardian may receive (mandatory institutional notices are outside this authorisation).
    CommunicationTopic {
        Academic => "academic", Attendance => "attendance", Finance => "finance",
        Discipline => "discipline", Emergency => "emergency", General => "general",
    }
);
wire_enum!(
    /// What a financial responsibility covers (L3-C-057; invoices/payments stay with Finance).
    ResponsibilityType { Tuition => "tuition", Hostel => "hostel", Transport => "transport", All => "all" }
);

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Guardian {
    pub id: GuardianId,
    pub institution_id: Uuid,
    pub person_id: Option<Uuid>,
    pub given_name: String,
    pub family_name: Option<String>,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub address: Option<String>,
    pub occupation: Option<String>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, serde::Serialize, utoipa::ToSchema)]
pub struct GuardianList {
    pub items: Vec<Guardian>,
}

#[derive(Clone, Debug, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NewGuardian {
    pub given_name: String,
    pub family_name: Option<String>,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub address: Option<String>,
    pub occupation: Option<String>,
}

/// Contact details update (absent members are kept).
#[derive(Clone, Debug, Default, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GuardianUpdate {
    pub given_name: Option<String>,
    pub family_name: Option<String>,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub address: Option<String>,
    pub occupation: Option<String>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GuardianConsent {
    pub id: GuardianConsentId,
    pub relationship_id: GuardianRelationshipId,
    pub student_id: StudentId,
    pub consent_type: ConsentType,
    pub scope_note: Option<String>,
    pub evidence_ref: Option<String>,
    pub granted_on: NaiveDate,
    pub valid_until: Option<NaiveDate>,
    pub status: GuardianConsentStatus,
    pub status_reason: Option<String>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GuardianCommunication {
    pub id: Uuid,
    pub relationship_id: GuardianRelationshipId,
    pub student_id: StudentId,
    pub channels: Vec<CommunicationChannel>,
    pub topics: Vec<CommunicationTopic>,
    pub status: AssignmentStatus,
    pub status_reason: Option<String>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
}

/// Guardian ↔ student relationship (§5.3: id, guardianId, studentId, relationshipType, hasLegalCustody, status, version).
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GuardianRelationship {
    pub id: GuardianRelationshipId,
    pub institution_id: Uuid,
    pub guardian_id: GuardianId,
    pub student_id: StudentId,
    pub relationship_type: RelationshipType,
    pub is_primary: bool,
    pub has_legal_custody: bool,
    pub custody_basis: Option<String>,
    pub custody_effective_from: Option<NaiveDate>,
    pub status: GuardianRelationshipStatus,
    pub verification_method: Option<String>,
    pub verification_evidence: Option<String>,
    pub verified_at: Option<DateTime<Utc>>,
    pub verified_by: Option<String>,
    pub process_instance_id: Option<String>,
    pub notes: Option<String>,
    pub status_reason: Option<String>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// The guardian (filled on reads).
    #[serde(default)]
    pub guardian: Option<Guardian>,
    #[serde(default)]
    pub consents: Vec<GuardianConsent>,
    #[serde(default)]
    pub communications: Vec<GuardianCommunication>,
}

#[derive(Clone, Debug, serde::Serialize, utoipa::ToSchema)]
pub struct GuardianRelationshipList {
    pub items: Vec<GuardianRelationship>,
}

#[derive(Clone, Debug, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NewGuardianRelationship {
    pub guardian_id: GuardianId,
    pub relationship_type: RelationshipType,
    #[serde(default)]
    pub is_primary: bool,
    pub notes: Option<String>,
}

/// Governed modification of a relationship (L3-C-052). Custody uses `/custody`.
#[derive(Clone, Debug, Default, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GuardianRelationshipUpdate {
    pub relationship_type: Option<RelationshipType>,
    pub is_primary: Option<bool>,
    pub notes: Option<String>,
}

#[derive(Clone, Debug, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SubmitVerification {
    /// e.g. `document`, `in_person`, `court_order`.
    pub method: String,
    pub evidence_ref: String,
}

/// Legal custody designation / change (L3-C-053). Granting needs the legal basis.
#[derive(Clone, Debug, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CustodyChange {
    pub has_legal_custody: bool,
    pub custody_basis: Option<String>,
    pub effective_from: Option<NaiveDate>,
    pub reason: String,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CustodyRecord {
    pub id: Uuid,
    pub relationship_id: GuardianRelationshipId,
    pub has_legal_custody: bool,
    pub custody_basis: Option<String>,
    pub effective_from: Option<NaiveDate>,
    pub reason: Option<String>,
    pub actor: Option<String>,
    pub recorded_at: DateTime<Utc>,
}

#[derive(Clone, Debug, serde::Serialize, utoipa::ToSchema)]
pub struct CustodyHistory {
    pub items: Vec<CustodyRecord>,
}

#[derive(Clone, Debug, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NewGuardianConsent {
    pub consent_type: ConsentType,
    pub scope_note: Option<String>,
    pub evidence_ref: Option<String>,
    pub valid_until: Option<NaiveDate>,
}

#[derive(Clone, Debug, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NewGuardianCommunication {
    pub channels: Vec<CommunicationChannel>,
    pub topics: Vec<CommunicationTopic>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct EmergencyContact {
    pub id: EmergencyContactId,
    pub student_id: StudentId,
    pub relationship_id: Option<GuardianRelationshipId>,
    pub name: String,
    pub relation: String,
    pub phone: String,
    pub alternate_phone: Option<String>,
    pub priority: i32,
    pub status: AssignmentStatus,
    pub status_reason: Option<String>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, serde::Serialize, utoipa::ToSchema)]
pub struct EmergencyContactList {
    pub items: Vec<EmergencyContact>,
}

/// Assign an emergency contact: an active guardian relationship or a named person.
#[derive(Clone, Debug, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NewEmergencyContact {
    pub guardian_relationship_id: Option<GuardianRelationshipId>,
    pub name: Option<String>,
    pub relation: Option<String>,
    pub phone: Option<String>,
    pub alternate_phone: Option<String>,
    /// 1 = first to call (unique among the student's current contacts).
    pub priority: i32,
}

#[derive(Clone, Debug, Default, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct EmergencyContactUpdate {
    pub phone: Option<String>,
    pub alternate_phone: Option<String>,
    pub priority: Option<i32>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct FinancialResponsibility {
    pub id: Uuid,
    pub student_id: StudentId,
    pub relationship_id: GuardianRelationshipId,
    pub responsibility_type: ResponsibilityType,
    pub share_percent: f64,
    pub effective_from: NaiveDate,
    pub effective_to: Option<NaiveDate>,
    pub status: AssignmentStatus,
    pub status_reason: Option<String>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, serde::Serialize, utoipa::ToSchema)]
pub struct FinancialResponsibilityList {
    pub items: Vec<FinancialResponsibility>,
}

#[derive(Clone, Debug, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NewFinancialResponsibility {
    pub guardian_relationship_id: GuardianRelationshipId,
    pub responsibility_type: ResponsibilityType,
    pub share_percent: f64,
    pub effective_from: Option<NaiveDate>,
    pub effective_to: Option<NaiveDate>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PickupAuthorisation {
    pub id: PickupAuthorisationId,
    pub student_id: StudentId,
    pub relationship_id: Option<GuardianRelationshipId>,
    pub person_name: String,
    pub phone: Option<String>,
    pub id_document_ref: Option<String>,
    pub valid_from: NaiveDate,
    pub valid_until: Option<NaiveDate>,
    pub status: AssignmentStatus,
    pub status_reason: Option<String>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, serde::Serialize, utoipa::ToSchema)]
pub struct PickupAuthorisationList {
    pub items: Vec<PickupAuthorisation>,
}

/// Grant pickup: an active guardian relationship or a named person with an identity document.
#[derive(Clone, Debug, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NewPickupAuthorisation {
    pub guardian_relationship_id: Option<GuardianRelationshipId>,
    pub person_name: Option<String>,
    pub phone: Option<String>,
    pub id_document_ref: Option<String>,
    pub valid_from: Option<NaiveDate>,
    pub valid_until: Option<NaiveDate>,
}

// ---- machines (pure) ---------------------------------------------------------------------------

/// Guardian relationship transitions (L2-C06 §6; `Modified` is an event, not a state).
pub fn relationship_can_transition(
    from: GuardianRelationshipStatus,
    to: GuardianRelationshipStatus,
) -> bool {
    use GuardianRelationshipStatus::*;
    matches!(
        (from, to),
        (Proposed, PendingVerification)
            | (PendingVerification, Verified)
            | (PendingVerification, Proposed)
            | (Verified, Active)
            | (Active, Suspended)
            | (Suspended, Active)
            | (
                Proposed | PendingVerification | Verified | Active | Suspended,
                Terminated
            )
            | (Terminated, Historical)
    )
}

/// Assignment (contact / pickup / responsibility / communication) transitions.
pub fn assignment_can_transition(from: AssignmentStatus, to: AssignmentStatus) -> bool {
    use AssignmentStatus::*;
    matches!(
        (from, to),
        (Active, Suspended) | (Suspended, Active) | (Active | Suspended, Revoked | Expired)
    )
}

/// Consent transitions (withdrawal never deletes the grant).
pub fn consent_can_transition(from: GuardianConsentStatus, to: GuardianConsentStatus) -> bool {
    use GuardianConsentStatus::*;
    matches!((from, to), (Granted, Withdrawn | Expired))
}

/// Largest total share (percent) over each responsibility type when `new` is added to `active`
/// (`all` counts towards every type).
pub fn max_share_with(active: &[(ResponsibilityType, f64)], new: (ResponsibilityType, f64)) -> f64 {
    let concrete = [
        ResponsibilityType::Tuition,
        ResponsibilityType::Hostel,
        ResponsibilityType::Transport,
    ];
    concrete
        .iter()
        .map(|t| {
            active
                .iter()
                .chain(std::iter::once(&new))
                .filter(|(k, _)| k == t || *k == ResponsibilityType::All)
                .map(|(_, s)| s)
                .sum::<f64>()
        })
        .fold(0.0, f64::max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relationship_machine() {
        use GuardianRelationshipStatus::*;
        assert!(relationship_can_transition(Proposed, PendingVerification));
        assert!(!relationship_can_transition(Proposed, Active));
        assert!(relationship_can_transition(Verified, Active));
        assert!(relationship_can_transition(Suspended, Active));
        assert!(!relationship_can_transition(Terminated, Active));
        assert!(relationship_can_transition(Suspended, Terminated));
    }

    #[test]
    fn assignments_consents_and_shares() {
        use AssignmentStatus::*;
        assert!(assignment_can_transition(Active, Suspended));
        assert!(!assignment_can_transition(Revoked, Active));
        assert!(consent_can_transition(
            GuardianConsentStatus::Granted,
            GuardianConsentStatus::Withdrawn
        ));
        assert!(!consent_can_transition(
            GuardianConsentStatus::Withdrawn,
            GuardianConsentStatus::Granted
        ));
        let active = [
            (ResponsibilityType::Tuition, 60.0),
            (ResponsibilityType::Hostel, 100.0),
        ];
        assert!(
            (max_share_with(&active, (ResponsibilityType::Tuition, 40.0)) - 100.0).abs() < 1e-9
        );
        assert!(max_share_with(&active, (ResponsibilityType::All, 10.0)) > 100.0);
    }
}
