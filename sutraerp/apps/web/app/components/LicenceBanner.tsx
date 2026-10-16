/**
 * Part 2 · S7-entitle: licence banner of the app shell (grace period, expired = read-only,
 * suspended, renewal due within 30 days), composed from @sutra/ui parts (IconChip, Badge). The
 * model comes from `licenceBanner()` in `lib/entitlements.ts`.
 */
import { Badge, IconChip } from "@sutra/ui";
import { CalendarClock, Lock, ShieldAlert } from "lucide-react";
import type { LicenceBannerModel } from "../lib/entitlements";

const ICONS = { grace: CalendarClock, renewal_due: CalendarClock, expired: Lock, suspended: ShieldAlert, active: CalendarClock } as const;
const BADGES = { grace: "Grace period", renewal_due: "Renewal", expired: "Read-only", suspended: "Suspended", active: "Active" } as const;

export function LicenceBanner({ model }: { model: LicenceBannerModel }) {
  return (
    <div className={`lic-banner lic-banner--${model.tone}`} role={model.status === "renewal_due" ? "status" : "alert"} data-licence-banner={model.status}>
      <IconChip icon={ICONS[model.status]} tone={model.tone} size="sm" />
      <div className="lic-banner-copy">
        <b>{model.title}</b>
        <small>{model.detail}</small>
      </div>
      <Badge kind={model.badge}>{BADGES[model.status]}</Badge>
    </div>
  );
}
