//! Pure 2A lifecycle rules: institution profile (L2-B01 §7), timezone format, activation
//! decisions and the shared command bodies. No I/O.

use chrono::NaiveDate;
use sqlx::Row;
use sutra_contracts::institution::status::InstitutionStatus;
use sutra_kernel::error::{FieldViolation, KernelError};

/// Institution type (identity master information). `school` drives 2B's school-model readiness.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum InstitutionType {
    School,
    College,
    University,
    Autonomous,
    Affiliated,
    Other,
}

impl InstitutionType {
    pub const ALL: &'static [Self] = &[
        Self::School,
        Self::College,
        Self::University,
        Self::Autonomous,
        Self::Affiliated,
        Self::Other,
    ];
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::School => "school",
            Self::College => "college",
            Self::University => "university",
            Self::Autonomous => "autonomous",
            Self::Affiliated => "affiliated",
            Self::Other => "other",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|v| v.as_str() == s)
    }
}

/// Academic pattern (operational master information).
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum AcademicPattern {
    Annual,
    Semester,
    Trimester,
    Term,
}

impl AcademicPattern {
    pub const ALL: &'static [Self] = &[Self::Annual, Self::Semester, Self::Trimester, Self::Term];
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Annual => "annual",
            Self::Semester => "semester",
            Self::Trimester => "trimester",
            Self::Term => "term",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|v| v.as_str() == s)
    }
}

/// Weekday keys accepted in `workingDays`.
pub const WEEKDAYS: &[&str] = &["mon", "tue", "wed", "thu", "fri", "sat", "sun"];

/// Profile / master information of an institution (L2-B01 §7). Name, code and timezone live on
/// the institution itself.
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "camelCase")]
pub struct InstitutionProfile {
    pub short_name: Option<String>,
    pub institution_type: Option<InstitutionType>,
    /// Required when `institutionType` is `affiliated`.
    pub affiliated_university: Option<String>,
    pub regulatory_bodies: Vec<String>,
    pub registration_number: Option<String>,
    pub address: Option<String>,
    pub contact_phone: Option<String>,
    pub official_email: Option<String>,
    pub website: Option<String>,
    pub academic_pattern: Option<AcademicPattern>,
    /// Subset of `mon`…`sun`.
    pub working_days: Vec<String>,
    /// BCP 47 language tag, e.g. `en-IN`.
    pub default_language: Option<String>,
    /// Assigned Institution Administrator (contact e-mail).
    pub administrator_email: Option<String>,
    /// Number of applied profile revisions.
    pub revision: i32,
}

impl InstitutionProfile {
    pub(crate) fn from_row(r: &sqlx::postgres::PgRow) -> Result<Self, sqlx::Error> {
        let it: Option<String> = r.try_get("institution_type")?;
        let ap: Option<String> = r.try_get("academic_pattern")?;
        Ok(Self {
            short_name: r.try_get("short_name")?,
            institution_type: it.as_deref().and_then(InstitutionType::parse),
            affiliated_university: r.try_get("affiliated_university")?,
            regulatory_bodies: r.try_get("regulatory_bodies")?,
            registration_number: r.try_get("registration_number")?,
            address: r.try_get("address")?,
            contact_phone: r.try_get("contact_phone")?,
            official_email: r.try_get("official_email")?,
            website: r.try_get("website")?,
            academic_pattern: ap.as_deref().and_then(AcademicPattern::parse),
            working_days: r.try_get("working_days")?,
            default_language: r.try_get("default_language")?,
            administrator_email: r.try_get("administrator_email")?,
            revision: r.try_get("profile_revision")?,
        })
    }

    /// camelCase names of mandatory master information that is still missing (activation
    /// validation, L2-B01 §9 / L4-B-001-BH-002).
    pub fn missing_fields(&self) -> Vec<&'static str> {
        let blank = |o: &Option<String>| o.as_deref().is_none_or(|s| s.trim().is_empty());
        let mut m = vec![];
        if blank(&self.short_name) {
            m.push("shortName");
        }
        if self.institution_type.is_none() {
            m.push("institutionType");
        }
        if self.institution_type == Some(InstitutionType::Affiliated)
            && blank(&self.affiliated_university)
        {
            m.push("affiliatedUniversity");
        }
        if blank(&self.registration_number) {
            m.push("registrationNumber");
        }
        if blank(&self.address) {
            m.push("address");
        }
        if blank(&self.contact_phone) {
            m.push("contactPhone");
        }
        if blank(&self.official_email) {
            m.push("officialEmail");
        }
        if self.academic_pattern.is_none() {
            m.push("academicPattern");
        }
        if self.working_days.is_empty() {
            m.push("workingDays");
        }
        if blank(&self.default_language) {
            m.push("defaultLanguage");
        }
        if blank(&self.administrator_email) {
            m.push("administratorEmail");
        }
        m
    }

    /// Current values as camelCase JSON (for revision history).
    pub fn field_value(&self, field: &str) -> serde_json::Value {
        serde_json::to_value(self)
            .ok()
            .and_then(|v| v.get(field).cloned())
            .unwrap_or(serde_json::Value::Null)
    }
}

