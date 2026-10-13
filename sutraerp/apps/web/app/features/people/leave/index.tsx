/**
 * Slice **7B** web components (leave, attendance & workload). The export names are imported by
 * the foundation frames `routes/admin/people.tsx` and `routes/faculty/leave.tsx`
 * (docs/batch7-contracts.md §10); every screen carries `data-slice="7B"`. Only @sutra/ui
 * components — no custom styling. Leave days and hours stay the decimal strings the API sends.
 *
 * - `LeaveAdminTab` — Admin → People → Leave: policy versions and leave types, the request queue
 *   (decide, consume, cancel), accrual and year-end runs, balances with adjustments and
 *   encashment, leave exceptions.
 * - `AttendanceAdminTab` — Admin → People → Attendance & workload: the daily attendance grid
 *   (bulk marking, corrections), long leave, faculty workload plans.
 * - `MyLeavePanel` — Faculty `/leave` (My HR): balances, apply / cancel, my requests and the
 *   approvals waiting for me as a reporting manager.
 */
import { AlertTriangle, CalendarCheck, CalendarDays, ClipboardList, Clock, HeartPulse, Hourglass, Palmtree, Plane, Scale, UserCheck } from "lucide-react";
import { BentoTile, Button, DataTable, EmptyState, FilterChip, Input, Select, StatTile, type Tone } from "@sutra/ui";
import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { Actions, fmtDate, REASON, todayIso, useAsk, type FieldSpec, type RowAction } from "../../institution/forms";
import { StatusBadge, statusLabel, useCommand } from "../../institution/hooks";
import type { Institution } from "../../institution/types";
import { lkeys, useLeaveApi } from "./hooks";
import type { AttendanceGridRow, LeaveBalance, LeavePolicy, LeaveRequest, LeaveType, StaffRow } from "./types";

export { LeaveApiProvider } from "./hooks";

const ACCRUAL = [
  { value: "yearly", label: "Yearly (opening entitlement)" },
  { value: "monthly", label: "Monthly accrual" },
  { value: "none", label: "No accrual" },
];
const EXCEPTION_CATEGORIES = [
  { value: "balance_shortfall", label: "Balance shortfall" },
  { value: "policy_conflict", label: "Policy conflict" },
  { value: "attendance_mismatch", label: "Attendance mismatch" },
  { value: "backdated_request", label: "Backdated request" },
  { value: "other", label: "Other" },
];
const DAY_STATUSES = [
  { value: "present", label: "Present" },
  { value: "absent", label: "Absent" },
  { value: "half_day", label: "Half day" },
  { value: "on_duty", label: "On duty" },
  { value: "holiday", label: "Holiday" },
  { value: "weekly_off", label: "Weekly off" },
  { value: "on_leave", label: "On leave" },
];
const AVAILABILITY = [
  { value: "available", label: "Available" },
  { value: "partially_available", label: "Partially available" },
  { value: "unavailable", label: "Unavailable" },
];
const REQUEST_FILTERS = ["pending_approval", "reserved", "consumed", ""] as const;

const currentYear = () => Number(todayIso().slice(0, 4));
const typeOptions = (types: LeaveType[]) => types.map((t) => ({ value: t.id, label: `${t.code} · ${t.name}` }));
const staffOptions = (staff: StaffRow[]) => staff.map((s) => ({ value: s.id, label: `${s.staffNumber} · ${s.displayName}` }));
const dates = (r: LeaveRequest) => (r.startDate === r.endDate ? fmtDate(r.startDate) : `${fmtDate(r.startDate)} – ${fmtDate(r.endDate)}`);

function NoInstitutionTile({ title }: { title: string }) {
  return (
    <BentoTile span={12} title={title} data-slice="7B">
      <EmptyState icon={CalendarDays} title="No institution selected">
        Register an institution in Campus setup → Institution first.
      </EmptyState>
    </BentoTile>
  );
}

function useActiveTypes(institutionId: string) {
  const api = useLeaveApi();
  const policies = useQuery({ queryKey: lkeys(institutionId, "policies"), queryFn: () => api.policies(institutionId) });
  return (policies.data ?? []).find((p) => p.status === "active")?.leaveTypes ?? [];
}

function useStaff(institutionId: string) {
  const api = useLeaveApi();
  return useQuery({ queryKey: lkeys(institutionId, "staff"), queryFn: () => api.staff(institutionId) }).data ?? [];
}

/** A leave type without an entitlement (e.g. leave without pay): "available" is meaningless (negative). */
function noEntitlement(b: LeaveBalance): boolean {
  return /^0(\.0+)?$/.test(b.entitled) && b.available.startsWith("-");
}

function BalanceTable({ rows }: { rows: LeaveBalance[] }) {
  return (
    <DataTable<LeaveBalance>
      caption="Leave balances"
      rows={rows}
      rowKey={(b) => `${b.leaveTypeId}:${b.leaveYear}`}
      empty={<EmptyState icon={Scale} title="No balances yet">Entitlements appear after the accrual run.</EmptyState>}
      columns={[
        { key: "type", header: "Leave type", render: (b) => b.leaveTypeCode },
        { key: "entitled", header: "Entitled", render: (b) => b.entitled },
        { key: "reserved", header: "Reserved", render: (b) => b.reserved, muted: true },
        { key: "consumed", header: "Taken", render: (b) => b.consumed, muted: true },
        { key: "available", header: "Available", render: (b) => (noEntitlement(b) ? "—" : b.available) },
      ]}
    />
  );
}

// ================================================================== Admin → People → Leave

