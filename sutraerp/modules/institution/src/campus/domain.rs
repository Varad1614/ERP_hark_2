//! Institutional campus: pure types, validation and lifecycle rules (no I/O).
//!
//! Wire states follow `CampusStatus` (contract §4.2). Mapping to the L3/L4 vocabulary:
//! `registered` = Proposed, `pending_activation` = activation decision in progress (Approved
//! pending effect), `active` = Operational, `suspended` = Temporarily Closed.

use crate::curriculum::support::{
    code_format, finish, optional_text, required_text, timezone_ok, violation,
};
use chrono::{DateTime, NaiveDate, Utc};
use sutra_contracts::institution::ids::CampusId;
use sutra_contracts::institution::status::CampusStatus;
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::{FieldViolation, KernelError};
use sutra_kernel::scope::InstitutionId;

/// Institutional classification of a campus (L2-B03 §23 "Campus Type").
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum CampusType {
    Main,
    Satellite,
    City,
    Medical,
    Engineering,
    Research,
    OffCampus,
    Online,
    Other,
}

impl CampusType {
    pub const ALL: &'static [CampusType] = &[
        Self::Main,
        Self::Satellite,
        Self::City,
        Self::Medical,
        Self::Engineering,
        Self::Research,
        Self::OffCampus,
        Self::Online,
        Self::Other,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Main => "main",
            Self::Satellite => "satellite",
            Self::City => "city",
            Self::Medical => "medical",
            Self::Engineering => "engineering",
            Self::Research => "research",
            Self::OffCampus => "off_campus",
            Self::Online => "online",
            Self::Other => "other",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|t| t.as_str() == s)
    }
}

/// Registered institutional address.
#[derive(
    Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CampusAddress {
    pub line1: Option<String>,
    pub line2: Option<String>,
    pub city: Option<String>,
    pub state: Option<String>,
    pub postal_code: Option<String>,
    /// ISO 3166-1 alpha-2 (default `IN`).
    #[schema(example = "IN")]
    pub country: Option<String>,
}

impl CampusAddress {
    fn validate(&self, v: &mut Vec<FieldViolation>) {
        optional_text("address.line1", self.line1.as_ref(), 200, v);
        optional_text("address.line2", self.line2.as_ref(), 200, v);
        optional_text("address.city", self.city.as_ref(), 100, v);
        optional_text("address.state", self.state.as_ref(), 100, v);
        if let Some(pc) = &self.postal_code {
            let pc = pc.trim();
            if pc.is_empty()
                || pc.len() > 12
                || !pc
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b' ' || b == b'-')
            {
                v.push(violation(
                    "address.postalCode",
                    "format",
                    "postal code must be 1-12 letters, digits, spaces or '-'",
                ));
            }
        }
        if let Some(c) = &self.country {
            if c.len() != 2 || !c.bytes().all(|b| b.is_ascii_uppercase()) {
                v.push(violation(
                    "address.country",
                    "format",
                    "country must be an ISO 3166-1 alpha-2 code such as IN",
                ));
            }
        }
    }

    /// Missing members required before the campus can become operational (L4-B-018-BH-002).
    pub fn missing_for_activation(&self) -> Vec<&'static str> {
        let blank = |s: &Option<String>| s.as_deref().is_none_or(|x| x.trim().is_empty());
        let mut m = vec![];
        if blank(&self.line1) {
            m.push("address.line1");
        }
        if blank(&self.city) {
            m.push("address.city");
        }
        if blank(&self.state) {
            m.push("address.state");
        }
        if blank(&self.postal_code) {
            m.push("address.postalCode");
        }
        m
    }
}

/// Geographic reference of the campus.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GeoPoint {
    pub latitude: f64,
    pub longitude: f64,
}

impl GeoPoint {
    fn validate(&self, v: &mut Vec<FieldViolation>) {
        if !(-90.0..=90.0).contains(&self.latitude) || !(-180.0..=180.0).contains(&self.longitude) {
            v.push(violation(
                "geo",
                "range",
                "latitude must be within ±90 and longitude within ±180",
            ));
        }
    }
}

