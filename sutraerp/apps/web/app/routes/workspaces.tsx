import { SearchX } from "lucide-react";
import { Badge, BentoGrid, BentoTile, EmptyState, FilterChip, IconChip, KeyTag, SearchField } from "@sutra/ui";
import { useMemo, useState } from "react";
import { useNavigate } from "react-router";
import { useWorkspaces } from "../lib/api";
import { useSession } from "../lib/auth";
import { GROUPS, groupMeta } from "../lib/icons";
import { ROLE_CONFIG } from "../lib/roles";
import { workspaceBatch, workspaceScreen } from "../lib/workspace-screens";

export function meta() {
  return [{ title: "Workspaces · SutraERP" }];
}

export default function Workspaces() {
  const { role } = useSession();
  const navigate = useNavigate();
  const { data, isPending, isError } = useWorkspaces(role);
  const [group, setGroup] = useState("all");
  const [query, setQuery] = useState("");
  const [status, setStatus] = useState<"all" | "live" | "coming">("all");
  const list = useMemo(() => data?.items ?? [], [data]);
  const counts = useMemo(() => list.reduce<Record<string, number>>((a, w) => ((a[w.group] = (a[w.group] ?? 0) + 1), a), {}), [list]);
  const q = query.trim().toLowerCase();
  const liveCount = list.filter((w) => workspaceScreen(w.key, role)).length;
  const shown = list.filter(
    (w) =>
      (group === "all" || w.group === group) &&
      (status === "all" || (status === "live") === !!workspaceScreen(w.key, role)) &&
      (!q || `${w.name} ${w.key}`.toLowerCase().includes(q)),
  );

  return (
    <BentoGrid>
      <BentoTile className="sb-page-head">
        <div>
          <h1>Workspaces</h1>
          <p>
            {isPending ? "Loading…" : `${list.length} workspaces open to you as ${ROLE_CONFIG[role].title.toLowerCase()} · ${liveCount} live now, the rest arrive batch by batch.`}
          </p>
        </div>
        <div className="sb-tabs" role="group" aria-label="Availability" style={{ marginLeft: "auto" }}>
          {(["all", "live", "coming"] as const).map((v) => (
            <button key={v} type="button" className={status === v ? "on" : ""} aria-pressed={status === v} data-ws-status={v} onClick={() => setStatus(v)}>
              {v === "all" ? "All" : v === "live" ? `Live · ${liveCount}` : `Coming · ${list.length - liveCount}`}
            </button>
          ))}
        </div>
        <SearchField label="Search workspaces" placeholder="Search name or key" value={query} onChange={(e) => setQuery(e.target.value)} />
      </BentoTile>
      <div className="sb-filters">
        <FilterChip active={group === "all"} label="All" count={list.length} onClick={() => setGroup("all")} />
        {Object.keys(GROUPS)
          .filter((g) => counts[g])
          .map((g) => {
            const meta = groupMeta(g);
            return <FilterChip key={g} active={group === g} label={meta.label} count={counts[g]} icon={<meta.icon strokeWidth={1.75} />} onClick={() => setGroup(g)} />;
          })}
      </div>
      {isError && <EmptyState title="Workspaces could not be loaded">Check that the API is running.</EmptyState>}
      {!isPending && !isError && shown.length === 0 && (
        <EmptyState icon={SearchX} title="No workspaces match">
          Try another group or search.
        </EmptyState>
      )}
      {shown.map((w) => {
        const g = groupMeta(w.group);
        const screen = workspaceScreen(w.key, role);
        return (
          <BentoTile
            key={w.key}
            as="button"
            type="button"
            interactive
            className={screen ? "sb-ws-card is-live" : "sb-ws-card is-coming"}
            data-ws={w.key}
            data-ws-live={screen ? "true" : "false"}
            onClick={() => navigate(screen ?? `/workspaces/${w.key}`)}
          >
            <div className="sb-ws-top">
              <IconChip icon={g.icon} tone={g.tone} />
              {screen ? <Badge kind="done">Live</Badge> : <Badge kind="neutral">Batch {workspaceBatch(w.key)}</Badge>}
            </div>
            <h3>{w.name}</h3>
            <div className="sb-meta">
              <KeyTag>{w.key}</KeyTag>
              <span>
                {w.workflows.length} workflow{w.workflows.length === 1 ? "" : "s"}
              </span>
            </div>
          </BentoTile>
        );
      })}
    </BentoGrid>
  );
}
