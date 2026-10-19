//! Pure organisation rules (L2-B02): unit lifecycle, type blueprint validation, hierarchy
//! cycle detection, governance settings. No I/O.

use crate::lifecycle::common::{finish, validate_code, validate_text};
use chrono::{DateTime, NaiveDate, Utc};
use std::collections::{HashMap, HashSet};
use sutra_contracts::institution::ids::{
    GovernanceConfigId, OrgRestructureId, OrgTypeBlueprintId, OrgUnitId,
};
use sutra_contracts::institution::status::{OrgUnitStatus, PublicationStatus};
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::{FieldViolation, KernelError};
use sutra_kernel::scope::InstitutionId;
use uuid::Uuid;

/// Allowed organisation unit transitions: `draft → active`, `active ⇄ suspended`,
/// `active | suspended → decommissioned`, `decommissioned → archived` (terminal).
pub fn can_transition(from: OrgUnitStatus, to: OrgUnitStatus) -> bool {
    use OrgUnitStatus::*;
    matches!(
        (from, to),
        (Draft, Active)
            | (Active, Suspended)
            | (Suspended, Active)
            | (Active | Suspended, Decommissioned)
            | (Decommissioned, Archived)
    )
}

/// States in which a unit is part of the live structure (may hold children / positions).
pub fn is_live(s: OrgUnitStatus) -> bool {
    matches!(
        s,
        OrgUnitStatus::Draft | OrgUnitStatus::Active | OrgUnitStatus::Suspended
    )
}

/// States under which a new child unit or position may be placed.
pub fn accepts_new_children(s: OrgUnitStatus) -> bool {
    matches!(s, OrgUnitStatus::Draft | OrgUnitStatus::Active)
}

// ---------------------------------------------------------------- type blueprints

/// One unit type of a blueprint version.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct UnitType {
    /// `a-z0-9_`, starting with a letter (2–40 characters), e.g. `department`.
    pub key: String,
    pub label: String,
    /// May be the root unit (no parent).
    #[serde(default)]
    pub allow_root: bool,
    /// Unit type keys allowed as parent (may include the type itself).
    #[serde(default)]
    pub allowed_parent_types: Vec<String>,
}

/// Organisation type blueprint version.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OrgTypeBlueprint {
    pub id: OrgTypeBlueprintId,
    pub institution_id: InstitutionId,
    pub version_no: i32,
    pub status: PublicationStatus,
    pub supersedes_id: Option<OrgTypeBlueprintId>,
    pub effective_from: NaiveDate,
    pub unit_types: Vec<UnitType>,
    pub process_instance_id: Option<String>,
    pub published_at: Option<DateTime<Utc>>,
    pub published_by: Option<String>,
    pub decision_reason: Option<String>,
    pub version: Version,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl OrgTypeBlueprint {
    pub fn unit_type(&self, key: &str) -> Option<&UnitType> {
        self.unit_types.iter().find(|t| t.key == key)
    }
}

/// `POST …/org-type-blueprints` and `PUT …/org-type-blueprints/{id}` (draft only).
#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DraftBlueprint {
    pub effective_from: NaiveDate,
    pub unit_types: Vec<UnitType>,
}

fn type_key_ok(k: &str) -> bool {
    (2..=40).contains(&k.len())
        && k.starts_with(|c: char| c.is_ascii_lowercase())
        && k.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

impl DraftBlueprint {
    pub fn validate(&self) -> Result<(), KernelError> {
        let mut v = vec![];
        if self.unit_types.is_empty() || self.unit_types.len() > 50 {
            v.push(FieldViolation::new(
                "unitTypes",
                "size",
                "a blueprint defines 1-50 unit types",
            ));
        }
        let keys: HashSet<&str> = self.unit_types.iter().map(|t| t.key.as_str()).collect();
        if keys.len() != self.unit_types.len() {
            v.push(FieldViolation::new(
                "unitTypes",
                "duplicate",
                "unit type keys must be unique",
            ));
        }
        for (i, t) in self.unit_types.iter().enumerate() {
            if !type_key_ok(&t.key) {
                v.push(FieldViolation::new(
                    format!("unitTypes[{i}].key"),
                    "format",
                    "key must be 2-40 characters of a-z, 0-9 or '_' starting with a letter",
                ));
            }
            validate_text(&format!("unitTypes[{i}].label"), &t.label, 100, &mut v);
            if let Some(bad) = t
                .allowed_parent_types
                .iter()
                .find(|p| !keys.contains(p.as_str()))
            {
                v.push(FieldViolation::new(
                    format!("unitTypes[{i}].allowedParentTypes"),
                    "unknown-type",
                    format!("`{bad}` is not a unit type of this blueprint"),
                ));
            }
            if !t.allow_root && t.allowed_parent_types.is_empty() {
                v.push(FieldViolation::new(
                    format!("unitTypes[{i}]"),
                    "unplaceable",
                    "a unit type must allow the root position or at least one parent type",
                ));
            }
        }
        if !self.unit_types.iter().any(|t| t.allow_root) {
            v.push(FieldViolation::new(
                "unitTypes",
                "no-root",
                "at least one unit type must allow the root position",
            ));
        }
        finish(v)
    }
}

/// Decision on a blueprint publication / restructure awaiting approval.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    Approve,
    Reject,
}

