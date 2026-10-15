/** Slice-9E accreditation rules shared by the screen and the mock (mirror `RecordKind` in `sutra-services`). */
import type { AccreditationItem, RecordKind } from "./types";

/** Outcomes a record of `kind` may close with (mirrors `RecordKind::outcomes`). */
export const RECORD_OUTCOMES: Record<RecordKind, string[]> = {
  obligation: ["compliant", "non_compliant", "not_applicable"],
  evidence: ["approved_evidence", "insufficient"],
  internal_audit: ["closed_no_findings", "closed_with_findings"],
  external_assessment: ["accepted", "appealed"],
  finding: ["capa_verified", "no_action"],
  improvement: ["effective", "not_effective"],
  readiness: ["ready", "not_ready"],
  compliance_review: ["compliant", "partially_compliant", "non_compliant"],
  self_study: ["approved"],
};

/** Item kinds a record of `kind` accepts (mirrors `RecordKind::item_kinds`). */
export function recordItemKinds(kind: RecordKind): AccreditationItem["kind"][] {
  if (kind === "obligation") return ["requirement"];
  if (kind === "internal_audit") return ["checklist"];
  if (kind === "finding" || kind === "improvement") return ["action"];
  return [];
}

/** Kinds that belong to an accreditation cycle (mirrors `RecordKind::needs_cycle`). */
export function needsCycle(kind: RecordKind): boolean {
  return kind === "evidence" || kind === "internal_audit" || kind === "external_assessment" || kind === "readiness" || kind === "self_study";
}

export const RECORD_LABELS: Record<RecordKind, string> = {
  obligation: "Compliance obligation",
  evidence: "Evidence record",
  internal_audit: "Internal audit",
  external_assessment: "External assessment",
  finding: "Compliance finding",
  improvement: "Quality improvement",
  readiness: "Readiness decision",
  compliance_review: "Periodic compliance review",
  self_study: "Self-study report",
};
