/**
 * Re-skin v2 (R7) — Admin → Campus → Overview (`/campus`), the prototype Campus page
 * (`adm-campus.png`): Rooms today (occupancy bars per building), Transport (routes with today's
 * trip status), Hostel (beds per block) and Maintenance tickets (Mark resolved). Every figure comes
 * from the 8A / 8B APIs; nothing is invented (no on-time feed exists, so a route shows its trip /
 * route status instead).
 */
import { BedDouble, Bus, Check, DoorOpen, Wrench } from "lucide-react";
import { BentoTile, DataTable, EmptyState, IconChip, KeyValue, ListRow, ProgressBar, StatusBadge, formatDateTime, formatNumber, formatPercent, formatTime, plural, type BadgeKind } from "@sutra/ui";
import { useQueries, useQuery } from "@tanstack/react-query";
import { Link, useNavigate } from "react-router";
import { todayIso, useAsk } from "../institution/forms";
import { useCommand } from "../institution/hooks";
import { fkeys, useFacilitiesApi } from "./facilities/hooks";
import type { InfraNode, Reservation, Ticket } from "./facilities/types";
import { rkeys, useResidenceApi } from "./residence/hooks";
import type { HostelBed, TransportRoute, Trip } from "./residence/types";
import { CodeName, inr } from "./ui";

const BOOKED = ["requested", "confirmed", "rescheduled"];
const OPEN_TICKET = ["open", "triaged", "assigned", "in_progress", "on_hold", "reopened"];
const RESOLVABLE = ["assigned", "in_progress"];
const PRIORITY_KIND: Record<string, BadgeKind> = { critical: "attention", high: "attention", medium: "review", low: "neutral" };
const PRIORITY_RANK: Record<string, number> = { critical: 0, high: 1, medium: 2, low: 3 };

/** The building (or site) a node belongs to, by walking up the tree. */
function buildingOf(n: InfraNode, byId: Map<string, InfraNode>): InfraNode | undefined {
  let cur: InfraNode | undefined = n;
  for (let i = 0; cur && i < 10; i++) {
    if (cur.nodeType === "building") return cur;
    cur = cur.parentId ? byId.get(cur.parentId) : undefined;
  }
  return undefined;
}

/** Live campus figures shared by the Overview tiles and the header summary line. */
export function useCampusFigures(institutionId: string | null) {
  const fac = useFacilitiesApi();
  const res = useResidenceApi();
  const inst = institutionId ?? "";
  const enabled = !!institutionId;
  const tree = useQuery({ queryKey: fkeys(inst, "tree"), queryFn: () => fac.tree(inst), enabled }).data ?? [];
  const bookings = useQuery({ queryKey: fkeys(inst, "reservations"), queryFn: () => fac.reservations(inst), enabled }).data ?? [];
  const tickets = useQuery({ queryKey: fkeys(inst, "tickets", "overview-open"), queryFn: () => fac.tickets(inst, { open: true }), enabled }).data?.items ?? [];
  const routes = useQuery({ queryKey: rkeys(inst, "routes"), queryFn: () => res.routes(inst), enabled }).data ?? [];
  const beds = useQuery({ queryKey: rkeys(inst, "beds"), queryFn: () => res.beds(inst), enabled }).data ?? [];
  const trips = useQuery({ queryKey: rkeys(inst, "trips"), queryFn: () => res.trips(inst), enabled }).data ?? [];
  const today = todayIso();
  const byId = new Map(tree.map((n) => [n.id, n]));
  const rooms = tree.filter((n) => n.nodeType === "space" && n.status === "active" && n.spaceType !== "hostel_room");
  const todays = bookings.filter((b) => b.date === today && BOOKED.includes(b.status));
  const busy = new Set(todays.map((b) => b.nodeId));
  const inUse = rooms.filter((r) => busy.has(r.id)).length;
  const openTickets = tickets.filter((t) => OPEN_TICKET.includes(t.status));
  const running = routes.filter((r) => r.status === "active");
  return { tree, byId, rooms, todays, busy, inUse, openTickets, routes, running, beds, trips, today };
}

/** Header summary: "Thursday 15 Oct · 3 of 12 rooms in use · 1 route running · 2 maintenance tickets open". */
export function campusSummary(f: ReturnType<typeof useCampusFigures>): string {
  return [
    new Intl.DateTimeFormat("en-IN", { weekday: "long", day: "numeric", month: "long", timeZone: "UTC" }).format(new Date(`${f.today}T00:00:00Z`)),
    `${formatNumber(f.inUse)} of ${plural(f.rooms.length, "room")} in use`,
    `${plural(f.running.length, "route")} running`,
    `${plural(f.openTickets.length, "maintenance ticket")} open`,
  ].join(" · ");
}