/// `POST …/decision` body of blueprint publications and restructures.
#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Decide {
    pub decision: ApprovalDecision,
    /// Required for `reject`.
    pub reason: Option<String>,
}

// ---------------------------------------------------------------- organisation units

/// Organisation unit (node of the tree).
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OrgUnit {
    pub id: OrgUnitId,
    pub institution_id: InstitutionId,
    pub parent_id: Option<OrgUnitId>,
    /// Materialised path `/<root id>/…/<id>/`.
    pub path: String,
    pub depth: i32,
    pub unit_type: String,
    /// Type blueprint version the unit is associated with.
    pub blueprint_id: OrgTypeBlueprintId,
    pub code: String,
    pub name: String,
    pub description: Option<String>,
    pub status: OrgUnitStatus,
    pub status_reason: Option<String>,
    pub version: Version,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// `POST …/institutions/{id}/org-units` (created in `draft`).
#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RegisterOrgUnit {
    /// Omit for the root unit.
    pub parent_id: Option<OrgUnitId>,
    /// Unit type key of the institution's published type blueprint.
    pub unit_type: String,
    #[schema(example = "CSE")]
    pub code: String,
    pub name: String,
    pub description: Option<String>,
}

impl RegisterOrgUnit {
    pub fn validate(&self) -> Result<(), KernelError> {
        let mut v = vec![];
        validate_code("code", &self.code, &mut v);
        validate_text("name", &self.name, 200, &mut v);
        if !type_key_ok(self.unit_type.trim()) {
            v.push(FieldViolation::new(
                "unitType",
                "format",
                "unitType must be a unit type key",
            ));
        }
        if let Some(d) = &self.description {
            if d.chars().count() > 2000 {
                v.push(FieldViolation::new(
                    "description",
                    "too-long",
                    "description must be at most 2000 characters",
                ));
            }
        }
        finish(v)
    }
}

/// `PATCH …/org-units/{id}`: ordinary governed modification of descriptive attributes.
/// Reparenting is a restructure, a type change is a type migration and decommissioning has
/// its own command: those members are refused with 422 (L4-B-008-BH-002..004).
#[derive(Debug, Clone, Default, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ModifyOrgUnit {
    pub name: Option<String>,
    /// Empty string clears it.
    pub description: Option<String>,
    /// Refused: use an organisation restructure.
    pub parent_id: Option<serde_json::Value>,
    /// Refused: use `POST …/migrate-type`.
    pub unit_type: Option<serde_json::Value>,
    /// Refused: use the lifecycle commands.
    pub status: Option<serde_json::Value>,
    /// Refused: the code is the unit's business identity.
    pub code: Option<serde_json::Value>,
}

impl ModifyOrgUnit {
    pub fn validate(&self) -> Result<(), KernelError> {
        let mut v = vec![];
        for (field, present, hint) in [
            (
                "parentId",
                self.parent_id.is_some(),
                "reparenting needs an approved organisation restructure",
            ),
            (
                "unitType",
                self.unit_type.is_some(),
                "a type change needs POST …/migrate-type",
            ),
            (
                "status",
                self.status.is_some(),
                "use the lifecycle commands",
            ),
            (
                "code",
                self.code.is_some(),
                "the code is the unit's business identity",
            ),
        ] {
            if present {
                v.push(FieldViolation::new(field, "not-modifiable", hint));
            }
        }
        if let Some(n) = &self.name {
            validate_text("name", n, 200, &mut v);
        }
        if let Some(d) = &self.description {
            if d.chars().count() > 2000 {
                v.push(FieldViolation::new(
                    "description",
                    "too-long",
                    "description must be at most 2000 characters",
                ));
            }
        }
        if self.name.is_none() && self.description.is_none() && v.is_empty() {
            v.push(FieldViolation::new(
                "name",
                "required",
                "nothing to modify (name or description)",
            ));
        }
        finish(v)
    }
}