/// `PUT …/institutions/{id}/profile`: every member is optional; an absent member is unchanged,
/// an empty string / empty list clears it.
#[derive(Debug, Clone, Default, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReviseProfile {
    pub short_name: Option<String>,
    pub institution_type: Option<InstitutionType>,
    pub affiliated_university: Option<String>,
    pub regulatory_bodies: Option<Vec<String>>,
    pub registration_number: Option<String>,
    pub address: Option<String>,
    pub contact_phone: Option<String>,
    pub official_email: Option<String>,
    pub website: Option<String>,
    pub academic_pattern: Option<AcademicPattern>,
    pub working_days: Option<Vec<String>>,
    pub default_language: Option<String>,
    pub administrator_email: Option<String>,
}

fn text_ok(v: &mut Vec<FieldViolation>, field: &str, value: &Option<String>, max: usize) {
    if let Some(s) = value {
        if s.trim().chars().count() > max {
            v.push(FieldViolation::new(
                field,
                "too-long",
                format!("{field} must be at most {max} characters"),
            ));
        }
    }
}

fn email_ok(s: &str) -> bool {
    let s = s.trim();
    let Some((local, domain)) = s.split_once('@') else {
        return false;
    };
    !local.is_empty()
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && s.len() <= 254
        && !s.contains(char::is_whitespace)
}

fn norm(o: &Option<String>) -> Option<String> {
    o.as_ref()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
}

impl ReviseProfile {
    pub fn validate(&self) -> Result<(), KernelError> {
        let mut v = vec![];
        text_ok(&mut v, "shortName", &self.short_name, 50);
        text_ok(
            &mut v,
            "affiliatedUniversity",
            &self.affiliated_university,
            200,
        );
        text_ok(&mut v, "registrationNumber", &self.registration_number, 100);
        text_ok(&mut v, "address", &self.address, 500);
        text_ok(&mut v, "defaultLanguage", &self.default_language, 35);
        for (field, value) in [
            ("officialEmail", &self.official_email),
            ("administratorEmail", &self.administrator_email),
        ] {
            if let Some(e) = norm(value) {
                if !email_ok(&e) {
                    v.push(FieldViolation::new(
                        field,
                        "format",
                        "must be an e-mail address",
                    ));
                }
            }
        }
        if let Some(p) = norm(&self.contact_phone) {
            let digits = p.chars().filter(char::is_ascii_digit).count();
            if !(6..=20).contains(&digits)
                || !p
                    .chars()
                    .all(|c| c.is_ascii_digit() || matches!(c, '+' | '-' | ' ' | '(' | ')'))
            {
                v.push(FieldViolation::new(
                    "contactPhone",
                    "format",
                    "contactPhone must be a phone number (6-20 digits)",
                ));
            }
        }
        if let Some(w) = norm(&self.website) {
            if !(w.starts_with("https://") || w.starts_with("http://")) || w.len() > 300 {
                v.push(FieldViolation::new(
                    "website",
                    "format",
                    "website must be an http(s) URL",
                ));
            }
        }
        if let Some(l) = norm(&self.default_language) {
            if !(2..=35).contains(&l.len())
                || !l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
            {
                v.push(FieldViolation::new(
                    "defaultLanguage",
                    "format",
                    "defaultLanguage must be a BCP 47 tag such as en-IN",
                ));
            }
        }
        if let Some(days) = &self.working_days {
            let mut seen = std::collections::HashSet::new();
            if days
                .iter()
                .any(|d| !WEEKDAYS.contains(&d.as_str()) || !seen.insert(d.as_str()))
            {
                v.push(FieldViolation::new(
                    "workingDays",
                    "format",
                    "workingDays must be distinct values of mon, tue, wed, thu, fri, sat, sun",
                ));
            }
        }
        if let Some(bodies) = &self.regulatory_bodies {
            if bodies.len() > 20
                || bodies
                    .iter()
                    .any(|b| b.trim().is_empty() || b.trim().chars().count() > 100)
            {
                v.push(FieldViolation::new(
                    "regulatoryBodies",
                    "format",
                    "regulatoryBodies takes up to 20 non-empty names of at most 100 characters",
                ));
            }
        }
        if v.is_empty() {
            Ok(())
        } else {
            Err(KernelError::Validation(v))
        }
    }

