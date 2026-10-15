/** Slice-9E governance rules shared by the screen and the mock (mirror `GovernanceKind` in `sutra-campus`). */
import type { GovernanceItem, GovernanceKind } from "./types";

/** Outcomes a case of `kind` may close with (mirrors `GovernanceKind::outcomes`). */
export const OUTCOMES: Record<GovernanceKind, string[]> = {
  readiness_assessment: ["ready", "conditionally_ready", "not_ready"],
  health_assessment: ["healthy", "at_risk", "critical"],
  maturity_assessment: ["level_1", "level_2", "level_3", "level_4", "level_5"],
  operational_review: ["closed_satisfactory", "closed_with_actions"],
  executive_review: ["recommendations_accepted", "recommendations_deferred"],
  risk: ["mitigated", "accepted", "transferred", "avoided"],
  action_plan: ["completed"],
  sla_deviation: ["resolved", "waived"],
  kpi_review: ["on_track", "off_track"],
  finding: ["corrective_action_approved", "no_action"],
  escalation: ["resolved", "referred_back"],
  exception: ["granted", "denied"],
};

/** Item kinds a case of `kind` accepts (mirrors `GovernanceKind::item_kinds`). */
export function itemKindsFor(kind: GovernanceKind): GovernanceItem["kind"][] {
  if (kind === "readiness_assessment" || kind === "health_assessment" || kind === "maturity_assessment") return ["criterion", "action"];
  if (kind === "executive_review") return ["recommendation", "action"];
  if (kind === "finding") return ["corrective_action"];
  return ["action"];
}