/// `POST …/org-units/{id}/migrate-type`.
#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MigrateOrgUnitType {
    /// Target (published) blueprint version.
    pub target_blueprint_id: OrgTypeBlueprintId,
    /// Target unit type key; default: the current key.
    pub unit_type: Option<String>,
}

/// One version of a unit in its append-only history.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OrgUnitHistoryEntry {
    pub unit_version: i64,
    /// `registered`, `modified`, `activated`, `suspended`, `restored`, `decommissioned`,
    /// `archived`, `moved`, `merged_into`, `type_migrated`.
    pub change: String,
    pub parent_id: Option<OrgUnitId>,
    pub path: String,
    pub unit_type: String,
    pub blueprint_id: OrgTypeBlueprintId,
    pub code: String,
    pub name: String,
    pub description: Option<String>,
    pub status: OrgUnitStatus,
    pub reason: Option<String>,
    pub restructure_id: Option<OrgRestructureId>,
    pub effective_from: NaiveDate,
    pub recorded_by: Option<String>,
    pub recorded_at: DateTime<Utc>,
}

// ---------------------------------------------------------------- hierarchy

/// Returns the first unit whose proposed parent chain loops back to itself, given the current
/// `parent` map and the proposed `moves` (unit → new parent). Pure; used before any
/// structure change (L4-B-009-BH-002).
pub fn find_cycle(parent: &HashMap<Uuid, Option<Uuid>>, moves: &[(Uuid, Uuid)]) -> Option<Uuid> {
    let mut next: HashMap<Uuid, Option<Uuid>> = parent.clone();
    for (unit, to) in moves {
        next.insert(*unit, Some(*to));
    }
    for (unit, _) in moves {
        let mut seen = HashSet::new();
        let mut cur = Some(*unit);
        while let Some(c) = cur {
            if !seen.insert(c) {
                return Some(*unit);
            }
            cur = next.get(&c).copied().flatten();
        }
    }
    None
}

// ---------------------------------------------------------------- governance

/// Governance settings of an organisation unit.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GovernanceSettings {
    /// Title of the head of the unit, e.g. `Head of Department`.
    pub head_title: String,
    /// Approval levels for unit decisions (1–5).
    pub approval_levels: i32,
    /// Decision quorum of the governing body (1–50), if any.
    pub quorum: Option<i32>,
    #[serde(default)]
    pub delegation_allowed: bool,
    /// Standing committees (at most 20).
    #[serde(default)]
    pub committees: Vec<String>,
}

impl GovernanceSettings {
    pub fn validate(&self, v: &mut Vec<FieldViolation>) {
        validate_text("settings.headTitle", &self.head_title, 100, v);
        if !(1..=5).contains(&self.approval_levels) {
            v.push(FieldViolation::new(
                "settings.approvalLevels",
                "range",
                "approvalLevels must be 1-5",
            ));
        }
        if self.quorum.is_some_and(|q| !(1..=50).contains(&q)) {
            v.push(FieldViolation::new(
                "settings.quorum",
                "range",
                "quorum must be 1-50",
            ));
        }
        if self.committees.len() > 20
            || self
                .committees
                .iter()
                .any(|c| c.trim().is_empty() || c.chars().count() > 100)
        {
            v.push(FieldViolation::new(
                "settings.committees",
                "format",
                "up to 20 committee names of 1-100 characters",
            ));
        }
    }
}

/// Governance configuration revision of a unit.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GovernanceConfig {
    pub id: GovernanceConfigId,
    pub institution_id: InstitutionId,
    pub org_unit_id: OrgUnitId,
    pub revision: i32,
    pub status: PublicationStatus,
    pub supersedes_id: Option<GovernanceConfigId>,
    pub settings: GovernanceSettings,
    /// Business-local date from which the revision governs.
    pub effective_from: NaiveDate,
    /// Last business-local day it governs (set when a successor is activated).
    pub effective_to: Option<NaiveDate>,
    pub published_at: Option<DateTime<Utc>>,
    pub published_by: Option<String>,
    pub version: Version,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// `POST …/org-units/{id}/governance-configs` / `PUT …/governance-configs/{id}` (draft only).
