//! Pure domain types and validation (no I/O).

use chrono::{DateTime, Utc};
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
    pub version: Version,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
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

fn validate_name(name: &str, v: &mut Vec<FieldViolation>) {
    let len = name.trim().chars().count();
    if len == 0 {
        v.push(FieldViolation::new("name", "required", "name is required"));
    } else if len > 200 {
        v.push(FieldViolation::new(
            "name",
            "too-long",
            "name must be at most 200 characters",
        ));
    }
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
            if tz.trim().is_empty() || !tz.contains('/') {
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
    fn validates_rename() {
        assert!(RenameInstitution {
            name: "x".repeat(201)
        }
        .validate()
        .is_err());
        assert!(RenameInstitution { name: "New".into() }.validate().is_ok());
    }
}
