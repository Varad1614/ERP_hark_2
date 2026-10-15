/**
 * Slice **8A** web components (campus infrastructure, room booking, service desk, maintenance,
 * laboratories). The export names are imported by the foundation frame
 * `routes/admin/campus-setup.tsx` (docs/batch8-contracts.md §10); every screen carries
 * `data-slice="8A"`. Only @sutra/ui components — no custom styling. Area stays the decimal string
 * the API sends.
 *
 * - `InfrastructureAdminTab` — Admin → Campus → Infrastructure & rooms: the hierarchy (register,
 *   activate / suspend / restore / decommission / archive, capacity), the node drawer with the
 *   space's free / busy week, and room bookings (book, confirm, cancel).
 * - `FacilitiesAdminTab` — Admin → Campus → Service desk & maintenance: the ticket queue with SLA
 *   badges (triage, assign, start, hold, resolve, close, cancel, escalate, SLA check), work
 *   orders (assign, start, complete, verify) and preventive plans (submit, generate, suspend).
 * - `LabsAdminTab` — Admin → Campus → Laboratories: lab profiles, readiness checklist,
 *   equipment status and practical sessions.
 */
import { AlertTriangle, Building2, CalendarCheck, ClipboardList, DoorOpen, FlaskConical, Layers, LayoutGrid, MapPin, Timer, Wrench, type LucideIcon } from "lucide-react";
import { Badge, BentoTile, Button, DataTable, Drawer, EmptyState, FilterChip, formatNumber, IconChip, KpiTile } from "@sutra/ui";
import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { fmtDate, REASON, todayIso, useAsk, type FieldSpec, type RowAction } from "../../institution/forms";
import { ApprovalBadge, Actions, CodeName, nameCode } from "../ui";
import { StatusBadge, useCommand, useInstitutionApi } from "../../institution/hooks";
import type { Institution } from "../../institution/types";
import { fkeys, useFacilitiesApi } from "./hooks";
import type { Equipment, InfraNode, Lab, Plan, Reservation, Session, Ticket, WorkOrder } from "./types";

export { FacilitiesApiProvider } from "./hooks";

/** Icon chip of an infrastructure node by type (the tree is drawn with indentation + chips, not dots). */
const NODE_ICON: Record<string, LucideIcon> = { site: MapPin, building: Building2, floor: Layers, space: DoorOpen, zone: LayoutGrid };

const SPACE_TYPES = ["classroom", "lab", "seminar_room", "auditorium", "office", "hostel_room", "library", "reading_room", "store", "workshop", "sports", "other"];
const NODE_TYPES = ["site", "building", "floor", "space", "zone"];
const CATEGORIES = ["facility", "maintenance", "it", "laboratory", "hostel", "transport", "library", "other"];
const PRIORITIES = ["low", "medium", "high", "critical"];
const FREQUENCIES = ["weekly", "monthly", "quarterly", "half_yearly", "yearly"];
const EQUIPMENT_STATUSES = ["operational", "restricted", "under_maintenance", "out_of_service"];
const READINESS_ITEMS = ["Safety equipment checked", "Gas and water valves closed", "First-aid kit present", "Waste disposal ready"];
const label = (s: string) => s.replace(/_/g, " ").replace(/^./, (c) => c.toUpperCase());
const options = (values: string[]) => values.map((v) => ({ value: v, label: label(v) }));
const OPEN_BOOKING = ["requested", "confirmed", "rescheduled"];
const isoPlus = (days: number) => {
  const d = new Date(`${todayIso()}T00:00:00Z`);
  d.setUTCDate(d.getUTCDate() + days);
  return d.toISOString().slice(0, 10);
};

function NoInstitutionTile({ title }: { title: string }) {
  return (
    <BentoTile span={12} title={title} data-slice="8A">
      <EmptyState icon={Building2} title="No institution selected" compact>
        Register an institution in Campus setup → Institution first.
      </EmptyState>
    </BentoTile>
  );
}

function useTree(institutionId: string) {
  const api = useFacilitiesApi();
  return useQuery({ queryKey: fkeys(institutionId, "tree"), queryFn: () => api.tree(institutionId) }).data ?? [];
}

const spaceOptions = (nodes: InfraNode[], filter: (n: InfraNode) => boolean = () => true) =>
  nodes.filter((n) => n.nodeType === "space" && filter(n)).map((n) => ({ value: n.id, label: nameCode(n.name, n.code) }));

/** Depth of a node in the hierarchy (for the indented code column). */
function depthOf(n: InfraNode, byId: Map<string, InfraNode>): number {
  let d = 0;
  let cur = n.parentId ? byId.get(n.parentId) : undefined;
  while (cur && d < 8) {
    d += 1;
    cur = cur.parentId ? byId.get(cur.parentId) : undefined;
  }
  return d;
}

/** Nodes in tree order: each node followed by its children (by code), so the indentation reads as the hierarchy. */
export function treeOrder(nodes: InfraNode[]): InfraNode[] {
  const ids = new Set(nodes.map((n) => n.id));
  const kids = new Map<string, InfraNode[]>();
  for (const n of nodes) {
    const key = n.parentId && ids.has(n.parentId) ? n.parentId : "";
    kids.set(key, [...(kids.get(key) ?? []), n]);
  }
  const out: InfraNode[] = [];
  const walk = (key: string, depth: number) => {
    for (const n of [...(kids.get(key) ?? [])].sort((a, b) => a.code.localeCompare(b.code))) {
      out.push(n);
      if (depth < 8) walk(n.id, depth + 1);
    }
  };
  walk("", 0);
  return out.length === nodes.length ? out : nodes;
}

