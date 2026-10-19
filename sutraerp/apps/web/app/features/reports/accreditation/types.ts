/**
 * Slice-9E accreditation (H09) wire types — straight from the generated OpenAPI client
 * (tag `services-accreditation`, operation ids `services_accreditation_*`).
 */
import type { components } from "@sutra/api-client";

type S = components["schemas"];

export interface Items<T> {
  items: T[];
}

export type AccreditationFramework = S["AccreditationFrameworkView"];
export type NewAccreditationFramework = S["AccreditationFrameworkCreate"];
export type Criterion = S["AccreditationCriterion"];
export type AccreditationCycle = S["AccreditationCycleView"];
export type NewAccreditationCycle = S["AccreditationCycleCreate"];
export type PlanEntry = S["PlanEntry"];
export type AccreditationRecord = S["AccreditationRecordView"];
export type NewAccreditationRecord = S["AccreditationRecordCreate"];
export type AccreditationItem = S["AccreditationItemView"];
export type NewAccreditationItem = S["AccreditationItemCreate"];
export type AccreditationItemProgress = S["AccreditationItemProgress"];
export type RecordKind = S["RecordKind"];

export type Decision = "approve" | "reject";
export type RecordCommand = "start" | "cancel";
