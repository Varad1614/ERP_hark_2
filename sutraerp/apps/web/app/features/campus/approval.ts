/**
 * Slice 9E — the one reader of the canonical approval state (Batch 9 contract §6.4).
 *
 * Every Batch 8 / 9 aggregate view with an approval step carries `approvalState` ∈
 * `not_requested | pending | approved | rejected`. 8B / 8D views still send the deprecated
 * `approvalPending` boolean; screens read `approvalState` through these helpers only.
 */
export type ApprovalState = "not_requested" | "pending" | "approved" | "rejected";

export const APPROVAL_STATES: readonly ApprovalState[] = ["not_requested", "pending", "approved", "rejected"];

const LABELS: Record<ApprovalState, string> = {
  not_requested: "Not submitted",
  pending: "Awaiting approval",
  approved: "Approved",
  rejected: "Rejected",
};

/** `true` for a canonical approval state. */
export function isApprovalState(s: unknown): s is ApprovalState {
  return typeof s === "string" && (APPROVAL_STATES as readonly string[]).includes(s);
}

/**
 * Human label of an approval state. Accepts the legacy 8C wire value `not_submitted` (pre-§6.4
 * clients and mocks) as `not_requested`; anything unknown reads "—".
 */
export function approvalStateLabel(s: string | null | undefined): string {
  if (s === "not_submitted" || s === "none") return LABELS.not_requested;
  return isApprovalState(s) ? LABELS[s] : "—";
}

/** Badge status for `StatusBadge` (`pending` → review, `rejected` → attention …). */
export function approvalBadge(s: string | null | undefined): string {
  if (s === "pending") return "under_review";
  if (s === "approved") return "approved";
  if (s === "rejected") return "rejected";
  return "draft";
}

/** Derives the canonical state from a legacy `approvalPending` flag (8B / 8D deprecated field). */
export function approvalFrom(view: { approvalState?: string | null; approvalPending?: boolean | null }): ApprovalState {
  if (isApprovalState(view.approvalState)) return view.approvalState;
  if (view.approvalState === "not_submitted" || view.approvalState === "none") return "not_requested";
  return view.approvalPending ? "pending" : "not_requested";
}