// ================================================================== Infrastructure & rooms

function NodeDrawer({ nodeId, onClose }: { nodeId: string; onClose: () => void }) {
  const api = useFacilitiesApi();
  const detail = useQuery({ queryKey: fkeys(nodeId, "node"), queryFn: () => api.node(nodeId) });
  const isSpace = detail.data?.node.nodeType === "space";
  const week = useQuery({
    queryKey: fkeys(nodeId, "calendar", todayIso()),
    queryFn: () => api.calendar(nodeId, todayIso(), isoPlus(6)),
    enabled: isSpace,
  });
  const d = detail.data;
  return (
    <Drawer open onClose={onClose} title={d ? d.node.name : "Infrastructure node"} eyebrow={d ? <>{d.node.code} · <StatusBadge status={d.node.status} /></> : undefined}>
      {d && (
        <>
          <p>
            {label(d.node.nodeType)}
            {d.node.spaceType ? ` · ${label(d.node.spaceType)}` : ""}
            {d.node.capacity != null ? ` · ${d.node.capacity} seats` : ""}
            {d.node.areaSqM ? ` · ${d.node.areaSqM} m²` : ""}
          </p>
          <p>
            {d.children.length} child node(s) · {d.futureReservations} upcoming booking(s)
            {d.decommission ? ` · decommissioning ${d.decommission.status}` : ""}
          </p>
          {d.capabilities.length > 0 && <p>Capabilities: {d.capabilities.map((c) => c.label).join(", ")}</p>}
          {d.availability?.configured && (
            <p>
              Open:{" "}
              {d.availability.weeklyHours.length
                ? d.availability.weeklyHours.map((w) => `day ${w.weekday} ${w.opensAt}–${w.closesAt}`).join(", ")
                : "all day"}
              {d.availability.bookable ? "" : " · not bookable"}
            </p>
          )}
          {isSpace && (
            <DataTable
              caption="Free / busy this week"
              rows={week.data?.days ?? []}
              rowKey={(x) => x.date}
              empty={<EmptyState icon={CalendarCheck} title="No calendar" compact />}
              columns={[
                { key: "date", header: "Day", render: (x) => fmtDate(x.date) },
                { key: "busy", header: "Busy", render: (x) => (x.busy.length ? x.busy.map((b) => `${b.startTime}–${b.endTime} ${b.number}`).join(", ") : "—") },
                { key: "free", header: "Free", render: (x) => (x.free.length ? x.free.map((f) => `${f.startTime}–${f.endTime}`).join(", ") : "Closed"), muted: true },
              ]}
            />
          )}
        </>
      )}
    </Drawer>
  );
}