#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DraftGovernanceConfig {
    pub settings: GovernanceSettings,
    pub effective_from: NaiveDate,
}

impl DraftGovernanceConfig {
    pub fn validate(&self) -> Result<(), KernelError> {
        let mut v = vec![];
        self.settings.validate(&mut v);
        finish(v)
    }
}

// ---------------------------------------------------------------- restructure

/// Kind of structure transformation.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum RestructureKind {
    /// Reparent units (with their subtrees).
    Move,
    /// Move the children of the source units under the target and decommission the sources.
    Merge,
}

/// One requested move.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RequestedMove {
    pub org_unit_id: OrgUnitId,
    pub to_parent_id: OrgUnitId,
}

/// `POST …/institutions/{id}/org-restructures`.
#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RequestRestructure {
    pub kind: RestructureKind,
    /// `move`: the units to reparent.
    #[serde(default)]
    pub moves: Vec<RequestedMove>,
    /// `merge`: units merged into `mergeTargetId` (then decommissioned).
    #[serde(default)]
    pub merge_source_ids: Vec<OrgUnitId>,
    pub merge_target_id: Option<OrgUnitId>,
    pub reason: String,
    /// Business-local effective date; must be today (institution timezone) in Batch 2 — the
    /// current structure stays authoritative until the transformation is applied.
    pub effective_from: Option<NaiveDate>,
}

impl RequestRestructure {
    pub fn validate(&self) -> Result<(), KernelError> {
        let mut v = vec![];
        validate_text("reason", &self.reason, 1000, &mut v);
        match self.kind {
            RestructureKind::Move => {
                if self.moves.is_empty() || self.moves.len() > 100 {
                    v.push(FieldViolation::new("moves", "size", "1-100 moves required"));
                }
                let ids: HashSet<_> = self.moves.iter().map(|m| m.org_unit_id).collect();
                if ids.len() != self.moves.len() {
                    v.push(FieldViolation::new(
                        "moves",
                        "duplicate",
                        "each unit may be moved once",
                    ));
                }
                if self.moves.iter().any(|m| m.org_unit_id == m.to_parent_id) {
                    v.push(FieldViolation::new(
                        "moves",
                        "cycle",
                        "a unit cannot become its own parent",
                    ));
                }
                if !self.merge_source_ids.is_empty() || self.merge_target_id.is_some() {
                    v.push(FieldViolation::new(
                        "mergeSourceIds",
                        "not-applicable",
                        "merge members are only used by kind `merge`",
                    ));
                }
            }
            RestructureKind::Merge => {
                if !self.moves.is_empty() {
                    v.push(FieldViolation::new(
                        "moves",
                        "not-applicable",
                        "moves are only used by kind `move`",
                    ));
                }
                let ids: HashSet<_> = self.merge_source_ids.iter().collect();
                if self.merge_source_ids.is_empty()
                    || self.merge_source_ids.len() > 20
                    || ids.len() != self.merge_source_ids.len()
                {
                    v.push(FieldViolation::new(
                        "mergeSourceIds",
                        "size",
                        "1-20 distinct source units required",
                    ));
                }
                match self.merge_target_id {
                    None => v.push(FieldViolation::new(
                        "mergeTargetId",
                        "required",
                        "mergeTargetId is required",
                    )),
                    Some(t) if ids.contains(&t) => v.push(FieldViolation::new(
                        "mergeTargetId",
                        "cycle",
                        "the target cannot also be a source",
                    )),
                    _ => {}
                }
            }
        }
        finish(v)
    }
}

/// One applied move (or merge source) of a restructure.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RestructureMove {
    pub org_unit_id: OrgUnitId,
    /// `move` or `merge_source`.
    pub role: String,
    pub from_parent_id: Option<OrgUnitId>,
    pub to_parent_id: Option<OrgUnitId>,
    /// Unit version the request was validated against (stale-baseline check).
    pub baseline_version: i64,
}