/// An institutional campus (B03). Physical facts (buildings, spaces, authoritative capacity)
/// are Domain G's and are not part of this resource.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Campus {
    pub id: CampusId,
    pub institution_id: InstitutionId,
    pub code: String,
    pub name: String,
    pub campus_type: CampusType,
    pub is_main: bool,
    pub address: CampusAddress,
    pub geo: Option<GeoPoint>,
    /// IANA zone overriding the institution's business timezone for this campus.
    pub timezone: Option<String>,
    pub established_on: Option<NaiveDate>,
    /// Capacity declared at registration (informational; authoritative physical capacity is
    /// owned by Domain G and never changed by a campus modification).
    pub declared_capacity: Option<i32>,
    pub contact_email: Option<String>,
    pub contact_phone: Option<String>,
    pub description: Option<String>,
    pub working_hours: Option<String>,
    pub status: CampusStatus,
    /// Governed decision awaiting a decision-maker (`activation` / `decommission`), if any.
    pub pending_action: Option<String>,
    pub process_instance_id: Option<String>,
    pub status_reason: Option<String>,
    pub decommission_effective_on: Option<NaiveDate>,
    pub status_changed_at: DateTime<Utc>,
    pub version: Version,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct CampusList {
    pub items: Vec<Campus>,
}

/// One entry of a campus' change history.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CampusRevision {
    pub revision: i64,
    /// `registered`, `modified` or `status`.
    pub kind: String,
    pub changed_fields: Vec<String>,
    pub from_status: Option<String>,
    pub to_status: Option<String>,
    pub reason: Option<String>,
    pub actor: Option<String>,
    /// The campus as it was after this change.
    #[schema(value_type = Object)]
    pub snapshot: serde_json::Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct CampusRevisionList {
    pub items: Vec<CampusRevision>,
}

/// `POST /institutions/{id}/campuses`.
#[derive(Clone, Debug, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RegisterCampus {
    /// Unique within the institution (`A-Z0-9-`, 2–32), never reused.
    #[schema(example = "MAIN")]
    pub code: String,
    #[schema(example = "Main Campus")]
    pub name: String,
    pub campus_type: CampusType,
    #[serde(default)]
    pub is_main: bool,
    #[serde(default)]
    pub address: CampusAddress,
    pub geo: Option<GeoPoint>,
    #[schema(example = "Asia/Kolkata")]
    pub timezone: Option<String>,
    pub established_on: Option<NaiveDate>,
    pub declared_capacity: Option<i32>,
    pub contact_email: Option<String>,
    pub contact_phone: Option<String>,
    pub description: Option<String>,
    pub working_hours: Option<String>,
}

/// `PATCH /campuses/{id}`: governed modification of B03 metadata (L3-B-019). Absent members
/// stay unchanged; `null` clears an optional member. Code, institution, type and declared
/// capacity are not modifiable.
#[derive(Clone, Debug, Default, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModifyCampus {
    pub name: Option<String>,
    pub is_main: Option<bool>,
    /// Replaces the whole address.
    pub address: Option<CampusAddress>,
    #[serde(default, with = "patch")]
    #[schema(value_type = Option<GeoPoint>)]
    pub geo: Option<Option<GeoPoint>>,
    #[serde(default, with = "patch")]
    #[schema(value_type = Option<String>)]
    pub timezone: Option<Option<String>>,
    #[serde(default, with = "patch")]
    #[schema(value_type = Option<String>)]
    pub contact_email: Option<Option<String>>,
    #[serde(default, with = "patch")]
    #[schema(value_type = Option<String>)]
    pub contact_phone: Option<Option<String>>,
    #[serde(default, with = "patch")]
    #[schema(value_type = Option<String>)]
    pub description: Option<Option<String>>,
    #[serde(default, with = "patch")]
    #[schema(value_type = Option<String>)]
    pub working_hours: Option<Option<String>>,
}

/// `Option<Option<T>>` patch semantics: absent → `None`, `null` → `Some(None)`.
mod patch {
    use serde::{Deserialize, Deserializer};

    pub fn deserialize<'de, D, T>(d: D) -> Result<Option<Option<T>>, D::Error>
    where
        D: Deserializer<'de>,
        T: Deserialize<'de>,
    {
        Option::<T>::deserialize(d).map(Some)
    }
}

/// `POST /campuses/{id}/decommission` (L3-B-021): explicit effective context + reason.
#[derive(Clone, Debug, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DecommissionCampus {
    pub reason: String,
    /// Business-local date from which the campus is decommissioned.
    pub effective_on: NaiveDate,
}

