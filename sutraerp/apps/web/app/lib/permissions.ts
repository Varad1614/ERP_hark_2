/**
 * Client-side copy of the few permission grants the UI uses to decide what to OFFER (navigation
 * entries, decision buttons). It is never the control: the API checks every request against
 * `crates/kernel/src/auth.rs::role_permissions` (deny by default), so a stale entry here can only
 * hide or show a button, never grant access.
 *
 * Part 2 · S4-ui-import: data import (`platform.import.manage`, administrators) and the second
 * approver of imports that need one (`platform.import.approve`, administrators and the finance
 * checker) — `part2_data_import_grants` in the kernel tests pins the same table.
 */
import type { ClientSession } from "./auth";
import type { ApiRole } from "./roles";

export type UiPermission = "platform.import.manage" | "platform.import.approve";

const GRANTS: Record<UiPermission, readonly ApiRole[]> = {
  "platform.import.manage": ["administrator"],
  "platform.import.approve": ["administrator", "finance_approver"],
};

/** True when one of `roles` holds `permission` (union of roles, deny by default). */
export function rolesGrant(roles: readonly ApiRole[] | undefined, permission: UiPermission): boolean {
  return (roles ?? []).some((r) => GRANTS[permission].includes(r));
}

/** True when the session's API roles hold `permission`. */
export function can(session: Pick<ClientSession, "apiRoles" | "role"> | null | undefined, permission: UiPermission): boolean {
  if (!session) return false;
  return rolesGrant(session.apiRoles ?? [session.role], permission);
}
