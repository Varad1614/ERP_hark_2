//! **2A** — organisation units (tree), type blueprints, governance configuration, structure
//! transformation, effective-dated position definitions.

use super::event_type;
use chrono::NaiveDate;
use uuid::Uuid;

event_type!(
    /// [`OrgUnitRegisteredV1`].
    ORG_UNIT_REGISTERED, ORG_UNIT_REGISTERED_SCHEMA, "org_unit.registered");
event_type!(
    /// Draft → Active (StatusChangedV1).
    ORG_UNIT_ACTIVATED, ORG_UNIT_ACTIVATED_SCHEMA, "org_unit.activated");
event_type!(
    /// Governed attribute change (ModifiedV1).
    ORG_UNIT_MODIFIED, ORG_UNIT_MODIFIED_SCHEMA, "org_unit.modified");
event_type!(
    /// Active → Suspended (StatusChangedV1).
    ORG_UNIT_SUSPENDED, ORG_UNIT_SUSPENDED_SCHEMA, "org_unit.suspended");
event_type!(
    /// Suspended → Active (StatusChangedV1).
    ORG_UNIT_RESTORED, ORG_UNIT_RESTORED_SCHEMA, "org_unit.restored");
event_type!(
    /// → Decommissioned (StatusChangedV1).
    ORG_UNIT_DECOMMISSIONED, ORG_UNIT_DECOMMISSIONED_SCHEMA, "org_unit.decommissioned");
event_type!(
    /// Decommissioned → Archived (StatusChangedV1).
    ORG_UNIT_ARCHIVED, ORG_UNIT_ARCHIVED_SCHEMA, "org_unit.archived");
event_type!(
    /// Approved structure transformation applied ([`OrgStructureRestructuredV1`]).
    ORG_STRUCTURE_RESTRUCTURED, ORG_STRUCTURE_RESTRUCTURED_SCHEMA, "org_structure.restructured");
event_type!(
    /// Type blueprint version published ([`OrgTypeBlueprintPublishedV1`]).
    ORG_TYPE_BLUEPRINT_PUBLISHED, ORG_TYPE_BLUEPRINT_PUBLISHED_SCHEMA,
    "org_type_blueprint.published");
event_type!(
    /// Governance configuration revision activated ([`GovernanceConfigActivatedV1`]).
    GOVERNANCE_CONFIG_ACTIVATED, GOVERNANCE_CONFIG_ACTIVATED_SCHEMA, "governance_config.activated");
event_type!(
    /// [`PositionDefinitionV1`] — established (Draft).
    POSITION_ESTABLISHED, POSITION_ESTABLISHED_SCHEMA, "position.established");
event_type!(
    /// Draft → Active (StatusChangedV1).
    POSITION_ACTIVATED, POSITION_ACTIVATED_SCHEMA, "position.activated");
event_type!(
    /// Effective-dated revision recorded ([`PositionDefinitionV1`]).
    POSITION_CHANGED, POSITION_CHANGED_SCHEMA, "position.changed");
event_type!(
    /// Active → Retired (StatusChangedV1).
    POSITION_RETIRED, POSITION_RETIRED_SCHEMA, "position.retired");
event_type!(
    /// Retired → Archived (StatusChangedV1).
    POSITION_ARCHIVED, POSITION_ARCHIVED_SCHEMA, "position.archived");

pub const EVENT_TYPES: &[&str] = &[
    ORG_UNIT_REGISTERED,
    ORG_UNIT_ACTIVATED,
    ORG_UNIT_MODIFIED,
    ORG_UNIT_SUSPENDED,
    ORG_UNIT_RESTORED,
    ORG_UNIT_DECOMMISSIONED,
    ORG_UNIT_ARCHIVED,
    ORG_STRUCTURE_RESTRUCTURED,
    ORG_TYPE_BLUEPRINT_PUBLISHED,
    GOVERNANCE_CONFIG_ACTIVATED,
    POSITION_ESTABLISHED,
    POSITION_ACTIVATED,
    POSITION_CHANGED,
    POSITION_RETIRED,
    POSITION_ARCHIVED,
];

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrgUnitRegisteredV1 {
    pub org_unit_id: Uuid,
    pub institution_id: Uuid,
    /// `None` for a root unit.
    pub parent_id: Option<Uuid>,
    /// Unit type key from the effective type blueprint (e.g. `department`).
    pub unit_type: String,
    pub code: String,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrgUnitMoveV1 {
    pub org_unit_id: Uuid,
    pub from_parent_id: Option<Uuid>,
    pub to_parent_id: Option<Uuid>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrgStructureRestructuredV1 {
    pub restructure_id: Uuid,
    pub institution_id: Uuid,
    pub moves: Vec<OrgUnitMoveV1>,
    /// Units decommissioned by the transformation (merge sources).
    pub decommissioned_unit_ids: Vec<Uuid>,
    /// Business-local effective date.
    pub effective_from: NaiveDate,
    pub process_instance_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrgTypeBlueprintPublishedV1 {
    pub blueprint_id: Uuid,
    pub institution_id: Uuid,
    pub version_no: i32,
    /// Allowed unit type keys.
    pub unit_types: Vec<String>,
    pub effective_from: NaiveDate,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GovernanceConfigActivatedV1 {
    pub governance_config_id: Uuid,
    pub org_unit_id: Uuid,
    pub institution_id: Uuid,
    pub revision: i32,
    pub effective_from: NaiveDate,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PositionDefinitionV1 {
    pub position_id: Uuid,
    pub institution_id: Uuid,
    pub org_unit_id: Uuid,
    pub code: String,
    pub title: String,
    /// Revision number of the effective-dated definition (1-based).
    pub revision: i32,
    pub effective_from: NaiveDate,
    pub effective_to: Option<NaiveDate>,
}