    /// Applies the command to `current`; returns the new profile and the changed camelCase
    /// members (empty = nothing changed).
    pub fn apply(&self, current: &InstitutionProfile) -> (InstitutionProfile, Vec<&'static str>) {
        let mut next = current.clone();
        let mut changed = vec![];
        macro_rules! text {
            ($field:ident, $name:literal) => {
                if let Some(v) = &self.$field {
                    let n = norm(&Some(v.clone()));
                    if n != current.$field {
                        next.$field = n;
                        changed.push($name);
                    }
                }
            };
        }
        text!(short_name, "shortName");
        if let Some(t) = self.institution_type {
            if Some(t) != current.institution_type {
                next.institution_type = Some(t);
                changed.push("institutionType");
            }
        }
        text!(affiliated_university, "affiliatedUniversity");
        if let Some(b) = &self.regulatory_bodies {
            let b: Vec<String> = b.iter().map(|x| x.trim().to_owned()).collect();
            if b != current.regulatory_bodies {
                next.regulatory_bodies = b;
                changed.push("regulatoryBodies");
            }
        }
        text!(registration_number, "registrationNumber");
        text!(address, "address");
        text!(contact_phone, "contactPhone");
        text!(official_email, "officialEmail");
        text!(website, "website");
        if let Some(p) = self.academic_pattern {
            if Some(p) != current.academic_pattern {
                next.academic_pattern = Some(p);
                changed.push("academicPattern");
            }
        }
        if let Some(d) = &self.working_days {
            // Canonical weekday order.
            let mut d = d.clone();
            d.sort_by_key(|x| WEEKDAYS.iter().position(|w| w == x));
            if d != current.working_days {
                next.working_days = d;
                changed.push("workingDays");
            }
        }
        text!(default_language, "defaultLanguage");
        text!(administrator_email, "administratorEmail");
        (next, changed)
    }
}

/// Syntactic IANA zone check (the repository also checks PostgreSQL's zone catalogue).
pub fn timezone_format_ok(tz: &str) -> bool {
    let tz = tz.trim();
    (tz == "UTC" || tz.contains('/'))
        && tz.len() <= 64
        && !tz.starts_with('/')
        && !tz.ends_with('/')
        && !tz.starts_with("posix/")
        && !tz.starts_with("right/")
        && tz
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '-' | '+'))
}

/// `PUT …/institutions/{id}/timezone`.
#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChangeTimezone {
    /// IANA zone, e.g. `Asia/Kolkata`.
    #[schema(example = "Asia/Kolkata")]
    pub timezone: String,
}

/// Profile and timezone may be revised while the institution is not under activation review
/// (configuration is locked in `pending_approval`) and not read-only (`suspended`, `archived`).
pub fn profile_editable(status: InstitutionStatus) -> bool {
    matches!(
        status,
        InstitutionStatus::Draft
            | InstitutionStatus::Configured
            | InstitutionStatus::Returned
            | InstitutionStatus::Rejected
            | InstitutionStatus::Active
    )
}

/// Optional body of a state command (`{ "reason": "…" }`).
#[derive(Debug, Clone, Default, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CommandBody {
    /// Business reason / decision note.
    pub reason: Option<String>,
}

impl CommandBody {
    pub fn reason(&self) -> Option<String> {
        norm(&self.reason)
    }

    /// Reason as mandatory evidence (422 when missing or longer than 1000 characters).
    pub fn required_reason(&self) -> Result<String, KernelError> {
        let r = self.reason().ok_or_else(|| {
            KernelError::Validation(vec![FieldViolation::new(
                "reason",
                "required",
                "a reason is required for this decision",
            )])
        })?;
        check_reason_len(&r)?;
        Ok(r)
    }

    pub fn optional_reason(&self) -> Result<Option<String>, KernelError> {
        let r = self.reason();
        if let Some(r) = &r {
            check_reason_len(r)?;
        }
        Ok(r)
    }
}

fn check_reason_len(r: &str) -> Result<(), KernelError> {
    if r.chars().count() > 1000 {
        return Err(KernelError::Validation(vec![FieldViolation::new(
            "reason",
            "too-long",
            "reason must be at most 1000 characters",
        )]));
    }
    Ok(())
}

/// Outcome chosen by the Institution Activation Authority.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ActivationDecision {
    Approve,
    Return,
    Reject,
}

impl ActivationDecision {
    pub fn target(&self) -> InstitutionStatus {
        match self {
            Self::Approve => InstitutionStatus::Active,
            Self::Return => InstitutionStatus::Returned,
            Self::Reject => InstitutionStatus::Rejected,
        }
    }
    pub fn request_status(&self) -> &'static str {
        match self {
            Self::Approve => "approved",
            Self::Return => "returned",
            Self::Reject => "rejected",
        }
    }
}

/// `POST …/institutions/{id}/decision`.
#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DecideActivation {
    pub decision: ActivationDecision,
    /// Required for `return` and `reject`.
    pub reason: Option<String>,
}