function BarRow({ label, used, total }: { label: string; used: number; total: number }) {
  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 6 }} data-bar={label}>
      <KeyValue label={label} value={`${formatNumber(used)} / ${formatNumber(total)}`} />
      <ProgressBar value={total ? (used / total) * 100 : 0} label={label} />
    </div>
  );
}

/** Tile heading as in the prototype (title + muted meta, no extra link text); the title itself links to the section. */
function TileHead({ icon, tone, title, meta, to }: { icon?: typeof DoorOpen; tone?: "teal" | "violet"; title: string; meta: string; to?: string }) {
  return (
    <>
      {icon && <IconChip icon={icon} tone={tone} />}
      <h4 style={{ marginTop: icon ? 14 : 0 }}>
        {to ? (
          <Link to={to} data-tab-link={to.split("/").pop()} style={{ color: "inherit", textDecoration: "none" }}>
            {title}
          </Link>
        ) : (
          title
        )}
        <span className="sb-tile-actions" style={{ fontSize: 13, fontWeight: 500, color: "var(--sb-mute)" }}>
          {meta}
        </span>
      </h4>
    </>
  );
}

function RoomsToday({ f }: { f: ReturnType<typeof useCampusFigures> }) {
  const groups = new Map<string, { label: string; used: number; total: number }>();
  for (const r of f.rooms) {
    const b = buildingOf(r, f.byId);
    const key = b?.id ?? "other";
    const g = groups.get(key) ?? { label: b?.name ?? "Other spaces", used: 0, total: 0 };
    g.total += 1;
    if (f.busy.has(r.id)) g.used += 1;
    groups.set(key, g);
  }
  const rows = [...groups.values()].sort((a, b) => b.total - a.total).slice(0, 6);
  const next = [...f.todays].sort((a, b) => a.startTime.localeCompare(b.startTime))[0] as Reservation | undefined;
  return (
    <BentoTile span={4} data-overview="rooms">
      <TileHead icon={DoorOpen} tone="teal" title="Rooms today" meta={`${formatNumber(f.inUse)} / ${formatNumber(f.rooms.length)} in use`} />
      {rows.length ? (
        <div style={{ display: "flex", flexDirection: "column", gap: 16 }}>
          {rows.map((g) => (
            <BarRow key={g.label} label={g.label} used={g.used} total={g.total} />
          ))}
          <p style={{ fontSize: 13, color: "var(--sb-mute)", margin: 0 }}>
            {plural(f.todays.length, "booking")} today
            {next ? ` · next ${next.nodeName} at ${formatTime(next.startTime)}` : ""}
          </p>
        </div>
      ) : (
        <EmptyState icon={DoorOpen} title="No rooms registered" compact>
          Register buildings and rooms in Spaces → Rooms &amp; buildings.
        </EmptyState>
      )}
    </BentoTile>
  );
}

function Transport({ f, institutionId }: { f: ReturnType<typeof useCampusFigures>; institutionId: string }) {
  const res = useResidenceApi();
  const versions = useQueries({
    queries: f.routes.map((r) => ({ queryKey: rkeys(institutionId, `versions:${r.id}`), queryFn: () => res.versions(r.id), enabled: !!r.currentVersionId })),
  });
  const stopsOf = (r: TransportRoute, i: number) => versions[i]?.data?.find((v) => v.id === r.currentVersionId)?.stops.length;
  const tripOf = (r: TransportRoute): Trip | undefined => f.trips.find((t) => t.routeId === r.id && t.tripDate === f.today);
  const riders = f.routes.reduce((n, r) => n + r.passengers, 0);
  return (
    <BentoTile span={8} data-overview="transport">
      <TileHead to="/campus/transport" title="Transport" meta={`${plural(f.running.length, "route")} running · ${plural(riders, "rider")}`} />
      <DataTable<TransportRoute>
        caption="Transport routes"
        rows={f.routes}
        rowKey={(r) => r.id}
        empty={<EmptyState icon={Bus} title="No routes yet" compact>Plan routes and stops in Services → Transport.</EmptyState>}
        columns={[
          { key: "route", header: "Route", render: (r) => <CodeName name={r.name} code={r.code} /> },
          { key: "stops", header: "Stops", render: (r) => formatNumber(stopsOf(r, f.routes.indexOf(r)) ?? null), muted: true },
          { key: "riders", header: "Riders", render: (r) => (r.capacity ? `${formatNumber(r.passengers)} / ${formatNumber(r.capacity)}` : formatNumber(r.passengers)), muted: true },
          { key: "fare", header: "Fare", render: (r) => inr(r.fare), muted: true },
          {
            key: "status",
            header: "Status",
            render: (r) => {
              const t = tripOf(r);
              return t ? <StatusBadge status={t.status} /> : <StatusBadge status={r.status} />;
            },
          },
        ]}
      />
    </BentoTile>
  );
}

