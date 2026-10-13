import { ArrowUpRight, ChartBar, Construction, LayoutDashboard, Table2 } from "lucide-react";
import { BentoGrid, BentoTile, Button, IconChip } from "@sutra/ui";
import { useNavigate, useParams } from "react-router";
import { useSession } from "../lib/auth";
import { ROLE_CONFIG, SCREEN_BATCH } from "../lib/roles";

export function meta() {
  return [{ title: "SutraERP" }];
}

const EXTRA: Record<string, string> = { profile: "My profile", notices: "Notices", todos: "To-dos" };

/** Route stub for every role navigation screen until its batch lands. */
export default function Screen() {
  const { screen = "" } = useParams();
  const { role } = useSession();
  const navigate = useNavigate();
  const entry = ROLE_CONFIG[role].nav.find((n) => n.id === screen);
  const label = entry?.label ?? EXTRA[screen];
  const Icon = entry?.icon ?? Construction;
  const plan = SCREEN_BATCH[screen];

  return (
    <BentoGrid data-screen={screen}>
      <BentoTile variant="accent" className="sb-soon" data-coming={plan ? String(plan.batch) : undefined}>
        <span className="sb-chip">
          <Icon strokeWidth={1.75} /> {label && plan ? `Coming in Batch ${plan.batch}` : label ?? "Not found"}
        </span>
        <h1>{label ?? "This screen isn’t available"}</h1>
        <p>
          {label && plan
            ? `${label} arrives in Batch ${plan.batch} with ${plan.area}. Until then, its workflows are listed in Workspaces.`
            : `There is no “${screen}” screen for your role.`}
        </p>
        <div className="sb-actions-row">
          <Button icon={<ArrowUpRight strokeWidth={1.75} />} onClick={() => navigate("/workspaces")}>
            Browse workspaces
          </Button>
          <Button variant="ghost" onClick={() => navigate("/")}>
            Back to home
          </Button>
        </div>
      </BentoTile>
      <div className="sb-side sb-span-4">
        {[LayoutDashboard, Table2, ChartBar].map((I, i) => (
          <BentoTile key={i} className="sb-notice">
            <IconChip icon={I} tone={(["orange", "blue", "violet"] as const)[i] ?? "grey"} size="md" />
            <div>
              <div className="sb-skeleton" style={{ width: `${70 - i * 12}%`, margin: 0 }} />
              <div className="sb-skeleton" style={{ width: `${45 + i * 8}%` }} />
            </div>
          </BentoTile>
        ))}
      </div>
    </BentoGrid>
  );
}
