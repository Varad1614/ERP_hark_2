/**
 * Slice **8C** web components (library). The export names are imported by the foundation frames
 * (docs/batch8-contracts.md §10): `routes/admin/campus-setup.tsx` (Admin → Campus → Library) and
 * `routes/student/library.tsx` (Student → Library). Every screen carries `data-slice="8C"`. Only
 * @sutra/ui components — no custom styling. Fines and replacement costs stay the decimal strings
 * the API sends.
 *
 * - `LibraryAdminTab` — borrowing policy versions (submit → decide → activate), memberships
 *   (suspend / restore / close, borrowing restriction), the catalogue (publish, withdraw, copies
 *   with condition / lost / withdraw), circulation (issue, return, renew, overdue check), the
 *   reservation queue (collect / expire / cancel), fines (bill a student's fine through Finance,
 *   waiver and its decision) and library clearance.
 * - `MyLibraryPanel` — the student's loans (renew), catalogue search (reserve), reservations
 *   (cancel), fines, library clearance and a library complaint (service desk ticket).
 */
import { AlertTriangle, BookCopy, BookMarked, BookmarkPlus, BookOpen, CalendarPlus, ClipboardCheck, Library, LibraryBig, Receipt, Users } from "lucide-react";
import { Badge, BentoTile, Button, DataTable, EmptyState, FilterChip, formatNumber, KpiTile, ListRow, NameCell, plural, SearchField, statusKind } from "@sutra/ui";
import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { fmtDate, REASON, todayIso, useAsk, type RowAction } from "../../institution/forms";
import { ApprovalBadge, Actions, inr as formatInr } from "../ui";
import { StatusBadge, statusLabel, useCommand } from "../../institution/hooks";
import type { Institution } from "../../institution/types";
import { lkeys, useLibraryApi } from "./hooks";
import { kindOf, personField, usePeople } from "../PersonPicker"; // 9E
import type { CatalogueRecord, Clearance, Copy, CopyCondition, Fine, LibraryPolicy, LibraryReservation, Loan, Membership } from "./types";

export { LibraryApiProvider } from "./hooks";

const OPEN_LOAN = ["active", "overdue"];
const LOAN_FILTERS = [
  { key: "open", label: "Out" },
  { key: "overdue", label: "Overdue" },
  { key: "all", label: "All" },
] as const;

function NoInstitutionTile({ title }: { title: string }) {
  return (
    <BentoTile span={12} title={title} data-slice="8C">
      <EmptyState icon={Library} title="No institution selected" compact>
        Register an institution in Campus setup → Institution first.
      </EmptyState>
    </BentoTile>
  );
}

const CONDITIONS: CopyCondition[] = ["good", "fair", "poor", "damaged"];
const UNSETTLED = ["assessed", "billed", "waiver_requested"];

// ================================================================== action rules (pure, tested)

/** Next steps offered for a borrowing policy version. */
export function policyActions(p: Pick<LibraryPolicy, "status" | "approvalState">): Array<"submit" | "approve" | "reject" | "activate"> {
  if (p.status !== "draft") return [];
  if (p.approvalState === "pending") return ["approve", "reject"];
  if (p.approvalState === "approved") return ["activate"];
  return ["submit"];
}

/** Next steps offered for a membership. */
export function memberActions(m: Pick<Membership, "status" | "restrictionReason">): Array<"suspend" | "restore" | "close" | "restrict" | "lift"> {
  switch (m.status) {
    case "active":
      return [m.restrictionReason ? "lift" : "restrict", "suspend", "close"];
    case "suspended":
      return ["restore", "close"];
    default:
      return [];
  }
}

/** Next steps offered for a copy. */
export function copyActions(c: Pick<Copy, "status">): Array<"condition" | "lost" | "withdraw"> {
  switch (c.status) {
    case "available":
      return ["condition", "lost", "withdraw"];
    case "issued":
      return ["condition", "lost"];
    case "reserved":
      return ["condition"];
    case "damaged":
      return ["condition", "withdraw"];
    default:
      return [];
  }
}

/** Next steps offered for a reservation at the desk. */
export function reservationActions(r: Pick<LibraryReservation, "status">): Array<"collect" | "expire" | "cancel"> {
  if (r.status === "ready") return ["collect", "expire", "cancel"];
  if (r.status === "queued") return ["cancel"];
  return [];
}

/** Next steps offered for a fine: only a student's assessed fine is billed through Finance. */
export function fineActions(f: Pick<Fine, "status" | "personKind">): Array<"bill" | "waive" | "approve" | "reject"> {
  if (f.status === "waiver_requested") return ["approve", "reject"];
  if (f.status !== "assessed") return [];
  return f.personKind === "student" ? ["bill", "waive"] : ["waive"];
}

/** Sum of decimal amounts in paise (no float drift), back as a 2-dp string. */
export function sumAmounts(amounts: string[]): string {
  const paise = amounts.reduce((s, a) => {
    const [whole = "0", frac = ""] = a.replace("-", "").split(".");
    const v = Number(whole) * 100 + Number((frac + "00").slice(0, 2));
    return s + (a.startsWith("-") ? -v : v);
  }, 0);
  const sign = paise < 0 ? "-" : "";
  const abs = Math.abs(paise);
  return `${sign}${Math.floor(abs / 100)}.${String(abs % 100).padStart(2, "0")}`;
}