function Hostel({ f }: { f: ReturnType<typeof useCampusFigures> }) {
  const groups = new Map<string, { label: string; used: number; total: number }>();
  const occupied = (b: HostelBed) => b.status === "occupied" || !!b.allocationId;
  for (const bed of f.beds) {
    const room = f.byId.get(bed.roomNodeId);
    const b = room ? buildingOf(room, f.byId) : undefined;
    const key = b?.id ?? "rooms";
    const g = groups.get(key) ?? { label: b?.name ?? "Hostel rooms", used: 0, total: 0 };
    g.total += 1;
    if (occupied(bed)) g.used += 1;
    groups.set(key, g);
  }
  const used = f.beds.filter(occupied).length;
  const total = f.beds.length;
  return (
    <BentoTile span={4} data-overview="hostel">
      <TileHead to="/campus/hostel" icon={BedDouble} tone="violet" title="Hostel" meta={`${formatNumber(used)} / ${plural(total, "bed")}`} />
      {total ? (
        <div style={{ display: "flex", flexDirection: "column", gap: 16 }}>
          {[...groups.values()].map((g) => (
            <BarRow key={g.label} label={g.label} used={g.used} total={g.total} />
          ))}
          <p style={{ fontSize: 13, color: "var(--sb-mute)", margin: 0 }}>
            {plural(total - used, "bed")} free · {formatPercent((used / total) * 100, { digits: 0 })} occupied
          </p>
        </div>
      ) : (
        <EmptyState icon={BedDouble} title="No hostel beds yet" compact>
          Add rooms and beds in Spaces → Hostel.
        </EmptyState>
      )}
    </BentoTile>
  );
}

function Maintenance({ f }: { f: ReturnType<typeof useCampusFigures> }) {
  const api = useFacilitiesApi();
  const navigate = useNavigate();
  const { busy, submit, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const where = (t: Ticket) => (t.nodeId ? f.byId.get(t.nodeId)?.name : undefined);
  const rows = [...f.openTickets].sort((a, b) => (PRIORITY_RANK[a.priority] ?? 9) - (PRIORITY_RANK[b.priority] ?? 9) || b.createdAt.localeCompare(a.createdAt)).slice(0, 8);
  const resolve = (t: Ticket) =>
    ask({
      title: `Resolve “${t.title}”`,
      name: "ticket-resolve",
      fields: [{ name: "resolution", label: "Resolution", required: true, wide: true }],
      submitLabel: "Mark resolved",
      onSubmit: (v) => submit(() => api.resolveTicket(t.id, t.version, v.resolution!), `${t.number} resolved`),
    });
  return (
    <BentoTile span={8} data-overview="maintenance">
      <TileHead to="/campus/facilities" title="Maintenance tickets" meta={`${formatNumber(f.openTickets.length)} open`} />
      {rows.length ? (
        <div style={{ display: "flex", flexDirection: "column", gap: 4 }}>
          {rows.map((t) => (
            <ListRow
              key={t.id}
              icon={Wrench}
              tone="amber"
              title={t.title}
              meta={[t.number, where(t), formatDateTime(t.createdAt)].filter(Boolean).join(" · ")}
              badge={{ label: t.priority.charAt(0).toUpperCase() + t.priority.slice(1), kind: PRIORITY_KIND[t.priority] ?? "neutral" }}
              primary={
                RESOLVABLE.includes(t.status)
                  ? { label: "Mark resolved", icon: Check, ghost: true, disabled: busy, onClick: () => resolve(t), "data-action": "overview-resolve" }
                  : { label: "Open", ghost: true, onClick: () => navigate("/campus/facilities"), "data-action": "overview-ticket-open" }
              }
              data-ticket={t.number}
            />
          ))}
        </div>
      ) : (
        <EmptyState icon={Wrench} title="No open maintenance tickets" compact>
          New requests from staff and students land here.
        </EmptyState>
      )}
      {prompt}
      {dialog}
    </BentoTile>
  );
}

/** Admin → Campus → Overview. */
export function CampusOverview({ institutionId }: { institutionId: string }) {
  const f = useCampusFigures(institutionId);
  return (
    <>
      <RoomsToday f={f} />
      <Transport f={f} institutionId={institutionId} />
      <Hostel f={f} />
      <Maintenance f={f} />
    </>
  );
}
