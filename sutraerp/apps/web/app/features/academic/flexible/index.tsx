/**
 * Slice **4C** web components (flexible / external learning, credit recognition, course
 * resources). The export names are imported by the route frames (docs/batch4-contracts.md §10).
 *
 * - `FlexibleLearningAdminTab`, `CreditRecognitionAdminTab` — Admin → Academic delivery tabs.
 * - `StudentLibraryScreen` — student `/library` (course resources and library links, plus the
 *   student's flexible-learning requests).
 * - `FacultyResourcesScreen` — faculty `/resources`.
 */
import { Award, BookOpen, ExternalLink, GraduationCap, Library, Plus, Scale } from "lucide-react";
import { BentoTile, Button, DataTable, EmptyState, Select } from "@sutra/ui";
import { useQueries, useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { Actions, fmtDate, NoInstitution, REASON, todayIso, useAsk, type RowAction } from "../../institution/forms";
import { StatusBadge, useCommand, useInstitutionApi } from "../../institution/hooks";
import type { Institution } from "../../institution/types";
import { parseProviderRecords } from "./api";
import { fkeys, useFlexibleApi } from "./hooks";
import type { FlexibleEnrolment, LearningMode, LearningOpportunity, ResourceKind } from "./types";

export { FlexibleApiProvider } from "./hooks";

const MODES: { value: LearningMode; label: string }[] = [
  { value: "mooc", label: "MOOC" },
  { value: "swayam", label: "SWAYAM" },
  { value: "nptel", label: "NPTEL" },
  { value: "internship", label: "Internship" },
  { value: "external_course", label: "External course" },
  { value: "open_elective", label: "Open elective" },
];
const KINDS: { value: ResourceKind; label: string }[] = [
  { value: "reading", label: "Reading" },
  { value: "document", label: "Document" },
  { value: "link", label: "Link" },
  { value: "video", label: "Video" },
  { value: "library_item", label: "Library item" },
];
const modeLabel = (m: string) => MODES.find((x) => x.value === m)?.label ?? m;

/** Published terms of the institution's calendars. */
function useTerms(institutionId: string | null | undefined) {
  const inst = useInstitutionApi();
  const cals = useQuery({ queryKey: fkeys.scoped(institutionId ?? "-", "calendars"), queryFn: () => inst.listCalendars(institutionId!), enabled: !!institutionId });
  const versions = useQueries({ queries: (cals.data ?? []).map((c) => ({ queryKey: fkeys.scoped(c.id, "calendar-versions"), queryFn: () => inst.listVersions(c.id) })) });
  return versions
    .flatMap((q) => q.data ?? [])
    .filter((v) => v.status === "published")
    .flatMap((v) => v.terms.map((t) => ({ value: t.id, label: `${t.name} (${t.code})` })));
}

function NoInstitutionTile() {
  return (
    <BentoTile span={12} data-slice="4C">
      <NoInstitution />
    </BentoTile>
  );
}

// ================================================================== admin: flexible learning

export function FlexibleLearningAdminTab({ institution }: { institution: Institution | null }) {
  const api = useFlexibleApi();
  const { run, submit, busy, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const terms = useTerms(institution?.id);
  const [selected, setSelected] = useState<string | null>(null);
  const iid = institution?.id ?? "-";
  const policies = useQuery({ queryKey: fkeys.scoped(iid, "policies"), queryFn: () => api.listPolicies(iid), enabled: !!institution });
  const providers = useQuery({ queryKey: fkeys.scoped(iid, "providers"), queryFn: () => api.listProviders(iid), enabled: !!institution });
  const opps = useQuery({ queryKey: fkeys.scoped(iid, "opportunities"), queryFn: () => api.listOpportunities(iid), enabled: !!institution });
  const enrolments = useQuery({ queryKey: fkeys.scoped(selected ?? "-", "enrolments"), queryFn: () => api.listEnrolments(selected!), enabled: !!selected });
  const exceptions = useQuery({ queryKey: fkeys.scoped(iid, "exceptions"), queryFn: () => api.listExceptions(iid), enabled: !!institution });
  if (!institution) return <NoInstitutionTile />;
  const effective = policies.data?.find((p) => p.status === "effective");
  const current = opps.data?.find((o) => o.id === selected) ?? null;

  const oppActions = (o: LearningOpportunity): RowAction[] => {
    const a: RowAction[] = [{ id: "view", label: "Demand", onClick: () => setSelected(o.id) }];
    if (o.status === "draft") a.push({ id: "submit-opportunity", label: "Activate", variant: "primary", onClick: () => run(() => api.opportunityCommand(o.id, o.version, "submit"), (r) => `Opportunity ${r.status.replace("_", " ")}`) });
    if (o.status === "active") {
      a.push({ id: "allocate", label: "Allocate seats", variant: "primary", onClick: () => run(() => api.seatCommand(o.id, o.version, "allocate"), (r) => `${r.allocated.length} allocated, ${r.waitlisted.length} waitlisted`) });
      a.push({ id: "promote", label: "Promote waitlist", onClick: () => run(() => api.seatCommand(o.id, o.version, "promote"), (r) => `${r.allocated.length} promoted`) });
      a.push({ id: "suspend-opportunity", label: "Suspend", onClick: () => ask({ title: `Suspend ${o.code}`, name: "suspend", submitLabel: "Suspend", fields: [REASON], onSubmit: (v) => submit(() => api.opportunityCommand(o.id, o.version, "suspend", v.reason), "Opportunity suspended") }) });
    }
    if (o.overAllocated) a.push({ id: "resolve-conflicts", label: "Resolve conflict", variant: "accent", onClick: () => run(() => api.seatCommand(o.id, o.version, "resolve"), (r) => `${r.released.length} allocation(s) released`) });
    if (o.status === "suspended") a.push({ id: "reactivate-opportunity", label: "Reactivate", onClick: () => run(() => api.opportunityCommand(o.id, o.version, "reactivate"), "Opportunity reactivated") });
    if (o.status !== "archived" && o.status !== "pending_approval")
      a.push({
        id: "capacity",
        label: "Capacity",
        onClick: () =>
          ask({
            title: `Capacity · ${o.code}`,
            name: "capacity",
            submitLabel: "Save",
            fields: [
              { name: "capacity", label: "Capacity", type: "number", required: true, initial: String(o.capacity) },
              { name: "reservedSeats", label: "Reserved seats", type: "number", initial: String(o.reservedSeats) },
            ],
            onSubmit: (v) => submit(() => api.setCapacity(o.id, o.version, Number(v.capacity), Number(v.reservedSeats || 0)), "Capacity saved"),
          }),
      });
    if (o.status !== "archived" && o.status !== "pending_approval") a.push({ id: "archive-opportunity", label: "Archive", onClick: () => run(() => api.opportunityCommand(o.id, o.version, "archive"), "Opportunity archived") });
    return a;
  };

  const enrolmentActions = (e: FlexibleEnrolment): RowAction[] => {
    const a: RowAction[] = [];
    const external = current && current.mode !== "open_elective";
    if (e.status === "requested") {
      a.push({ id: "approve-enrolment", label: "Approve", variant: "primary", onClick: () => run(() => api.enrolmentDecision(e.id, e.version, true), "Request approved") });
      a.push({ id: "reject-enrolment", label: "Reject", onClick: () => ask({ title: "Reject request", name: "reject", submitLabel: "Reject", fields: [REASON], onSubmit: (v) => submit(() => api.enrolmentDecision(e.id, e.version, false, v.reason), "Request rejected") }) });
    }
    if (e.status === "allocated") {
      const pending = e.statusReason === "provider registration requested";
      if (!pending) a.push({ id: "register", label: external ? "Send to provider" : "Register", variant: "primary", onClick: () => run(() => api.register(e.id, e.version), "Registration handed off") });
      else
        a.push({
          id: "registration-outcome",
          label: "Provider reply",
          variant: "primary",
          onClick: () =>
            ask({
              title: "Provider registration outcome",
              name: "registration-outcome",
              submitLabel: "Record",
              fields: [
                { name: "outcome", label: "Outcome", type: "select", required: true, initial: "accepted", options: [{ value: "accepted", label: "Accepted" }, { value: "rejected", label: "Rejected" }, { value: "recovery_required", label: "Recovery required" }] },
                { name: "providerReference", label: "Provider enrolment reference" },
                { name: "reason", label: "Reason (when unsuccessful)", wide: true },
              ],
              onSubmit: (v) => submit(() => api.registrationOutcome(e.id, e.version, v.outcome!, v.providerReference || undefined, v.reason || undefined), "Provider outcome recorded"),
            }),
        });
    }
    if (e.status === "registered" || e.status === "in_progress")
      a.push({
        id: "complete",
        label: "Record completion",
        variant: "primary",
        onClick: () =>
          ask({
            title: `Learning completion · ${e.displayName}`,
            name: "completion",
            submitLabel: "Record",
            fields: [
              { name: "hoursCompleted", label: "Hours completed", type: "number", required: true, initial: String(current?.minHours ?? 0) },
              { name: "assessmentStatus", label: "Assessment", type: "select", required: true, initial: "passed", options: [{ value: "passed", label: "Passed" }, { value: "not_applicable", label: "Not applicable" }, { value: "failed", label: "Failed" }] },
              { name: "providerEvidenceRef", label: "Provider completion evidence" },
              { name: "grade", label: "Grade" },
            ],
            onSubmit: (v) =>
              submit(() => api.complete(e.id, e.version, { hoursCompleted: Number(v.hoursCompleted), assessmentStatus: v.assessmentStatus!, providerEvidenceRef: v.providerEvidenceRef || undefined, grade: v.grade || undefined }), "Completion recorded"),
          }),
      });
    if (e.status === "completed" || e.status === "registered" || e.status === "in_progress")
      a.push({
        id: "certificate",
        label: "Add certificate",
        onClick: () =>
          ask({
            title: "Certificate evidence",
            name: "certificate",
            submitLabel: "Record",
            fields: [
              { name: "certificateNumber", label: "Certificate number", required: true },
              { name: "certificateUrl", label: "Certificate link", placeholder: "https://…", wide: true },
              { name: "issuedOn", label: "Issued on", type: "date", initial: todayIso() },
            ],
            onSubmit: (v) => submit(() => api.submitCertificate(e.id, { certificateNumber: v.certificateNumber!, certificateUrl: v.certificateUrl || undefined, issuedOn: v.issuedOn || undefined }), "Certificate recorded"),
          }),
      });
    if (e.status === "completed") a.push({ id: "request-recognition", label: "Request credit", onClick: () => run(() => api.requestRecognition(e.id), (r) => `Recognition ${r.status.replace("_", " ")}`) });
    if (!["completed", "withdrawn", "rejected"].includes(e.status))
      a.push({ id: "remove", label: "Remove", onClick: () => ask({ title: "Remove from the opportunity", name: "remove", submitLabel: "Remove", fields: [REASON], onSubmit: (v) => submit(() => api.withdraw(e.id, e.version, v.reason!), "Removed") }) });
    return a;
  };

  return (
    <>
      <BentoTile
        span={12}
        title="Flexible-learning policy"
        data-slice="4C"
        action={
          <Button
            size="sm"
            icon={<Plus strokeWidth={1.75} />}
            data-new-policy
            onClick={() =>
              ask({
                title: "New policy version",
                name: "policy",
                submitLabel: "Draft",
                fields: [
                  { name: "name", label: "Name", required: true, initial: "Flexible learning policy", wide: true },
                  { name: "maxConcurrentEnrolments", label: "Live enrolments per student", type: "number", required: true, initial: "2" },
                  { name: "maxCreditsPerTerm", label: "Flexible credits per term", type: "number", required: true, initial: "8" },
                  { name: "waitlistValidityDays", label: "Waitlist validity (days)", type: "number", required: true, initial: "14" },
                  { name: "effectiveFrom", label: "Effective from", type: "date", required: true, initial: todayIso() },
                  { name: "requireHumanApproval", label: "Every request needs an academic decision", type: "checkbox" },
                ],
                onSubmit: (v) =>
                  submit(
                    () =>
                      api.createPolicy(institution.id, {
                        name: v.name!,
                        effectiveFrom: v.effectiveFrom!,
                        rules: {
                          maxConcurrentEnrolments: Number(v.maxConcurrentEnrolments),
                          maxCreditsPerTerm: Number(v.maxCreditsPerTerm),
                          waitlistValidityDays: Number(v.waitlistValidityDays),
                          requireHumanApproval: v.requireHumanApproval === "true",
                          withdrawalAllowedUntil: "registered",
                        },
                      }),
                    "Policy version drafted",
                  ),
              })
            }
          >
            New version
          </Button>
        }
      >
        <p data-effective-policy={effective?.id ?? ""}>
          {effective
            ? `Version ${effective.versionNo} in force since ${fmtDate(effective.effectiveFrom)}: ${effective.rules.maxConcurrentEnrolments} live enrolments, ${effective.rules.maxCreditsPerTerm} credits per term, ${effective.rules.requireHumanApproval ? "academic review of every request" : "approval by policy"}.`
            : "No policy is in force yet — requests are refused until one is published."}
        </p>
        <DataTable
          caption="Policy versions"
          rows={policies.data ?? []}
          rowKey={(p) => p.id}
          empty={<EmptyState icon={Scale} title="No policy versions">Draft the first version.</EmptyState>}
          columns={[
            { key: "v", header: "Version", render: (p) => `v${p.versionNo} · ${p.name}` },
            { key: "f", header: "Effective", render: (p) => `${fmtDate(p.effectiveFrom)}${p.effectiveTo ? ` – ${fmtDate(p.effectiveTo)}` : ""}`, muted: true },
            { key: "s", header: "Status", render: (p) => <StatusBadge status={p.status} /> },
            { key: "a", header: "", render: (p) => (p.status === "draft" ? <Actions disabled={busy} actions={[{ id: "publish-policy", label: "Publish", variant: "primary", onClick: () => run(() => api.submitPolicy(p.id, p.version), "Policy published") }]} /> : null) },
          ]}
        />
      </BentoTile>

      <BentoTile
        span={12}
        title="Learning providers"
        action={
          <Button
            size="sm"
            icon={<Plus strokeWidth={1.75} />}
            data-new-provider
            onClick={() =>
              ask({
                title: "Onboard a learning provider",
                name: "provider",
                submitLabel: "Propose",
                fields: [
                  { name: "code", label: "Code", required: true, placeholder: "NPTEL" },
                  { name: "name", label: "Name", required: true },
                  { name: "providerKind", label: "Kind", type: "select", required: true, initial: "mooc_platform", options: [{ value: "mooc_platform", label: "MOOC platform" }, { value: "government_platform", label: "Government platform" }, { value: "university", label: "University" }, { value: "industry", label: "Industry" }, { value: "other", label: "Other" }] },
                  { name: "mode", label: "Approved mode", type: "select", required: true, initial: "mooc", options: MODES.filter((m) => m.value !== "open_elective") },
                  { name: "accreditationRef", label: "Accreditation / approval reference", required: true },
                  { name: "accreditationValidUntil", label: "Accreditation valid until", type: "date", required: true },
                ],
                onSubmit: (v) =>
                  submit(
                    () => api.createProvider(institution.id, { code: v.code!, name: v.name!, providerKind: v.providerKind!, modes: [v.mode as LearningMode], accreditationRef: v.accreditationRef, accreditationValidUntil: v.accreditationValidUntil }),
                    "Provider proposed",
                  ),
              })
            }
          >
            Onboard provider
          </Button>
        }
      >
        <DataTable
          caption="Learning providers"
          rows={providers.data ?? []}
          rowKey={(p) => p.id}
          empty={<EmptyState icon={GraduationCap} title="No providers">MOOC platforms, SWAYAM/NPTEL, universities and internship hosts are onboarded here.</EmptyState>}
          columns={[
            { key: "n", header: "Provider", render: (p) => <span data-provider={p.code}>{`${p.code} · ${p.name}`}</span> },
            { key: "m", header: "Scope", render: (p) => p.modes.map(modeLabel).join(", "), muted: true },
            { key: "acc", header: "Accreditation", render: (p) => `${p.accreditationRef ?? "—"} · until ${fmtDate(p.accreditationValidUntil)}`, muted: true },
            { key: "s", header: "Status", render: (p) => <StatusBadge status={p.status} /> },
            {
              key: "a",
              header: "",
              render: (p) => (
                <Actions
                  disabled={busy}
                  actions={[
                    ...(p.status === "proposed" ? [{ id: "submit-provider", label: "Submit for onboarding", variant: "primary" as const, onClick: () => run(() => api.providerCommand(p.id, p.version, "submit"), (r) => `Provider ${r.status.replace("_", " ")}`) }] : []),
                    ...(p.status === "active" ? [{ id: "suspend-provider", label: "Suspend", onClick: () => ask({ title: `Suspend ${p.code}`, name: "suspend-provider", submitLabel: "Suspend", fields: [REASON], onSubmit: (v) => submit(() => api.providerCommand(p.id, p.version, "suspend", v.reason), "Provider suspended") }) }] : []),
                    ...(p.status === "suspended" ? [{ id: "reactivate-provider", label: "Reactivate", onClick: () => run(() => api.providerCommand(p.id, p.version, "reactivate"), "Provider reactivated") }] : []),
                  ]}
                />
              ),
            },
          ]}
        />
      </BentoTile>

      <BentoTile
        span={12}
        title="Learning opportunities"
        action={
          <Button
            size="sm"
            icon={<Plus strokeWidth={1.75} />}
            data-new-opportunity
            onClick={() =>
              ask({
                title: "New learning opportunity",
                name: "opportunity",
                submitLabel: "Establish",
                fields: [
                  { name: "code", label: "Code", required: true, placeholder: "NPTEL-ML" },
                  { name: "title", label: "Title", required: true, wide: true },
                  { name: "mode", label: "Mode", type: "select", required: true, initial: "mooc", options: MODES.filter((m) => m.value !== "open_elective") },
                  { name: "providerId", label: "Provider", type: "select", required: true, initial: providers.data?.find((p) => p.status === "active")?.id ?? "", options: (providers.data ?? []).filter((p) => p.status === "active").map((p) => ({ value: p.id, label: p.name })) },
                  { name: "academicTermId", label: "Academic term", type: "select", required: true, initial: terms[0]?.value ?? "", options: terms },
                  { name: "credits", label: "Credits", type: "number", required: true, initial: "4" },
                  { name: "minHours", label: "Minimum hours", type: "number", initial: "30" },
                  { name: "capacity", label: "Seats", type: "number", required: true, initial: "30" },
                  { name: "enrolmentOpensOn", label: "Enrolment opens", type: "date", required: true, initial: todayIso() },
                  { name: "enrolmentClosesOn", label: "Enrolment closes", type: "date", required: true },
                ],
                onSubmit: (v) =>
                  submit(
                    () =>
                      api.createOpportunity(institution.id, {
                        code: v.code!,
                        title: v.title!,
                        mode: v.mode as LearningMode,
                        providerId: v.providerId || null,
                        academicTermId: v.academicTermId!,
                        credits: Number(v.credits),
                        minHours: v.minHours ? Number(v.minHours) : null,
                        capacity: Number(v.capacity),
                        enrolmentOpensOn: v.enrolmentOpensOn || null,
                        enrolmentClosesOn: v.enrolmentClosesOn || null,
                      }),
                    "Opportunity established",
                  ),
              })
            }
          >
            New opportunity
          </Button>
        }
      >
        <DataTable
          caption="Learning opportunities"
          rows={opps.data ?? []}
          rowKey={(o) => o.id}
          empty={<EmptyState icon={BookOpen} title="No opportunities">Open electives, MOOCs, SWAYAM/NPTEL courses and internships appear here.</EmptyState>}
          columns={[
            { key: "c", header: "Opportunity", render: (o) => <span data-opportunity={o.code}>{`${o.code} · ${o.title}`}</span> },
            { key: "m", header: "Mode", render: (o) => `${modeLabel(o.mode)}${o.providerName ? ` · ${o.providerName}` : o.offeringCode ? ` · ${o.offeringCode}` : ""}`, muted: true },
            { key: "seats", header: "Seats", render: (o) => <span data-seats={o.id}>{`${o.seatsAllocated}/${o.capacity - o.reservedSeats}${o.waitlistCount ? ` · ${o.waitlistCount} waiting` : ""}${o.overAllocated ? " · over capacity" : ""}`}</span> },
            { key: "cr", header: "Credits", render: (o) => String(o.credits), muted: true },
            { key: "s", header: "Status", render: (o) => <StatusBadge status={o.status} /> },
            { key: "a", header: "", render: (o) => <Actions disabled={busy} actions={oppActions(o)} /> },
          ]}
        />
      </BentoTile>

      {current && (
        <BentoTile span={12} title={`Demand · ${current.code}`} data-demand={current.id}>
          <DataTable
            caption="Flexible enrolments"
            rows={enrolments.data ?? []}
            rowKey={(e) => e.id}
            empty={<EmptyState icon={GraduationCap} title="No requests yet">Students request this opportunity from their Library screen.</EmptyState>}
            columns={[
              { key: "st", header: "Student", render: (e) => <span data-enrolment={e.studentNumber}>{`${e.displayName} (${e.studentNumber})`}</span> },
              { key: "p", header: "Priority", render: (e) => String(e.priority), muted: true },
              { key: "s", header: "Status", render: (e) => <span data-enrolment-status={e.status}><StatusBadge status={e.status} />{e.waitlistPosition ? ` #${e.waitlistPosition}` : ""}</span> },
              { key: "r", header: "Note", render: (e) => e.statusReason ?? "—", muted: true },
              { key: "a", header: "", render: (e) => <Actions disabled={busy} actions={enrolmentActions(e)} /> },
            ]}
          />
        </BentoTile>
      )}

      <BentoTile
        span={12}
        title="Exception cases"
        action={
          <Button
            size="sm"
            icon={<Plus strokeWidth={1.75} />}
            data-new-exception
            onClick={() =>
              ask({
                title: "Raise an exception case",
                name: "exception",
                submitLabel: "Raise",
                fields: [
                  { name: "category", label: "Category", type: "select", required: true, initial: "registration", options: ["allocation", "registration", "completion", "certificate", "recognition", "reconciliation", "other"].map((c) => ({ value: c, label: c })) },
                  { name: "title", label: "Title", required: true, wide: true },
                  { name: "description", label: "What happened", wide: true },
                ],
                onSubmit: (v) => submit(() => api.raiseException(institution.id, { category: v.category!, title: v.title!, description: v.description || undefined }), "Exception case raised"),
              })
            }
          >
            Raise case
          </Button>
        }
      >
        <DataTable
          caption="Exception cases"
          rows={exceptions.data ?? []}
          rowKey={(x) => x.id}
          empty={<EmptyState icon={Scale} title="No exception cases">Failures in allocation, registration, completion or recognition are tracked to closure here.</EmptyState>}
          columns={[
            { key: "t", header: "Case", render: (x) => <span data-exception={x.id}>{`${x.title} · ${x.category}`}</span> },
            { key: "s", header: "Status", render: (x) => <StatusBadge status={x.status} /> },
            {
              key: "a",
              header: "",
              render: (x) => (
                <Actions
                  disabled={busy}
                  actions={[
                    ...(x.status === "open" ? [{ id: "plan-exception", label: "Propose plan", onClick: () => ask({ title: "Corrective plan", name: "plan", submitLabel: "Submit", fields: [{ name: "correctivePlan", label: "Plan", required: true, wide: true }], onSubmit: (v) => submit(() => api.planException(x.id, x.version, v.correctivePlan!), "Plan submitted") }) }] : []),
                    ...(x.status === "plan_approved"
                      ? [{ id: "resolve-exception", label: "Record correction", onClick: () => ask({ title: "Corrective action", name: "resolve", submitLabel: "Record", fields: [{ name: "correctiveAction", label: "Action taken", required: true, wide: true }, { name: "resolutionEvidence", label: "Evidence", required: true, wide: true }], onSubmit: (v) => submit(() => api.resolveException(x.id, x.version, v.correctiveAction!, v.resolutionEvidence!), "Correction recorded") }) }]
                      : []),
                    ...(x.status === "resolved" ? [{ id: "close-exception", label: "Close", variant: "primary" as const, onClick: () => run(() => api.closeException(x.id, x.version), "Case closed") }] : []),
                  ]}
                />
              ),
            },
          ]}
        />
      </BentoTile>
      {prompt}
      {dialog}
    </>
  );
}

// ================================================================== admin: credit recognition

export function CreditRecognitionAdminTab({ institution }: { institution: Institution | null }) {
  const api = useFlexibleApi();
  const { run, submit, busy, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const iid = institution?.id ?? "-";
  const certs = useQuery({ queryKey: fkeys.scoped(iid, "certificates"), queryFn: () => api.certificateQueue(iid), enabled: !!institution });
  const opps = useQuery({ queryKey: fkeys.scoped(iid, "opportunities"), queryFn: () => api.listOpportunities(iid), enabled: !!institution });
  const maps = useQuery({ queryKey: fkeys.scoped(iid, "mappings"), queryFn: () => api.listMappings(iid), enabled: !!institution });
  const recs = useQuery({ queryKey: fkeys.scoped(iid, "recognitions"), queryFn: () => api.listRecognitions(iid), enabled: !!institution });
  const runs = useQuery({ queryKey: fkeys.scoped(iid, "reconciliations"), queryFn: () => api.listReconciliations(iid), enabled: !!institution });
  if (!institution) return <NoInstitutionTile />;
  const oppCode = (id: string) => opps.data?.find((o) => o.id === id)?.code ?? id.slice(0, 8);
  const oppOptions = (opps.data ?? []).filter((o) => o.status !== "archived").map((o) => ({ value: o.id, label: `${o.code} · ${o.title}` }));
  return (
    <>
      <BentoTile span={12} title="Certificate verification" data-slice="4C">
        <DataTable
          caption="Certificates"
          rows={certs.data ?? []}
          rowKey={(c) => c.id}
          empty={<EmptyState icon={Award} title="No certificates">Certificates recorded for external learning are verified here.</EmptyState>}
          columns={[
            { key: "n", header: "Certificate", render: (c) => <span data-certificate={c.certificateNumber}>{c.certificateNumber}</span> },
            { key: "i", header: "Issued", render: (c) => fmtDate(c.issuedOn), muted: true },
            { key: "s", header: "Outcome", render: (c) => <StatusBadge status={c.status} /> },
            { key: "r", header: "Reason", render: (c) => c.decisionReason ?? "—", muted: true },
            {
              key: "a",
              header: "",
              render: (c) =>
                c.status === "submitted" || c.status === "under_verification" ? (
                  <Actions
                    disabled={busy}
                    actions={[
                      {
                        id: "verify-certificate",
                        label: "Verify",
                        variant: "primary",
                        onClick: () =>
                          ask({
                            title: `Verify ${c.certificateNumber}`,
                            name: "verify",
                            submitLabel: "Record checks",
                            fields: [
                              { name: "authenticityConfirmed", label: "Authenticity confirmed with the provider", type: "checkbox", initial: "true" },
                              { name: "identityMatched", label: "Learner identity matches", type: "checkbox", initial: "true" },
                              { name: "fraud", label: "Fraud indicators (if any)", wide: true },
                            ],
                            onSubmit: (v) =>
                              submit(
                                () => api.verifyCertificate(c.id, c.version, { authenticityConfirmed: v.authenticityConfirmed === "true", identityMatched: v.identityMatched === "true", fraudIndicators: v.fraud ? [v.fraud] : [] }),
                                (r) => `Certificate ${r.status.replace("_", " ")}`,
                              ),
                          }),
                      },
                    ]}
                  />
                ) : null,
            },
          ]}
        />
      </BentoTile>

      <BentoTile
        span={12}
        title="Credit mapping rules"
        action={
          <Button
            size="sm"
            icon={<Plus strokeWidth={1.75} />}
            data-new-mapping
            onClick={() =>
              ask({
                title: "New credit mapping version",
                name: "mapping",
                submitLabel: "Draft",
                fields: [
                  { name: "opportunityId", label: "Opportunity", type: "select", required: true, initial: oppOptions[0]?.value ?? "", options: oppOptions, wide: true },
                  { name: "equivalency", label: "Equivalency", type: "select", required: true, initial: "elective_credit", options: [{ value: "elective_credit", label: "Elective credit" }, { value: "additional_credit", label: "Additional credit" }] },
                  { name: "credits", label: "Credits", type: "number", required: true, initial: "4" },
                  { name: "effectiveFrom", label: "Effective from", type: "date", required: true, initial: todayIso() },
                ],
                onSubmit: (v) => submit(() => api.createMapping(institution.id, { opportunityId: v.opportunityId!, equivalency: v.equivalency!, credits: Number(v.credits), effectiveFrom: v.effectiveFrom! }), "Mapping drafted"),
              })
            }
          >
            New mapping
          </Button>
        }
      >
        <DataTable
          caption="Credit mapping rules"
          rows={maps.data ?? []}
          rowKey={(m) => m.id}
          empty={<EmptyState icon={Scale} title="No mapping rules">Map each opportunity to the credits it earns before recognising credit.</EmptyState>}
          columns={[
            { key: "o", header: "Opportunity", render: (m) => <span data-mapping={oppCode(m.opportunityId)}>{`${oppCode(m.opportunityId)} · v${m.versionNo}`}</span> },
            { key: "e", header: "Equivalency", render: (m) => `${m.equivalency.replace(/_/g, " ")} · ${m.credits} cr`, muted: true },
            { key: "s", header: "Status", render: (m) => <StatusBadge status={m.status} /> },
            { key: "a", header: "", render: (m) => (m.status === "draft" ? <Actions disabled={busy} actions={[{ id: "publish-mapping", label: "Publish", variant: "primary", onClick: () => run(() => api.submitMapping(m.id, m.version), "Mapping published") }]} /> : null) },
          ]}
        />
      </BentoTile>

      <BentoTile span={12} title="Credit recognition">
        <DataTable
          caption="Credit recognitions"
          rows={recs.data ?? []}
          rowKey={(r) => r.id}
          empty={<EmptyState icon={Award} title="No recognitions">Request credit from a completed enrolment in Flexible learning.</EmptyState>}
          columns={[
            { key: "o", header: "Opportunity", render: (r) => <span data-recognition={r.id}>{oppCode(r.opportunityId)}</span> },
            { key: "c", header: "Credits", render: (r) => (r.credits < r.requestedCredits ? `${r.credits} of ${r.requestedCredits}` : String(r.credits)) },
            { key: "s", header: "Status", render: (r) => <span data-recognition-status={r.status}><StatusBadge status={r.status} /></span> },
            { key: "l", header: "Ledger entry", render: (r) => (r.ledgerEntryId ? r.ledgerEntryId.slice(0, 8) : "—"), muted: true },
            {
              key: "a",
              header: "",
              render: (r) => (
                <Actions
                  disabled={busy}
                  actions={[
                    ...(r.status === "under_review"
                      ? [
                          { id: "approve-recognition", label: "Approve", variant: "primary" as const, onClick: () => run(() => api.decideRecognition(r.id, r.version, true), "Recognition approved") },
                          { id: "reject-recognition", label: "Reject", onClick: () => ask({ title: "Reject recognition", name: "reject-recognition", submitLabel: "Reject", fields: [REASON], onSubmit: (v) => submit(() => api.decideRecognition(r.id, r.version, false, undefined, v.reason), "Recognition rejected") }) },
                        ]
                      : []),
                    ...(r.status === "approved" ? [{ id: "post-recognition", label: "Post to credit record", variant: "primary" as const, onClick: () => run(() => api.postRecognition(r.id, r.version), "Credits posted") }] : []),
                  ]}
                />
              ),
            },
          ]}
        />
      </BentoTile>

      <BentoTile
        span={12}
        title="Record reconciliation"
        action={
          <Button
            size="sm"
            icon={<Plus strokeWidth={1.75} />}
            data-new-reconciliation
            onClick={() =>
              ask({
                title: "Reconcile provider records",
                name: "reconciliation",
                submitLabel: "Compare",
                fields: [
                  { name: "opportunityId", label: "Opportunity", type: "select", required: true, initial: oppOptions[0]?.value ?? "", options: oppOptions, wide: true },
                  { name: "records", label: "Provider records (student:reference:done|open:hours; …)", required: true, wide: true },
                ],
                onSubmit: (v) => submit(() => api.runReconciliation(institution.id, v.opportunityId!, parseProviderRecords(v.records!)), (r) => (r.status === "reconciled" ? "Records consistent" : `${r.discrepancies.length} discrepancies found`)),
              })
            }
          >
            Reconcile
          </Button>
        }
      >
        <DataTable
          caption="Reconciliation runs"
          rows={runs.data ?? []}
          rowKey={(r) => r.id}
          empty={<EmptyState icon={Scale} title="No reconciliation runs">Compare provider registration and completion records with the institutional record.</EmptyState>}
          columns={[
            { key: "o", header: "Opportunity", render: (r) => <span data-reconciliation={r.id}>{oppCode(r.opportunityId)}</span> },
            { key: "d", header: "Discrepancies", render: (r) => (r.discrepancies.length ? r.discrepancies.map((d) => `${d.studentNumber}: ${d.kind.replace(/_/g, " ")}`).join("; ") : "none") },
            { key: "s", header: "Status", render: (r) => <StatusBadge status={r.status} /> },
            {
              key: "a",
              header: "",
              render: (r) =>
                r.status === "discrepancies" ? (
                  <Actions
                    disabled={busy}
                    actions={[
                      { id: "uphold", label: "Uphold our record", onClick: () => ask({ title: "Uphold the institutional record", name: "uphold", submitLabel: "Resolve", fields: [{ name: "notes", label: "Provider correction requested", required: true, wide: true }], onSubmit: (v) => submit(() => api.resolveReconciliation(r.id, r.version, "institutional_record_upheld", v.notes), "Reconciled") }) },
                      { id: "escalate", label: "Escalate", onClick: () => run(() => api.resolveReconciliation(r.id, r.version, "escalated"), "Escalated to an exception case") },
                    ]}
                  />
                ) : null,
            },
          ]}
        />
      </BentoTile>
      {prompt}
      {dialog}
    </>
  );
}

// ================================================================== student: library

export function StudentLibraryScreen() {
  const api = useFlexibleApi();
  const { run, busy, dialog } = useCommand();
  const lib = useQuery({ queryKey: fkeys.scoped("me", "library"), queryFn: () => api.myLibrary() });
  const flex = useQuery({ queryKey: fkeys.scoped("me", "flexible"), queryFn: () => api.myFlexible() });
  const mine = flex.data?.enrolments ?? [];
  const groups = new Map<string, NonNullable<typeof lib.data>>();
  for (const i of lib.data ?? []) groups.set(i.offeringCode, [...(groups.get(i.offeringCode) ?? []), i]);
  return (
    <>
      <BentoTile span={12} title="Course resources and library links" data-slice="4C" data-library>
        {groups.size === 0 ? (
          <EmptyState icon={Library} title="Nothing published yet">
            Your teachers’ reading lists and links appear here once they publish them.
          </EmptyState>
        ) : (
          [...groups.entries()].map(([code, rows]) => (
            <section key={code} data-library-course={code} style={{ marginBottom: "1rem" }}>
              <h3>
                {code} · {rows[0]!.offeringTitle}
              </h3>
              <ul style={{ listStyle: "none", padding: 0, margin: 0 }}>
                {rows.map((r) => (
                  <li key={r.id} data-library-item={r.id} style={{ padding: "0.35rem 0" }}>
                    <a href={r.url} target="_blank" rel="noreferrer noopener">
                      <ExternalLink size={14} strokeWidth={1.75} /> {r.title}
                    </a>{" "}
                    <small>
                      {KINDS.find((k) => k.value === r.kind)?.label ?? r.kind}
                      {r.sectionCode ? ` · section ${r.sectionCode}` : " · whole course"}
                    </small>
                    {r.description ? <div><small>{r.description}</small></div> : null}
                  </li>
                ))}
              </ul>
            </section>
          ))
        )}
      </BentoTile>
      <BentoTile span={12} title="Flexible & external learning" data-flexible-self>
        <DataTable
          caption="Open opportunities"
          rows={flex.data?.opportunities ?? []}
          rowKey={(o) => o.id}
          empty={<EmptyState icon={GraduationCap} title="No open opportunities">Open electives, MOOCs and internships appear here when enrolment opens.</EmptyState>}
          columns={[
            { key: "t", header: "Opportunity", render: (o) => <span data-open-opportunity={o.code}>{o.title.includes(`(${modeLabel(o.mode)})`) ? o.title : `${o.title} (${modeLabel(o.mode)})`}</span> },
            { key: "c", header: "Credits", render: (o) => String(o.credits), muted: true },
            { key: "w", header: "Enrol by", render: (o) => fmtDate(o.enrolmentClosesOn), muted: true },
            {
              key: "a",
              header: "Your request",
              render: (o) => {
                const live = mine.find((e) => e.opportunityId === o.id && !["withdrawn", "rejected"].includes(e.status));
                return live ? <StatusBadge status={live.status} /> : <Actions disabled={busy} actions={[{ id: "request-opportunity", label: "Request", variant: "primary", onClick: () => run(() => api.requestOpportunity(o.id), "Request sent") }]} />;
              },
            },
          ]}
        />
        {mine.length > 0 && <h3>My requests</h3>}
        {mine.length > 0 && (
          <DataTable
            caption="My requests"
            rows={mine}
            rowKey={(e) => e.id}
            columns={[
              { key: "t", header: "Opportunity", render: (e) => <span data-my-enrolment={e.opportunityCode ?? ""}>{e.opportunityTitle ?? e.opportunityCode}</span> },
              { key: "s", header: "Status", render: (e) => <span><StatusBadge status={e.status} />{e.waitlistPosition ? ` · #${e.waitlistPosition} on the waitlist` : ""}</span> },
              { key: "n", header: "Note", render: (e) => e.statusReason ?? "—", muted: true },
            ]}
          />
        )}
      </BentoTile>
      {dialog}
    </>
  );
}

// ================================================================== faculty: resources

export function FacultyResourcesScreen() {
  const api = useFlexibleApi();
  const { run, submit, busy, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const data = useQuery({ queryKey: fkeys.scoped("my", "resources"), queryFn: () => api.myResources() });
  const [section, setSection] = useState("");
  const sections = data.data?.sections ?? [];
  const chosen = section || sections[0]?.sectionId || "";
  const rows = (data.data?.items ?? []).filter((r) => r.sectionId === chosen);
  return (
    <>
      <BentoTile
        span={12}
        title="Course resources"
        data-slice="4C"
        action={
          chosen ? (
            <Button
              size="sm"
              icon={<Plus strokeWidth={1.75} />}
              data-new-resource
              onClick={() =>
                ask({
                  title: "Add a resource",
                  name: "resource",
                  submitLabel: "Add",
                  fields: [
                    { name: "title", label: "Title", required: true, wide: true },
                    { name: "kind", label: "Kind", type: "select", required: true, initial: "reading", options: KINDS },
                    { name: "url", label: "Link", required: true, placeholder: "https://…", wide: true },
                    { name: "description", label: "Note for students", wide: true },
                    { name: "publish", label: "Publish now", type: "checkbox", initial: "true" },
                  ],
                  onSubmit: (v) => submit(() => api.createResource(chosen, { title: v.title!, kind: v.kind as ResourceKind, url: v.url!, description: v.description || undefined, publish: v.publish === "true" }), "Resource added"),
                })
              }
            >
              Add resource
            </Button>
          ) : null
        }
      >
        {sections.length === 0 ? (
          <EmptyState icon={BookOpen} title="No classes assigned">
            Resources are added to the sections you actively teach.
          </EmptyState>
        ) : (
          <>
            <Select
              label="Section"
              value={chosen}
              onChange={(e) => setSection(e.target.value)}
              options={sections.map((s) => ({ value: s.sectionId, label: `${s.offeringCode} · ${s.sectionCode} — ${s.title}` }))}
              data-resource-section
            />
            <DataTable
              caption="Resources of the section"
              rows={rows}
              rowKey={(r) => r.id}
              empty={<EmptyState icon={Library} title="No resources yet">Add reading lists, documents, videos or library links.</EmptyState>}
              columns={[
                { key: "t", header: "Resource", render: (r) => <a href={r.url} target="_blank" rel="noreferrer noopener" data-resource={r.title}>{r.title}</a> },
                { key: "k", header: "Kind", render: (r) => KINDS.find((k) => k.value === r.kind)?.label ?? r.kind, muted: true },
                { key: "s", header: "Status", render: (r) => <StatusBadge status={r.status} /> },
                {
                  key: "a",
                  header: "",
                  render: (r) => (
                    <Actions
                      disabled={busy}
                      actions={[
                        ...(r.status !== "published" ? [{ id: "publish-resource", label: "Publish", variant: "primary" as const, onClick: () => run(() => api.resourceCommand(r.id, r.version, "publish"), "Published") }] : []),
                        ...(r.status === "published" ? [{ id: "withdraw-resource", label: "Withdraw", onClick: () => run(() => api.resourceCommand(r.id, r.version, "withdraw"), "Withdrawn") }] : []),
                      ]}
                    />
                  ),
                },
              ]}
            />
          </>
        )}
      </BentoTile>
      {prompt}
      {dialog}
    </>
  );
}
