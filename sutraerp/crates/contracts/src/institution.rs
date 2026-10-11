//! Domain B — Institution contracts.

use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId};

/// Read port other modules use to confirm an institution exists inside the caller's tenant.
#[async_trait::async_trait]
pub trait InstitutionDirectory: Send + Sync + 'static {
    async fn exists(&self, scope: &ExecutionScope, id: InstitutionId) -> Result<bool, KernelError>;
}

/// Released event contracts published by Domain B through the outbox (CloudEvents `type` +
/// `dataschema`, camelCase `data`). Consumers (e.g. the platform inbox projector) deserialize
/// these types instead of guessing at the payload.
pub mod events {
    use uuid::Uuid;

    pub const SOURCE: &str = "/sutraerp/institution";

    pub const INSTITUTION_REGISTERED: &str = "sutraerp.institution.institution.registered";
    pub const INSTITUTION_REGISTERED_SCHEMA: &str =
        "urn:sutraerp:schema:institution.institution.registered:1";
    pub const INSTITUTION_RENAMED: &str = "sutraerp.institution.institution.renamed";
    pub const INSTITUTION_RENAMED_SCHEMA: &str =
        "urn:sutraerp:schema:institution.institution.renamed:1";

    #[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct InstitutionRegisteredV1 {
        pub institution_id: Uuid,
        pub code: String,
        pub name: String,
    }

    #[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct InstitutionRenamedV1 {
        pub institution_id: Uuid,
        pub name: String,
        pub version: i64,
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn payloads_are_camel_case() {
            let v = serde_json::to_value(InstitutionRenamedV1 {
                institution_id: Uuid::nil(),
                name: "N".into(),
                version: 2,
            })
            .unwrap_or_default();
            assert!(v.get("institutionId").is_some() && v.get("version").is_some());
        }
    }
}
