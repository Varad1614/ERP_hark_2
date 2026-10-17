/**
 * Slice **8B** web components (hostel and transport). Export names are imported by the foundation
 * frame `routes/admin/campus-setup.tsx` (docs/batch8-contracts.md §10): keep them. Every screen
 * carries `data-slice="8B"`. Only @sutra/ui components — no custom styling. Money and dates stay
 * the strings the API sends.
 *
 * - `HostelAdminTab` — Admin → Campus → Hostel: policy versions, rooms / beds occupancy, the
 *   applications queue (decide, allocate, waiting list), residents (leave, transfer, inspection,
 *   complaints, vacate / close), resident leave, inspections and mess.
 * - `TransportAdminTab` — Admin → Campus → Transport: routes (versions, stops, activation),
 *   stops, vehicles, drivers, passenger allocations, passes and trips.
 */
import { BedDouble, Bus, CalendarDays, ClipboardList, DoorOpen, Hourglass, MapPin, Route, Ticket, Truck, Users, Utensils, Wrench } from "lucide-react";
import { BentoTile, Button, DataTable, EmptyState, FilterChip, KpiTile, NameCell } from "@sutra/ui";
import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { fmtDate, REASON, todayIso, useAsk, type FieldSpec, type RowAction } from "../../institution/forms";
import { Actions, CodeName, inr, inr as formatInr } from "../ui";
import { StatusBadge, statusLabel, useCommand } from "../../institution/hooks";
import type { Institution } from "../../institution/types";
import { rkeys, useResidenceApi } from "./hooks";
import { FleetMaintenanceTile } from "../fleet/FleetMaintenanceTile";
import { kindOf, personField, usePeople, type PersonOption } from "../PersonPicker"; // 9E
import type { HostelAllocation, HostelApplication, HostelBed, HostelPolicy, Mess, ResidentLeave, RoomInspection, RouteVersion, TransportAllocation, TransportPass, TransportRoute, TransportStop, Trip, Vehicle } from "./types";

export { ResidenceApiProvider } from "./hooks";

const PERSON_KINDS = [
  { value: "student", label: "Student" },
  { value: "staff", label: "Staff" },
];
const MENU_TYPES = [
  { value: "veg", label: "Vegetarian" },
  { value: "non_veg", label: "Non-vegetarian" },
  { value: "mixed", label: "Mixed" },
];
const DIRECTIONS = [
  { value: "pickup", label: "Pickup" },
  { value: "drop", label: "Drop" },
];
const VEHICLE_STATES = [
  { value: "available", label: "Available" },
  { value: "under_maintenance", label: "Under maintenance" },
  { value: "retired", label: "Retired" },
];
// 9E: PersonPicker — people are chosen by number + name from the directories, not by record id.
const personFields = (people: readonly PersonOption[]): FieldSpec[] => [
  { name: "personKind", label: "Person", type: "select", options: PERSON_KINDS, initial: "student" },
  personField(people),
];
const APPLICATION_FILTERS = ["under_review", "approved", "waitlisted", "allocated", ""] as const;
const OPTIONAL_REASON: FieldSpec = { name: "reason", label: "Reason", wide: true };

function NoInstitutionTile({ title }: { title: string }) {
  return (
    <BentoTile span={12} title={title} data-slice="8B">
      <EmptyState icon={CalendarDays} title="No institution selected" compact>
        Register an institution in Campus setup → Institution first.
      </EmptyState>
    </BentoTile>
  );
}

const count = (rows: unknown[] | undefined) => (rows ? String(rows.length) : "…");

// ================================================================== Hostel

function useHostel(inst: string) {
  const api = useResidenceApi();
  return {
    policies: useQuery({ queryKey: rkeys(inst, "policies"), queryFn: () => api.policies(inst) }).data,
    beds: useQuery({ queryKey: rkeys(inst, "beds"), queryFn: () => api.beds(inst) }).data,
    rooms: useQuery({ queryKey: rkeys(inst, "rooms"), queryFn: () => api.rooms(inst) }).data,
    residents: useQuery({ queryKey: rkeys(inst, "allocations"), queryFn: () => api.allocations(inst) }).data,
    messes: useQuery({ queryKey: rkeys(inst, "messes"), queryFn: () => api.messes(inst) }).data,
  };
}

function HostelStats({ inst }: { inst: string }) {
  const api = useResidenceApi();
  const { policies, beds, residents } = useHostel(inst);
  const review = useQuery({ queryKey: rkeys(inst, "applications:under_review"), queryFn: () => api.applications(inst, "under_review") }).data;
  const waiting = useQuery({ queryKey: rkeys(inst, "applications:waitlisted"), queryFn: () => api.applications(inst, "waitlisted") }).data;
  const active = policies?.find((p) => p.status === "active");
  const occupied = beds?.filter((b) => b.status === "occupied").length;
  return (
    <>
      <KpiTile icon={ClipboardList} tone="orange" label="Active policy" value={policies ? (active ? formatInr(active.feePerTerm) : "None") : "…"} sub={active ? `${active.name} · fee per term` : "Create and activate a policy"} data-kpi="hostel-policy" />
      <KpiTile icon={BedDouble} tone="blue" label="Beds occupied" value={beds ? `${occupied ?? 0} / ${beds.length}` : "…"} sub="Across active hostel rooms" data-kpi="hostel-occupancy" />
      <KpiTile icon={Hourglass} tone="violet" label="Applications to decide" value={count(review)} sub={`${waiting?.length ?? 0} on the waiting list`} data-kpi="hostel-review" />
      <KpiTile icon={Users} tone="green" label="Residents" value={residents ? String(residents.filter((r) => r.status !== "closed").length) : "…"} sub="Live residencies" data-kpi="hostel-residents" />
    </>
  );
}