function InfrastructureScreen({ institutionId }: { institutionId: string }) {
  const api = useFacilitiesApi();
  const institutions = useInstitutionApi();
  const { run, submit, busy, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const [open, setOpen] = useState<string | null>(null);
  const nodes = treeOrder(useTree(institutionId));
  const campuses = useQuery({ queryKey: fkeys(institutionId, "campuses"), queryFn: () => institutions.listCampuses(institutionId) }).data ?? [];
  const bookings = useQuery({ queryKey: fkeys(institutionId, "reservations"), queryFn: () => api.reservations(institutionId) }).data ?? [];
  const byId = new Map(nodes.map((n) => [n.id, n]));
  const spaces = nodes.filter((n) => n.nodeType === "space");
  const upcoming = bookings.filter((r) => OPEN_BOOKING.includes(r.status) && r.date >= todayIso());

  const book = (spaceId?: string) =>
    ask({
      title: "Book a space",
      name: "reservation",
      fields: [
        { name: "nodeId", label: "Space", type: "select", required: true, options: spaceOptions(nodes, (n) => n.status === "active"), initial: spaceId },
        { name: "date", label: "Date", type: "date", required: true, initial: isoPlus(1) },
        { name: "startTime", label: "From", required: true, placeholder: "09:00", initial: "09:00" },
        { name: "endTime", label: "To", required: true, placeholder: "11:00", initial: "11:00" },
        { name: "attendees", label: "Attendees", type: "number", required: true, initial: "20" },
        { name: "purpose", label: "Purpose", required: true, wide: true },
      ],
      submitLabel: "Book",
      onSubmit: (v) =>
        submit(
          () => api.book(institutionId, { nodeId: v.nodeId!, date: v.date!, startTime: v.startTime!, endTime: v.endTime!, attendees: Number(v.attendees), purpose: v.purpose }),
          (r) => `${r.number} ${r.status}`,
        ),
    });

  const nodeActions = (n: InfraNode): RowAction[] => {
    const a: RowAction[] = [];
    const reasonCmd = (cmd: "suspend" | "decommission", title: string) => () =>
      ask({ title: `${title} ${n.code}`, name: `node-${cmd}`, fields: [REASON], submitLabel: title, onSubmit: (v) => submit(() => api.nodeCommand(n.id, n.version, cmd, v.reason), `${n.code}: ${cmd} requested`) });
    if (n.status === "planned") a.push({ id: "node-activate", label: "Activate", variant: "primary", onClick: () => void run(() => api.nodeCommand(n.id, n.version, "activate"), `${n.code} active`) });
    if (n.status === "active") {
      if (n.nodeType === "space") a.push({ id: "node-book", label: "Book", onClick: () => book(n.id) });
      a.push({ id: "node-suspend", label: "Suspend", onClick: reasonCmd("suspend", "Suspend") });
    }
    if (n.status === "suspended") a.push({ id: "node-restore", label: "Restore", variant: "primary", onClick: () => void run(() => api.nodeCommand(n.id, n.version, "restore"), `${n.code} restored`) });
    if (n.status === "active" || n.status === "suspended") {
      if (n.nodeType === "space")
        a.push({
          id: "node-capacity",
          label: "Capacity",
          onClick: () =>
            ask({
              title: `Capacity of ${n.code}`,
              name: "node-capacity",
              fields: [{ name: "capacity", label: "Seats / beds", type: "number", required: true, initial: String(n.capacity ?? 0) }, { name: "reason", label: "Reason", wide: true }],
              submitLabel: "Save",
              onSubmit: (v) => submit(() => api.setCapacity(n.id, n.version, Number(v.capacity), v.reason), "Capacity changed"),
            }),
        });
      a.push({ id: "node-decommission", label: "Decommission", onClick: reasonCmd("decommission", "Decommission") });
    }
    if (n.status === "planned" || n.status === "decommissioned") a.push({ id: "node-archive", label: "Archive", onClick: () => void run(() => api.nodeCommand(n.id, n.version, "archive"), `${n.code} archived`) });
    return a;
  };

  const register = () =>
    ask({
      title: "Register infrastructure",
      name: "node",
      fields: [
        { name: "nodeType", label: "Level", type: "select", required: true, options: options(NODE_TYPES), initial: nodes.length ? "space" : "site" },
        { name: "parentId", label: "Under", type: "select", options: [{ value: "", label: "— (a site)" }, ...nodes.filter((n) => n.nodeType !== "space").map((n) => ({ value: n.id, label: nameCode(n.name, n.code) }))] },
        { name: "campusId", label: "Campus (sites)", type: "select", options: [{ value: "", label: "Parent's campus" }, ...campuses.map((c) => ({ value: c.id, label: c.name }))] },
        { name: "code", label: "Code", required: true, placeholder: "MAIN-B1-101" },
        { name: "name", label: "Name", required: true },
        { name: "spaceType", label: "Space type (spaces)", type: "select", options: [{ value: "", label: "—" }, ...options(SPACE_TYPES)] },
        { name: "capacity", label: "Capacity", type: "number" },
        { name: "status", label: "Start as", type: "select", options: options(["planned", "active"]), initial: "planned" },
      ],
      submitLabel: "Register",
      onSubmit: (v) =>
        submit(
          () =>
            api.createNode(institutionId, {
              nodeType: v.nodeType as InfraNode["nodeType"],
              parentId: v.parentId || undefined,
              campusId: v.campusId || undefined,
              code: v.code!.trim().toUpperCase(),
              name: v.name!,
              spaceType: (v.spaceType || undefined) as InfraNode["spaceType"],
              capacity: v.capacity ? Number(v.capacity) : undefined,
              status: v.status as InfraNode["status"],
            }),
          (d) => `${d.node.code} registered`,
        ),
    });

  const bookingActions = (r: Reservation): RowAction[] => {
    const a: RowAction[] = [];
    if (r.status === "requested") a.push({ id: "booking-confirm", label: "Confirm", variant: "primary", onClick: () => void run(() => api.reservationCommand(r.id, r.version, "confirm"), `${r.number} confirmed`) });
    if (OPEN_BOOKING.includes(r.status))
      a.push({
        id: "booking-cancel",
        label: "Cancel",
        onClick: () => ask({ title: `Cancel ${r.number}`, name: "booking-cancel", fields: [REASON], submitLabel: "Cancel booking", onSubmit: (v) => submit(() => api.reservationCommand(r.id, r.version, "cancel", v.reason), `${r.number} cancelled`) }),
      });
    return a;
  };

  return (
    <>
      <KpiTile span={3} icon={Building2} tone="blue" label="Buildings" value={String(nodes.filter((n) => n.nodeType === "building").length)} data-slice="8A" />
      <KpiTile span={3} icon={DoorOpen} tone="green" label="Active spaces" value={String(spaces.filter((n) => n.status === "active").length)} />
      <KpiTile span={3} icon={ClipboardList} tone="violet" label="Seats" value={String(spaces.filter((n) => n.status === "active").reduce((s, n) => s + (n.capacity ?? 0), 0))} />
      <KpiTile span={3} icon={CalendarCheck} tone="orange" label="Upcoming bookings" value={String(upcoming.length)} />
      <BentoTile
        span={12}
        title="Campus infrastructure"
        data-slice="8A"
        action={
          <Button size="sm" data-action="node-new" onClick={register}>
            Register
          </Button>
        }
      >
        <DataTable<InfraNode>
          caption="Infrastructure nodes"
          rows={nodes}
          rowKey={(n) => n.id}
          empty={<EmptyState icon={Building2} title="No infrastructure yet" compact>Register a site, then its buildings, floors and spaces.</EmptyState>}
          columns={[
            {
              key: "name",
              header: "Building / space",
              render: (n) => (
                <span style={{ display: "inline-flex", alignItems: "center", gap: 10, paddingLeft: depthOf(n, byId) * 24 }} data-depth={depthOf(n, byId)}>
                  <IconChip icon={NODE_ICON[n.nodeType] ?? DoorOpen} tone={n.nodeType === "space" ? "teal" : n.nodeType === "building" ? "orange" : "grey"} size="sm" />
                  <button type="button" className="sb-link-btn" onClick={() => setOpen(n.id)} data-action="node-open" style={{ background: "none", border: 0, padding: 0, cursor: "pointer", textAlign: "left" }}>
                    <CodeName name={n.name} code={n.code} />
                  </button>
                </span>
              ),
            },
            { key: "type", header: "Type", render: (n) => (n.spaceType ? label(n.spaceType) : label(n.nodeType)), muted: true },
            { key: "capacity", header: "Capacity", render: (n) => (n.capacity != null ? formatNumber(n.capacity) : "—"), muted: true },
            { key: "status", header: "Status", render: (n) => <StatusBadge status={n.status} /> },
            { key: "actions", header: "", render: (n) => <Actions actions={nodeActions(n)} disabled={busy} /> },
          ]}
        />
      </BentoTile>
      <BentoTile
        span={12}
        title="Room bookings"
        data-slice="8A"
        action={
          <Button size="sm" data-action="booking-new" onClick={() => book()}>
            Book a space
          </Button>
        }
      >
        <DataTable<Reservation>
          caption="Reservations"
          rows={bookings}
          rowKey={(r) => r.id}
          empty={<EmptyState icon={CalendarCheck} title="No bookings yet" compact>Book a room, lab or hall for a date and time window.</EmptyState>}
          columns={[
            { key: "purpose", header: "Booking", render: (r) => <CodeName name={r.purpose} code={r.number} /> },
            { key: "space", header: "Space", render: (r) => <CodeName name={r.nodeName} code={r.nodeCode} /> },
            { key: "when", header: "When", render: (r) => `${fmtDate(r.date)} ${r.startTime}–${r.endTime}` },
            { key: "attendees", header: "Attendees", render: (r) => formatNumber(r.attendees), muted: true },
            { key: "status", header: "Status", render: (r) => (r.flagged ? <Badge kind="attention">Space suspended</Badge> : <StatusBadge status={r.status} />) },
            { key: "actions", header: "", render: (r) => <Actions actions={bookingActions(r)} disabled={busy} /> },
          ]}
        />
      </BentoTile>
      {open && <NodeDrawer nodeId={open} onClose={() => setOpen(null)} />}
      {prompt}
      {dialog}
    </>
  );
}

/** Admin → Campus → Infrastructure & rooms. */
export function InfrastructureAdminTab({ institution }: { institution: Institution | null }) {
  if (!institution) return <NoInstitutionTile title="Infrastructure & rooms" />;
  return <InfrastructureScreen institutionId={institution.id} />;
}

// ================================================================== Service desk & maintenance

const TICKET_FILTERS = [
  { key: "open", label: "Open" },
  { key: "breached", label: "SLA breached" },
  { key: "all", label: "All" },
] as const;

function SlaBadge({ t }: { t: Ticket }) {
  if (t.slaBreached) return <Badge kind="attention">{`Breached · L${t.escalationLevel}`}</Badge>;
  if (["resolved", "closed", "cancelled", "merged"].includes(t.status)) return <Badge kind="done">Met</Badge>;
  return <Badge kind="info">{`Due ${fmtDate(t.slaDueAt.slice(0, 10))}`}</Badge>;
}

function FacilitiesScreen({ institutionId }: { institutionId: string }) {
  const api = useFacilitiesApi();
  const { run, submit, busy, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const [filter, setFilter] = useState<(typeof TICKET_FILTERS)[number]["key"]>("open");
  const nodes = useTree(institutionId);
  const tickets =
    useQuery({
      queryKey: fkeys(institutionId, "tickets", filter),
      queryFn: () => api.tickets(institutionId, filter === "open" ? { open: true } : filter === "breached" ? { breached: true } : {}),
    }).data?.items ?? [];
  const workOrders = useQuery({ queryKey: fkeys(institutionId, "work-orders"), queryFn: () => api.workOrders(institutionId) }).data ?? [];
  const plans = useQuery({ queryKey: fkeys(institutionId, "plans"), queryFn: () => api.plans(institutionId) }).data ?? [];
  const reasonAsk = (title: string, name: string, action: (reason: string) => Promise<unknown>, done: string) =>
    ask({ title, name, fields: [REASON], submitLabel: "Save", onSubmit: (v) => submit(() => action(v.reason!), done) });
  const textAsk = (title: string, name: string, field: FieldSpec, action: (value: string) => Promise<unknown>, done: string) =>
    ask({ title, name, fields: [field], submitLabel: "Save", onSubmit: (v) => submit(() => action(v[field.name]!), done) });

  const ticketActions = (t: Ticket): RowAction[] => {
    const a: RowAction[] = [];
    const s = t.status;
    if (s === "open")
      a.push({
        id: "ticket-triage",
        label: "Triage",
        variant: "primary",
        onClick: () =>
          ask({
            title: `Triage ${t.number}`,
            name: "ticket-triage",
            fields: [{ name: "priority", label: "Priority", type: "select", options: options(PRIORITIES), initial: t.priority }],
            submitLabel: "Triage",
            onSubmit: (v) => submit(() => api.ticketCommand(t.id, t.version, "triage", undefined, v.priority as Ticket["priority"]), `${t.number} triaged`),
          }),
      });
    if (["open", "triaged", "on_hold", "reopened", "assigned", "in_progress"].includes(s))
      a.push({ id: "ticket-assign", label: s === "assigned" || s === "in_progress" ? "Reassign" : "Assign", onClick: () => textAsk(`Assign ${t.number}`, "ticket-assign", { name: "assignee", label: "Technician / team", required: true, initial: t.assignee ?? "" }, (x) => api.assignTicket(t.id, t.version, x), `${t.number} assigned`) });
    if (["assigned", "on_hold", "reopened"].includes(s) && t.assignee) a.push({ id: "ticket-start", label: "Start", onClick: () => void run(() => api.ticketCommand(t.id, t.version, "start"), `${t.number} in progress`) });
    if (["assigned", "in_progress"].includes(s)) {
      a.push({ id: "ticket-hold", label: "Hold", onClick: () => reasonAsk(`Hold ${t.number}`, "ticket-hold", (r) => api.ticketCommand(t.id, t.version, "hold", r), `${t.number} on hold`) });
      a.push({ id: "ticket-resolve", label: "Resolve", variant: "primary", onClick: () => textAsk(`Resolve ${t.number}`, "ticket-resolve", { name: "resolution", label: "Resolution", required: true, wide: true }, (x) => api.resolveTicket(t.id, t.version, x), `${t.number} resolved`) });
    }
    if (["open", "triaged", "assigned", "in_progress", "on_hold", "reopened"].includes(s)) {
      a.push({ id: "ticket-workorder", label: "Work order", onClick: () => textAsk(`Work order for ${t.number}`, "ticket-workorder", { name: "assignee", label: "Technician (optional)", initial: t.assignee ?? "" }, (x) => api.raiseWorkOrder(t.id, x || undefined), "Work order raised") });
      a.push({ id: "ticket-escalate", label: "Escalate", onClick: () => reasonAsk(`Escalate ${t.number}`, "ticket-escalate", (r) => api.ticketCommand(t.id, t.version, "escalate", r), `${t.number} escalated`) });
    }
    if (s === "resolved") {
      a.push({ id: "ticket-close", label: "Close", variant: "primary", onClick: () => void run(() => api.ticketCommand(t.id, t.version, "close"), `${t.number} closed`) });
      a.push({ id: "ticket-reopen", label: "Reopen", onClick: () => reasonAsk(`Reopen ${t.number}`, "ticket-reopen", (r) => api.ticketCommand(t.id, t.version, "reopen", r), `${t.number} reopened`) });
    }
    if (["open", "triaged", "assigned", "reopened"].includes(s))
      a.push({ id: "ticket-cancel", label: "Cancel", onClick: () => reasonAsk(`Cancel ${t.number}`, "ticket-cancel", (r) => api.ticketCommand(t.id, t.version, "cancel", r), `${t.number} cancelled`) });
    return a;
  };

  const woActions = (w: WorkOrder): RowAction[] => {
    const a: RowAction[] = [];
    if (["created", "assigned", "on_hold", "reopened", "in_progress"].includes(w.status))
      a.push({ id: "wo-assign", label: w.assignee ? "Reassign" : "Assign", onClick: () => textAsk(`Assign ${w.number}`, "wo-assign", { name: "assignee", label: "Technician", required: true }, (x) => api.assignWorkOrder(w.id, w.version, x), `${w.number} assigned`) });
    if (["assigned", "on_hold", "reopened"].includes(w.status) && w.assignee) a.push({ id: "wo-start", label: "Start", onClick: () => void run(() => api.workOrderCommand(w.id, w.version, "start"), `${w.number} started`) });
    if (w.status === "in_progress") a.push({ id: "wo-complete", label: "Complete", variant: "primary", onClick: () => textAsk(`Complete ${w.number}`, "wo-complete", { name: "notes", label: "Work done", required: true, wide: true }, (x) => api.completeWorkOrder(w.id, w.version, x), `${w.number} completed`) });
    if (w.status === "completed") {
      a.push({ id: "wo-verify", label: "Verify", variant: "primary", onClick: () => void run(() => api.verifyWorkOrder(w.id, w.version, true), `${w.number} verified`) });
      a.push({ id: "wo-reject", label: "Not fixed", onClick: () => textAsk(`Reject ${w.number}`, "wo-reject", { name: "notes", label: "What is still wrong", required: true, wide: true }, (x) => api.verifyWorkOrder(w.id, w.version, false, x), `${w.number} reopened`) });
    }
    if (["created", "assigned", "on_hold", "reopened"].includes(w.status)) a.push({ id: "wo-cancel", label: "Cancel", onClick: () => reasonAsk(`Cancel ${w.number}`, "wo-cancel", (r) => api.workOrderCommand(w.id, w.version, "cancel", r), `${w.number} cancelled`) });
    return a;
  };

  const planActions = (p: Plan): RowAction[] => {
    const a: RowAction[] = [];
    if (p.status === "draft" && p.approvalState !== "pending") a.push({ id: "plan-submit", label: "Submit", variant: "primary", onClick: () => void run(() => api.planCommand(p.id, p.version, "submit"), (r) => `Plan ${r.plan.status}`) });
    if (p.status === "active") {
      a.push({ id: "plan-generate", label: "Generate", variant: "primary", onClick: () => textAsk(`Generate work for ${p.title}`, "plan-generate", { name: "through", label: "Through", type: "date", required: true, initial: isoPlus(90) }, (x) => api.generate(p.id, x), "Cycles generated") });
      a.push({ id: "plan-suspend", label: "Suspend", onClick: () => reasonAsk(`Suspend ${p.title}`, "plan-suspend", (r) => api.planCommand(p.id, p.version, "suspend", r), "Plan suspended") });
    }
    if (p.status === "suspended") a.push({ id: "plan-activate", label: "Resume", variant: "primary", onClick: () => void run(() => api.planCommand(p.id, p.version, "activate"), "Plan resumed") });
    if (p.status !== "retired") a.push({ id: "plan-retire", label: "Retire", onClick: () => void run(() => api.planCommand(p.id, p.version, "retire"), "Plan retired") });
    return a;
  };

  const newTicket = () =>
    ask({
      title: "New service request",
      name: "ticket",
      fields: [
        { name: "category", label: "Category", type: "select", required: true, options: options(CATEGORIES), initial: "facility" },
        { name: "priority", label: "Priority", type: "select", options: options(PRIORITIES), initial: "medium" },
        { name: "nodeId", label: "Where", type: "select", options: [{ value: "", label: "—" }, ...nodes.map((n) => ({ value: n.id, label: nameCode(n.name, n.code) }))] },
        { name: "title", label: "Title", required: true, wide: true },
        { name: "isComplaint", label: "Complaint", type: "checkbox", initial: "false" },
      ],
      submitLabel: "Open",
      onSubmit: (v) =>
        submit(
          () =>
            api.createTicket(institutionId, {
              category: v.category as Ticket["category"],
              priority: v.priority as Ticket["priority"],
              nodeId: v.nodeId || undefined,
              title: v.title!,
              isComplaint: v.isComplaint === "true",
            }),
          (t) => `${t.number} opened`,
        ),
    });

  const newPlan = () =>
    ask({
      title: "New preventive plan",
      name: "plan",
      fields: [
        { name: "nodeId", label: "Asset / space", type: "select", required: true, options: nodes.map((n) => ({ value: n.id, label: nameCode(n.name, n.code) })) },
        { name: "title", label: "Title", required: true, placeholder: "Fire extinguisher check" },
        { name: "frequency", label: "Frequency", type: "select", options: options(FREQUENCIES), initial: "monthly" },
        { name: "startDate", label: "Starts", type: "date", required: true, initial: todayIso() },
        { name: "defaultAssignee", label: "Technician" },
      ],
      submitLabel: "Create draft",
      onSubmit: (v) =>
        submit(
          () =>
            api.createPlan(institutionId, {
              nodeId: v.nodeId!,
              title: v.title!,
              frequency: v.frequency as Plan["frequency"],
              startDate: v.startDate!,
              defaultAssignee: v.defaultAssignee || undefined,
            }),
          "Draft plan created",
        ),
    });

  const openTickets = tickets.filter((t) => !["closed", "cancelled", "merged"].includes(t.status));
  return (
    <>
      <KpiTile span={3} icon={ClipboardList} tone="blue" label="Tickets shown" value={String(tickets.length)} data-slice="8A" />
      <KpiTile span={3} icon={AlertTriangle} tone="rose" label="Past SLA" value={String(tickets.filter((t) => t.slaBreached).length)} />
      <KpiTile span={3} icon={Wrench} tone="orange" label="Open work orders" value={String(workOrders.filter((w) => !["completed", "verified", "cancelled"].includes(w.status)).length)} />
      <KpiTile span={3} icon={Timer} tone="green" label="Active plans" value={String(plans.filter((p) => p.status === "active").length)} />
      <BentoTile
        span={12}
        title="Service desk"
        data-slice="8A"
        action={
          <>
            <Button size="sm" variant="ghost" data-action="sla-check" onClick={() => void run(() => api.slaCheck(institutionId), (r) => `SLA check: ${r.processed} escalated`)}>
              Run SLA check
            </Button>
            <Button size="sm" data-action="ticket-new" onClick={newTicket}>
              New request
            </Button>
          </>
        }
      >
        {TICKET_FILTERS.map((f) => (
          <FilterChip key={f.key} active={filter === f.key} label={f.label} count={filter === f.key ? (f.key === "open" ? openTickets.length : tickets.length) : undefined} onClick={() => setFilter(f.key)} />
        ))}
        <DataTable<Ticket>
          caption="Service tickets"
          rows={tickets}
          rowKey={(t) => t.id}
          empty={<EmptyState icon={ClipboardList} title="No tickets" compact>Service requests and complaints from students, staff and the office land here.</EmptyState>}
          columns={[
            { key: "title", header: "Request", render: (t) => <CodeName name={t.isComplaint ? `${t.title} (complaint)` : t.title} code={t.number} /> },
            { key: "category", header: "Category", render: (t) => (t.category === "it" ? "IT" : label(t.category)), muted: true },
            { key: "priority", header: "Priority", render: (t) => label(t.priority), muted: true },
            { key: "sla", header: "SLA", render: (t) => <SlaBadge t={t} /> },
            { key: "assignee", header: "Assignee", render: (t) => t.assignee ?? "—", muted: true },
            { key: "status", header: "Status", render: (t) => <StatusBadge status={t.status} /> },
            { key: "actions", header: "", render: (t) => <Actions actions={ticketActions(t)} disabled={busy} /> },
          ]}
        />
      </BentoTile>
      <BentoTile span={workOrders.length || plans.length ? 12 : 6} title="Work orders" data-slice="8A">
        <DataTable<WorkOrder>
          caption="Work orders"
          rows={workOrders}
          rowKey={(w) => w.id}
          empty={<EmptyState icon={Wrench} title="No work orders" compact>Raise one from a ticket or generate preventive work from a plan.</EmptyState>}
          columns={[
            { key: "title", header: "Work order", render: (w) => <CodeName name={w.title} code={w.number} sub={[label(w.kind), w.ticketNumber ?? w.nodeCode].filter(Boolean).join(" · ")} /> },
            { key: "assignee", header: "Technician", render: (w) => w.assignee ?? "—", muted: true },
            { key: "due", header: "Scheduled", render: (w) => (w.scheduledFor ? fmtDate(w.scheduledFor) : "—"), muted: true },
            { key: "status", header: "Status", render: (w) => <StatusBadge status={w.status} /> },
            { key: "actions", header: "", render: (w) => <Actions actions={woActions(w)} disabled={busy} /> },
          ]}
        />
      </BentoTile>
      <BentoTile
        span={workOrders.length || plans.length ? 12 : 6}
        title="Preventive maintenance"
        data-slice="8A"
        action={
          <Button size="sm" data-action="plan-new" onClick={newPlan}>
            New plan
          </Button>
        }
      >
        <DataTable<Plan>
          caption="Preventive plans"
          rows={plans}
          rowKey={(p) => p.id}
          empty={<EmptyState icon={Timer} title="No preventive plans" compact>Plan recurring checks for a building or space; each period becomes a work order.</EmptyState>}
          columns={[
            { key: "title", header: "Plan", render: (p) => p.title },
            { key: "node", header: "Where", render: (p) => p.nodeCode, muted: true },
            { key: "frequency", header: "Every", render: (p) => label(p.frequency), muted: true },
            { key: "start", header: "From", render: (p) => fmtDate(p.startDate), muted: true },
            { key: "approval", header: "Approval", render: (p) => <ApprovalBadge state={p.approvalState} /> },
            { key: "status", header: "Status", render: (p) => <StatusBadge status={p.status} /> },
            { key: "actions", header: "", render: (p) => <Actions actions={planActions(p)} disabled={busy} /> },
          ]}
        />
      </BentoTile>
      {prompt}
      {dialog}
    </>
  );
}

/** Admin → Campus → Service desk & maintenance. */
export function FacilitiesAdminTab({ institution }: { institution: Institution | null }) {
  if (!institution) return <NoInstitutionTile title="Service desk & maintenance" />;
  return <FacilitiesScreen institutionId={institution.id} />;
}

// ================================================================== Laboratories

function LabSessions({ lab }: { lab: Lab }) {
  const api = useFacilitiesApi();
  const { run, submit, busy, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const detail = useQuery({ queryKey: fkeys(lab.id, "lab"), queryFn: () => api.lab(lab.id) }).data;
  const equipment = detail?.equipment ?? [];
  const sessions = detail?.sessions ?? [];
  const eqActions = (e: Equipment): RowAction[] => [
    {
      id: "equipment-status",
      label: "Status",
      onClick: () =>
        ask({
          title: `Status of ${e.code}`,
          name: "equipment-status",
          fields: [
            { name: "status", label: "Status", type: "select", options: options(EQUIPMENT_STATUSES), initial: e.status },
            { name: "reason", label: "Reason", wide: true },
          ],
          submitLabel: "Save",
          onSubmit: (v) => submit(() => api.equipmentStatus(e.id, e.version, v.status as Equipment["status"], v.reason), `${e.code} ${label(v.status ?? "")}`),
        }),
    },
  ];
  const sessionActions = (s: Session): RowAction[] => {
    const a: RowAction[] = [];
    if (s.status === "planned" || s.status === "postponed") a.push({ id: "session-ready", label: "Ready", variant: "primary", onClick: () => void run(() => api.sessionCommand(s.id, s.version, "ready"), "Session ready") });
    if (s.status === "ready") a.push({ id: "session-start", label: "Start", variant: "primary", onClick: () => void run(() => api.sessionCommand(s.id, s.version, "start"), "Session started") });
    if (s.status === "in_progress") a.push({ id: "session-complete", label: "Complete", variant: "primary", onClick: () => void run(() => api.sessionCommand(s.id, s.version, "complete"), "Session completed") });
    if (["planned", "ready", "postponed"].includes(s.status))
      a.push({ id: "session-cancel", label: "Cancel", onClick: () => ask({ title: `Cancel ${s.title}`, name: "session-cancel", fields: [REASON], submitLabel: "Cancel session", onSubmit: (v) => submit(() => api.sessionCommand(s.id, s.version, "cancel", v.reason), "Session cancelled") }) });
    return a;
  };
  return (
    <>
      <BentoTile
        span={6}
        title={`Equipment · ${lab.name}`}
        data-slice="8A"
        action={
          <Button
            size="sm"
            data-action="equipment-new"
            onClick={() =>
              ask({
                title: `Equipment for ${lab.name}`,
                name: "equipment",
                fields: [
                  { name: "code", label: "Code", required: true, placeholder: "FUME-01" },
                  { name: "name", label: "Name", required: true },
                ],
                submitLabel: "Add",
                onSubmit: (v) => submit(() => api.addEquipment(lab.id, v.code!, v.name!), "Equipment added"),
              })
            }
          >
            Add equipment
          </Button>
        }
      >
        <DataTable<Equipment>
          caption="Lab equipment"
          rows={equipment}
          rowKey={(e) => e.id}
          empty={<EmptyState icon={FlaskConical} title="No equipment registered" compact />}
          columns={[
            { key: "code", header: "Code", render: (e) => e.code },
            { key: "name", header: "Name", render: (e) => e.name, muted: true },
            { key: "status", header: "Status", render: (e) => <StatusBadge status={e.status} /> },
            { key: "actions", header: "", render: (e) => <Actions actions={eqActions(e)} disabled={busy} /> },
          ]}
        />
      </BentoTile>
      <BentoTile
        span={6}
        title={`Practical sessions · ${lab.name}`}
        data-slice="8A"
        action={
          <Button
            size="sm"
            data-action="session-new"
            onClick={() =>
              ask({
                title: `Plan a session in ${lab.name}`,
                name: "session",
                fields: [
                  { name: "title", label: "Title", required: true, wide: true },
                  { name: "courseCode", label: "Course" },
                  { name: "sessionDate", label: "Date", type: "date", required: true, initial: isoPlus(1) },
                  { name: "startTime", label: "From", required: true, initial: "14:00" },
                  { name: "endTime", label: "To", required: true, initial: "16:00" },
                  { name: "capacity", label: "Seats", type: "number", initial: String(lab.capacity) },
                ],
                submitLabel: "Plan",
                onSubmit: (v) =>
                  submit(
                    () =>
                      api.createSession(lab.id, {
                        title: v.title!,
                        courseCode: v.courseCode || undefined,
                        sessionDate: v.sessionDate!,
                        startTime: v.startTime!,
                        endTime: v.endTime!,
                        capacity: v.capacity ? Number(v.capacity) : undefined,
                      }),
                    "Session planned",
                  ),
              })
            }
          >
            Plan session
          </Button>
        }
      >
        <DataTable<Session>
          caption="Practical sessions"
          rows={sessions}
          rowKey={(s) => s.id}
          empty={<EmptyState icon={CalendarCheck} title="No sessions planned" compact />}
          columns={[
            { key: "title", header: "Session", render: (s) => (s.courseCode ? `${s.courseCode} · ${s.title}` : s.title) },
            { key: "when", header: "When", render: (s) => `${fmtDate(s.sessionDate)} ${s.startTime}–${s.endTime}`, muted: true },
            { key: "seats", header: "Students", render: (s) => `${s.allocated} / ${s.capacity}`, muted: true },
            { key: "status", header: "Status", render: (s) => <StatusBadge status={s.status} /> },
            { key: "actions", header: "", render: (s) => <Actions actions={sessionActions(s)} disabled={busy} /> },
          ]}
        />
      </BentoTile>
      {prompt}
      {dialog}
    </>
  );
}

function LabsScreen({ institutionId }: { institutionId: string }) {
  const api = useFacilitiesApi();
  const { submit, busy, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const nodes = useTree(institutionId);
  const labs = useQuery({ queryKey: fkeys(institutionId, "labs"), queryFn: () => api.labs(institutionId) }).data ?? [];
  const [selected, setSelected] = useState<string | null>(null);
  const current = labs.find((l) => l.id === selected) ?? labs[0];
  const readiness = (l: Lab) =>
    ask({
      title: `Readiness check · ${l.name}`,
      name: "lab-readiness",
      fields: [
        ...READINESS_ITEMS.map((item, i): FieldSpec => ({ name: `item${i}`, label: item, type: "checkbox", initial: l.checklist.find((c) => c.item === item)?.done ? "true" : "false" })),
        { name: "notes", label: "Notes", wide: true },
      ],
      submitLabel: "Record check",
      onSubmit: (v) =>
        submit(
          () => api.readiness(l.id, l.version, READINESS_ITEMS.map((item, i) => ({ item, done: v[`item${i}`] === "true" })), v.notes),
          (d) => `Lab ${d.lab.status.replace(/_/g, " ")}`,
        ),
    });
  const labActions = (l: Lab): RowAction[] => [
    { id: "lab-readiness", label: "Readiness", variant: "primary", onClick: () => readiness(l) },
    { id: "lab-open", label: "Sessions", onClick: () => setSelected(l.id) },
  ];
  const withProfile = new Set(labs.map((l) => l.nodeId));
  return (
    <>
      <KpiTile span={4} icon={FlaskConical} tone="green" label="Labs ready" value={String(labs.filter((l) => l.status === "ready").length)} data-slice="8A" />
      <KpiTile span={4} icon={AlertTriangle} tone="amber" label="Restricted" value={String(labs.filter((l) => l.status === "restricted").length)} />
      <KpiTile span={4} icon={ClipboardList} tone="grey" label="Not ready" value={String(labs.filter((l) => l.status === "not_ready" || l.status === "unavailable").length)} />
      <BentoTile
        span={12}
        title="Laboratories"
        data-slice="8A"
        action={
          <Button
            size="sm"
            data-action="lab-new"
            onClick={() =>
              ask({
                title: "Lab profile",
                name: "lab",
                fields: [
                  { name: "nodeId", label: "Lab space", type: "select", required: true, options: spaceOptions(nodes, (n) => n.spaceType === "lab" && !withProfile.has(n.id)) },
                  { name: "name", label: "Name" },
                ],
                submitLabel: "Create",
                onSubmit: (v) => submit(() => api.createLab(institutionId, v.nodeId!, v.name || undefined), "Lab profile created"),
              })
            }
          >
            New lab profile
          </Button>
        }
      >
        <DataTable<Lab>
          caption="Laboratories"
          rows={labs}
          rowKey={(l) => l.id}
          empty={<EmptyState icon={FlaskConical} title="No laboratories yet" compact>Register a `lab` space under Infrastructure, then create its lab profile.</EmptyState>}
          columns={[
            { key: "name", header: "Lab", render: (l) => l.name },
            { key: "space", header: "Space", render: (l) => l.nodeCode, muted: true },
            { key: "capacity", header: "Seats", render: (l) => String(l.capacity), muted: true },
            { key: "check", header: "Last check", render: (l) => (l.readinessCheckedAt ? fmtDate(l.readinessCheckedAt.slice(0, 10)) : "—"), muted: true },
            { key: "status", header: "Status", render: (l) => <StatusBadge status={l.status} /> },
            { key: "actions", header: "", render: (l) => <Actions actions={labActions(l)} disabled={busy} /> },
          ]}
        />
      </BentoTile>
      {current && <LabSessions lab={current} />}
      {prompt}
      {dialog}
    </>
  );
}

/** Admin → Campus → Laboratories. */
export function LabsAdminTab({ institution }: { institution: Institution | null }) {
  if (!institution) return <NoInstitutionTile title="Laboratories" />;
  return <LabsScreen institutionId={institution.id} />;
}