fn validate_email(field: &str, e: Option<&String>, v: &mut Vec<FieldViolation>) {
    if let Some(e) = e {
        let ok = e.len() <= 254
            && e.split_once('@')
                .is_some_and(|(l, d)| !l.is_empty() && d.contains('.') && !d.starts_with('.'));
        if !ok {
            v.push(violation(field, "format", "must be an email address"));
        }
    }
}

fn validate_phone(field: &str, p: Option<&String>, v: &mut Vec<FieldViolation>) {
    if let Some(p) = p {
        let digits = p.bytes().filter(u8::is_ascii_digit).count();
        if !(6..=15).contains(&digits)
            || !p
                .bytes()
                .all(|b| b.is_ascii_digit() || matches!(b, b'+' | b' ' | b'-' | b'(' | b')'))
        {
            v.push(violation(field, "format", "must be a phone number"));
        }
    }
}

fn validate_tz(tz: Option<&String>, v: &mut Vec<FieldViolation>) {
    if let Some(tz) = tz {
        if !timezone_ok(tz) {
            v.push(violation(
                "timezone",
                "format",
                "timezone must be an IANA zone such as Asia/Kolkata",
            ));
        }
    }
}

impl RegisterCampus {
    pub fn validate(&self) -> Result<(), KernelError> {
        let mut v = vec![];
        code_format("code", &self.code, &mut v);
        required_text("name", &self.name, 200, &mut v);
        self.address.validate(&mut v);
        if let Some(g) = &self.geo {
            g.validate(&mut v);
        }
        validate_tz(self.timezone.as_ref(), &mut v);
        if let Some(c) = self.declared_capacity {
            if c <= 0 {
                v.push(violation(
                    "declaredCapacity",
                    "range",
                    "declared capacity must be positive",
                ));
            }
        }
        validate_email("contactEmail", self.contact_email.as_ref(), &mut v);
        validate_phone("contactPhone", self.contact_phone.as_ref(), &mut v);
        optional_text("description", self.description.as_ref(), 2000, &mut v);
        optional_text("workingHours", self.working_hours.as_ref(), 200, &mut v);
        if self.campus_type == CampusType::Main && !self.is_main {
            v.push(violation(
                "isMain",
                "inconsistent",
                "a campus of type `main` must be the main campus",
            ));
        }
        finish(v)
    }
}

impl ModifyCampus {
    /// Validates and returns the camelCase names of the members present.
    pub fn validate(&self) -> Result<Vec<&'static str>, KernelError> {
        let mut v = vec![];
        let mut fields = vec![];
        if let Some(n) = &self.name {
            required_text("name", n, 200, &mut v);
            fields.push("name");
        }
        if self.is_main.is_some() {
            fields.push("isMain");
        }
        if let Some(a) = &self.address {
            a.validate(&mut v);
            fields.push("address");
        }
        if let Some(g) = &self.geo {
            if let Some(g) = g {
                g.validate(&mut v);
            }
            fields.push("geo");
        }
        if let Some(tz) = &self.timezone {
            validate_tz(tz.as_ref(), &mut v);
            fields.push("timezone");
        }
        if let Some(e) = &self.contact_email {
            validate_email("contactEmail", e.as_ref(), &mut v);
            fields.push("contactEmail");
        }
        if let Some(p) = &self.contact_phone {
            validate_phone("contactPhone", p.as_ref(), &mut v);
            fields.push("contactPhone");
        }
        if let Some(d) = &self.description {
            optional_text("description", d.as_ref(), 2000, &mut v);
            fields.push("description");
        }
        if let Some(w) = &self.working_hours {
            optional_text("workingHours", w.as_ref(), 200, &mut v);
            fields.push("workingHours");
        }
        if fields.is_empty() {
            v.push(violation("body", "empty", "no modifiable member given"));
        }
        finish(v)?;
        Ok(fields)
    }
}

impl DecommissionCampus {
    pub fn validate(&self) -> Result<(), KernelError> {
        let mut v = vec![];
        required_text("reason", &self.reason, 2000, &mut v);
        finish(v)
    }
}