function PoliciesTile({ inst }: { inst: string }) {
  const api = useResidenceApi();
  const { run, submit, busy, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const { policies } = useHostel(inst);
  const actions = (p: HostelPolicy): RowAction[] => {
    const a: RowAction[] = [];
    if (p.status === "draft" && !p.processInstanceId)
      a.push({ id: "hostel-policy-submit", label: "Submit for approval", variant: "primary", onClick: () => void run(() => api.policyCommand(p.id, p.version, "submit"), (r) => `Policy ${statusLabel(r.status).toLowerCase()}`) });
    if (p.status === "draft" && p.processInstanceId) {
      a.push({ id: "hostel-policy-approve", label: "Approve", variant: "primary", onClick: () => void run(() => api.decidePolicy(p.id, p.version, "approve"), "Policy approved") });
      a.push({ id: "hostel-policy-reject", label: "Reject", onClick: () => ask({ title: `Reject ${p.name}`, name: "hostel-policy-reject", fields: [REASON], submitLabel: "Reject", onSubmit: (v) => submit(() => api.decidePolicy(p.id, p.version, "reject", v.reason), "Policy returned to draft") }) });
    }
    if (p.status === "approved") a.push({ id: "hostel-policy-activate", label: "Activate", variant: "primary", onClick: () => void run(() => api.policyCommand(p.id, p.version, "activate"), "Policy activated") });
    return a;
  };
  return (
    <BentoTile
      span={12}
      title="Hostel policy"
      data-slice="8B"
      action={
        <Button
          size="sm"
          data-action="hostel-policy-new"
          onClick={() =>
            ask({
              title: "New hostel policy version",
              name: "hostel-policy",
              fields: [
                { name: "name", label: "Name", required: true, placeholder: "Hostel policy 2026", wide: true },
                { name: "feePerTerm", label: "Fee per term (₹)", required: true, placeholder: "45000.00" },
                { name: "deposit", label: "Deposit (₹)", placeholder: "5000.00" },
                { name: "curfewTime", label: "Curfew (HH:MM)", placeholder: "21:30" },
                { name: "maxStayTerms", label: "Maximum stay (terms)", type: "number" },
                { name: "effectiveFrom", label: "Effective from", type: "date", required: true, initial: todayIso() },
                { name: "eligibilityNote", label: "Eligibility (programmes, gender note)", wide: true },
              ],
              submitLabel: "Create draft",
              onSubmit: (v) =>
                submit(
                  () =>
                    api.createPolicy(inst, {
                      name: v.name,
                      feePerTerm: v.feePerTerm,
                      deposit: v.deposit || undefined,
                      curfewTime: v.curfewTime || undefined,
                      maxStayTerms: v.maxStayTerms ? Number(v.maxStayTerms) : undefined,
                      effectiveFrom: v.effectiveFrom,
                      eligibilityNote: v.eligibilityNote || undefined,
                    }),
                  "Draft policy created",
                ),
            })
          }
        >
          New policy
        </Button>
      }
    >
      <DataTable<HostelPolicy>
        caption="Hostel policy versions"
        rows={policies ?? []}
        rowKey={(p) => p.id}
        empty={<EmptyState icon={ClipboardList} title="No hostel policy yet" compact>Draft a policy with the fee per term, submit it for approval and activate it — admissions need an active policy.</EmptyState>}
        columns={[
          { key: "name", header: "Policy", render: (p) => p.name },
          { key: "fee", header: "Fee / term", render: (p) => formatInr(p.feePerTerm) },
          { key: "deposit", header: "Deposit", render: (p) => formatInr(p.deposit), muted: true },
          { key: "curfew", header: "Curfew", render: (p) => p.curfewTime ?? "—", muted: true },
          { key: "from", header: "Effective", render: (p) => fmtDate(p.effectiveFrom), muted: true },
          { key: "status", header: "Status", render: (p) => <StatusBadge status={p.status === "draft" && p.processInstanceId ? "pending_approval" : p.status} /> },
          { key: "actions", header: "", render: (p) => <Actions actions={actions(p)} disabled={busy} /> },
        ]}
      />
      {prompt}
      {dialog}
    </BentoTile>
  );
}

function BedsTile({ inst }: { inst: string }) {
  const api = useResidenceApi();
  const { submit, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const { beds, rooms } = useHostel(inst);
  const roomOptions = (rooms ?? []).filter((r) => r.status === "active").map((r) => ({ value: r.id, label: `${r.code} · ${r.name} (${r.capacity ?? 0} beds)` }));
  return (
    <BentoTile
      span={12}
      title="Rooms & beds"
      data-slice="8B"
      action={
        <Button
          size="sm"
          data-action="hostel-bed-new"
          disabled={roomOptions.length === 0}
          onClick={() =>
            ask({
              title: "Add a bed to a hostel room",
              name: "hostel-bed",
              fields: [
                { name: "roomId", label: "Hostel room", type: "select", options: roomOptions, required: true, initial: roomOptions[0]?.value },
                { name: "bedCode", label: "Bed code", required: true, placeholder: "A" },
              ],
              submitLabel: "Add bed",
              onSubmit: (v) => submit(() => api.addBed(v.roomId!, v.bedCode!), "Bed added"),
            })
          }
        >
          Add bed
        </Button>
      }
    >
      <DataTable<HostelBed>
        caption="Beds and occupancy"
        rows={beds ?? []}
        rowKey={(b) => b.id}
        empty={<EmptyState icon={BedDouble} title="No beds yet" compact>Hostel rooms are `hostel_room` spaces of the infrastructure tree; add beds up to each room's capacity.</EmptyState>}
        columns={[
          { key: "room", header: "Room", render: (b) => <CodeName name={b.roomName} code={b.roomCode} /> },
          { key: "bed", header: "Bed", render: (b) => `Bed ${b.bedCode}` },
          { key: "occupant", header: "Occupant", render: (b) => (b.occupantNumber ? <NameCell name={b.occupantName ?? b.occupantNumber} sub={b.occupantNumber} /> : "—"), muted: true },
          { key: "status", header: "Status", render: (b) => <StatusBadge status={b.status} /> },
        ]}
      />
      {prompt}
      {dialog}
    </BentoTile>
  );
}

function ApplicationsTile({ inst }: { inst: string }) {
  const api = useResidenceApi();
  const { run, submit, busy, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const [filter, setFilter] = useState<string>("under_review");
  const { beds } = useHostel(inst);
  const { people } = usePeople(inst); // 9E
  const apps = useQuery({ queryKey: rkeys(inst, `applications:${filter}`), queryFn: () => api.applications(inst, filter || undefined) }).data;
  const freeBeds = (beds ?? []).filter((b) => b.status === "available").map((b) => ({ value: b.id, label: `${b.roomCode} · bed ${b.bedCode}` }));
  const actions = (a: HostelApplication): RowAction[] => {
    const out: RowAction[] = [];
    if (a.status === "under_review") {
      out.push({ id: "hostel-app-approve", label: "Approve", variant: "primary", onClick: () => void run(() => api.decideApplication(a.id, a.version, "approve"), "Admission approved") });
      out.push({ id: "hostel-app-reject", label: "Reject", onClick: () => ask({ title: `Reject ${a.number}`, name: "hostel-app-reject", fields: [REASON], submitLabel: "Reject", onSubmit: (v) => submit(() => api.decideApplication(a.id, a.version, "reject", v.reason), "Application rejected") }) });
    }
    if (a.status === "approved" || a.status === "waitlisted")
      out.push({
        id: "hostel-app-allocate",
        label: "Allocate bed",
        variant: "primary",
        onClick: () =>
          ask({
            title: `Allocate a bed to ${a.personName}`,
            name: "hostel-allocate",
            fields: [{ name: "bedId", label: "Bed (empty = first free bed)", type: "select", options: [{ value: "", label: "First free bed" }, ...freeBeds], initial: "" }],
            submitLabel: "Allocate",
            onSubmit: (v) => submit(() => api.allocate(a.id, a.version, v.bedId || undefined), (r) => (r.status === "waitlisted" ? "No free bed — placed on the waiting list" : "Bed allocated, hostel fee raised")),
          }),
      });
    if (["submitted", "under_review", "approved", "waitlisted"].includes(a.status))
      out.push({ id: "hostel-app-withdraw", label: "Withdraw", onClick: () => ask({ title: `Withdraw ${a.number}`, name: "hostel-app-withdraw", fields: [OPTIONAL_REASON], submitLabel: "Withdraw", onSubmit: (v) => submit(() => api.withdrawApplication(a.id, a.version, v.reason), "Application withdrawn") }) });
    return out;
  };
  return (
    <BentoTile
      span={12}
      title="Admission applications"
      data-slice="8B"
      action={
        <Button
          size="sm"
          data-action="hostel-app-new"
          onClick={() =>
            ask({
              title: "Apply on behalf of a student or staff member",
              name: "hostel-application",
              fields: [...personFields(people), { name: "preference", label: "Preference", wide: true }],
              submitLabel: "Submit application",
              onSubmit: (v) => submit(() => api.apply(inst, { personKind: kindOf(people, v.personId, v.personKind), personId: v.personId?.trim(), preference: v.preference || undefined }), (r) => `Application ${r.number} ${statusLabel(r.status).toLowerCase()}`),
            })
          }
        >
          New application
        </Button>
      }
    >
      <div>
        {APPLICATION_FILTERS.map((f) => (
          <FilterChip key={f || "all"} active={filter === f} label={f ? statusLabel(f) : "All"} onClick={() => setFilter(f)} />
        ))}
      </div>
      <DataTable<HostelApplication>
        caption="Hostel applications"
        rows={apps ?? []}
        rowKey={(a) => a.id}
        empty={<EmptyState icon={ClipboardList} title="No applications" compact>Students and staff apply from self-service; the office can apply on their behalf.</EmptyState>}
        columns={[
          { key: "number", header: "Application", render: (a) => a.number },
          { key: "person", header: "Applicant", render: (a) => <NameCell name={a.personName} sub={a.personNumber} /> },
          { key: "kind", header: "Kind", render: (a) => statusLabel(a.personKind), muted: true },
          { key: "channel", header: "Channel", render: (a) => statusLabel(a.channel), muted: true },
          { key: "status", header: "Status", render: (a) => <StatusBadge status={a.status} /> },
          { key: "actions", header: "", render: (a) => <Actions actions={actions(a)} disabled={busy} /> },
        ]}
      />
      {prompt}
      {dialog}
    </BentoTile>
  );
}

function ResidentsTile({ inst }: { inst: string }) {
  const api = useResidenceApi();
  const { submit, busy, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const { residents, beds } = useHostel(inst);
  const freeBeds = (beds ?? []).filter((b) => b.status === "available").map((b) => ({ value: b.id, label: `${b.roomCode} · bed ${b.bedCode}` }));
  const actions = (r: HostelAllocation): RowAction[] => {
    const out: RowAction[] = [];
    if (r.status === "active") {
      out.push({
        id: "hostel-leave",
        label: "Leave",
        onClick: () =>
          ask({
            title: `Leave for ${r.personName}`,
            name: "hostel-leave",
            fields: [
              { name: "fromDate", label: "From", type: "date", required: true, initial: todayIso() },
              { name: "toDate", label: "To", type: "date", required: true, initial: todayIso() },
              { name: "destination", label: "Destination", wide: true },
            ],
            submitLabel: "Request leave",
            onSubmit: (v) => submit(() => api.requestLeave(r.id, { fromDate: v.fromDate, toDate: v.toDate, destination: v.destination || undefined }), (l) => `Leave ${statusLabel(l.status).toLowerCase()}`),
          }),
      });
      out.push({
        id: "hostel-transfer",
        label: "Transfer",
        onClick: () =>
          ask({
            title: `Move ${r.personName} to another bed`,
            name: "hostel-transfer",
            fields: [{ name: "toBedId", label: "Free bed", type: "select", options: freeBeds, required: true, initial: freeBeds[0]?.value }, OPTIONAL_REASON],
            submitLabel: "Request transfer",
            onSubmit: (v) => submit(() => api.transfer(r.id, v.toBedId!, v.reason), (t) => `Transfer ${statusLabel(t.status).toLowerCase()}`),
          }),
      });
      out.push({
        id: "hostel-inspect",
        label: "Inspect room",
        onClick: () =>
          ask({
            title: `Schedule an inspection of ${r.roomCode}`,
            name: "hostel-inspection",
            fields: [{ name: "scheduledOn", label: "Scheduled on", type: "date", required: true, initial: todayIso() }],
            submitLabel: "Schedule",
            onSubmit: (v) => submit(() => api.scheduleInspection(r.roomNodeId, { scheduledOn: v.scheduledOn, allocationId: r.id }), "Inspection scheduled"),
          }),
      });
      out.push({ id: "hostel-vacate", label: "Vacate", onClick: () => ask({ title: `Vacate ${r.roomCode} · ${r.bedCode}`, name: "hostel-vacate", fields: [OPTIONAL_REASON], submitLabel: "Start vacating", onSubmit: (v) => submit(() => api.allocationCommand(r.id, r.version, "vacate", v.reason), "Vacating started") }) });
    }
    if (r.status === "vacating")
      out.push({ id: "hostel-close", label: "Clear & close", variant: "primary", onClick: () => ask({ title: `Close the residency of ${r.personName}`, name: "hostel-close", fields: [OPTIONAL_REASON], submitLabel: "Close", onSubmit: (v) => submit(() => api.allocationCommand(r.id, r.version, "close", v.reason), "Residency closed, bed released") }) });
    if (r.status !== "closed")
      out.push({
        id: "hostel-complaint",
        label: "Complaint",
        onClick: () =>
          ask({
            title: `Complaint for ${r.personName}`,
            name: "hostel-complaint",
            fields: [
              { name: "title", label: "Title", required: true, wide: true },
              { name: "description", label: "Details", wide: true },
            ],
            submitLabel: "Open ticket",
            onSubmit: (v) => submit(() => api.hostelComplaint(r.id, { title: v.title, description: v.description || undefined }), (c) => `Ticket ${c.number} opened`),
          }),
      });
    return out;
  };
  return (
    <BentoTile span={12} title="Residents" data-slice="8B">
      <DataTable<HostelAllocation>
        caption="Residencies"
        rows={(residents ?? []).filter((r) => r.status !== "closed")}
        rowKey={(r) => r.id}
        empty={<EmptyState icon={Users} title="No residents" compact>Allocate a bed to an approved application.</EmptyState>}
        columns={[
          { key: "person", header: "Resident", render: (r) => <NameCell name={r.personName} sub={r.personNumber} /> },
          { key: "bed", header: "Room / bed", render: (r) => <CodeName name={`Bed ${r.bedCode}`} code={r.roomCode} /> },
          { key: "since", header: "Since", render: (r) => fmtDate(r.startDate), muted: true },
          { key: "fee", header: "Hostel fee", render: (r) => <CodeName name={inr(r.feeAmount)} code={r.chargeNumber} />, muted: true },
          { key: "status", header: "Status", render: (r) => <StatusBadge status={r.status} /> },
          { key: "actions", header: "", render: (r) => <Actions actions={actions(r)} disabled={busy} /> },
        ]}
      />
      {prompt}
      {dialog}
    </BentoTile>
  );
}

function LeaveTile({ inst }: { inst: string }) {
  const api = useResidenceApi();
  const { run, submit, busy, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const leaves = useQuery({ queryKey: rkeys(inst, "leaves"), queryFn: () => api.leaves(inst) }).data;
  const actions = (l: ResidentLeave): RowAction[] => {
    const out: RowAction[] = [];
    if (l.status === "requested") {
      out.push({ id: "leave-approve", label: "Approve", variant: "primary", onClick: () => void run(() => api.decideLeave(l.id, l.version, "approve"), "Leave approved") });
      out.push({ id: "leave-reject", label: "Reject", onClick: () => ask({ title: "Reject leave", name: "leave-reject", fields: [REASON], submitLabel: "Reject", onSubmit: (v) => submit(() => api.decideLeave(l.id, l.version, "reject", v.reason), "Leave rejected") }) });
    }
    if (l.status === "approved") out.push({ id: "leave-depart", label: "Mark out", onClick: () => void run(() => api.leaveCommand(l.id, l.version, "depart"), "Resident is out") });
    if (l.status === "out" || l.status === "overdue") out.push({ id: "leave-return", label: "Verify return", variant: "primary", onClick: () => void run(() => api.leaveCommand(l.id, l.version, "return"), (r) => (r.returnedLate ? "Returned late" : "Returned")) });
    return out;
  };
  return (
    <BentoTile
      span={leaves?.length ? 12 : 4}
      title="Resident leave"
      data-slice="8B"
      action={
        <Button size="sm" data-action="leave-overdue" onClick={() => void run(() => api.overdueCheck(inst), (r) => `${r.processed} resident(s) flagged overdue`)}>
          Check overdue
        </Button>
      }
    >
      <DataTable<ResidentLeave>
        caption="Resident leave"
        rows={leaves ?? []}
        rowKey={(l) => l.id}
        empty={<EmptyState icon={DoorOpen} title="No leave requests" compact>Residents request leave from self-service; the office can request it for them.</EmptyState>}
        columns={[
          { key: "person", header: "Resident", render: (l) => <NameCell name={l.personName} sub={l.personNumber} /> },
          { key: "dates", header: "Dates", render: (l) => `${fmtDate(l.fromDate)} – ${fmtDate(l.toDate)}` },
          { key: "dest", header: "Destination", render: (l) => l.destination ?? "—", muted: true },
          { key: "late", header: "Return", render: (l) => (l.status === "returned" ? (l.returnedLate ? "Late" : "On time") : "—"), muted: true },
          { key: "status", header: "Status", render: (l) => <StatusBadge status={l.status} /> },
          { key: "actions", header: "", render: (l) => <Actions actions={actions(l)} disabled={busy} /> },
        ]}
      />
      {prompt}
      {dialog}
    </BentoTile>
  );
}

function InspectionsTile({ inst }: { inst: string }) {
  const api = useResidenceApi();
  const { submit, busy, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const rows = useQuery({ queryKey: rkeys(inst, "inspections"), queryFn: () => api.inspections(inst) }).data;
  const actions = (x: RoomInspection): RowAction[] => {
    const out: RowAction[] = [];
    if (x.status === "scheduled")
      out.push({
        id: "inspection-complete",
        label: "Record outcome",
        variant: "primary",
        onClick: () =>
          ask({
            title: `Inspection of ${x.roomCode}`,
            name: "inspection-complete",
            fields: [
              { name: "findings", label: "Findings", wide: true },
              { name: "damageAmount", label: "Damage to charge (₹)", placeholder: "0.00" },
            ],
            submitLabel: "Complete",
            onSubmit: (v) => submit(() => api.completeInspection(x.id, x.version, { findings: v.findings || undefined, damageAmount: v.damageAmount || undefined }), (r) => (r.chargeNumber ? `Damage charged (${r.chargeNumber})` : `Inspection ${statusLabel(r.status).toLowerCase()}`)),
          }),
      });
    if (["completed", "damage_found", "charged"].includes(x.status))
      out.push({ id: "inspection-close", label: "Close", onClick: () => ask({ title: `Close the inspection of ${x.roomCode}`, name: "inspection-close", fields: [OPTIONAL_REASON], submitLabel: "Close", onSubmit: (v) => submit(() => api.closeInspection(x.id, x.version, v.reason), "Inspection closed") }) });
    return out;
  };
  return (
    <BentoTile span={rows?.length ? 12 : 4} title="Room inspections" data-slice="8B">
      <DataTable<RoomInspection>
        caption="Room inspections"
        rows={rows ?? []}
        rowKey={(x) => x.id}
        empty={<EmptyState icon={Wrench} title="No inspections" compact>Schedule one from a resident's row.</EmptyState>}
        columns={[
          { key: "room", header: "Room", render: (x) => x.roomCode },
          { key: "resident", header: "Resident", render: (x) => x.personNumber ?? "—", muted: true },
          { key: "on", header: "Scheduled", render: (x) => fmtDate(x.scheduledOn), muted: true },
          { key: "damage", header: "Damage", render: (x) => (x.damageAmount ? <CodeName name={inr(x.damageAmount)} code={x.chargeNumber} /> : "—") },
          { key: "status", header: "Status", render: (x) => <StatusBadge status={x.status} /> },
          { key: "actions", header: "", render: (x) => <Actions actions={actions(x)} disabled={busy} /> },
        ]}
      />
      {prompt}
      {dialog}
    </BentoTile>
  );
}

function MessTile({ inst }: { inst: string }) {
  const api = useResidenceApi();
  const { submit, busy, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const { messes, residents } = useHostel(inst);
  const [selected, setSelected] = useState<string | null>(null);
  const mess = messes?.find((m) => m.id === selected) ?? messes?.[0];
  const assignments = useQuery({ queryKey: rkeys(inst, `mess:${mess?.id ?? ""}`), queryFn: () => api.messAllocations(mess!.id), enabled: !!mess }).data;
  const residentOptions = (residents ?? []).filter((r) => r.status === "active").map((r) => ({ value: r.id, label: `${r.personNumber} · ${r.personName}` }));
  const messOptions = (messes ?? []).map((m) => ({ value: m.id, label: `${m.code} · ${m.name}` }));
  const messActions = (m: Mess): RowAction[] => [
    {
      id: "mess-assign",
      label: "Assign resident",
      onClick: () =>
        ask({
          title: `Assign a resident to ${m.name}`,
          name: "mess-assign",
          fields: [
            { name: "hostelAllocationId", label: "Resident", type: "select", options: residentOptions, required: true, initial: residentOptions[0]?.value },
            { name: "startDate", label: "From", type: "date", initial: todayIso() },
          ],
          submitLabel: "Assign",
          onSubmit: (v) => submit(() => api.assignMess(m.id, { hostelAllocationId: v.hostelAllocationId, startDate: v.startDate || undefined }), "Dining assignment active"),
        }),
    },
    { id: "mess-view", label: "Assignments", onClick: () => setSelected(m.id) },
  ];
  return (
    <BentoTile
      span={messes?.length ? 12 : 4}
      title="Mess"
      data-slice="8B"
      action={
        <Button
          size="sm"
          data-action="mess-new"
          onClick={() =>
            ask({
              title: "New mess",
              name: "mess",
              fields: [
                { name: "code", label: "Code", required: true, placeholder: "MESS-A" },
                { name: "name", label: "Name", required: true, placeholder: "Main mess" },
                { name: "menuType", label: "Menu", type: "select", options: MENU_TYPES, initial: "mixed" },
                { name: "capacity", label: "Capacity", type: "number", required: true, initial: "100" },
              ],
              submitLabel: "Create",
              onSubmit: (v) => submit(() => api.createMess(inst, { code: v.code, name: v.name, menuType: v.menuType, capacity: Number(v.capacity) }), "Mess created"),
            })
          }
        >
          New mess
        </Button>
      }
    >
      <DataTable<Mess>
        caption="Messes"
        rows={messes ?? []}
        rowKey={(m) => m.id}
        empty={<EmptyState icon={Utensils} title="No mess yet" compact>Define a mess, then assign residents to it.</EmptyState>}
        columns={[
          { key: "code", header: "Mess", render: (m) => <CodeName name={m.name} code={m.code} /> },
          { key: "menu", header: "Menu", render: (m) => statusLabel(m.menuType), muted: true },
          { key: "load", header: "Assigned", render: (m) => `${m.assigned} / ${m.capacity}` },
          { key: "status", header: "Status", render: (m) => <StatusBadge status={m.status} /> },
          { key: "actions", header: "", render: (m) => <Actions actions={messActions(m)} disabled={busy} /> },
        ]}
      />
      {mess ? (
        <DataTable
          caption={`Dining assignments · ${mess.code}`}
          rows={assignments ?? []}
          rowKey={(a) => a.id}
          empty={<EmptyState icon={Utensils} title="Nobody assigned" compact>Assign residents from the mess row.</EmptyState>}
          columns={[
            { key: "person", header: "Resident", render: (a) => <NameCell name={a.personName} sub={a.personNumber} /> },
            { key: "from", header: "From", render: (a) => fmtDate(a.startDate), muted: true },
            { key: "to", header: "To", render: (a) => fmtDate(a.endDate), muted: true },
            { key: "status", header: "Status", render: (a) => <StatusBadge status={a.status} /> },
            {
              key: "actions",
              header: "",
              render: (a) =>
                a.status === "active" ? (
                  <Actions
                    disabled={busy}
                    actions={[
                      {
                        id: "mess-change",
                        label: "Change mess",
                        onClick: () =>
                          ask({
                            title: `Move ${a.personName} to another mess`,
                            name: "mess-change",
                            fields: [
                              { name: "messId", label: "New mess", type: "select", options: messOptions.filter((o) => o.value !== a.messId), required: true, initial: messOptions.find((o) => o.value !== a.messId)?.value },
                              { name: "effectiveFrom", label: "From", type: "date", required: true, initial: todayIso() },
                            ],
                            submitLabel: "Change",
                            onSubmit: (v) => submit(() => api.changeMess(a.id, a.version, { messId: v.messId, effectiveFrom: v.effectiveFrom }), "Mess changed"),
                          }),
                      },
                    ]}
                  />
                ) : null,
            },
          ]}
        />
      ) : null}
      {prompt}
      {dialog}
    </BentoTile>
  );
}

/** Admin → Campus → Hostel. */
export function HostelAdminTab({ institution }: { institution: Institution | null }) {
  if (!institution) return <NoInstitutionTile title="Hostel" />;
  const inst = institution.id;
  return (
    <>
      <HostelStats inst={inst} />
      <PoliciesTile inst={inst} />
      <BedsTile inst={inst} />
      <ApplicationsTile inst={inst} />
      <ResidentsTile inst={inst} />
      <LeaveTile inst={inst} />
      <InspectionsTile inst={inst} />
      <MessTile inst={inst} />
    </>
  );
}

// ================================================================== Transport

function useTransport(inst: string) {
  const api = useResidenceApi();
  return {
    routes: useQuery({ queryKey: rkeys(inst, "routes"), queryFn: () => api.routes(inst) }).data,
    stops: useQuery({ queryKey: rkeys(inst, "stops"), queryFn: () => api.stops(inst) }).data,
    vehicles: useQuery({ queryKey: rkeys(inst, "vehicles"), queryFn: () => api.vehicles(inst) }).data,
    passengers: useQuery({ queryKey: rkeys(inst, "passengers"), queryFn: () => api.passengers(inst) }).data,
  };
}

function TransportStats({ inst }: { inst: string }) {
  const api = useResidenceApi();
  const { routes, vehicles, passengers } = useTransport(inst);
  const trips = useQuery({ queryKey: rkeys(inst, "trips"), queryFn: () => api.trips(inst) }).data;
  const open = trips?.filter((t) => ["planned", "ready", "in_progress"].includes(t.status)).length;
  return (
    <>
      <KpiTile icon={Route} tone="orange" label="Active routes" value={routes ? String(routes.filter((r) => r.status === "active").length) : "…"} sub={`${routes?.length ?? 0} in total`} data-kpi="transport-routes" />
      <KpiTile icon={Users} tone="green" label="Passengers" value={passengers ? String(passengers.filter((p) => p.status === "active").length) : "…"} sub={`${passengers?.filter((p) => p.flagged && p.status === "active").length ?? 0} need adjustment`} data-kpi="transport-passengers" />
      <KpiTile icon={Bus} tone="blue" label="Vehicles available" value={vehicles ? String(vehicles.filter((v) => v.status === "available").length) : "…"} sub={`${vehicles?.filter((v) => v.status === "broken_down").length ?? 0} broken down`} data-kpi="transport-vehicles" />
      <KpiTile icon={CalendarDays} tone="violet" label="Open trips" value={open === undefined ? "…" : String(open)} sub="Planned, ready or running" data-kpi="transport-trips" />
    </>
  );
}

function stopsText(v: RouteVersion | undefined): string {
  return (v?.stops ?? []).map((s) => (s.pickupTime ? `${s.code}@${s.pickupTime}` : s.code)).join(", ");
}

/** Readable stop sequence of a route version: "Main gate 07:30 → Station 07:50". */
function stopsLine(v: RouteVersion): string {
  return [...v.stops]
    .sort((a, b) => a.sequence - b.sequence)
    .map((s) => (s.pickupTime ? `${s.name} ${s.pickupTime}` : s.name))
    .join(" → ");
}

function RoutesTile({ inst }: { inst: string }) {
  const api = useResidenceApi();
  const { run, submit, busy, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const { routes, stops } = useTransport(inst);
  const [selected, setSelected] = useState<string | null>(null);
  const route = routes?.find((r) => r.id === selected) ?? routes?.[0];
  const versions = useQuery({ queryKey: rkeys(inst, `versions:${route?.id ?? ""}`), queryFn: () => api.versions(route!.id), enabled: !!route }).data;
  const editStops = (v: RouteVersion) =>
    ask({
      title: `Stops of ${route?.code ?? ""} v${v.versionNo}`,
      name: "route-stops",
      fields: [{ name: "stops", label: "Stop codes in order (CODE or CODE@HH:MM, comma separated)", required: true, wide: true, initial: stopsText(v) }],
      submitLabel: "Save stops",
      onSubmit: (vals) => {
        const list = (vals.stops ?? "")
          .split(",")
          .map((s) => s.trim())
          .filter(Boolean)
          .map((entry, i) => {
            const [code, time] = entry.split("@");
            const stop = (stops ?? []).find((s) => s.code === code?.trim().toUpperCase());
            if (!stop) throw new Error(`Unknown stop ${code ?? entry}`);
            return { stopId: stop.id, sequence: i + 1, pickupTime: time?.trim() || undefined };
          });
        return submit(() => api.setStops(v.id, v.version, list), "Stops saved");
      },
    });
  const actions = (r: TransportRoute): RowAction[] => {
    const out: RowAction[] = [{ id: "route-view", label: "Versions", onClick: () => setSelected(r.id) }];
    if (r.processInstanceId) {
      out.push({ id: "route-approve", label: "Approve activation", variant: "primary", onClick: () => void run(() => api.decideRoute(r.id, r.version, "approve"), "Route active") });
      out.push({ id: "route-reject", label: "Reject", onClick: () => ask({ title: `Reject activation of ${r.code}`, name: "route-reject", fields: [REASON], submitLabel: "Reject", onSubmit: (v) => submit(() => api.decideRoute(r.id, r.version, "reject", v.reason), "Activation rejected") }) });
    } else if ((r.status !== "retired" && r.hasDraft) || r.status === "suspended") {
      out.push({ id: "route-activate", label: r.hasDraft ? "Activate version" : "Restore", variant: "primary", onClick: () => void run(() => api.routeCommand(r.id, r.version, "activate"), (x) => (x.processInstanceId ? "Activation awaits a decision" : "Route active")) });
    }
    if (r.status === "active") out.push({ id: "route-suspend", label: "Suspend", onClick: () => ask({ title: `Suspend ${r.code}`, name: "route-suspend", fields: [REASON], submitLabel: "Suspend", onSubmit: (v) => submit(() => api.routeCommand(r.id, r.version, "suspend", v.reason), "Route suspended, passengers flagged") }) });
    if (r.status !== "retired" && !r.hasDraft)
      out.push({
        id: "route-version",
        label: "New version",
        onClick: () =>
          ask({
            title: `New version of ${r.code}`,
            name: "route-version",
            fields: [
              { name: "effectiveFrom", label: "Effective from", type: "date", required: true, initial: todayIso() },
              { name: "capacity", label: "Seats", type: "number", initial: r.capacity ? String(r.capacity) : "" },
              { name: "fare", label: "Pass fee (₹)", initial: r.fare ?? "" },
              { name: "note", label: "Change note", wide: true },
            ],
            submitLabel: "Draft version",
            onSubmit: (v) => submit(() => api.createVersion(r.id, { effectiveFrom: v.effectiveFrom, capacity: v.capacity ? Number(v.capacity) : undefined, fare: v.fare || undefined, note: v.note || undefined }), "Draft version created (stops copied)"),
          }),
      });
    if (r.status !== "retired") out.push({ id: "route-retire", label: "Retire", onClick: () => ask({ title: `Retire ${r.code}`, name: "route-retire", fields: [OPTIONAL_REASON], submitLabel: "Retire", onSubmit: (v) => submit(() => api.routeCommand(r.id, r.version, "retire", v.reason), "Route retired") }) });
    return out;
  };
  return (
    <BentoTile
      span={12}
      title="Routes"
      data-slice="8B"
      action={
        <Button
          size="sm"
          data-action="route-new"
          onClick={() =>
            ask({
              title: "New transport route",
              name: "route",
              fields: [
                { name: "code", label: "Code", required: true, placeholder: "R-01" },
                { name: "name", label: "Name", required: true, placeholder: "City loop" },
                { name: "capacity", label: "Seats", type: "number", required: true, initial: "40" },
                { name: "fare", label: "Pass fee (₹)", required: true, initial: "0.00" },
                { name: "effectiveFrom", label: "Effective from", type: "date", required: true, initial: todayIso() },
              ],
              submitLabel: "Create draft",
              onSubmit: (v) => submit(() => api.createRoute(inst, { code: v.code, name: v.name, capacity: Number(v.capacity), fare: v.fare, effectiveFrom: v.effectiveFrom }), "Draft route created — add its stops"),
            })
          }
        >
          New route
        </Button>
      }
    >
      <DataTable<TransportRoute>
        caption="Transport routes"
        rows={routes ?? []}
        rowKey={(r) => r.id}
        empty={<EmptyState icon={Route} title="No routes yet" compact>Create a route, give its draft version at least two stops and activate it.</EmptyState>}
        columns={[
          { key: "route", header: "Route", render: (r) => <CodeName name={r.name} code={r.code} /> },
          { key: "version", header: "Version", render: (r) => (r.currentVersionNo ? `v${r.currentVersionNo}${r.hasDraft ? " (+ draft)" : ""}` : r.hasDraft ? "Draft" : "—"), muted: true },
          { key: "seats", header: "Passengers", render: (r) => `${r.passengers} / ${r.capacity ?? "—"}` },
          { key: "fare", header: "Pass fee", render: (r) => (r.fare ? formatInr(r.fare) : "—"), muted: true },
          { key: "status", header: "Status", render: (r) => <StatusBadge status={r.processInstanceId ? "pending_approval" : r.status} /> },
          { key: "actions", header: "", render: (r) => <Actions actions={actions(r)} disabled={busy} /> },
        ]}
      />
      {route ? (
        <DataTable<RouteVersion>
          caption={`Versions · ${route.code}`}
          rows={versions ?? []}
          rowKey={(v) => v.id}
          empty={<EmptyState icon={Route} title="No versions" compact>Every route starts with a draft version.</EmptyState>}
          columns={[
            { key: "no", header: "Version", render: (v) => `v${v.versionNo}` },
            { key: "eff", header: "Effective", render: (v) => `${fmtDate(v.effectiveFrom)}${v.effectiveTo ? ` – ${fmtDate(v.effectiveTo)}` : ""}`, muted: true },
            { key: "stops", header: "Stops", render: (v) => stopsLine(v) || "—" },
            { key: "seats", header: "Seats", render: (v) => String(v.capacity), muted: true },
            { key: "status", header: "Status", render: (v) => <StatusBadge status={v.status} /> },
            { key: "actions", header: "", render: (v) => (v.status === "draft" ? <Actions disabled={busy} actions={[{ id: "route-stops", label: "Edit stops", onClick: () => editStops(v) }]} /> : null) },
          ]}
        />
      ) : null}
      {prompt}
      {dialog}
    </BentoTile>
  );
}

function StopsTile({ inst }: { inst: string }) {
  const api = useResidenceApi();
  const { run, submit, busy, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const { stops } = useTransport(inst);
  const actions = (s: TransportStop): RowAction[] => {
    if (s.status === "active") return [{ id: "stop-suspend", label: "Suspend", onClick: () => ask({ title: `Suspend ${s.code}`, name: "stop-suspend", fields: [REASON], submitLabel: "Suspend", onSubmit: (v) => submit(() => api.stopCommand(s.id, s.version, "suspend", v.reason), "Stop suspended, passengers flagged") }) }];
    if (s.status === "suspended") return [{ id: "stop-restore", label: "Restore", onClick: () => void run(() => api.stopCommand(s.id, s.version, "restore"), "Stop restored") }];
    return [];
  };
  return (
    <BentoTile
      span={6}
      title="Stops"
      data-slice="8B"
      action={
        <Button
          size="sm"
          data-action="stop-new"
          onClick={() =>
            ask({
              title: "New stop",
              name: "stop",
              fields: [
                { name: "code", label: "Code", required: true, placeholder: "ST-GATE" },
                { name: "name", label: "Name", required: true, placeholder: "Main gate" },
                { name: "landmark", label: "Landmark", wide: true },
              ],
              submitLabel: "Create",
              onSubmit: (v) => submit(() => api.createStop(inst, { code: v.code, name: v.name, landmark: v.landmark || undefined }), "Stop created"),
            })
          }
        >
          New stop
        </Button>
      }
    >
      <DataTable<TransportStop>
        caption="Stops"
        rows={stops ?? []}
        rowKey={(s) => s.id}
        empty={<EmptyState icon={MapPin} title="No stops yet" compact>Create the pickup and drop points first.</EmptyState>}
        columns={[
          { key: "stop", header: "Stop", render: (s) => <CodeName name={s.name} code={s.code} /> },
          { key: "pax", header: "Boarding", render: (s) => String(s.passengers), muted: true },
          { key: "status", header: "Status", render: (s) => <StatusBadge status={s.status} /> },
          { key: "actions", header: "", render: (s) => <Actions actions={actions(s)} disabled={busy} /> },
        ]}
      />
      {prompt}
      {dialog}
    </BentoTile>
  );
}

function VehiclesTile({ inst }: { inst: string }) {
  const api = useResidenceApi();
  const { submit, busy, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const { vehicles } = useTransport(inst);
  const actions = (v: Vehicle): RowAction[] => {
    const out: RowAction[] = [];
    if (v.status !== "retired" && v.status !== "assigned")
      out.push({
        id: "vehicle-status",
        label: "Availability",
        onClick: () =>
          ask({
            title: `Availability of ${v.registrationNo}`,
            name: "vehicle-status",
            fields: [{ name: "status", label: "Status", type: "select", options: VEHICLE_STATES.filter((s) => s.value !== v.status), initial: VEHICLE_STATES.find((s) => s.value !== v.status)?.value }, OPTIONAL_REASON],
            submitLabel: "Update",
            onSubmit: (x) => submit(() => api.vehicleStatus(v.id, v.version, x.status!, x.reason), "Vehicle updated"),
          }),
      });
    if (v.status === "available" || v.status === "assigned")
      out.push({ id: "vehicle-breakdown", label: "Breakdown", onClick: () => ask({ title: `Breakdown of ${v.registrationNo}`, name: "vehicle-breakdown", fields: [REASON], submitLabel: "Report", onSubmit: (x) => submit(() => api.breakdown(v.id, v.version, x.reason), "Breakdown recorded — replace the vehicle on its trips") }) });
    return out;
  };
  return (
    <BentoTile
      span={6}
      title="Vehicles"
      data-slice="8B"
      action={
        <Button
          size="sm"
          data-action="vehicle-new"
          onClick={() =>
            ask({
              title: "Register a vehicle",
              name: "vehicle",
              fields: [
                { name: "registrationNo", label: "Registration no.", required: true, placeholder: "KA01AB1234" },
                { name: "label", label: "Label", required: true, placeholder: "Bus 1" },
                { name: "capacity", label: "Seats", type: "number", required: true, initial: "40" },
              ],
              submitLabel: "Register",
              onSubmit: (x) => submit(() => api.createVehicle(inst, { registrationNo: x.registrationNo, label: x.label, capacity: Number(x.capacity) }), "Vehicle registered"),
            })
          }
        >
          New vehicle
        </Button>
      }
    >
      <DataTable<Vehicle>
        caption="Vehicles"
        rows={vehicles ?? []}
        rowKey={(v) => v.id}
        empty={<EmptyState icon={Truck} title="No vehicles yet" compact>Register the fleet; trips need an available vehicle.</EmptyState>}
        columns={[
          { key: "reg", header: "Vehicle", render: (v) => `${v.registrationNo} · ${v.label}` },
          { key: "seats", header: "Seats", render: (v) => String(v.capacity), muted: true },
          { key: "status", header: "Status", render: (v) => <StatusBadge status={v.status} /> },
          { key: "actions", header: "", render: (v) => <Actions actions={actions(v)} disabled={busy} /> },
        ]}
      />
      {prompt}
      {dialog}
    </BentoTile>
  );
}

function DriversTile({ inst }: { inst: string }) {
  const api = useResidenceApi();
  const { submit, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const { routes } = useTransport(inst);
  const { people: staff } = usePeople(inst, ["staff"]); // 9E
  const [selected, setSelected] = useState<string | null>(null);
  const route = routes?.find((r) => r.id === selected) ?? routes?.[0];
  const drivers = useQuery({ queryKey: rkeys(inst, `drivers:${route?.id ?? ""}`), queryFn: () => api.drivers(route!.id), enabled: !!route }).data;
  return (
    <BentoTile
      span={12}
      title="Drivers"
      data-slice="8B"
      action={
        <Button
          size="sm"
          data-action="driver-new"
          disabled={!route}
          onClick={() =>
            route &&
            ask({
              title: `Assign a driver to ${route.code}`,
              name: "driver",
              fields: [
                personField(staff, { name: "staffId", label: "Driver (staff member)", kind: "staff" }),
                { name: "licenceNo", label: "Licence no.", required: true },
                { name: "validFrom", label: "Valid from", type: "date", required: true, initial: todayIso() },
              ],
              submitLabel: "Assign",
              onSubmit: (v) => submit(() => api.assignDriver(route.id, { staffId: v.staffId?.trim(), licenceNo: v.licenceNo, validFrom: v.validFrom }), "Driver assigned"),
            })
          }
        >
          Assign driver
        </Button>
      }
    >
      <div>
        {(routes ?? []).map((r) => (
          <FilterChip key={r.id} active={route?.id === r.id} label={r.code} onClick={() => setSelected(r.id)} />
        ))}
      </div>
      <DataTable
        caption={route ? `Drivers · ${route.code}` : "Drivers"}
        rows={drivers ?? []}
        rowKey={(d) => d.id}
        empty={<EmptyState icon={Users} title="No drivers" compact>Assign staff members with a valid licence to the route.</EmptyState>}
        columns={[
          { key: "driver", header: "Driver", render: (d) => <NameCell name={d.driverName} sub={d.staffNumber} /> },
          { key: "licence", header: "Licence", render: (d) => d.licenceNo, muted: true },
          { key: "valid", header: "Valid", render: (d) => `${fmtDate(d.validFrom)}${d.validTo ? ` – ${fmtDate(d.validTo)}` : ""}`, muted: true },
          { key: "status", header: "Status", render: (d) => <StatusBadge status={d.status} /> },
        ]}
      />
      {prompt}
      {dialog}
    </BentoTile>
  );
}

function PassengersTile({ inst }: { inst: string }) {
  const api = useResidenceApi();
  const { run, submit, busy, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const { passengers, routes, stops } = useTransport(inst);
  const { people } = usePeople(inst); // 9E
  const routeOptions = (routes ?? []).filter((r) => r.status === "active").map((r) => ({ value: r.id, label: `${r.code} · ${r.name}` }));
  const stopOptions = (stops ?? []).filter((s) => s.status === "active").map((s) => ({ value: s.id, label: `${s.code} · ${s.name}` }));
  const actions = (p: TransportAllocation): RowAction[] => {
    const out: RowAction[] = [];
    if (p.status === "requested") out.push({ id: "passenger-activate", label: "Activate", variant: "primary", onClick: () => void run(() => api.passengerCommand(p.id, p.version, "activate"), "Seat allocated") });
    if (p.status === "active") {
      if (!p.passId)
        out.push({
          id: "pass-issue",
          label: "Issue pass",
          variant: "primary",
          onClick: () =>
            ask({
              title: `Transport pass for ${p.personName}`,
              name: "pass-issue",
              fields: [
                { name: "validFrom", label: "Valid from", type: "date", required: true, initial: todayIso() },
                { name: "validTo", label: "Valid to", type: "date", required: true },
              ],
              submitLabel: "Issue",
              onSubmit: (v) => submit(() => api.issuePass(p.id, { validFrom: v.validFrom, validTo: v.validTo }), (x) => `Pass ${x.number} issued${x.chargeNumber ? `, fee ${x.chargeNumber}` : ""}`),
            }),
        });
      out.push({
        id: "passenger-change",
        label: "Change stop",
        onClick: () =>
          ask({
            title: `Change the stop of ${p.personName}`,
            name: "passenger-change",
            fields: [
              { name: "routeId", label: "Route", type: "select", options: routeOptions, initial: p.routeId },
              { name: "stopId", label: "Stop", type: "select", options: stopOptions, required: true, initial: stopOptions.find((s) => s.value !== p.stopId)?.value },
              { name: "effectiveFrom", label: "From", type: "date", required: true, initial: todayIso() },
            ],
            submitLabel: "Change",
            onSubmit: (v) => submit(() => api.changePassenger(p.id, p.version, { routeId: v.routeId || undefined, stopId: v.stopId, effectiveFrom: v.effectiveFrom }), "Allocation changed"),
          }),
      });
    }
    if (p.status === "requested" || p.status === "active")
      out.push({ id: "passenger-withdraw", label: "Withdraw", onClick: () => ask({ title: `Withdraw ${p.personName}`, name: "passenger-withdraw", fields: [OPTIONAL_REASON], submitLabel: "Withdraw", onSubmit: (v) => submit(() => api.passengerCommand(p.id, p.version, "withdraw", v.reason), "Passenger withdrawn") }) });
    return out;
  };
  return (
    <BentoTile
      span={12}
      title="Passenger allocations"
      data-slice="8B"
      action={
        <Button
          size="sm"
          data-action="passenger-new"
          disabled={routeOptions.length === 0}
          onClick={() =>
            ask({
              title: "Allocate a passenger",
              name: "passenger",
              fields: [
                ...personFields(people),
                { name: "routeId", label: "Route", type: "select", options: routeOptions, required: true, initial: routeOptions[0]?.value },
                { name: "stopId", label: "Boarding stop", type: "select", options: stopOptions, required: true, initial: stopOptions[0]?.value },
              ],
              submitLabel: "Allocate",
              onSubmit: (v) => submit(() => api.allocatePassenger(inst, { personKind: kindOf(people, v.personId, v.personKind), personId: v.personId?.trim(), routeId: v.routeId, stopId: v.stopId }), (r) => (r.status === "active" ? "Seat allocated" : "Route full — request waiting for a seat")),
            })
          }
        >
          Allocate passenger
        </Button>
      }
    >
      <DataTable<TransportAllocation>
        caption="Passenger allocations"
        rows={(passengers ?? []).filter((p) => p.status === "requested" || p.status === "active")}
        rowKey={(p) => p.id}
        empty={<EmptyState icon={Users} title="No passengers" compact>Students and staff request a seat from self-service; the office can allocate them here.</EmptyState>}
        columns={[
          { key: "person", header: "Passenger", render: (p) => <NameCell name={p.personName} sub={p.personNumber} /> },
          { key: "route", header: "Route / stop", render: (p) => <CodeName name={p.stopName} code={p.routeCode} /> },
          { key: "pass", header: "Pass", render: (p) => p.passNumber ?? "—", muted: true },
          { key: "flag", header: "Adjustment", render: (p) => (p.flagged ? statusLabel(p.flagReason ?? "flagged") : "—"), muted: true },
          { key: "status", header: "Status", render: (p) => <StatusBadge status={p.status} /> },
          { key: "actions", header: "", render: (p) => <Actions actions={actions(p)} disabled={busy} /> },
        ]}
      />
      {prompt}
      {dialog}
    </BentoTile>
  );
}

function PassesTile({ inst }: { inst: string }) {
  const api = useResidenceApi();
  const { run, submit, busy, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const passes = useQuery({ queryKey: rkeys(inst, "passes"), queryFn: () => api.passes(inst) }).data;
  const actions = (x: TransportPass): RowAction[] => {
    const out: RowAction[] = [];
    if (x.status === "issued" || x.status === "active") {
      out.push({ id: "pass-renew", label: "Renew", onClick: () => ask({ title: `Renew ${x.number}`, name: "pass-renew", fields: [{ name: "validTo", label: "New end date", type: "date", required: true }], submitLabel: "Renew", onSubmit: (v) => submit(() => api.renewPass(x.id, x.version, v.validTo!), (r) => `Renewal pass ${r.number} issued`) }) });
      out.push({ id: "pass-suspend", label: "Suspend", onClick: () => ask({ title: `Suspend ${x.number}`, name: "pass-suspend", fields: [REASON], submitLabel: "Suspend", onSubmit: (v) => submit(() => api.passCommand(x.id, x.version, "suspend", v.reason), "Pass suspended") }) });
    }
    if (x.status === "suspended") out.push({ id: "pass-restore", label: "Restore", variant: "primary", onClick: () => void run(() => api.passCommand(x.id, x.version, "restore"), "Pass restored") });
    if (["issued", "active", "suspended"].includes(x.status))
      out.push({
        id: "pass-replace",
        label: "Replace",
        onClick: () =>
          ask({
            title: `Replace ${x.number} (lost / damaged)`,
            name: "pass-replace",
            fields: [REASON, { name: "fee", label: "Replacement fee (₹)", placeholder: "0.00" }],
            submitLabel: "Replace",
            onSubmit: (v) => submit(() => api.replacePass(x.id, x.version, v.reason!, v.fee || undefined), (r) => `Replacement pass ${r.number} issued`),
          }),
      });
    return out;
  };
  return (
    <BentoTile span={passes?.length ? 12 : 6} title="Transport passes" data-slice="8B">
      <DataTable<TransportPass>
        caption="Transport passes"
        rows={passes ?? []}
        rowKey={(x) => x.id}
        empty={<EmptyState icon={Ticket} title="No passes" compact>Issue a pass from an active passenger allocation; students are charged the route's pass fee.</EmptyState>}
        columns={[
          { key: "number", header: "Pass", render: (x) => <CodeName name={`${statusLabel(x.kind)} pass`} code={x.number} /> },
          { key: "person", header: "Passenger", render: (x) => <NameCell name={x.personName} sub={x.personNumber} /> },
          { key: "valid", header: "Valid", render: (x) => `${fmtDate(x.validFrom)} – ${fmtDate(x.validTo)}`, muted: true },
          { key: "fee", header: "Fee", render: (x) => <CodeName name={inr(x.feeAmount)} code={x.chargeNumber} />, muted: true },
          { key: "status", header: "Status", render: (x) => <StatusBadge status={x.status} /> },
          { key: "actions", header: "", render: (x) => <Actions actions={actions(x)} disabled={busy} /> },
        ]}
      />
      {prompt}
      {dialog}
    </BentoTile>
  );
}

function TripsTile({ inst }: { inst: string }) {
  const api = useResidenceApi();
  const { run, submit, busy, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const { routes, vehicles } = useTransport(inst);
  const trips = useQuery({ queryKey: rkeys(inst, "trips"), queryFn: () => api.trips(inst) }).data;
  const routeOptions = (routes ?? []).filter((r) => r.status === "active").map((r) => ({ value: r.id, label: `${r.code} · ${r.name}` }));
  const vehicleOptions = (vehicles ?? []).filter((v) => v.status === "available" || v.status === "assigned").map((v) => ({ value: v.id, label: `${v.registrationNo} · ${v.capacity} seats` }));
  const readyTrip = async (t: Trip) => {
    const drivers = await api.drivers(t.routeId);
    const driverOptions = drivers.filter((d) => d.status === "active").map((d) => ({ value: d.id, label: `${d.staffNumber} · ${d.driverName}` }));
    ask({
      title: `Make ${t.routeCode} ${fmtDate(t.tripDate)} ready`,
      name: "trip-ready",
      fields: [
        { name: "vehicleId", label: "Vehicle", type: "select", options: vehicleOptions, required: true, initial: t.vehicleId ?? vehicleOptions[0]?.value },
        { name: "driverAssignmentId", label: "Driver", type: "select", options: driverOptions, required: true, initial: t.driverAssignmentId ?? driverOptions[0]?.value },
        { name: "brakes", label: "Brakes checked", type: "checkbox" },
        { name: "tyres", label: "Tyres checked", type: "checkbox" },
        { name: "firstAid", label: "First-aid kit on board", type: "checkbox" },
      ],
      submitLabel: "Ready",
      onSubmit: (v) => submit(() => api.readyTrip(t.id, t.version, { vehicleId: v.vehicleId, driverAssignmentId: v.driverAssignmentId, checklist: { brakes: v.brakes === "true", tyres: v.tyres === "true", firstAid: v.firstAid === "true" } }), "Trip ready"),
    });
  };
  const actions = (t: Trip): RowAction[] => {
    const out: RowAction[] = [];
    if (t.status === "planned") out.push({ id: "trip-ready", label: "Make ready", variant: "primary", onClick: () => void readyTrip(t) });
    if (t.status === "ready" && !t.needsVehicle) out.push({ id: "trip-start", label: "Start", variant: "primary", onClick: () => void run(() => api.tripCommand(t.id, t.version, "start"), "Trip running") });
    if (t.status === "in_progress") out.push({ id: "trip-complete", label: "Complete", variant: "primary", onClick: () => ask({ title: `Complete ${t.routeCode} trip`, name: "trip-complete", fields: [{ name: "passengerCount", label: "Passengers carried", type: "number", initial: String(t.allocatedPassengers) }], submitLabel: "Complete", onSubmit: (v) => submit(() => api.tripCommand(t.id, t.version, "complete", { passengerCount: v.passengerCount ? Number(v.passengerCount) : undefined }), "Trip completed") }) });
    if (["planned", "ready", "in_progress"].includes(t.status) && (t.needsVehicle || t.status !== "in_progress"))
      out.push({ id: "trip-replace", label: "Replace vehicle", onClick: () => ask({ title: `Replacement vehicle for ${t.routeCode}`, name: "trip-replace", fields: [{ name: "vehicleId", label: "Vehicle", type: "select", options: vehicleOptions.filter((o) => o.value !== t.vehicleId), required: true, initial: vehicleOptions.find((o) => o.value !== t.vehicleId)?.value }, OPTIONAL_REASON], submitLabel: "Replace", onSubmit: (v) => submit(() => api.replaceVehicle(t.id, t.version, v.vehicleId!, v.reason), "Vehicle replaced") }) });
    if (t.status === "planned" || t.status === "ready") out.push({ id: "trip-cancel", label: "Cancel", onClick: () => ask({ title: `Cancel ${t.routeCode} trip`, name: "trip-cancel", fields: [REASON], submitLabel: "Cancel trip", onSubmit: (v) => submit(() => api.tripCommand(t.id, t.version, "cancel", { reason: v.reason }), "Trip cancelled") }) });
    return out;
  };
  return (
    <BentoTile
      span={trips?.length ? 12 : 6}
      title="Trips"
      data-slice="8B"
      action={
        <Button
          size="sm"
          data-action="trip-new"
          disabled={routeOptions.length === 0}
          onClick={() =>
            ask({
              title: "Plan a trip",
              name: "trip",
              fields: [
                { name: "routeId", label: "Route", type: "select", options: routeOptions, required: true, initial: routeOptions[0]?.value },
                { name: "tripDate", label: "Date", type: "date", required: true, initial: todayIso() },
                { name: "direction", label: "Direction", type: "select", options: DIRECTIONS, initial: "pickup" },
              ],
              submitLabel: "Plan",
              onSubmit: (v) => submit(() => api.planTrip(v.routeId!, { tripDate: v.tripDate, direction: v.direction }), "Trip planned"),
            })
          }
        >
          Plan trip
        </Button>
      }
    >
      <DataTable<Trip>
        caption="Trips"
        rows={trips ?? []}
        rowKey={(t) => t.id}
        empty={<EmptyState icon={Bus} title="No trips" compact>Plan trips of active routes; a trip is ready once it has a vehicle, a driver and a complete checklist.</EmptyState>}
        columns={[
          { key: "route", header: "Trip", render: (t) => `${t.routeCode} · ${fmtDate(t.tripDate)} · ${statusLabel(t.direction)}` },
          { key: "vehicle", header: "Vehicle", render: (t) => (t.needsVehicle ? `${t.vehicleRegistration ?? "—"} (broken down)` : (t.vehicleRegistration ?? "—")) },
          { key: "driver", header: "Driver", render: (t) => t.driverName ?? "—", muted: true },
          { key: "pax", header: "Passengers", render: (t) => (t.passengerCount !== null && t.passengerCount !== undefined ? `${t.passengerCount} / ${t.allocatedPassengers}` : String(t.allocatedPassengers)), muted: true },
          { key: "status", header: "Status", render: (t) => <StatusBadge status={t.status} /> },
          { key: "actions", header: "", render: (t) => <Actions actions={actions(t)} disabled={busy} /> },
        ]}
      />
      {prompt}
      {dialog}
    </BentoTile>
  );
}

/** Admin → Campus → Transport. */
export function TransportAdminTab({ institution }: { institution: Institution | null }) {
  if (!institution) return <NoInstitutionTile title="Transport" />;
  const inst = institution.id;
  return (
    <>
      <TransportStats inst={inst} />
      <RoutesTile inst={inst} />
      <StopsTile inst={inst} />
      <VehiclesTile inst={inst} />
      <FleetMaintenanceTile institutionId={inst} />
      <DriversTile inst={inst} />
      <PassengersTile inst={inst} />
      <PassesTile inst={inst} />
      <TripsTile inst={inst} />
    </>
  );
}
