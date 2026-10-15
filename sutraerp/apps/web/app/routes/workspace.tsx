import { ArrowLeft, ArrowRight, ChevronRight, CircleCheck, Inbox, Layers, Workflow } from "lucide-react";
import { Badge, BentoGrid, BentoTile, Button, DataTable, Drawer, EmptyState, IconChip, KeyTag, StatTile, Steps, Tabs, useToast, type NavLinkLikeProps } from "@sutra/ui";
import { useState } from "react";
import { Link, NavLink, useParams } from "react-router";
import { useWorkspaces } from "../lib/api";
import { useSession } from "../lib/auth";
import { groupMeta } from "../lib/icons";
import { rememberWorkspace } from "../features/home/widgets";
import { ROLE_CONFIG } from "../lib/roles";

const STAGES = ["Draft", "Submitted", "In review", "Approved"];
const TABS = ["overview", "workflows", "records"] as const;
type Tab = (typeof TABS)[number];

export function meta() {
  return [{ title: "Workspace · SutraERP" }];
}

function RouterLink({ to, className, end, children }: NavLinkLikeProps) {
  return (
    <NavLink to={to} end={end} className={className}>
      {children}
    </NavLink>
  );
}

export default function WorkspaceRoute() {
  const { key = "", tab: rawTab } = useParams();
  const { role } = useSession();
  const toast = useToast();
  const { data, isPending } = useWorkspaces(role);
  const [stage, setStage] = useState<Record<number, number>>({});
  const [openWf, setOpenWf] = useState<number | null>(null);
  const tab: Tab = (TABS as readonly string[]).includes(rawTab ?? "") ? (rawTab as Tab) : "overview";

  if (isPending) return null;
  const ws = data?.items.find((w) => w.key === key);
  if (!ws) {
    return (
      <BentoGrid>
        <BentoTile variant="accent" className="sb-soon">
          <span className="sb-chip">Workspaces</span>
          <h1>Workspace not available</h1>
          <p>
            <b>{key}</b> does not exist or is not open to you as {ROLE_CONFIG[role].title.toLowerCase()}.
          </p>
          <div className="sb-actions-row">
            <Link className="sb-btn" to="/workspaces">
              Browse workspaces
            </Link>
          </div>
        </BentoTile>
      </BentoGrid>
    );
  }

  const g = groupMeta(ws.group);
  rememberWorkspace(ws.key);
  const scenarios = ws.workflows.reduce((n, f) => n + f.scenarios, 0);
  const wf = openWf === null ? undefined : ws.workflows[openWf];
  const s = openWf === null ? 0 : (stage[openWf] ?? 0);
  const badgeFor = (i: number) => {
    const v = stage[i] ?? 0;
    return <Badge kind={v === 3 ? "done" : v === 0 ? "neutral" : "review"}>{STAGES[v]}</Badge>;
  };
  const row = (i: number, withBadge: boolean) => {
    const f = ws.workflows[i];
    if (!f) return null;
    return (
      <button key={f.label} type="button" className="sb-row" onClick={() => setOpenWf(i)}>
        <IconChip icon={Workflow} tone={g.tone} size="sm" />
        <div>
          <b>{f.label}</b>
          <small>
            {f.scenarios} scenario{f.scenarios === 1 ? "" : "s"}
          </small>
        </div>
        {withBadge && badgeFor(i)}
        <ChevronRight strokeWidth={1.75} />
      </button>
    );
  };

  return (
    <BentoGrid data-workspace={ws.key}>
      <BentoTile className="sb-page-head">
        <div style={{ flex: 1, minWidth: 260 }}>
          <Link className="sb-back" to="/workspaces">
            <ArrowLeft strokeWidth={1.75} /> Workspaces
          </Link>
          <div style={{ display: "flex", gap: 16, alignItems: "center" }}>
            <IconChip icon={g.icon} tone={g.tone} size="lg" />
            <div>
              <h1>{ws.name}</h1>
              <p>
                <KeyTag>{ws.key}</KeyTag> · {g.label} · {ws.workflows.length} workflows for you
              </p>
            </div>
          </div>
        </div>
        <Tabs
          label="Workspace sections"
          linkComponent={RouterLink}
          items={[
            { id: "overview", label: "Overview", to: `/workspaces/${ws.key}` },
            { id: "workflows", label: `Workflows · ${ws.workflows.length}`, to: `/workspaces/${ws.key}/workflows` },
            { id: "records", label: "Records", to: `/workspaces/${ws.key}/records` },
          ]}
        />
      </BentoTile>

      {tab === "overview" && (
        <>
          <StatTile span={4} icon={Workflow} tone={g.tone} label="Workflows" value={String(ws.workflows.length)} hint="Available to your role" />
          <StatTile span={4} icon={Layers} tone="amber" label="Scenarios" value={String(scenarios)} hint="Specified behaviours" />
          <StatTile span={4} icon={CircleCheck} tone="green" label="Roles" value={String(ws.roles.length)} hint={ws.roles.map((r) => ROLE_CONFIG[r].title).join(", ")} />
          <BentoTile span={12} title="Workflows you can start" action={<Link to={`/workspaces/${ws.key}/workflows`}>See all</Link>}>
            {ws.workflows.length ? ws.workflows.slice(0, 3).map((_, i) => row(i, false)) : <p>No workflows for your role.</p>}
          </BentoTile>
        </>
      )}

      {tab === "workflows" && (
        <BentoTile span={12}>{ws.workflows.length ? ws.workflows.map((_, i) => row(i, true)) : <EmptyState title="No workflows for your role in this workspace" />}</BentoTile>
      )}

      {tab === "records" && (
        <BentoTile span={12} title="Recent records">
          <DataTable
            columns={[]}
            rows={[]}
            rowKey={() => ""}
            empty={
              <EmptyState icon={Inbox} title="Records arrive with this workspace’s module">
                {ws.name} records are served by the owning domain module once its batch lands. The route, permissions and layout are in place.
              </EmptyState>
            }
          />
        </BentoTile>
      )}

      <Drawer
        open={wf !== undefined}
        onClose={() => setOpenWf(null)}
        title={wf?.label ?? ""}
        eyebrow={`${ws.key} · ${ws.name}`}
        media={<IconChip icon={Workflow} tone={g.tone} />}
        footer={
          <>
            <Button
              disabled={s === 3}
              icon={<ArrowRight strokeWidth={1.75} />}
              onClick={() => {
                if (openWf === null || !wf) return;
                const next = Math.min(3, s + 1);
                setStage((st) => ({ ...st, [openWf]: next }));
                toast(`${wf.label}: moved to ${STAGES[next]}`);
              }}
            >
              {s === 3 ? "Approved" : `Advance to ${STAGES[s + 1]}`}
            </Button>
            <Button variant="ghost" onClick={() => setOpenWf(null)}>
              Close
            </Button>
          </>
        }
      >
        {wf && (
          <>
            <dl className="sb-dl">
              <dt>Scenarios</dt>
              <dd>{wf.scenarios}</dd>
              <dt>Roles</dt>
              <dd>{wf.roles.map((r) => ROLE_CONFIG[r].title).join(", ")}</dd>
              <dt>Stage</dt>
              <dd>{badgeFor(openWf ?? 0)}</dd>
            </dl>
            <Steps stages={STAGES} current={s} />
            <p style={{ fontSize: 13, color: "var(--sb-mute)" }}>Stage changes are local in Batch 0; the workflow port (Flowable) drives them later.</p>
          </>
        )}
      </Drawer>
    </BentoGrid>
  );
}