/// Allowed campus lifecycle transitions (L2-B03 / L4-B-017 … B-022).
///
/// * registered → pending_activation (activation requested) → active (decision approved) or
///   back to registered (rejected); no registered → active shortcut (L4-B-018-BH-001).
/// * active ⇄ suspended (temporary closure / restoration, L4-B-020).
/// * active → decommissioned only (L4-B-021-BH-001); no way back (BH-006).
/// * decommissioned → archived only (L4-B-022-BH-001/002); archived is terminal.
pub fn can_transition(from: CampusStatus, to: CampusStatus) -> bool {
    use CampusStatus as S;
    matches!(
        (from, to),
        (S::Registered, S::PendingActivation)
            | (S::PendingActivation, S::Active | S::Registered)
            | (S::Active, S::Suspended | S::Decommissioned)
            | (S::Suspended, S::Active)
            | (S::Decommissioned, S::Archived)
    )
}

/// 409 unless `from → to` is allowed.
pub fn ensure_transition(from: CampusStatus, to: CampusStatus) -> Result<(), KernelError> {
    if can_transition(from, to) {
        Ok(())
    } else {
        Err(KernelError::Conflict(format!(
            "campus cannot move from {from} to {to}"
        )))
    }
}

/// Metadata may change while the campus is not decommissioned / archived.
pub fn modifiable(status: CampusStatus) -> bool {
    !matches!(
        status,
        CampusStatus::Decommissioned | CampusStatus::Archived
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use CampusStatus as S;

    #[test]
    fn lifecycle_matrix() {
        let allowed = [
            (S::Registered, S::PendingActivation),
            (S::PendingActivation, S::Active),
            (S::PendingActivation, S::Registered),
            (S::Active, S::Suspended),
            (S::Suspended, S::Active),
            (S::Active, S::Decommissioned),
            (S::Decommissioned, S::Archived),
        ];
        for from in S::ALL {
            for to in S::ALL {
                assert_eq!(
                    can_transition(*from, *to),
                    allowed.contains(&(*from, *to)),
                    "{from} -> {to}"
                );
            }
        }
        assert!(ensure_transition(S::Registered, S::Active).is_err());
        assert!(ensure_transition(S::Suspended, S::Archived).is_err());
        assert!(ensure_transition(S::Archived, S::Active).is_err());
        assert!(modifiable(S::Suspended) && !modifiable(S::Archived));
    }

    fn reg() -> RegisterCampus {
        RegisterCampus {
            code: "MAIN".into(),
            name: "Main Campus".into(),
            campus_type: CampusType::Main,
            is_main: true,
            address: CampusAddress::default(),
            geo: None,
            timezone: None,
            established_on: None,
            declared_capacity: None,
            contact_email: None,
            contact_phone: None,
            description: None,
            working_hours: None,
        }
    }

    #[test]
    fn registration_validation() {
        assert!(reg().validate().is_ok());
        let mut bad = reg();
        bad.code = "m".into();
        bad.is_main = false;
        bad.timezone = Some("IST".into());
        bad.geo = Some(GeoPoint {
            latitude: 91.0,
            longitude: 0.0,
        });
        bad.contact_email = Some("nope".into());
        bad.declared_capacity = Some(0);
        let Err(KernelError::Validation(v)) = bad.validate() else {
            panic!("expected violations")
        };
        let f: Vec<_> = v.iter().map(|x| x.field.as_str()).collect();
        assert_eq!(
            f,
            vec![
                "code",
                "geo",
                "timezone",
                "declaredCapacity",
                "contactEmail",
                "isMain"
            ]
        );
        assert_eq!(
            CampusAddress::default().missing_for_activation(),
            vec![
                "address.line1",
                "address.city",
                "address.state",
                "address.postalCode"
            ]
        );
        for t in CampusType::ALL {
            assert_eq!(CampusType::parse(t.as_str()), Some(*t));
        }
    }

    #[test]
    fn modification_patch_semantics() {
        let m: ModifyCampus =
            serde_json::from_str(r#"{"description": null, "name": "City"}"#).unwrap_or_default();
        assert_eq!(m.description, Some(None));
        assert_eq!(m.geo, None);
        assert_eq!(m.validate().ok(), Some(vec!["name", "description"]));
        assert!(ModifyCampus::default().validate().is_err());
        assert!(serde_json::from_str::<ModifyCampus>(r#"{"code": "X"}"#).is_err());
    }
}
