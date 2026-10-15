/**
 * Slice-9E campus governance (G00) wire types — straight from the generated OpenAPI client
 * (tag `campus-governance`, operation ids `campus_governance_*`).
 */
import type { components } from "@sutra/api-client";

type S = components["schemas"];

export interface Items<T> {
  items: T[];
}

export type GovernanceCase = S["GovernanceCaseView"];
export type NewGovernanceCase = S["GovernanceCaseCreate"];
export type GovernanceItem = S["GovernanceItemView"];
export type NewGovernanceItem = S["GovernanceItemCreate"];
export type GovernanceItemProgress = S["GovernanceItemProgress"];
export type GovernanceKind = S["GovernanceKind"];
export type GovernanceStatus = S["GovernanceStatus"];
export type GovernanceConfigVersion = S["GovernanceConfigView"];
export type GovernanceConfigKey = S["ConfigKey"];
export type GovernanceEvidence = S["GovernanceEvidenceView"];
export type GovernancePolicyView = S["GovernancePolicyView"];

export type Decision = "approve" | "reject";
export type CaseCommand = "start" | "refresh" | "cancel";
