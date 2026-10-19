//! Pure domain types and validation (no I/O).

use chrono::{DateTime, Utc};
use sutra_contracts::institution::status::InstitutionStatus;
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::{FieldViolation, KernelError};
use sutra_kernel::scope::{InstitutionId, TenantId};

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Institution {
    pub id: InstitutionId,
    pub tenant_id: TenantId,
    pub code: String,
    pub name: String,
    /// IANA timezone governing business-local dates for this institution.
    pub timezone: String,
    /// Lifecycle state (L2-B01 §6).
    pub status: InstitutionStatus,
    pub status_changed_at: DateTime<Utc>,
    /// Profile / master information (L2-B01 §7), revised through `PUT …/profile`.
    pub profile: crate::lifecycle::domain::InstitutionProfile,
    pub version: Version,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Allowed institution lifecycle transitions (L2-B01 §6 + activation-authority amendment):
/// `draft → configured → pending_approval → {active | returned | rejected}`,
/// `returned | rejected → configured` (a new request cycle; never directly active),
/// `active ⇄ suspended`, `active | suspended → archived` (terminal).
pub fn can_transition(from: InstitutionStatus, to: InstitutionStatus) -> bool {
    use InstitutionStatus::*;
    matches!(
        (from, to),
        (Draft | Returned | Rejected, Configured)
            | (Configured, PendingApproval)
            | (PendingApproval, Active | Returned | Rejected)
            | (Active, Suspended)
            | (Suspended, Active)
            | (Active | Suspended, Archived)
    )
}

/// 409 for a transition [`can_transition`] refuses.
pub fn illegal_transition(
    entity: &str,
    from: impl std::fmt::Display,
    to: impl std::fmt::Display,
) -> KernelError {
    KernelError::Conflict(format!("{entity} cannot move from `{from}` to `{to}`"))
}

#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RegisterInstitution {
    /// Short institution code, unique within the tenant (`A-Z0-9-`, 2–32 chars).
    #[schema(example = "SIT")]
    pub code: String,
    #[schema(example = "Sutra Institute of Technology")]
    pub name: String,
    #[schema(example = "Asia/Kolkata")]
    pub timezone: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RenameInstitution {
    pub name: String,
}

/// Institution names are identity text (audit 2026-10 A2, SEC-1): trimmed, 1-200 characters,
/// single line, no control characters and no `<` / `>`.
fn validate_name(name: &str, v: &mut Vec<FieldViolation>) {
    sutra_kernel::text::identity("name", name, 200, true, v);
}

impl RegisterInstitution {
    pub fn validate(&self) -> Result<(), KernelError> {
        let mut v = vec![];
        let code = self.code.trim();
        if !(2..=32).contains(&code.len())
            || !code
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'-')
        {
            v.push(FieldViolation::new(
                "code",
                "format",
                "code must be 2-32 characters of A-Z, 0-9 or '-'",
            ));
        }
        validate_name(&self.name, &mut v);
        if let Some(tz) = &self.timezone {
            if !crate::lifecycle::domain::timezone_format_ok(tz) {
                v.push(FieldViolation::new(
                    "timezone",
                    "format",
                    "timezone must be an IANA zone such as Asia/Kolkata",
                ));
            }
        }
        if v.is_empty() {
            Ok(())
        } else {
            Err(KernelError::Validation(v))
        }
    }
}

impl RenameInstitution {
    pub fn validate(&self) -> Result<(), KernelError> {
        let mut v = vec![];
        validate_name(&self.name, &mut v);
        if v.is_empty() {
            Ok(())
        } else {
            Err(KernelError::Validation(v))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reg(code: &str, name: &str, tz: Option<&str>) -> RegisterInstitution {
        RegisterInstitution {
            code: code.into(),
            name: name.into(),
            timezone: tz.map(Into::into),
        }
    }

    #[test]
    fn validates_registration() {
        assert!(reg("SIT", "Sutra Institute", Some("Asia/Kolkata"))
            .validate()
            .is_ok());
        let Err(KernelError::Validation(v)) = reg("s", "", Some("IST")).validate() else {
            panic!("expected violations")
        };
        let fields: Vec<_> = v.iter().map(|x| x.field.as_str()).collect();
        assert_eq!(fields, vec!["code", "name", "timezone"]);
    }

    #[test]
    fn institution_lifecycle_transitions() {
        use InstitutionStatus::*;
        let allowed = [
            (Draft, Configured),
            (Returned, Configured),
            (Rejected, Configured),
            (Configured, PendingApproval),
            (PendingApproval, Active),
            (PendingApproval, Returned),
            (PendingApproval, Rejected),
            (Active, Suspended),
            (Suspended, Active),
            (Active, Archived),
            (Suspended, Archived),
        ];
        for from in InstitutionStatus::ALL {
            for to in InstitutionStatus::ALL {
                assert_eq!(
                    can_transition(*from, *to),
                    allowed.contains(&(*from, *to)),
                    "{from} -> {to}"
                );
            }
        }
        // L2-B01 rule 7: Rejected never goes straight to Active; Archived is terminal.
        assert!(!can_transition(Rejected, Active));
        assert!(InstitutionStatus::ALL
            .iter()
            .all(|t| !can_transition(Archived, *t)));
        assert!(matches!(
            illegal_transition("institution", Draft, Active),
            KernelError::Conflict(_)
        ));
    }

    #[test]
    fn validates_rename() {
        assert!(RenameInstitution {
            name: "x".repeat(201)
        }
        .validate()
        .is_err());
        assert!(RenameInstitution { name: "New".into() }.validate().is_ok());
        // SEC-1: markup and control characters are refused, field-level.
        for bad in ["<img src=x onerror=alert(1)>", "Sutra\u{0}", "a\nb"] {
            let Err(KernelError::Validation(v)) = RenameInstitution { name: bad.into() }.validate()
            else {
                panic!("{bad} accepted");
            };
            assert_eq!(v[0].field, "name");
        }
    }
}