/// One governed activation request cycle.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ActivationRequest {
    pub id: sutra_contracts::institution::ids::ActivationRequestId,
    pub institution_id: sutra_kernel::scope::InstitutionId,
    /// `pending`, `approved`, `returned` or `rejected`.
    pub status: String,
    pub submitted_version: i64,
    pub process_instance_id: Option<String>,
    /// Readiness evidence evaluated at submission.
    pub readiness: crate::shared::readiness::ReadinessReport,
    pub submitted_by: Option<String>,
    pub submitted_at: chrono::DateTime<chrono::Utc>,
    pub decided_by: Option<String>,
    pub decided_at: Option<chrono::DateTime<chrono::Utc>>,
    pub decision_reason: Option<String>,
}

/// One applied profile revision (previous and new values of the changed members).
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProfileRevision {
    pub revision: i32,
    pub changed_fields: Vec<String>,
    pub previous_values: serde_json::Value,
    pub new_values: serde_json::Value,
    /// Business-local date (institution timezone) the revision applied from.
    pub effective_from: NaiveDate,
    pub revised_by: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn complete() -> InstitutionProfile {
        InstitutionProfile {
            short_name: Some("SIT".into()),
            institution_type: Some(InstitutionType::College),
            registration_number: Some("REG-1".into()),
            address: Some("1 Campus Road".into()),
            contact_phone: Some("+91 20 1234 5678".into()),
            official_email: Some("office@sit.edu".into()),
            academic_pattern: Some(AcademicPattern::Semester),
            working_days: vec!["mon".into(), "tue".into()],
            default_language: Some("en-IN".into()),
            administrator_email: Some("admin@sit.edu".into()),
            ..Default::default()
        }
    }

    #[test]
    fn mandatory_master_information() {
        assert!(complete().missing_fields().is_empty());
        assert_eq!(InstitutionProfile::default().missing_fields().len(), 10);
        let mut p = complete();
        p.institution_type = Some(InstitutionType::Affiliated);
        assert_eq!(p.missing_fields(), vec!["affiliatedUniversity"]);
        p.address = Some("  ".into());
        assert_eq!(p.missing_fields(), vec!["affiliatedUniversity", "address"]);
    }

    #[test]
    fn revise_validates_and_reports_changes() {
        let bad = ReviseProfile {
            official_email: Some("nope".into()),
            contact_phone: Some("12".into()),
            website: Some("ftp://x".into()),
            working_days: Some(vec!["mon".into(), "mon".into()]),
            default_language: Some("en_IN".into()),
            ..Default::default()
        };
        let Err(KernelError::Validation(v)) = bad.validate() else {
            panic!("violations expected")
        };
        let fields: Vec<_> = v.iter().map(|x| x.field.as_str()).collect();
        for f in [
            "officialEmail",
            "contactPhone",
            "website",
            "defaultLanguage",
            "workingDays",
        ] {
            assert!(fields.contains(&f), "{f}");
        }
        let cmd = ReviseProfile {
            short_name: Some(" SIT ".into()),
            address: Some("2 New Road".into()),
            working_days: Some(vec!["fri".into(), "mon".into()]),
            website: Some(String::new()),
            ..Default::default()
        };
        assert!(cmd.validate().is_ok());
        let (next, changed) = cmd.apply(&complete());
        assert_eq!(changed, vec!["address", "workingDays"]);
        assert_eq!(next.working_days, vec!["mon", "fri"]);
        assert_eq!(next.website, None);
        assert_eq!(next.field_value("address"), "2 New Road");
    }

    #[test]
    fn timezone_formats() {
        for ok in [
            "Asia/Kolkata",
            "America/Argentina/Buenos_Aires",
            "UTC",
            "Etc/GMT+5",
        ] {
            assert!(timezone_format_ok(ok), "{ok}");
        }
        for bad in [
            "",
            "IST",
            "/Asia",
            "posix/Asia/Kolkata",
            "Asia/Kolkata;drop",
            "Asia/",
        ] {
            assert!(!timezone_format_ok(bad), "{bad}");
        }
    }

    #[test]
    fn editability_and_decisions() {
        use InstitutionStatus::*;
        for s in InstitutionStatus::ALL {
            assert_eq!(
                profile_editable(*s),
                !matches!(s, PendingApproval | Suspended | Archived),
                "{s}"
            );
        }
        assert_eq!(ActivationDecision::Approve.target(), Active);
        assert_eq!(ActivationDecision::Return.request_status(), "returned");
        assert!(CommandBody::default().required_reason().is_err());
        let b = CommandBody {
            reason: Some("  audit  ".into()),
        };
        assert_eq!(b.required_reason().ok().as_deref(), Some("audit"));
        assert!(CommandBody {
            reason: Some("x".repeat(1001))
        }
        .optional_reason()
        .is_err());
    }
}