/// Structure transformation request.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OrgRestructure {
    pub id: OrgRestructureId,
    pub institution_id: InstitutionId,
    pub kind: RestructureKind,
    /// `pending_approval`, `applied`, `returned` or `rejected`.
    pub status: String,
    pub reason: String,
    pub effective_from: NaiveDate,
    pub merge_target_id: Option<OrgUnitId>,
    pub moves: Vec<RestructureMove>,
    pub process_instance_id: Option<String>,
    pub requested_by: Option<String>,
    pub decided_by: Option<String>,
    pub decided_at: Option<DateTime<Utc>>,
    pub decision_reason: Option<String>,
    pub version: Version,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn org_unit_transitions() {
        use OrgUnitStatus::*;
        let allowed = [
            (Draft, Active),
            (Active, Suspended),
            (Suspended, Active),
            (Active, Decommissioned),
            (Suspended, Decommissioned),
            (Decommissioned, Archived),
        ];
        for f in OrgUnitStatus::ALL {
            for t in OrgUnitStatus::ALL {
                assert_eq!(
                    can_transition(*f, *t),
                    allowed.contains(&(*f, *t)),
                    "{f}->{t}"
                );
            }
        }
        assert!(is_live(Suspended) && !is_live(Decommissioned));
        assert!(accepts_new_children(Draft) && !accepts_new_children(Suspended));
    }

    fn ut(key: &str, root: bool, parents: &[&str]) -> UnitType {
        UnitType {
            key: key.into(),
            label: key.into(),
            allow_root: root,
            allowed_parent_types: parents.iter().map(|s| (*s).into()).collect(),
        }
    }

    #[test]
    fn blueprint_validation() {
        let ok = DraftBlueprint {
            effective_from: NaiveDate::default(),
            unit_types: vec![
                ut("university", true, &[]),
                ut("faculty", false, &["university"]),
                ut("department", false, &["faculty", "department"]),
            ],
        };
        assert!(ok.validate().is_ok());
        let bad = DraftBlueprint {
            effective_from: NaiveDate::default(),
            unit_types: vec![ut("Faculty", false, &["ghost"]), ut("cell", false, &[])],
        };
        let Err(KernelError::Validation(v)) = bad.validate() else {
            panic!("violations expected")
        };
        let codes: Vec<_> = v.iter().map(|x| x.code.as_str()).collect();
        assert!(codes.contains(&"format"));
        assert!(codes.contains(&"unknown-type"));
        assert!(codes.contains(&"unplaceable"));
        assert!(codes.contains(&"no-root"));
    }

    #[test]
    fn detects_cycles() {
        let (a, b, c, d) = (
            Uuid::now_v7(),
            Uuid::now_v7(),
            Uuid::now_v7(),
            Uuid::now_v7(),
        );
        // a ← b ← c, a ← d
        let parent = HashMap::from([(a, None), (b, Some(a)), (c, Some(b)), (d, Some(a))]);
        assert_eq!(find_cycle(&parent, &[(c, d)]), None);
        assert_eq!(find_cycle(&parent, &[(b, c)]), Some(b));
        assert_eq!(find_cycle(&parent, &[(a, c)]), Some(a));
        // Two moves that together form a loop.
        assert!(find_cycle(&parent, &[(b, d), (d, c)]).is_some());
    }

    #[test]
    fn modification_refuses_structural_members() {
        let m = ModifyOrgUnit {
            parent_id: Some(serde_json::json!("x")),
            unit_type: Some(serde_json::json!("y")),
            ..Default::default()
        };
        let Err(KernelError::Validation(v)) = m.validate() else {
            panic!("violations expected")
        };
        assert_eq!(v.len(), 2);
        assert!(ModifyOrgUnit::default().validate().is_err());
        assert!(ModifyOrgUnit {
            name: Some("New".into()),
            ..Default::default()
        }
        .validate()
        .is_ok());
    }

    #[test]
    fn restructure_validation() {
        let u = OrgUnitId::new();
        let mv = RequestRestructure {
            kind: RestructureKind::Move,
            moves: vec![RequestedMove {
                org_unit_id: u,
                to_parent_id: u,
            }],
            merge_source_ids: vec![],
            merge_target_id: None,
            reason: "x".into(),
            effective_from: None,
        };
        assert!(mv.validate().is_err());
        let merge = RequestRestructure {
            kind: RestructureKind::Merge,
            moves: vec![],
            merge_source_ids: vec![u],
            merge_target_id: Some(u),
            reason: "x".into(),
            effective_from: None,
        };
        assert!(merge.validate().is_err());
        let gs = GovernanceSettings {
            head_title: "Head".into(),
            approval_levels: 6,
            quorum: Some(0),
            delegation_allowed: false,
            committees: vec![],
        };
        let mut v = vec![];
        gs.validate(&mut v);
        assert_eq!(v.len(), 2);
    }
}