function PoliciesTile({ institutionId }: { institutionId: string }) {
  const api = useLeaveApi();
  const { run, submit, busy, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const policies = useQuery({ queryKey: lkeys(institutionId, "policies"), queryFn: () => api.policies(institutionId) });
  const typeFields: FieldSpec[] = [
    { name: "code", label: "Code", required: true, placeholder: "CL" },
    { name: "name", label: "Name", required: true, placeholder: "Casual leave" },
    { name: "annualEntitlement", label: "Days per year", placeholder: "12.00" },
    { name: "accrualFrequency", label: "Accrual", type: "select", options: ACCRUAL, initial: "yearly" },
    { name: "carryForwardCap", label: "Carry forward cap", placeholder: "5.00" },
    { name: "encashmentCap", label: "Encashment cap", placeholder: "0.00" },
    { name: "isPaid", label: "Paid leave", type: "checkbox", initial: "true" },
    { name: "requiresBalance", label: "Needs a balance", type: "checkbox", initial: "true" },
  ];
  const actions = (p: LeavePolicy): RowAction[] => {
    const a: RowAction[] = [];
    if (p.status === "draft") {
      a.push({
        id: "policy-type",
        label: "Add leave type",
        onClick: () =>
          ask({
            title: `Add a leave type to ${p.name}`,
            name: "leave-type",
            fields: typeFields,
            submitLabel: "Add",
            onSubmit: (v) =>
              submit(
                () =>
                  api.addType(p.id, {
                    code: v.code!.trim().toUpperCase(),
                    name: v.name!,
                    annualEntitlement: v.annualEntitlement || undefined,
                    accrualFrequency: v.accrualFrequency,
                    carryForwardCap: v.carryForwardCap || undefined,
                    encashmentCap: v.encashmentCap || undefined,
                    isPaid: v.isPaid === "true",
                    requiresBalance: v.requiresBalance === "true",
                  }),
                "Leave type added",
              ),
          }),
      });
      a.push({ id: "policy-submit", label: "Submit for approval", variant: "primary", onClick: () => void run(() => api.policyCommand(p.id, p.version, "submit"), (r) => `Policy ${statusLabel(r.status).toLowerCase()}`) });
    }
    if (p.status === "under_review") {
      a.push({ id: "policy-approve", label: "Approve", variant: "primary", onClick: () => void run(() => api.decidePolicy(p.id, p.version, true), "Policy approved") });
      a.push({ id: "policy-reject", label: "Reject", onClick: () => ask({ title: `Reject ${p.name}`, name: "policy-reject", fields: [REASON], submitLabel: "Reject", onSubmit: (v) => submit(() => api.decidePolicy(p.id, p.version, false, v.reason), "Policy returned to draft") }) });
    }
    if (p.status === "approved") a.push({ id: "policy-activate", label: "Activate", variant: "primary", onClick: () => void run(() => api.policyCommand(p.id, p.version, "activate"), "Policy activated") });
    return a;
  };
  return (
    <BentoTile
      span={12}
      title="Leave policies"
      data-slice="7B"
      action={
        <Button
          size="sm"
          data-action="policy-new"
          onClick={() =>
            ask({
              title: "New leave policy version",
              name: "leave-policy",
              fields: [
                { name: "name", label: "Name", required: true, placeholder: "Leave policy 2026" },
                { name: "effectiveFrom", label: "Effective from", type: "date", required: true, initial: `${currentYear()}-01-01` },
              ],
              submitLabel: "Create draft",
              onSubmit: (v) => submit(() => api.createPolicy(institutionId, { name: v.name!, effectiveFrom: v.effectiveFrom! }), "Draft policy created"),
            })
          }
        >
          New policy
        </Button>
      }
    >
      <DataTable<LeavePolicy>
        caption="Leave policies"
        rows={policies.data ?? []}
        rowKey={(p) => p.id}
        empty={<EmptyState icon={ClipboardList} title="No leave policy yet">Create a draft, add leave types, submit it for approval and activate it.</EmptyState>}
        columns={[
          { key: "name", header: "Policy", render: (p) => `${p.name} · v${p.versionNo}` },
          { key: "from", header: "Effective", render: (p) => fmtDate(p.effectiveFrom), muted: true },
          { key: "types", header: "Leave types", render: (p) => (p.leaveTypes.length ? p.leaveTypes.map((t) => t.code).join(", ") : "—") },
          { key: "findings", header: "Findings", render: (p) => (p.validationFindings.length ? p.validationFindings.join("; ") : "—"), muted: true },
          { key: "status", header: "Status", render: (p) => <StatusBadge status={p.status} /> },
          { key: "actions", header: "", render: (p) => <Actions actions={actions(p)} disabled={busy} /> },
        ]}
      />
      {prompt}
      {dialog}
    </BentoTile>
  );
}

function RequestsTile({ institutionId }: { institutionId: string }) {
  const api = useLeaveApi();
  const { run, submit, busy, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const [filter, setFilter] = useState<string>("pending_approval");
  const types = useActiveTypes(institutionId);
  const staff = useStaff(institutionId);
  const requests = useQuery({ queryKey: lkeys(institutionId, `requests:${filter}`), queryFn: () => api.requests(institutionId, filter || undefined) });
  const actions = (r: LeaveRequest): RowAction[] => {
    const a: RowAction[] = [];
    if (r.status === "pending_approval") {
      a.push({ id: "request-approve", label: "Approve", variant: "primary", onClick: () => void run(() => api.decideRequest(r.id, r.version, true), `${r.number} approved`) });
      a.push({ id: "request-reject", label: "Reject", onClick: () => ask({ title: `Reject ${r.number}`, name: "request-reject", fields: [REASON], submitLabel: "Reject", onSubmit: (v) => submit(() => api.decideRequest(r.id, r.version, false, v.reason), `${r.number} rejected`) }) });
    }
    if (r.status === "reserved") a.push({ id: "request-consume", label: "Consume", onClick: () => void run(() => api.consumeRequest(r.id, r.version), `${r.number} consumed`) });
    if (["pending_approval", "approved", "reserved", "consumed"].includes(r.status))
      a.push({ id: "request-cancel", label: "Cancel", onClick: () => ask({ title: `Cancel ${r.number}`, name: "request-cancel", fields: [REASON], submitLabel: "Cancel leave", onSubmit: (v) => submit(() => api.cancelRequest(r.id, r.version, v.reason), `${r.number} cancelled`) }) });
    return a;
  };
  return (
    <BentoTile
      span={12}
      title="Leave requests"
      action={
        <Actions
          disabled={busy}
          actions={[
            {
              id: "request-new",
              label: "Request on behalf",
              onClick: () =>
                ask({
                  title: "Request leave on behalf of a staff member",
                  name: "request-new",
                  fields: [
                    { name: "staffId", label: "Staff member", type: "select", required: true, options: staffOptions(staff), initial: staff[0]?.id ?? "" },
                    { name: "leaveTypeId", label: "Leave type", type: "select", required: true, options: typeOptions(types), initial: types[0]?.id ?? "" },
                    { name: "startDate", label: "From", type: "date", required: true, initial: todayIso() },
                    { name: "endDate", label: "To", type: "date", required: true, initial: todayIso() },
                    { name: "reason", label: "Reason", wide: true },
                  ],
                  submitLabel: "Submit",
                  onSubmit: (v) => submit(() => api.requestLeave(institutionId, { staffId: v.staffId!, leaveTypeId: v.leaveTypeId!, startDate: v.startDate!, endDate: v.endDate!, reason: v.reason || undefined }), (r) => `${r.number} ${statusLabel(r.status).toLowerCase()}`),
                }),
            },
            { id: "request-sweep", label: "Consume started leave", onClick: () => void run(() => api.sweep(institutionId), (s) => `${s.processed} consumed, ${s.skipped} not started yet`) },
          ]}
        />
      }
    >
      {REQUEST_FILTERS.map((f) => (
        <FilterChip key={f || "all"} active={filter === f} label={f ? statusLabel(f) : "All"} onClick={() => setFilter(f)} />
      ))}
      <DataTable<LeaveRequest>
        caption="Leave requests"
        rows={requests.data?.items ?? []}
        rowKey={(r) => r.id}
        empty={<EmptyState icon={CalendarCheck} title="Nothing here">No leave requests with this status.</EmptyState>}
        columns={[
          { key: "number", header: "Request", render: (r) => r.number },
          { key: "staff", header: "Staff", render: (r) => `${r.staffNumber} · ${r.displayName}` },
          { key: "type", header: "Type", render: (r) => r.leaveTypeCode, muted: true },
          { key: "dates", header: "Dates", render: dates },
          { key: "days", header: "Days", render: (r) => r.days },
          { key: "status", header: "Status", render: (r) => <StatusBadge status={r.status} /> },
          { key: "actions", header: "", render: (r) => <Actions actions={actions(r)} disabled={busy} /> },
        ]}
      />
      {prompt}
      {dialog}
    </BentoTile>
  );
}

function RunsTile({ institutionId }: { institutionId: string }) {
  const api = useLeaveApi();
  const { run, submit, busy, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const accruals = useQuery({ queryKey: lkeys(institutionId, "accruals"), queryFn: () => api.accruals(institutionId) });
  const yearEnds = useQuery({ queryKey: lkeys(institutionId, "year-ends"), queryFn: () => api.yearEnds(institutionId) });
  const year = currentYear();
  const runs = [
    ...(accruals.data ?? []).map((a) => ({ id: a.id, what: a.period.length > 4 ? `Monthly accrual ${a.period}` : `Opening entitlements ${a.period}`, result: `${a.processed} posted · ${a.skipped} already posted`, runs: a.runs })),
    ...(yearEnds.data ?? []).map((y) => ({ id: y.id, what: `Year end ${y.leaveYear}`, result: `${y.carriedDays} carried · ${y.expiredDays} expired`, runs: y.runs })),
  ];
  return (
    <BentoTile
      span={6}
      title="Accrual & year end"
      action={
        <Actions
          disabled={busy}
          actions={[
            { id: "accrual-opening", label: `Opening ${year}`, onClick: () => void run(() => api.accrue(institutionId, year), (r) => `Opening entitlements: ${r.processed} posted`) },
            {
              id: "accrual-monthly",
              label: "Monthly accrual",
              onClick: () =>
                ask({
                  title: "Monthly accrual",
                  name: "accrual-monthly",
                  fields: [
                    { name: "year", label: "Year", type: "number", required: true, initial: String(year) },
                    { name: "month", label: "Month (1–12)", type: "number", required: true, initial: todayIso().slice(5, 7).replace(/^0/, "") },
                  ],
                  submitLabel: "Run",
                  onSubmit: (v) => submit(() => api.accrue(institutionId, Number(v.year), Number(v.month)), (r) => `Accrual ${r.period}: ${r.processed} posted`),
                }),
            },
            {
              id: "year-end",
              label: "Close leave year",
              onClick: () =>
                ask({
                  title: "Close a leave year (carry forward and expiry)",
                  name: "year-end",
                  fields: [{ name: "year", label: "Leave year", type: "number", required: true, initial: String(year - 1) }],
                  submitLabel: "Close year",
                  onSubmit: (v) => submit(() => api.yearEnd(institutionId, Number(v.year)), (r) => `Year ${r.leaveYear} closed: ${r.carriedDays} carried, ${r.expiredDays} expired`),
                }),
            },
          ]}
        />
      }
    >
      <DataTable
        caption="Leave runs"
        rows={runs}
        rowKey={(r) => r.id}
        empty={<EmptyState icon={CalendarDays} title="No runs yet">Run the opening entitlements once the policy is active.</EmptyState>}
        columns={[
          { key: "what", header: "Run", render: (r) => r.what },
          { key: "result", header: "Result", render: (r) => r.result, muted: true },
          { key: "runs", header: "Executions", render: (r) => String(r.runs) },
        ]}
      />
      {prompt}
      {dialog}
    </BentoTile>
  );
}

function BalancesTile({ institutionId }: { institutionId: string }) {
  const api = useLeaveApi();
  const { submit, busy, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const staff = useStaff(institutionId);
  const types = useActiveTypes(institutionId).filter((t) => t.requiresBalance);
  const [picked, setPicked] = useState<string>("");
  const staffId = picked || staff[0]?.id || "";
  const balances = useQuery({ queryKey: lkeys(staffId, "balances"), queryFn: () => api.balances(staffId), enabled: !!staffId });
  const year = currentYear();
  return (
    <BentoTile
      span={6}
      title="Balances"
      action={
        staffId ? (
          <Actions
            disabled={busy}
            actions={[
              {
                id: "balance-adjust",
                label: "Adjust",
                onClick: () =>
                  ask({
                    title: "Adjust a leave balance",
                    name: "balance-adjust",
                    fields: [
                      { name: "leaveTypeId", label: "Leave type", type: "select", required: true, options: typeOptions(types), initial: types[0]?.id ?? "" },
                      { name: "days", label: "Days (negative to deduct)", required: true, placeholder: "1.00" },
                      REASON,
                    ],
                    submitLabel: "Request adjustment",
                    onSubmit: (v) => submit(() => api.adjust(staffId, { leaveTypeId: v.leaveTypeId!, leaveYear: year, days: v.days!, reason: v.reason! }), (r) => `Adjustment ${statusLabel(r.status).toLowerCase()}`),
                  }),
              },
              {
                id: "balance-encash",
                label: "Encash",
                onClick: () =>
                  ask({
                    title: "Leave encashment",
                    name: "balance-encash",
                    fields: [
                      { name: "leaveTypeId", label: "Leave type", type: "select", required: true, options: typeOptions(types), initial: types[0]?.id ?? "" },
                      { name: "days", label: "Days", required: true, placeholder: "5.00" },
                    ],
                    submitLabel: "Check eligibility",
                    onSubmit: (v) => submit(() => api.encash(staffId, { leaveTypeId: v.leaveTypeId!, leaveYear: year, days: v.days! }), (r) => `Encashment ${statusLabel(r.status).toLowerCase()}${r.statusReason ? ` (${statusLabel(r.statusReason).toLowerCase()})` : ""}`),
                  }),
              },
            ]}
          />
        ) : undefined
      }
    >
      {staff.length > 0 && <Select label="Staff member" value={staffId} options={staffOptions(staff)} onChange={(e) => setPicked(e.target.value)} />}
      <BalanceTable rows={balances.data ?? []} />
      {prompt}
      {dialog}
    </BentoTile>
  );
}

function ExceptionsTile({ institutionId }: { institutionId: string }) {
  const api = useLeaveApi();
  const { run, submit, busy, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const staff = useStaff(institutionId);
  const exceptions = useQuery({ queryKey: lkeys(institutionId, "exceptions"), queryFn: () => api.exceptions(institutionId) });
  const name = (id?: string | null) => staff.find((s) => s.id === id)?.displayName ?? "—";
  return (
    <BentoTile
      span={12}
      title="Leave exceptions"
      action={
        <Button
          size="sm"
          variant="ghost"
          data-action="exception-new"
          onClick={() =>
            ask({
              title: "Raise a leave exception",
              name: "exception-new",
              fields: [
                { name: "category", label: "Category", type: "select", required: true, options: EXCEPTION_CATEGORIES, initial: "attendance_mismatch" },
                { name: "staffId", label: "Staff member", type: "select", options: [{ value: "", label: "—" }, ...staffOptions(staff)], initial: "" },
                { name: "description", label: "Description", required: true, wide: true },
              ],
              submitLabel: "Raise",
              onSubmit: (v) => submit(() => api.raiseException(institutionId, { category: v.category!, staffId: v.staffId || undefined, description: v.description! }), (e) => `${e.number} raised`),
            })
          }
        >
          Raise exception
        </Button>
      }
    >
      <DataTable
        caption="Leave exceptions"
        rows={exceptions.data ?? []}
        rowKey={(e) => e.id}
        empty={<EmptyState icon={Scale} title="No exceptions">Balance, policy or attendance conflicts raised for review appear here.</EmptyState>}
        columns={[
          { key: "number", header: "Exception", render: (e) => e.number },
          { key: "category", header: "Category", render: (e) => statusLabel(e.category) },
          { key: "staff", header: "Staff", render: (e) => name(e.staffId), muted: true },
          { key: "description", header: "Description", render: (e) => e.resolution ?? e.description, muted: true },
          { key: "status", header: "Status", render: (e) => <StatusBadge status={e.status} /> },
          {
            key: "actions",
            header: "",
            render: (e) => (
              <Actions
                disabled={busy}
                actions={[
                  ...(e.status === "raised" ? [{ id: "exception-review", label: "Review", onClick: () => void run(() => api.exceptionCommand(e.id, e.version, "review"), `${e.number} under review`) }] : []),
                  ...(e.status === "under_review"
                    ? [
                        {
                          id: "exception-resolve",
                          label: "Resolve",
                          variant: "primary" as const,
                          onClick: () =>
                            ask({
                              title: `Resolve ${e.number}`,
                              name: "exception-resolve",
                              fields: [{ name: "resolution", label: "Resolution", required: true, wide: true }],
                              submitLabel: "Resolve",
                              onSubmit: (v) => submit(() => api.exceptionCommand(e.id, e.version, "resolve", v.resolution), `${e.number} resolved`),
                            }),
                        },
                      ]
                    : []),
                ]}
              />
            ),
          },
        ]}
      />
      {prompt}
      {dialog}
    </BentoTile>
  );
}

function LeaveStats({ institutionId }: { institutionId: string }) {
  const api = useLeaveApi();
  const policies = useQuery({ queryKey: lkeys(institutionId, "policies"), queryFn: () => api.policies(institutionId) }).data;
  const pending = useQuery({ queryKey: lkeys(institutionId, "requests:pending_approval"), queryFn: () => api.requests(institutionId, "pending_approval") }).data;
  const reserved = useQuery({ queryKey: lkeys(institutionId, "requests:reserved"), queryFn: () => api.requests(institutionId, "reserved") }).data;
  const exceptions = useQuery({ queryKey: lkeys(institutionId, "exceptions"), queryFn: () => api.exceptions(institutionId) }).data;
  const active = policies?.find((p) => p.status === "active");
  return (
    <>
      <StatTile icon={ClipboardList} tone="orange" label="Active policy" value={policies ? (active ? `v${active.versionNo}` : "None") : "…"} hint={active ? `${active.name} · ${active.leaveTypes.length} leave types` : "Create and activate a policy"} data-kpi="leave-policy" />
      <StatTile icon={Hourglass} tone="violet" label="To decide" value={pending ? String(pending.items.length) : "…"} hint="Requests pending approval" data-kpi="leave-pending" />
      <StatTile icon={CalendarCheck} tone="green" label="Reserved" value={reserved ? String(reserved.items.length) : "…"} hint="Approved, not yet taken" data-kpi="leave-reserved" />
      <StatTile icon={AlertTriangle} tone="rose" label="Open exceptions" value={exceptions ? String(exceptions.filter((e) => e.status !== "resolved").length) : "…"} hint="Raised or under review" data-kpi="leave-exceptions" />
    </>
  );
}

export function LeaveAdminTab({ institution }: { institution: Institution | null }) {
  if (!institution) return <NoInstitutionTile title="Leave" />;
  return (
    <>
      <LeaveStats institutionId={institution.id} />
      <PoliciesTile institutionId={institution.id} />
      <RequestsTile institutionId={institution.id} />
      <RunsTile institutionId={institution.id} />
      <BalancesTile institutionId={institution.id} />
      <ExceptionsTile institutionId={institution.id} />
    </>
  );
}

// ================================================================== Admin → People → Attendance & workload

function AttendanceTile({ institutionId }: { institutionId: string }) {
  const api = useLeaveApi();
  const { run, submit, busy, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const [date, setDate] = useState(todayIso());
  const grid = useQuery({ queryKey: lkeys(institutionId, `attendance:${date}`), queryFn: () => api.grid(institutionId, date) });
  const rows = grid.data?.items ?? [];
  const unmarked = rows.filter((r) => r.available && !r.day);
  const mark = (r: AttendanceGridRow, status: string) => run(() => api.bulk(institutionId, date, [{ staffId: r.staffId, status }]), (s) => (s.failed[0] ? s.failed[0].reason : `${r.displayName}: ${statusLabel(status).toLowerCase()}`));
  const actions = (r: AttendanceGridRow): RowAction[] => {
    const day = r.day;
    if (day)
      return [
        {
          id: "attendance-correct",
          label: "Correct",
          onClick: () =>
            ask({
              title: `Correct ${r.displayName} · ${fmtDate(date)}`,
              name: "attendance-correct",
              fields: [{ name: "status", label: "Status", type: "select", required: true, options: DAY_STATUSES, initial: day.status }, { name: "remark", label: "Remark", required: true, wide: true }],
              submitLabel: "Correct",
              onSubmit: (v) => submit(() => api.correct(day.id, day.version, v.status!, v.remark!), "Attendance corrected"),
            }),
        },
      ];
    return [
      { id: "attendance-present", label: "Present", variant: "primary", onClick: () => void mark(r, "present") },
      { id: "attendance-absent", label: "Absent", onClick: () => void mark(r, "absent") },
    ];
  };
  return (
    <BentoTile
      span={12}
      title="Staff attendance"
      data-slice="7B"
      action={
        <Button size="sm" data-action="attendance-all" disabled={busy || unmarked.length === 0} onClick={() => void run(() => api.bulk(institutionId, date, unmarked.map((r) => ({ staffId: r.staffId, status: "present" }))), (s) => `${s.processed} marked present, ${s.failed.length} refused`)}>
          Mark all present
        </Button>
      }
    >
      <Input label="Date" type="date" value={date} onChange={(e) => setDate(e.target.value)} />
      <DataTable<AttendanceGridRow>
        caption="Attendance"
        rows={rows}
        rowKey={(r) => r.staffId}
        empty={<EmptyState icon={UserCheck} title="No staff on this date">Staff with an engagement covering the date appear here.</EmptyState>}
        columns={[
          { key: "staff", header: "Staff", render: (r) => `${r.staffNumber} · ${r.displayName}` },
          { key: "engagement", header: "Availability", render: (r) => (r.available ? "Available" : statusLabel(r.engagementStatus)), muted: true },
          { key: "status", header: "Day", render: (r) => (r.day ? <StatusBadge status={r.day.status} /> : "—") },
          { key: "source", header: "Source", render: (r) => (r.day ? statusLabel(r.day.source) : "—"), muted: true },
          { key: "actions", header: "", render: (r) => <Actions actions={actions(r)} disabled={busy} /> },
        ]}
      />
      {prompt}
      {dialog}
    </BentoTile>
  );
}

function LongLeaveTile({ institutionId, span = 6 }: { institutionId: string; span?: 6 | 12 }) {
  const api = useLeaveApi();
  const { run, submit, busy, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const staff = useStaff(institutionId).filter((s) => s.engagement);
  const [picked, setPicked] = useState("");
  const person = staff.find((s) => s.id === picked) ?? staff[0];
  const engagementId = person?.engagement?.id ?? "";
  const leaves = useQuery({ queryKey: lkeys(engagementId, "long-leave"), queryFn: () => api.longLeaves(engagementId), enabled: !!engagementId });
  return (
    <BentoTile
      span={span}
      title="Long leave"
      action={
        engagementId ? (
          <Button
            size="sm"
            variant="ghost"
            data-action="long-leave-plan"
            onClick={() =>
              ask({
                title: `Plan long leave · ${person?.displayName ?? ""}`,
                name: "long-leave-plan",
                fields: [
                  { name: "plannedStart", label: "From", type: "date", required: true, initial: todayIso() },
                  { name: "plannedEnd", label: "Until", type: "date" },
                  REASON,
                ],
                submitLabel: "Plan",
                onSubmit: (v) => submit(() => api.planLongLeave(engagementId, { plannedStart: v.plannedStart!, plannedEnd: v.plannedEnd || undefined, reason: v.reason! }), "Long leave planned"),
              })
            }
          >
            Plan long leave
          </Button>
        ) : undefined
      }
    >
      {staff.length > 0 && <Select label="Staff member" value={person?.id ?? ""} options={staffOptions(staff)} onChange={(e) => setPicked(e.target.value)} />}
      <DataTable
        caption="Long leave"
        rows={leaves.data ?? []}
        rowKey={(l) => l.id}
        empty={<EmptyState icon={Plane} title="No long leave">Planned and running long leave of the engagement appear here.</EmptyState>}
        columns={[
          { key: "period", header: "Period", render: (l) => `${fmtDate(l.actualStart ?? l.plannedStart)} – ${fmtDate(l.actualEnd ?? l.plannedEnd)}` },
          { key: "reason", header: "Reason", render: (l) => l.reason, muted: true },
          { key: "status", header: "Status", render: (l) => <StatusBadge status={l.status} /> },
          {
            key: "actions",
            header: "",
            render: (l) => (
              <Actions
                disabled={busy}
                actions={[
                  ...(l.status === "planned" ? [{ id: "long-leave-start", label: "Start", variant: "primary" as const, onClick: () => void run(() => api.longLeaveCommand(l.id, l.version, "start"), "Long leave started") }] : []),
                  ...(l.status === "active" ? [{ id: "long-leave-end", label: "End", onClick: () => void run(() => api.longLeaveCommand(l.id, l.version, "end"), "Long leave ended") }] : []),
                ]}
              />
            ),
          },
        ]}
      />
      {prompt}
      {dialog}
    </BentoTile>
  );
}

function WorkloadTile({ institutionId, span = 6 }: { institutionId: string; span?: 6 | 12 }) {
  const api = useLeaveApi();
  const { run, submit, busy, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const faculty = useStaff(institutionId).filter((s) => s.staffCategory === "teaching");
  const [picked, setPicked] = useState("");
  const staffId = picked || faculty[0]?.id || "";
  const workload = useQuery({ queryKey: lkeys(staffId, "workload"), queryFn: () => api.workload(staffId), enabled: !!staffId });
  const plans = useQuery({ queryKey: lkeys(staffId, "workload-plans"), queryFn: () => api.workloadPlans(staffId), enabled: !!staffId });
  const w = workload.data;
  return (
    <BentoTile
      span={span}
      title="Faculty workload"
      data-slice="7B"
      action={
        staffId ? (
          <Button
            size="sm"
            variant="ghost"
            data-action="workload-plan"
            onClick={() =>
              ask({
                title: "New workload plan",
                name: "workload-plan",
                fields: [
                  { name: "termLabel", label: "Planning period", required: true, placeholder: "Term 1" },
                  { name: "availability", label: "Availability", type: "select", required: true, options: AVAILABILITY, initial: "available" },
                  { name: "plannedWeeklyHours", label: "Planned weekly hours", required: true, placeholder: "12.00" },
                  { name: "maxWeeklyHours", label: "Maximum weekly hours", placeholder: "16.00" },
                ],
                submitLabel: "Create draft",
                onSubmit: (v) => submit(() => api.createPlan(staffId, { termLabel: v.termLabel!, availability: v.availability!, plannedWeeklyHours: v.plannedWeeklyHours!, maxWeeklyHours: v.maxWeeklyHours || undefined }), "Workload plan drafted"),
              })
            }
          >
            New plan
          </Button>
        ) : undefined
      }
    >
      {faculty.length > 0 && <Select label="Faculty member" value={staffId} options={staffOptions(faculty)} onChange={(e) => setPicked(e.target.value)} />}
      {w && (
        <p data-workload-total>
          Teaching load {w.totalWeeklyHours} h/week{w.effectiveMaxHours ? ` of ${w.effectiveMaxHours}` : ""}
          {w.overAllocated ? " · over-allocated" : ""}
        </p>
      )}
      <DataTable
        caption="Workload plans"
        rows={plans.data ?? []}
        rowKey={(p) => p.id}
        empty={<EmptyState icon={ClipboardList} title="No workload plan">Draft a plan with the faculty member’s availability and hours.</EmptyState>}
        columns={[
          { key: "term", header: "Period", render: (p) => p.termLabel },
          { key: "availability", header: "Availability", render: (p) => statusLabel(p.availability), muted: true },
          { key: "hours", header: "Hours", render: (p) => `${p.plannedWeeklyHours}${p.maxWeeklyHours ? ` / ${p.maxWeeklyHours}` : ""}` },
          { key: "status", header: "Status", render: (p) => <StatusBadge status={p.overAllocated && p.status === "active" ? "active" : p.status} /> },
          {
            key: "actions",
            header: "",
            render: (p) => (
              <Actions
                disabled={busy}
                actions={[
                  ...(p.status === "draft"
                    ? [
                        {
                          id: "workload-submit",
                          label: "Submit",
                          variant: "primary" as const,
                          onClick: () =>
                            ask({
                              title: `Submit ${p.termLabel}`,
                              name: "workload-submit",
                              fields: [{ name: "exceptionReason", label: "Exception reason (if over the maximum)", wide: true }],
                              submitLabel: "Submit",
                              onSubmit: (v) => submit(() => api.submitPlan(p.id, p.version, v.exceptionReason || undefined), (r) => `Plan ${statusLabel(r.status).toLowerCase()}`),
                            }),
                        },
                      ]
                    : []),
                  ...(p.status === "submitted"
                    ? [
                        { id: "workload-approve", label: "Approve", variant: "primary" as const, onClick: () => void run(() => api.decidePlan(p.id, p.version, true), "Plan approved") },
                        { id: "workload-reject", label: "Reject", onClick: () => ask({ title: `Reject ${p.termLabel}`, name: "workload-reject", fields: [REASON], submitLabel: "Reject", onSubmit: (v) => submit(() => api.decidePlan(p.id, p.version, false, v.reason), "Plan returned to draft") }) },
                      ]
                    : []),
                ]}
              />
            ),
          },
        ]}
      />
      {prompt}
      {dialog}
    </BentoTile>
  );
}

/** Admin → People → Attendance: the daily staff attendance grid and long leave. */
export function AttendanceAdminTab({ institution }: { institution: Institution | null }) {
  if (!institution) return <NoInstitutionTile title="Attendance" />;
  return (
    <>
      <AttendanceTile institutionId={institution.id} />
      <LongLeaveTile institutionId={institution.id} span={12} />
    </>
  );
}

/** Admin → People → Workload: faculty teaching load (Domain D) against workload plans. */
export function WorkloadAdminTab({ institution }: { institution: Institution | null }) {
  if (!institution) return <NoInstitutionTile title="Workload" />;
  return <WorkloadTile institutionId={institution.id} span={12} />;
}

// ================================================================== Faculty → Leave (My HR)

/** My HR (`/leave`): balances, apply / cancel leave, my requests, approvals for my reports. */
export function MyLeavePanel() {
  const api = useLeaveApi();
  const { run, submit, busy, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const balances = useQuery({ queryKey: lkeys("me", "balances"), queryFn: () => api.myBalances(), retry: false });
  const types = useQuery({ queryKey: lkeys("me", "types"), queryFn: () => api.myTypes(), retry: false });
  const requests = useQuery({ queryKey: lkeys("me", "requests"), queryFn: () => api.myRequests(), retry: false });
  const approvals = useQuery({ queryKey: lkeys("me", "approvals"), queryFn: () => api.approvals(), retry: false });
  const typeList = types.data ?? [];
  const apply = () =>
    ask({
      title: "Apply for leave",
      name: "leave-apply",
      fields: [
        { name: "leaveTypeId", label: "Leave type", type: "select", required: true, options: typeOptions(typeList), initial: typeList[0]?.id ?? "" },
        { name: "startDate", label: "From", type: "date", required: true, initial: todayIso() },
        { name: "endDate", label: "To", type: "date", required: true, initial: todayIso() },
        { name: "halfDayEnd", label: "Half day on the last day", type: "checkbox" },
        { name: "reason", label: "Reason", wide: true },
      ],
      submitLabel: "Apply",
      onSubmit: (v) =>
        submit(
          () => api.apply({ leaveTypeId: v.leaveTypeId!, startDate: v.startDate!, endDate: v.endDate!, halfDayEnd: v.halfDayEnd === "true", reason: v.reason || undefined }),
          (r) => `${r.number}: ${r.days} days ${statusLabel(r.status).toLowerCase()}`,
        ),
    });
  const mine = (r: LeaveRequest): RowAction[] =>
    ["pending_approval", "approved", "reserved"].includes(r.status)
      ? [{ id: "my-cancel", label: "Cancel", onClick: () => ask({ title: `Cancel ${r.number}`, name: "my-cancel", fields: [{ name: "reason", label: "Reason", wide: true }], submitLabel: "Cancel leave", onSubmit: (v) => submit(() => api.cancelMine(r.id, r.version, v.reason || undefined), `${r.number} cancelled`) }) }]
      : [];
  const decide = (r: LeaveRequest): RowAction[] => [
    { id: "approval-approve", label: "Approve", variant: "primary", onClick: () => void run(() => api.decideApproval(r.id, r.version, true), `${r.number} approved`) },
    { id: "approval-reject", label: "Reject", onClick: () => ask({ title: `Reject ${r.number}`, name: "approval-reject", fields: [REASON], submitLabel: "Reject", onSubmit: (v) => submit(() => api.decideApproval(r.id, r.version, false, v.reason), `${r.number} rejected`) }) },
  ];
  const waiting = approvals.data ?? [];
  return (
    <BentoTile
      span={12}
      title="My leave"
      data-slice="7B"
      action={
        <Button size="sm" data-action="leave-apply" disabled={busy || typeList.length === 0} onClick={apply}>
          Apply for leave
        </Button>
      }
    >
      {balances.isError ? (
        <EmptyState icon={CalendarDays} title="No staff record">
          Leave becomes available once HR links your login to a staff record.
        </EmptyState>
      ) : (
        <>
          <BalanceTable rows={balances.data ?? []} />
          <DataTable<LeaveRequest>
            caption="My leave requests"
            rows={requests.data ?? []}
            rowKey={(r) => r.id}
            empty={<EmptyState icon={CalendarCheck} title="No leave requests">Apply for leave and follow it here.</EmptyState>}
            columns={[
              { key: "number", header: "Request", render: (r) => r.number },
              { key: "type", header: "Type", render: (r) => r.leaveTypeCode, muted: true },
              { key: "dates", header: "Dates", render: dates },
              { key: "days", header: "Days", render: (r) => r.days },
              { key: "status", header: "Status", render: (r) => <StatusBadge status={r.status} /> },
              { key: "actions", header: "", render: (r) => <Actions actions={mine(r)} disabled={busy} /> },
            ]}
          />
          {waiting.length > 0 && (
            <DataTable<LeaveRequest>
              caption="Approvals waiting for me"
              rows={waiting}
              rowKey={(r) => r.id}
              columns={[
                { key: "staff", header: "Waiting for you", render: (r) => r.displayName },
                { key: "type", header: "Type", render: (r) => r.leaveTypeCode, muted: true },
                { key: "dates", header: "Dates", render: dates },
                { key: "days", header: "Days", render: (r) => r.days },
                { key: "actions", header: "", render: (r) => <Actions actions={decide(r)} disabled={busy} /> },
              ]}
            />
          )}
        </>
      )}
      {prompt}
      {dialog}
    </BentoTile>
  );
}

// ================================================================== Faculty → Leave (My HR): stats, attendance, workload

const BALANCE_ICONS = [CalendarDays, HeartPulse, Palmtree, Clock] as const;
const BALANCE_TONES: readonly Tone[] = ["blue", "rose", "green", "violet"];

/** My HR → Leave: one stat tile per leave balance (available of entitled days, decimal strings). */
export function MyLeaveStats() {
  const api = useLeaveApi();
  const balances = useQuery({ queryKey: lkeys("me", "balances"), queryFn: () => api.myBalances(), retry: false });
  const rows = (balances.data ?? []).slice(0, 4);
  if (balances.isError || rows.length === 0) return null;
  const span = rows.length >= 4 ? 3 : rows.length === 3 ? 4 : 6;
  return (
    <>
      {rows.map((b, i) => (
        noEntitlement(b) ? (
          <StatTile key={b.leaveTypeId} icon={BALANCE_ICONS[i] ?? CalendarDays} tone={BALANCE_TONES[i] ?? "blue"} span={span} label={`${b.leaveTypeCode} taken`} value={b.consumed} hint={`${b.reserved} reserved · no entitlement`} data-kpi={`balance-${b.leaveTypeCode}`} />
        ) : (
          <StatTile key={b.leaveTypeId} icon={BALANCE_ICONS[i] ?? CalendarDays} tone={BALANCE_TONES[i] ?? "blue"} span={span} label={`${b.leaveTypeCode} available`} value={b.available} hint={`of ${b.entitled} days · ${b.consumed} taken · ${b.reserved} reserved`} data-kpi={`balance-${b.leaveTypeCode}`} />
        )
      ))}
    </>
  );
}

const monthStart = () => `${todayIso().slice(0, 8)}01`;

/** My HR → Attendance: my attendance days in a date range. */
export function MyAttendancePanel() {
  const api = useLeaveApi();
  const [from, setFrom] = useState(monthStart());
  const [to, setTo] = useState(todayIso());
  const days = useQuery({ queryKey: lkeys("me", `attendance:${from}:${to}`), queryFn: () => api.myAttendance(from, to), retry: false });
  const rows = days.data ?? [];
  const count = (st: string) => rows.filter((d) => d.status === st).length;
  return (
    <BentoTile span={12} title="My attendance" data-slice="7B" data-my-attendance>
      {days.isError ? (
        <EmptyState icon={UserCheck} title="No staff record">
          Attendance appears once HR links your login to a staff record.
        </EmptyState>
      ) : (
        <>
          <Input label="From" type="date" value={from} onChange={(e) => setFrom(e.target.value)} />
          <Input label="To" type="date" value={to} onChange={(e) => setTo(e.target.value)} />
          <p data-attendance-summary>
            {count("present")} present · {count("absent")} absent · {count("half_day")} half days · {count("on_leave")} on leave
          </p>
          <DataTable
            caption="Attendance days"
            rows={rows}
            rowKey={(d) => d.id}
            empty={<EmptyState icon={UserCheck} title="No days recorded">HR records staff attendance day by day.</EmptyState>}
            columns={[
              { key: "date", header: "Date", render: (d) => fmtDate(d.workDate) },
              { key: "status", header: "Day", render: (d) => <StatusBadge status={d.status} /> },
              { key: "source", header: "Source", render: (d) => statusLabel(d.source), muted: true },
              { key: "remark", header: "Remark", render: (d) => d.remark ?? "—", muted: true },
            ]}
          />
        </>
      )}
    </BentoTile>
  );
}

/** My HR → Workload: my teaching load (Domain D) and the active workload plan. */
export function MyWorkloadPanel() {
  const api = useLeaveApi();
  const w = useQuery({ queryKey: lkeys("me", "workload"), queryFn: () => api.myWorkload(), retry: false });
  const d = w.data;
  return (
    <BentoTile span={12} title="My workload" data-slice="7B" data-my-workload>
      {w.isError ? (
        <EmptyState icon={ClipboardList} title="No staff record">
          Your workload appears once HR links your login to a staff record.
        </EmptyState>
      ) : (
        <>
          {d && (
            <p data-workload-total>
              Teaching load {d.totalWeeklyHours} h/week{d.effectiveMaxHours ? ` of ${d.effectiveMaxHours}` : ""}
              {d.activePlan ? ` · plan ${d.activePlan.termLabel}: ${d.activePlan.plannedWeeklyHours} h` : ""}
              {d.overAllocated ? " · over-allocated" : ""}
            </p>
          )}
          <DataTable
            caption="Teaching assignments"
            rows={d?.items ?? []}
            rowKey={(i) => i.assignmentId}
            empty={<EmptyState icon={ClipboardList} title="No teaching assignments">Approved faculty assignments appear here.</EmptyState>}
            columns={[
              { key: "offering", header: "Offering", render: (i) => `${i.offeringCode} · ${i.title}` },
              { key: "role", header: "Role", render: (i) => statusLabel(i.role), muted: true },
              { key: "hours", header: "Hours / week", render: (i) => i.weeklyHours ?? "—" },
              { key: "status", header: "Status", render: (i) => <StatusBadge status={i.status} /> },
            ]}
          />
        </>
      )}
    </BentoTile>
  );
}

/** Home shortcut label for faculty: "Apply leave · 6.00 CL left" once a balance exists (else the plain label). */
export function useLeaveShortcutLabel(enabled: boolean, label: string): string {
  const api = useLeaveApi();
  const balances = useQuery({ queryKey: lkeys("me", "balances"), queryFn: () => api.myBalances(), retry: false, enabled });
  const first = balances.data?.[0];
  return first ? `${label} · ${first.available} ${first.leaveTypeCode} left` : label;
}
