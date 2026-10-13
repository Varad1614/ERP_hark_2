import type { ReactNode } from "react";
import { BentoGrid, BentoTile, PageHeader } from "@sutra/ui";
import { useSession } from "../../lib/auth";
import type { Role } from "../../lib/roles";

/**
 * Frame of a role-facing Batch 4 screen (foundation-owned): renders `children` for the allowed
 * roles and a polite "not available" tile for the others. The API enforces the same rule.
 */
export function RoleScreen({ screen, roles, title, note, heading, children }: { screen: string; roles: readonly Role[]; title: string; note: string; heading?: (role: Role) => string; children: (role: Role) => ReactNode }) {
  const { role } = useSession();
  if (!roles.includes(role))
    return (
      <BentoGrid data-screen={screen}>
        <BentoTile span={12} variant="accent" className="sb-soon">
          <h1>This screen isn’t available</h1>
          <p>
            {title} {note}
          </p>
        </BentoTile>
      </BentoGrid>
    );
  return (
    <BentoGrid data-screen={screen}>
      {heading && <PageHeader title={title} subtitle={heading(role)} data-page-head={screen} />}
      {children(role)}
    </BentoGrid>
  );
}