/** A returned loan is a completed loan, not a "returned for correction" workflow state. */
function LoanBadge({ status }: { status: string }) {
  return status === "returned" ? <Badge kind="done">{statusLabel(status)}</Badge> : <StatusBadge status={status} />;
}

const dueText = (l: Loan) => (l.status === "returned" ? `Returned ${fmtDate(l.returnedOn)}` : `Due ${fmtDate(l.dueOn)}`);

// ================================================================== Admin → Campus → Library

function LibraryScreen({ institutionId }: { institutionId: string }) {
  const { people } = usePeople(institutionId); // 9E
  const api = useLibraryApi();
  const { run, submit, busy, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const [loanFilter, setLoanFilter] = useState<(typeof LOAN_FILTERS)[number]["key"]>("open");
  const [copiesOf, setCopiesOf] = useState<CatalogueRecord | null>(null);
  const policies = useQuery({ queryKey: lkeys(institutionId, "policies"), queryFn: () => api.policies(institutionId) }).data ?? [];
  const members = useQuery({ queryKey: lkeys(institutionId, "members"), queryFn: () => api.memberships(institutionId) }).data ?? [];
  const records = useQuery({ queryKey: lkeys(institutionId, "catalogue"), queryFn: () => api.catalogue(institutionId) }).data ?? [];
  const allLoans = useQuery({ queryKey: lkeys(institutionId, "loans"), queryFn: () => api.loans(institutionId) }).data ?? [];
  const fines = useQuery({ queryKey: lkeys(institutionId, "fines"), queryFn: () => api.fines(institutionId) }).data ?? [];
  const reservations = useQuery({ queryKey: lkeys(institutionId, "reservations"), queryFn: () => api.reservations(institutionId) }).data ?? [];
  const clearances = useQuery({ queryKey: lkeys(institutionId, "clearances"), queryFn: () => api.clearances(institutionId) }).data ?? [];
  const copiesId = copiesOf?.id ?? "";
  const copies = useQuery({ queryKey: lkeys(copiesId || "none", "copies"), queryFn: () => api.copies(copiesId), enabled: copiesId !== "" }).data ?? [];
  const out = allLoans.filter((l) => OPEN_LOAN.includes(l.status));
  const overdue = allLoans.filter((l) => l.status === "overdue");
  const loans = loanFilter === "open" ? out : loanFilter === "overdue" ? overdue : allLoans;
  const unsettled = fines.filter((f) => UNSETTLED.includes(f.status));
  const liveReservations = reservations.filter((r) => r.status === "queued" || r.status === "ready");
  const active = policies.find((p) => p.status === "active");

  const newPolicy = () =>
    ask({
      title: "New borrowing policy",
      name: "library-policy",
      fields: [
        { name: "name", label: "Name", required: true, initial: `Library rules ${todayIso().slice(0, 4)}` },
        { name: "effectiveFrom", label: "Effective from", type: "date", required: true, initial: todayIso() },
        { name: "loanDaysStudent", label: "Loan days (students)", type: "number", required: true, initial: "14" },
        { name: "loanDaysStaff", label: "Loan days (staff)", type: "number", required: true, initial: "30" },
        { name: "maxLoansStudent", label: "Max loans (students)", type: "number", required: true, initial: "3" },
        { name: "maxLoansStaff", label: "Max loans (staff)", type: "number", required: true, initial: "6" },
        { name: "maxRenewals", label: "Renewals", type: "number", required: true, initial: "1" },
        { name: "finePerDay", label: "Fine per day (₹)", required: true, initial: "2.00" },
        { name: "maxFine", label: "Fine cap (₹)", initial: "100.00" },
        { name: "reservationHoldDays", label: "Hold days", type: "number", required: true, initial: "3" },
        { name: "lostChargeFactor", label: "Lost copy factor", required: true, initial: "1.5" },
      ],
      submitLabel: "Create",
      onSubmit: (v) =>
        submit(
          () =>
            api.createPolicy(institutionId, {
              name: v.name!,
              effectiveFrom: v.effectiveFrom!,
              loanDaysStudent: Number(v.loanDaysStudent),
              loanDaysStaff: Number(v.loanDaysStaff),
              maxLoansStudent: Number(v.maxLoansStudent),
              maxLoansStaff: Number(v.maxLoansStaff),
              maxRenewals: Number(v.maxRenewals),
              finePerDay: v.finePerDay!,
              maxFine: v.maxFine || undefined,
              reservationHoldDays: Number(v.reservationHoldDays),
              lostChargeFactor: v.lostChargeFactor!,
            }),
          (p) => `Policy “${p.name}” drafted`,
        ),
    });

  const decide = (title: string, name: string, approve: () => Promise<unknown>, reject: (reason: string) => Promise<unknown>, what: string) => ({
    approve: () => void run(approve, `${what} approved`),
    reject: () => ask({ title, name, fields: [REASON], submitLabel: "Reject", onSubmit: (v) => submit(() => reject(v.reason ?? ""), `${what} rejected`) }),
  });

  const policyRowActions = (p: LibraryPolicy): RowAction[] => {
    const d = decide(`Reject “${p.name}”`, "policy-reject", () => api.decidePolicy(p.id, p.version, "approve"), (reason) => api.decidePolicy(p.id, p.version, "reject", reason), "Policy");
    return policyActions(p).map((a): RowAction => {
      switch (a) {
        case "submit":
          return { id: "policy-submit", label: "Submit", variant: "primary", onClick: () => void run(() => api.policyCommand(p.id, p.version, "submit"), (r) => `Policy ${statusLabel(r.approvalState).toLowerCase()}`) };
        case "approve":
          return { id: "policy-approve", label: "Approve", variant: "primary", onClick: d.approve };
        case "reject":
          return { id: "policy-reject", label: "Reject", onClick: d.reject };
        default:
          return { id: "policy-activate", label: "Activate", variant: "primary", onClick: () => void run(() => api.policyCommand(p.id, p.version, "activate"), "Policy active") };
      }
    });
  };

  const addMember = () =>
    ask({
      title: "Add a library member",
      name: "library-member",
      fields: [
        { name: "personKind", label: "Member", type: "select", required: true, options: [{ value: "student", label: "Student" }, { value: "staff", label: "Staff" }], initial: "student" },
        personField(people), // 9E: PersonPicker
      ],
      submitLabel: "Add",
      onSubmit: (v) => submit(() => api.createMembership(institutionId, { personKind: kindOf(people, v.personId, v.personKind) as Membership["personKind"], personId: v.personId!.trim() }), (m) => `${m.number} ${statusLabel(m.status).toLowerCase()}`),
    });

  const memberRowActions = (m: Membership): RowAction[] =>
    memberActions(m).map((a): RowAction => {
      const withReason = (title: string, label: string, act: (reason: string) => Promise<Membership>, done: string): RowAction => ({
        id: `member-${a}`,
        label,
        onClick: () => ask({ title, name: `member-${a}`, fields: [REASON], submitLabel: label, onSubmit: (v) => submit(() => act(v.reason ?? ""), done) }),
      });
      switch (a) {
        case "restrict":
          return withReason(`Restrict borrowing for ${m.number}`, "Restrict", (r) => api.restrict(m.id, m.version, true, r), `${m.number} restricted`);
        case "lift":
          return { id: "member-lift", label: "Lift restriction", variant: "primary", onClick: () => void run(() => api.restrict(m.id, m.version, false), `${m.number} may borrow again`) };
        case "suspend":
          return withReason(`Suspend ${m.number}`, "Suspend", (r) => api.membershipCommand(m.id, m.version, "suspend", r), `${m.number} suspended`);
        case "restore":
          return { id: "member-restore", label: "Restore", variant: "primary", onClick: () => void run(() => api.membershipCommand(m.id, m.version, "restore"), `${m.number} restored`) };
        default:
          return withReason(`Close ${m.number}`, "Close", (r) => api.membershipCommand(m.id, m.version, "close", r), `${m.number} closed`);
      }
    });

  const newRecord = () =>
    ask({
      title: "Catalogue a title",
      name: "catalogue-record",
      fields: [
        { name: "title", label: "Title", required: true, wide: true },
        { name: "authors", label: "Authors (comma separated)", wide: true },
        { name: "isbn", label: "ISBN" },
        { name: "publisher", label: "Publisher" },
        { name: "replacementCost", label: "Replacement cost (₹)", initial: "500.00" },
      ],
      submitLabel: "Create",
      onSubmit: (v) =>
        submit(
          () =>
            api.createRecord(institutionId, {
              title: v.title!,
              authors: (v.authors ?? "")
                .split(",")
                .map((x) => x.trim())
                .filter(Boolean),
              isbn: v.isbn || undefined,
              publisher: v.publisher || undefined,
              replacementCost: v.replacementCost || undefined,
            }),
          (r) => `“${r.title}” catalogued`,
        ),
    });

  const recordActions = (r: CatalogueRecord): RowAction[] => {
    const a: RowAction[] = [{ id: "record-copies", label: "Copies", onClick: () => setCopiesOf(r) }];
    if (r.status === "draft") a.push({ id: "record-publish", label: "Publish", variant: "primary", onClick: () => void run(() => api.publishRecord(r.id, r.version), `“${r.title}” published`) });
    if (r.status === "published")
      a.push({
        id: "record-withdraw",
        label: "Withdraw",
        onClick: () => ask({ title: `Withdraw “${r.title}”`, name: "record-withdraw", fields: [REASON], submitLabel: "Withdraw", onSubmit: (v) => submit(() => api.withdrawRecord(r.id, r.version, v.reason), `“${r.title}” withdrawn`) }),
      });
    if (r.status !== "withdrawn")
      a.push({
        id: "record-copy",
        label: "Add copy",
        onClick: () =>
          ask({
            title: `Add a copy of “${r.title}”`,
            name: "library-copy",
            fields: [{ name: "shelfMark", label: "Shelf mark", placeholder: "ENG-12" }],
            submitLabel: "Add copy",
            onSubmit: (v) => submit(() => api.addCopy(r.id, { shelfMark: v.shelfMark || undefined }), (c) => `${c.accessionNumber} on the shelf`),
          }),
      });
    return a;
  };

  const copyRowActions = (c: Copy): RowAction[] =>
    copyActions(c).map((a): RowAction => {
      if (a === "condition")
        return {
          id: "copy-condition",
          label: "Condition",
          onClick: () =>
            ask({
              title: `Condition of ${c.accessionNumber}`,
              name: "copy-condition",
              fields: [
                { name: "condition", label: "Condition", type: "select", required: true, options: CONDITIONS.map((x) => ({ value: x, label: statusLabel(x) })), initial: c.condition },
                { name: "reason", label: "Note", wide: true },
              ],
              submitLabel: "Save",
              onSubmit: (v) => submit(() => api.copyCondition(c.id, c.version, v.condition as CopyCondition, v.reason), (r) => `${r.accessionNumber} ${statusLabel(r.condition).toLowerCase()}`),
            }),
        };
      const cmd = a;
      const label = cmd === "lost" ? "Mark lost" : "Withdraw";
      return {
        id: `copy-${cmd}`,
        label,
        onClick: () =>
          ask({
            title: `${label} · ${c.accessionNumber}`,
            name: `copy-${cmd}`,
            fields: [REASON],
            submitLabel: label,
            onSubmit: (v) => submit(() => api.copyCommand(c.id, c.version, cmd, v.reason), (r) => (cmd === "lost" ? `${r.accessionNumber} lost · replacement charge assessed` : `${r.accessionNumber} withdrawn`)),
          }),
      };
    });

  const reservationRowActions = (r: LibraryReservation): RowAction[] =>
    reservationActions(r).map((a): RowAction => {
      if (a === "collect") return { id: "reservation-collect", label: "Collect", variant: "primary", onClick: () => void run(() => api.reservationCommand(r.id, r.version, "collect"), (x) => `${x.title} collected`) };
      const label = a === "expire" ? "Expire" : "Cancel";
      return {
        id: `reservation-${a}`,
        label,
        onClick: () => ask({ title: `${label} the reservation of ${r.displayName}`, name: `reservation-${a}`, fields: [REASON], submitLabel: label, onSubmit: (v) => submit(() => api.reservationCommand(r.id, r.version, a, v.reason), (x) => `Reservation ${statusLabel(x.status).toLowerCase()}`) }),
      };
    });

  const fineRowActions = (f: Fine): RowAction[] => {
    const d = decide(`Reject the waiver of ${formatInr(f.amount)}`, "fine-reject", () => api.decideFine(f.id, f.version, "approve"), (reason) => api.decideFine(f.id, f.version, "reject", reason), "Waiver");
    return fineActions(f).map((a): RowAction => {
      switch (a) {
        case "bill":
          return { id: "fine-bill", label: "Bill", variant: "primary", onClick: () => void run(() => api.fineCommand(f.id, f.version, "bill"), (r) => `Billed as ${r.chargeNumber ?? "a Finance charge"}`) };
        case "waive":
          return {
            id: "fine-waive",
            label: "Waive",
            onClick: () => ask({ title: `Waive ${formatInr(f.amount)}`, name: "fine-waive", fields: [REASON], submitLabel: "Waive", onSubmit: (v) => submit(() => api.fineCommand(f.id, f.version, "waiver", v.reason), (r) => `Fine ${statusLabel(r.status).toLowerCase()}`) }),
          };
        case "approve":
          return { id: "fine-approve", label: "Approve waiver", variant: "primary", onClick: d.approve };
        default:
          return { id: "fine-reject", label: "Reject", onClick: d.reject };
      }
    });
  };

  const clearanceRowActions = (c: Clearance): RowAction[] => {
    if (c.status !== "requested") return [];
    const d = decide(`Reject the clearance of ${c.memberNumber}`, "clearance-reject", () => api.decideClearance(c.id, c.version, "approve"), (reason) => api.decideClearance(c.id, c.version, "reject", reason), "Clearance");
    return [
      { id: "clearance-approve", label: "Approve", variant: "primary", onClick: d.approve },
      { id: "clearance-reject", label: "Reject", onClick: d.reject },
    ];
  };

  const requestClearance = () =>
    ask({
      title: "Library clearance",
      name: "library-clearance",
      fields: [
        { name: "membershipId", label: "Member", type: "select", required: true, options: members.filter((m) => m.status !== "closed").map((m) => ({ value: m.id, label: `${m.number} · ${m.displayName}` })) },
        { name: "purpose", label: "Purpose", required: true, initial: "Programme completion", wide: true },
      ],
      submitLabel: "Check and record",
      onSubmit: (v) => submit(() => api.requestClearance(institutionId, v.membershipId!, v.purpose!), (c) => `${c.memberNumber}: clearance ${statusLabel(c.status).toLowerCase()}`),
    });

  const issue = () =>
    ask({
      title: "Issue a copy",
      name: "library-issue",
      fields: [
        { name: "membershipId", label: "Member", type: "select", required: true, options: members.filter((m) => m.status === "active").map((m) => ({ value: m.id, label: `${m.number} · ${m.displayName}` })) },
        { name: "accessionNumber", label: "Accession number", required: true, placeholder: "ACC-000001" },
      ],
      submitLabel: "Issue",
      onSubmit: (v) => submit(() => api.issue(institutionId, { membershipId: v.membershipId!, accessionNumber: v.accessionNumber!.trim().toUpperCase() }), (l) => `${l.number} due ${fmtDate(l.dueOn)}`),
    });

  const loanActions = (l: Loan): RowAction[] => {
    if (!OPEN_LOAN.includes(l.status)) return [];
    const a: RowAction[] = [{ id: "loan-return", label: "Return", variant: "primary", onClick: () => void run(() => api.loanCommand(l.id, l.version, "return"), (r) => (r.fineAmount ? `${r.number} returned · fine ${formatInr(r.fineAmount)}` : `${r.number} returned`)) }];
    if (l.status === "active") a.push({ id: "loan-renew", label: "Renew", onClick: () => void run(() => api.loanCommand(l.id, l.version, "renew"), (r) => `${r.number} due ${fmtDate(r.dueOn)}`) });
    return a;
  };

  return (
    <>
      <KpiTile span={3} icon={Users} tone="blue" label="Members" value={String(members.filter((m) => m.status === "active").length)} data-slice="8C" data-kpi="library-members" />
      <KpiTile span={3} icon={LibraryBig} tone="violet" label="Titles" value={String(records.filter((r) => r.status === "published").length)} data-kpi="library-titles" />
      <KpiTile span={3} icon={BookOpen} tone="green" label="Loans out" value={String(out.length)} sub={liveReservations.length ? `${liveReservations.length} reservation(s) waiting` : undefined} data-kpi="library-loans" />
      <KpiTile span={3} icon={AlertTriangle} tone="rose" label="Overdue" value={String(overdue.length)} data-kpi="library-overdue" />
      <BentoTile
        span={12}
        title="Borrowing policy"
        data-slice="8C"
        action={
          <Button size="sm" data-action="policy-new" onClick={newPolicy}>
            New version
          </Button>
        }
      >
        <DataTable<LibraryPolicy>
          caption="Library policies"
          rows={policies}
          rowKey={(p) => p.id}
          empty={<EmptyState icon={BookMarked} title="No borrowing policy yet" compact>Members can borrow once a policy is active.</EmptyState>}
          columns={[
            { key: "name", header: "Policy", render: (p) => p.name },
            { key: "from", header: "From", render: (p) => fmtDate(p.effectiveFrom), muted: true },
            { key: "loans", header: "Loan days", render: (p) => `${p.loanDaysStudent} student · ${p.loanDaysStaff} staff`, muted: true },
            { key: "fine", header: "Fine / day", render: (p) => formatInr(p.finePerDay), muted: true },
            { key: "approval", header: "Approval", render: (p) => <ApprovalBadge state={p.approvalState} /> },
            { key: "status", header: "Status", render: (p) => <StatusBadge status={p.status} /> },
            { key: "actions", header: "", render: (p) => <Actions actions={policyRowActions(p)} disabled={busy} /> },
          ]}
        />
      </BentoTile>
      <BentoTile
        span={12}
        title="Members"
        data-slice="8C"
        action={
          <Button size="sm" data-action="member-new" onClick={addMember} disabled={!active}>
            Add member
          </Button>
        }
      >
        <DataTable<Membership>
          caption="Library members"
          rows={members}
          rowKey={(m) => m.id}
          empty={<EmptyState icon={Users} title="No members yet" compact>Students and staff become members to borrow.</EmptyState>}
          columns={[
            { key: "number", header: "Member", render: (m) => m.number },
            { key: "name", header: "Name", render: (m) => m.displayName },
            { key: "kind", header: "Kind", render: (m) => statusLabel(m.personKind), muted: true },
            { key: "loans", header: "Loans", render: (m) => String(m.activeLoans), muted: true },
            { key: "status", header: "Status", render: (m) => (m.restrictionReason && m.status === "active" ? <StatusBadge status="restricted" /> : <StatusBadge status={m.status} />) },
            { key: "actions", header: "", render: (m) => <Actions actions={memberRowActions(m)} disabled={busy} /> },
          ]}
        />
      </BentoTile>
      <BentoTile
        span={12}
        title="Catalogue"
        data-slice="8C"
        action={
          <Button size="sm" data-action="record-new" onClick={newRecord}>
            Catalogue a title
          </Button>
        }
      >
        <DataTable<CatalogueRecord>
          caption="Catalogue records"
          rows={records}
          rowKey={(r) => r.id}
          empty={<EmptyState icon={LibraryBig} title="The catalogue is empty" compact>Catalogue a title, publish it and add copies.</EmptyState>}
          columns={[
            { key: "title", header: "Title", render: (r) => r.title },
            { key: "copies", header: "On shelf", render: (r) => `${r.availableCopies} of ${r.copies}`, muted: true },
            { key: "status", header: "Status", render: (r) => <StatusBadge status={r.status} /> },
            { key: "actions", header: "", render: (r) => <Actions actions={recordActions(r)} disabled={busy} /> },
          ]}
        />
      </BentoTile>
      {copiesOf && (
        <BentoTile
          span={12}
          title={`Copies · ${copiesOf.title}`}
          data-slice="8C"
          action={
            <Button size="sm" variant="ghost" data-action="copies-close" onClick={() => setCopiesOf(null)}>
              Close
            </Button>
          }
        >
          <DataTable<Copy>
            caption="Copies"
            rows={copies}
            rowKey={(c) => c.id}
            empty={<EmptyState icon={BookOpen} title="No copies yet" compact>Add a copy to put the title on the shelf.</EmptyState>}
            columns={[
              { key: "acc", header: "Accession", render: (c) => c.accessionNumber },
              { key: "where", header: "Location", render: (c) => c.nodeCode ?? c.shelfMark ?? "—", muted: true },
              { key: "condition", header: "Condition", render: (c) => statusLabel(c.condition), muted: true },
              { key: "status", header: "Status", render: (c) => <StatusBadge status={c.status} /> },
              { key: "actions", header: "", render: (c) => <Actions actions={copyRowActions(c)} disabled={busy} /> },
            ]}
          />
        </BentoTile>
      )}
      <BentoTile
        span={12}
        title="Circulation"
        data-slice="8C"
        action={
          <>
            <Button size="sm" variant="ghost" data-action="overdue-sweep" onClick={() => void run(() => api.overdueSweep(institutionId), (r) => `Overdue check: ${r.processed} fined, ${r.skipped} unchanged`)} disabled={busy}>
              Overdue check
            </Button>
            <Button size="sm" data-action="loan-new" onClick={issue} disabled={!members.length}>
              Issue
            </Button>
          </>
        }
      >
        {LOAN_FILTERS.map((f) => (
          <FilterChip key={f.key} active={loanFilter === f.key} label={f.label} count={f.key === "open" ? out.length : f.key === "overdue" ? overdue.length : allLoans.length} onClick={() => setLoanFilter(f.key)} />
        ))}
        <DataTable<Loan>
          caption="Loans"
          rows={loans}
          rowKey={(l) => l.id}
          empty={<EmptyState icon={BookOpen} title="No loans here" compact>Issued copies and their due dates appear here.</EmptyState>}
          columns={[
            { key: "number", header: "Loan", render: (l) => l.number },
            { key: "title", header: "Title", render: (l) => `${l.title} · ${l.accessionNumber}` },
            { key: "member", header: "Member", render: (l) => l.displayName, muted: true },
            { key: "due", header: "Due", render: (l) => dueText(l), muted: true },
            { key: "fine", header: "Fine", render: (l) => (l.fineAmount ? formatInr(l.fineAmount) : "—"), muted: true },
            { key: "status", header: "Status", render: (l) => <LoanBadge status={l.status} /> },
            { key: "actions", header: "", render: (l) => <Actions actions={loanActions(l)} disabled={busy} /> },
          ]}
        />
      </BentoTile>
      <BentoTile span={12} title={unsettled.length ? `Fines · ${formatInr(sumAmounts(unsettled.map((f) => f.amount)))} unsettled` : "Fines"} data-slice="8C">
        <DataTable<Fine>
          caption="Library fines"
          rows={fines}
          rowKey={(f) => f.id}
          empty={<EmptyState icon={Receipt} title="No fines" compact>Fines are assessed when an overdue copy comes back.</EmptyState>}
          columns={[
            { key: "member", header: "Member", render: (f) => <NameCell name={f.displayName} sub={f.memberNumber} /> },
            { key: "kind", header: "For", render: (f) => (f.kind === "overdue" ? `Overdue ${f.days} day(s)${f.loanNumber ? ` · ${f.loanNumber}` : ""}` : "Replacement"), muted: true },
            { key: "amount", header: "Amount", render: (f) => formatInr(f.amount) },
            { key: "charge", header: "Finance", render: (f) => f.chargeNumber ?? (f.personKind === "staff" ? "Not billed (staff)" : "—"), muted: true },
            { key: "status", header: "Status", render: (f) => <StatusBadge status={f.status} /> },
            { key: "actions", header: "", render: (f) => <Actions actions={fineRowActions(f)} disabled={busy} /> },
          ]}
        />
      </BentoTile>
      <BentoTile span={6} title="Reservation queue" data-slice="8C">
        <DataTable<LibraryReservation>
          caption="Reservations"
          rows={reservations}
          rowKey={(r) => r.id}
          empty={<EmptyState icon={BookMarked} title="No reservations" compact>Members reserve titles that are out on loan; a returned copy is held for the first in the queue.</EmptyState>}
          columns={[
            { key: "title", header: "Title", render: (r) => r.title },
            { key: "member", header: "Member", render: (r) => <NameCell name={r.displayName} sub={r.memberNumber} />, muted: true },
            { key: "queue", header: "Queue", render: (r) => (r.status === "ready" ? `${r.accessionNumber ?? "Held"} · until ${fmtDate(r.holdUntil)}` : `#${r.currentPosition ?? r.queuePosition}`), muted: true },
            { key: "status", header: "Status", render: (r) => <StatusBadge status={r.status} /> },
            { key: "actions", header: "", render: (r) => <Actions actions={reservationRowActions(r)} disabled={busy} /> },
          ]}
        />
      </BentoTile>
      <BentoTile
        span={6}
        title="Clearance"
        data-slice="8C"
        action={
          <Button size="sm" data-action="clearance-new" onClick={requestClearance} disabled={!members.length}>
            Check clearance
          </Button>
        }
      >
        <DataTable<Clearance>
          caption="Library clearances"
          rows={clearances}
          rowKey={(c) => c.id}
          empty={<EmptyState icon={ClipboardCheck} title="No clearance requests" compact>A member is cleared with no loans out and no unsettled fines.</EmptyState>}
          columns={[
            { key: "member", header: "Member", render: (c) => <NameCell name={c.displayName} sub={c.memberNumber} /> },
            { key: "purpose", header: "Purpose", render: (c) => c.purpose, muted: true },
            { key: "check", header: "Check", render: (c) => (c.verification.blockers.length ? c.verification.blockers.join("; ") : "No loans or fines outstanding"), muted: true },
            { key: "status", header: "Status", render: (c) => <StatusBadge status={c.status} /> },
            { key: "actions", header: "", render: (c) => <Actions actions={clearanceRowActions(c)} disabled={busy} /> },
          ]}
        />
      </BentoTile>
      {prompt}
      {dialog}
    </>
  );
}

/** Admin → Campus → Library. */
export function LibraryAdminTab({ institution }: { institution: Institution | null }) {
  if (!institution) return <NoInstitutionTile title="Library" />;
  return <LibraryScreen institutionId={institution.id} />;
}

// ================================================================== Student → Library

/** Student → Library: My library (loans, catalogue search + reservations, fines, complaints). */
export function MyLibraryPanel() {
  const api = useLibraryApi();
  const { run, submit, busy, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const [q, setQ] = useState("");
  const mine = useQuery({ queryKey: lkeys("me", "library"), queryFn: () => api.mine() });
  const m = mine.data;
  const member = !!m?.membership;
  const results = useQuery({ queryKey: lkeys("me", "catalogue", q), queryFn: () => api.myCatalogue(q || undefined), enabled: member }).data ?? [];
  // `me/library` lists loan history newest first: the panel shows what is out now.
  const loans = (m?.loans ?? []).filter((l) => OPEN_LOAN.includes(l.status));
  const fines = m?.fines ?? [];
  const reservations = m?.reservations ?? [];
  const due = sumAmounts(fines.filter((f) => UNSETTLED.includes(f.status)).map((f) => f.amount));
  const clearance = m?.clearance ?? null;

  const requestClearance = () =>
    ask({
      title: "Request library clearance",
      name: "my-clearance",
      fields: [{ name: "purpose", label: "Purpose", required: true, initial: "Programme completion", wide: true }],
      submitLabel: "Request",
      onSubmit: (v) => submit(() => api.myClearance(v.purpose!), (c) => `Clearance ${statusLabel(c.status).toLowerCase()}`),
    });

  const complain = () =>
    ask({
      title: "Report a library problem",
      name: "library-complaint",
      fields: [
        { name: "title", label: "What is wrong?", required: true, wide: true },
        { name: "description", label: "Details", wide: true },
      ],
      submitLabel: "Send",
      onSubmit: (v) => submit(() => api.complain(v.title!, v.description), (t) => `Ticket ${t.number} opened`),
    });

  if (mine.isError)
    return (
      <BentoTile span={12} title="My library" data-slice="8C" data-my-library>
        <EmptyState icon={Library} title="Your library record is not available" compact>
          The library could not find a student record for this sign-in.
        </EmptyState>
      </BentoTile>
    );

  if (mine.isSuccess && !member)
    return (
      <BentoTile span={12} title="My library" data-slice="8C" data-my-library>
        <EmptyState icon={Library} title="You are not a library member yet" compact>
          The library office adds students as members once the borrowing policy is active.
        </EmptyState>
      </BentoTile>
    );

  const settled = due === "0.00" || due === "0";
  const sideTile = reservations.length > 0 || fines.length > 0 || !!clearance;
  return (
    <>
      <BentoTile
        span={12}
        title="Borrowed"
        data-slice="8C"
        data-my-library
        data-kpi="my-loans"
        action={
          <>
            <span data-kpi="my-fines" style={{ fontSize: 13, fontWeight: 500, color: settled ? "var(--sb-mute)" : "var(--tint-rose-fg)" }}>
              Fines due <b>{formatInr(due)}</b>
            </span>
            <Button size="sm" variant="ghost" data-action="library-complaint" onClick={complain}>
              Report a problem
            </Button>
            {clearance?.status !== "approved" && (
              <Button size="sm" variant="ghost" data-action="my-clearance" onClick={requestClearance} disabled={busy}>
                Request clearance
              </Button>
            )}
          </>
        }
      >
        {loans.length ? (
          <div style={{ display: "flex", flexDirection: "column", gap: 4 }}>
            {loans.map((l) => (
              <ListRow
                key={l.id}
                icon={BookOpen}
                tone={l.status === "overdue" ? "rose" : "amber"}
                title={l.title}
                meta={[l.accessionNumber, `Due ${fmtDate(l.dueOn)}`, l.fineAmount ? `fine ${formatInr(l.fineAmount)}` : null].filter(Boolean).join(" · ")}
                badge={l.status === "active" ? undefined : { label: statusLabel(l.status), kind: statusKind(l.status) }}
                primary={l.status === "active" ? { label: "Renew", icon: CalendarPlus, ghost: true, disabled: busy, onClick: () => void run(() => api.renewMine(l.id, l.version), (r) => `Due ${fmtDate(r.dueOn)}`), "data-action": "my-renew" } : undefined}
                data-loan={l.title}
              />
            ))}
          </div>
        ) : (
          <EmptyState icon={BookOpen} title="Nothing on loan" compact>
            Borrowed books and their due dates appear here.
          </EmptyState>
        )}
      </BentoTile>
      <BentoTile
        span={12}
        title="Catalogue"
        data-slice="8C"
        data-catalogue=""
        action={<SearchField label="Search title or author" placeholder="Search title or author" value={q} onChange={(e) => setQ(e.target.value)} data-library-search />}
      >
        {results.length ? (
          <div style={{ display: "flex", flexDirection: "column", gap: 4 }}>
            {results.map((r) => {
              const available = r.availableCopies > 0;
              return (
                <ListRow
                  key={r.id}
                  icon={BookCopy}
                  tone={available ? "teal" : "grey"}
                  title={r.title}
                  meta={[r.authors.join(", ") || null, r.copies ? `${r.availableCopies} of ${plural(r.copies, "copy", "copies")} on shelf` : null].filter(Boolean).join(" · ")}
                  badge={{ label: available ? "Available" : r.copies ? "Issued" : "No copies", kind: available ? "done" : "neutral" }}
                  primary={!available && r.copies > 0 ? { label: "Reserve", icon: BookmarkPlus, ghost: true, disabled: busy, onClick: () => void run(() => api.reserve(r.id), (x) => `Reserved · position ${x.queuePosition}`), "data-action": "my-reserve" } : undefined}
                  data-title={r.title}
                />
              );
            })}
          </div>
        ) : (
          <EmptyState icon={LibraryBig} title="No titles found" compact>
            Try another title or author.
          </EmptyState>
        )}
      </BentoTile>
      {sideTile && (
        <BentoTile span={12} title="Reservations & fines" data-slice="8C">
          {reservations.length > 0 && (
            <DataTable<LibraryReservation>
              caption="My reservations"
              rows={reservations}
              rowKey={(r) => r.id}
              columns={[
                { key: "title", header: "Reserved", render: (r) => r.title },
                { key: "pos", header: "Queue", render: (r) => (r.status === "ready" ? `Collect by ${fmtDate(r.holdUntil)}` : `#${r.currentPosition ?? r.queuePosition}`), muted: true },
                { key: "status", header: "Status", render: (r) => <StatusBadge status={r.status} /> },
                {
                  key: "actions",
                  header: "",
                  render: (r) => (
                    <Actions
                      actions={r.status === "queued" || r.status === "ready" ? [{ id: "my-reservation-cancel", label: "Cancel", onClick: () => void run(() => api.cancelMine(r.id, r.version), `Reservation of ${r.title} cancelled`) }] : []}
                      disabled={busy}
                    />
                  ),
                },
              ]}
            />
          )}
          {fines.length > 0 && (
            <DataTable<Fine>
              caption="My fines"
              rows={fines}
              rowKey={(f) => f.id}
              columns={[
                { key: "for", header: "Fine", render: (f) => (f.kind === "overdue" ? `${plural(f.days, "day")} late` : "Replacement") },
                { key: "amount", header: "Amount", render: (f) => formatInr(f.amount) },
                { key: "status", header: "Status", render: (f) => <StatusBadge status={f.status} /> },
              ]}
            />
          )}
          {clearance && (
            <DataTable<Clearance>
              caption="My clearance"
              rows={[clearance]}
              rowKey={(c) => c.id}
              columns={[
                { key: "purpose", header: "Clearance", render: (c) => c.purpose },
                { key: "loans", header: "Loans out", render: (c) => formatNumber(c.verification.activeLoans), muted: true },
                { key: "fines", header: "Unsettled fines", render: (c) => formatInr(c.verification.unsettledFines), muted: true },
                { key: "why", header: "Notes", render: (c) => c.statusReason ?? (c.verification.blockers.join("; ") || "Clear"), muted: true },
                { key: "status", header: "Status", render: (c) => <StatusBadge status={c.status} /> },
              ]}
            />
          )}
        </BentoTile>
      )}
      {prompt}
      {dialog}
    </>
  );
}

/** Student → Library header line: "3 books borrowed · 9 of 15 titles available · fines ₹10". */
export function useMyLibrarySummary(enabled = true): string | undefined {
  const api = useLibraryApi();
  const mine = useQuery({ queryKey: lkeys("me", "library"), queryFn: () => api.mine(), enabled });
  const member = !!mine.data?.membership;
  const titles = useQuery({ queryKey: lkeys("me", "catalogue", ""), queryFn: () => api.myCatalogue(undefined), enabled: enabled && member }).data;
  if (!mine.data) return undefined;
  if (!member) return "Not a library member yet";
  const out = (mine.data.loans ?? []).filter((l) => OPEN_LOAN.includes(l.status)).length;
  const due = sumAmounts((mine.data.fines ?? []).filter((f) => UNSETTLED.includes(f.status)).map((f) => f.amount));
  const parts = [`${plural(out, "book")} borrowed`];
  if (titles) parts.push(`${formatNumber(titles.filter((t) => t.availableCopies > 0).length)} of ${plural(titles.length, "title")} available`);
  if (due !== "0.00" && due !== "0") parts.push(`fines due ${formatInr(due)}`);
  return parts.join(" · ");
}
