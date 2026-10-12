/**
 * Admin → Academics → Academic calendar (slice 2C, L3-B-027 … B-033): calendars, academic
 * years (open / close into the next cycle), versions with terms and key dates (publish,
 * revise into a successor, retire), scope assignments, temporary exceptions and the change
 * simulation preview. All dates are business dates in the institution timezone.
 */
import { CalendarDays, FlaskConical, Plus } from "lucide-react";
import { BentoTile, Button, DataTable, EmptyState, Modal, Select } from "@sutra/ui";
import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import type { ExceptionCommand } from "./api";
import { Actions, fmtDate, NoInstitution, QuickForm, type RowAction } from "./forms";
import { keys, statusLabel, StatusBadge, useCommand, useInstitutionApi } from "./hooks";
import type { AcademicYear, Calendar, CalendarException, CalendarSimulation, CalendarVersion, ExceptionKind, Institution, KeyDateKind, ScopeKind } from "./types";

const KEY_DATE_KINDS: KeyDateKind[] = ["holiday", "instruction_start", "instruction_end", "exam_window", "registration_window", "break", "event"];
const EXCEPTION_KINDS: ExceptionKind[] = ["closure", "rescheduled_working_day", "extended_term", "additional_holiday", "other"];

/** Version actions per status (published is immutable: revise creates a successor draft). */
export function versionActions(v: Pick<CalendarVersion, "status">): ("publish" | "revise" | "retire")[] {
  if (v.status === "draft") return ["publish", "retire"];
  if (v.status === "published") return ["revise", "retire"];
  return [];
}

export function exceptionActions(e: Pick<CalendarException, "status">): ExceptionCommand[] {
  if (e.status === "approved") return ["apply", "withdraw", "complete"];
  if (e.status === "requested") return ["withdraw"];
  if (e.status === "in_effect") return ["complete"];
  return [];
}

function YearClose({ year, onDone }: { year: AcademicYear; onDone: () => void }) {
  const api = useInstitutionApi();
  const { submit } = useCommand();
  return (
    <>
      <h2>Close academic year {year.label}</h2>
      <p>
        Closing completes temporary exceptions and prepares the next year as a <strong>planned</strong> draft (it is not opened automatically). Before {fmtDate(year.endsOn)}, an early-closure reason is required.
      </p>
      <QuickForm
        name="close-year"
        submitLabel="Close year"
        onCancel={onDone}
        fields={[
          { name: "earlyClosureReason", label: "Early-closure reason (if before the end date)", wide: true },
          { name: "prepareSuccessor", label: "Prepare the next academic year", type: "checkbox", initial: "true" },
        ]}
        onSubmit={async (v) => {
          const r = await submit(
            () => api.closeYear(year.id, year.version, { earlyClosureReason: v.earlyClosureReason?.trim() || null, prepareSuccessor: v.prepareSuccessor === "true" }),
            (res) => `Academic year ${res.year.label} closed${res.year.successorYearId ? " — next year prepared" : ""}`,
          );
          if (r) onDone();
        }}
      />
    </>
  );
}

function VersionPanel({
  version,
  years,
  onRun,
  onSubmit,
  onSelect,
  busy,
}: {
  version: CalendarVersion;
  years: AcademicYear[];
  onRun: ReturnType<typeof useCommand>["run"];
  onSubmit: ReturnType<typeof useCommand>["submit"];
  /** Shows another version (the successor after a revision). */
  onSelect: (id: string) => void;
  busy: boolean;
}) {
  const api = useInstitutionApi();
  const draft = version.status === "draft";
  const year = years.find((y) => y.id === version.academicYearId);
  const label: Record<string, string> = { publish: "Publish", revise: "Revise (successor)", retire: version.status === "draft" ? "Discard draft" : "Retire" };
  return (
    <div data-version-panel={version.versionNo} data-version-status={version.status}>
      <p>
        <strong>Version {version.versionNo}</strong> <StatusBadge status={version.status} /> · {year?.label ?? "—"} · {fmtDate(version.startsOn)} – {fmtDate(version.endsOn)} · dates in <strong>{version.timezone}</strong>
        {version.supersedesVersionId ? " · successor revision" : ""}
      </p>
      <Actions
        disabled={busy}
        actions={versionActions(version).map(
          (a): RowAction => ({ id: `version-${a}`, label: label[a]!, variant: a === "publish" ? "primary" : "ghost", onClick: () => void onRun(() => api.versionCommand(version.id, version.version, a), (r) => `Version ${r.versionNo} is ${statusLabel(r.status).toLowerCase()}`).then((r) => r && onSelect(r.id)) }),
        )}
      />
      <h3>Terms</h3>
      <DataTable
        caption="Terms"
        rows={version.terms}
        rowKey={(t) => t.id}
        empty={<EmptyState title="No terms yet">{draft ? "Add at least one term before publishing." : "This version has no terms."}</EmptyState>}
        columns={[
          { key: "seq", header: "#", render: (t) => t.sequence },
          { key: "name", header: "Term", render: (t) => <span data-term={t.code}>{t.name} ({t.code})</span> },
          { key: "dates", header: "Dates", render: (t) => `${fmtDate(t.startsOn)} – ${fmtDate(t.endsOn)}`, muted: true },
          { key: "a", header: "", render: (t) => (draft ? <Actions disabled={busy} actions={[{ id: "remove-term", label: "Remove", onClick: () => void onRun(() => api.removeTerm(version.id, version.version, t.id), "Term removed") }]} /> : null) },
        ]}
      />
      <h3>Key dates</h3>
      <DataTable
        caption="Key dates"
        rows={version.keyDates}
        rowKey={(k) => k.id}
        empty={<EmptyState title="No key dates">Holidays, exam windows and other key dates appear here.</EmptyState>}
        columns={[
          { key: "kind", header: "Kind", render: (k) => statusLabel(k.kind) },
          { key: "name", header: "Name", render: (k) => <span data-key-date={k.name}>{k.name}</span> },
          { key: "dates", header: "Dates", render: (k) => (k.startsOn === k.endsOn ? fmtDate(k.startsOn) : `${fmtDate(k.startsOn)} – ${fmtDate(k.endsOn)}`), muted: true },
          { key: "a", header: "", render: (k) => (draft ? <Actions disabled={busy} actions={[{ id: "remove-key-date", label: "Remove", onClick: () => void onRun(() => api.removeKeyDate(version.id, version.version, k.id), "Key date removed") }]} /> : null) },
        ]}
      />
      {draft && (
        <div className="sb-form">
          <div>
            <QuickForm
              name="new-term"
              submitLabel="Add term"
              fields={[
                { name: "code", label: "Code", required: true, placeholder: `T${version.terms.length + 1}` },
                { name: "name", label: "Name", required: true, placeholder: `Semester ${version.terms.length + 1}` },
                { name: "sequence", label: "Sequence", type: "number", required: true, initial: String(version.terms.length + 1) },
                { name: "startsOn", label: "Starts", type: "date", required: true },
                { name: "endsOn", label: "Ends", type: "date", required: true },
              ]}
              onSubmit={(v) => onSubmit(() => api.addTerm(version.id, version.version, { code: v.code!.trim().toUpperCase(), name: v.name!.trim(), sequence: Number(v.sequence), startsOn: v.startsOn!, endsOn: v.endsOn! }), "Term added")}
            />
          </div>
          <div>
            <QuickForm
              name="new-key-date"
              submitLabel="Add key date"
              fields={[
                { name: "kind", label: "Kind", type: "select", initial: "holiday", options: KEY_DATE_KINDS.map((k) => ({ value: k, label: statusLabel(k) })) },
                { name: "name", label: "Name", required: true, placeholder: "Independence Day" },
                { name: "startsOn", label: "From", type: "date", required: true },
                { name: "endsOn", label: "To (optional)", type: "date" },
              ]}
              onSubmit={(v) => onSubmit(() => api.addKeyDate(version.id, version.version, { kind: v.kind as KeyDateKind, name: v.name!.trim(), startsOn: v.startsOn!, endsOn: v.endsOn || null }), "Key date added")}
            />
          </div>
        </div>
      )}
    </div>
  );
}

function SimulationPreview({ calendar, baseline }: { calendar: Calendar; baseline: CalendarVersion | undefined }) {
  const api = useInstitutionApi();
  const { run, submit, busy, dialog } = useCommand();
  const [result, setResult] = useState<CalendarSimulation | null>(null);
  if (!baseline) {
    return (
      <BentoTile span={12} title="Change simulation">
        <EmptyState icon={FlaskConical} title="Nothing to simulate yet">Publish a version first; a simulation compares a proposed change with it.</EmptyState>
      </BentoTile>
    );
  }
  return (
    <BentoTile span={12} title="Change simulation (preview — changes nothing)">
      <p>
        <small>
          Baseline: version {baseline.versionNo} ({fmtDate(baseline.startsOn)} – {fmtDate(baseline.endsOn)}). The result is an impact report; to apply a change, revise the version and publish the successor.
        </small>
      </p>
      <QuickForm
        name="simulate"
        submitLabel="Run simulation"
        fields={[
          { name: "startsOn", label: "Proposed start", type: "date", initial: baseline.startsOn },
          { name: "endsOn", label: "Proposed end", type: "date", initial: baseline.endsOn },
        ]}
        onSubmit={async (v) => {
          const s = await submit(() => api.simulate(calendar.id, baseline.id, { startsOn: v.startsOn || null, endsOn: v.endsOn || null }));
          if (s) setResult(s);
        }}
      />
      {result && (
        <div data-simulation={result.id} data-outcome={result.outcome}>
          <h3>
            Impact: <StatusBadge status={result.outcome} /> {result.status !== "evaluated" && <StatusBadge status={result.status} />}
          </h3>
          <p>
            <small>
              Instructional days {result.impact.baselineInstructionalDays} → {result.impact.proposedInstructionalDays}
            </small>
          </p>
          {result.impact.items.length === 0 ? (
            <p>No impact found.</p>
          ) : (
            <ul data-impact-items>
              {result.impact.items.map((i, n) => (
                <li key={`${i.code}-${n}`} data-impact={i.code}>
                  <StatusBadge status={i.severity === "blocking" ? "unfavourable" : i.severity === "uncertain" ? "uncertain" : i.severity === "warning" ? "returned" : "draft"} /> {i.message}
                  {i.affected.length > 0 && <small> · {i.affected.length} affected</small>}
                </li>
              ))}
            </ul>
          )}
          {result.status === "evaluated" && (
            <Actions
              disabled={busy}
              actions={[
                { id: "simulation-accept", label: "Record decision: accept", onClick: () => void run(() => api.decideSimulation(result.id, result.version, "accept"), "Decision recorded").then((s) => s && setResult(s)) },
                { id: "simulation-discard", label: "Discard", onClick: () => void run(() => api.decideSimulation(result.id, result.version, "discard"), "Simulation discarded").then((s) => s && setResult(s)) },
              ]}
            />
          )}
        </div>
      )}
      {dialog}
    </BentoTile>
  );
}

export function CalendarTab({ institution }: { institution: Institution | null }) {
  const api = useInstitutionApi();
  const { run, submit, busy, dialog } = useCommand();
  const inst = institution?.id ?? "-";
  const [calendarId, setCalendarId] = useState("");
  const [versionId, setVersionId] = useState("");
  const [closing, setClosing] = useState<AcademicYear | null>(null);
  const [addingYear, setAddingYear] = useState(false);
  const calendars = useQuery({ queryKey: keys.scoped(inst, "calendars"), queryFn: () => api.listCalendars(institution!.id), enabled: !!institution });
  const calendar = calendars.data?.find((c) => c.id === calendarId) ?? calendars.data?.find((c) => c.status !== "retired") ?? calendars.data?.[0];
  const cid = calendar?.id ?? "";
  const years = useQuery({ queryKey: keys.scoped(inst, "years"), queryFn: () => api.listYears(institution!.id), enabled: !!institution });
  const versions = useQuery({ queryKey: keys.scoped(inst, "calendar-versions", cid), queryFn: () => api.listVersions(cid), enabled: !!cid });
  const assignments = useQuery({ queryKey: keys.scoped(inst, "calendar-assignments", cid), queryFn: () => api.listAssignments(cid), enabled: !!cid });
  const exceptions = useQuery({ queryKey: keys.scoped(inst, "calendar-exceptions", cid), queryFn: () => api.listExceptions(cid), enabled: !!cid });
  // Governing scopes from the real 2A/2B registers (campus / unit / programme override the institution).
  const campuses = useQuery({ queryKey: keys.scoped(inst, "campuses"), queryFn: () => api.listCampuses(institution!.id), enabled: !!institution });
  const units = useQuery({ queryKey: keys.scoped(inst, "org-units"), queryFn: () => api.listOrgUnits(institution!.id), enabled: !!institution });
  const programmes = useQuery({ queryKey: keys.scoped(inst, "programmes"), queryFn: () => api.listProgrammes(institution!.id), enabled: !!institution });
  const scopes = [
    { value: "institution:", label: "Whole institution" },
    ...(campuses.data ?? []).filter((c) => c.status !== "archived" && c.status !== "decommissioned").map((c) => ({ value: `campus:${c.id}`, label: `Campus · ${c.name}` })),
    ...(units.data ?? []).filter((u) => u.status !== "archived" && u.status !== "decommissioned").map((u) => ({ value: `org_unit:${u.id}`, label: `Unit · ${u.name}` })),
    ...(programmes.data ?? []).filter((p) => p.status !== "retired").map((p) => ({ value: `programme:${p.id}`, label: `Programme · ${p.name}` })),
  ];

  if (!institution) return <BentoTile span={12}><NoInstitution /></BentoTile>;
  const calYears = (years.data ?? []).filter((y) => y.calendarId === cid);
  const vlist = versions.data ?? [];
  const version = vlist.find((v) => v.id === versionId) ?? vlist.find((v) => v.status === "draft") ?? vlist.find((v) => v.status === "published") ?? vlist[vlist.length - 1];
  const published = vlist.filter((v) => v.status === "published");
  const yearsWithoutVersion = calYears.filter((y) => y.status !== "closed" && !vlist.some((v) => v.academicYearId === y.id && (v.status === "draft" || v.status === "published")));
  const retired = calendar?.status === "retired";

  return (
    <>
      <BentoTile span={12} title="Academic calendars">
        {calendars.data && calendars.data.length === 0 ? (
          <EmptyState icon={CalendarDays} title="No academic calendar yet">Create the institution’s calendar, plan an academic year, then publish a version with its terms.</EmptyState>
        ) : calendar ? (
          <div className="sb-actions-row" data-calendar={calendar.code} data-calendar-status={calendar.status}>
            {(calendars.data ?? []).length > 1 && (
              <Select label="Calendar" name="calendar" value={calendar.id} options={(calendars.data ?? []).map((c) => ({ value: c.id, label: `${c.name} (${c.code})` }))} onChange={(e) => setCalendarId(e.target.value)} />
            )}
            <span>
              <strong>{calendar.name}</strong> · {calendar.code} <StatusBadge status={calendar.status} /> · dates in {institution.timezone}
            </span>
            {!retired && (
              <Button size="sm" variant="ghost" disabled={busy} onClick={() => void run(() => api.retireCalendar(calendar.id, calendar.version), "Calendar retired (history kept)")} data-action="calendar-retire">
                Retire calendar
              </Button>
            )}
          </div>
        ) : null}
        <QuickForm
          name="new-calendar"
          submitLabel="Create calendar"
          fields={[
            { name: "code", label: "Code", required: true, placeholder: "MAIN" },
            { name: "name", label: "Name", required: true, placeholder: "Main academic calendar" },
          ]}
          onSubmit={async (v) => {
            const c = await submit(() => api.createCalendar(institution.id, { code: v.code!.trim().toUpperCase(), name: v.name!.trim() }), "Calendar created");
            if (c) setCalendarId(c.id);
          }}
        />
      </BentoTile>

      {calendar && (
        <>
          <BentoTile
            span={12}
            title="Academic years"
            action={
              !retired && (
                <Button size="sm" icon={<Plus strokeWidth={1.75} />} onClick={() => setAddingYear(true)} data-new-year>
                  Plan academic year
                </Button>
              )
            }
          >
            <DataTable
              caption="Academic years"
              rows={calYears}
              rowKey={(y) => y.id}
              empty={<EmptyState title="No academic year yet">Plan the first academic year (for example 2026-27).</EmptyState>}
              columns={[
                { key: "label", header: "Year", render: (y) => <span data-year={y.label}>{y.label}</span> },
                { key: "dates", header: "Dates", render: (y) => `${fmtDate(y.startsOn)} – ${fmtDate(y.endsOn)}`, muted: true },
                { key: "status", header: "Status", render: (y) => <StatusBadge status={y.status} /> },
                {
                  key: "a",
                  header: "",
                  render: (y) => (
                    <Actions
                      disabled={busy}
                      actions={[
                        ...(y.status === "planned" ? [{ id: "year-open", label: "Open year", variant: "primary" as const, onClick: () => void run(() => api.openYear(y.id, y.version), `Academic year ${y.label} is open`) }] : []),
                        ...(y.status === "open" ? [{ id: "year-close", label: "Close year…", onClick: () => setClosing(y) }] : []),
                      ]}
                    />
                  ),
                },
              ]}
            />
          </BentoTile>

          <BentoTile span={12} title="Calendar versions">
            {vlist.length > 1 && (
              <Select label="Version" name="version" value={version?.id ?? ""} options={vlist.map((v) => ({ value: v.id, label: `Version ${v.versionNo} · ${statusLabel(v.status)}` }))} onChange={(e) => setVersionId(e.target.value)} />
            )}
            {version ? <VersionPanel version={version} years={calYears} onRun={run} onSubmit={submit} onSelect={setVersionId} busy={busy} /> : <EmptyState title="No version yet">Create a draft version for an academic year.</EmptyState>}
            {!retired && yearsWithoutVersion.length > 0 && (
              <QuickForm
                name="new-version"
                submitLabel="Create draft version"
                fields={[{ name: "academicYearId", label: "Academic year", type: "select", initial: yearsWithoutVersion[0]!.id, options: yearsWithoutVersion.map((y) => ({ value: y.id, label: y.label })) }]}
                onSubmit={async (v) => {
                  const nv = await submit(() => api.createVersion(cid, { academicYearId: v.academicYearId! }), "Draft version created");
                  if (nv) setVersionId(nv.id);
                }}
              />
            )}
          </BentoTile>

          <BentoTile span={6} title="Assignments">
            <DataTable
              caption="Calendar assignments"
              rows={assignments.data ?? []}
              rowKey={(a) => a.id}
              empty={<EmptyState title="Not assigned">A published version governs nothing until it is assigned to the institution (or a campus / unit / programme).</EmptyState>}
              columns={[
                { key: "scope", header: "Scope", render: (a) => <span data-assignment={a.scopeKind}>{statusLabel(a.scopeKind)}</span> },
                { key: "v", header: "Version", render: (a) => `v${vlist.find((v) => v.id === a.calendarVersionId)?.versionNo ?? "?"}`, muted: true },
                { key: "from", header: "From", render: (a) => `${fmtDate(a.effectiveFrom)}${a.effectiveTo ? ` – ${fmtDate(a.effectiveTo)}` : ""}`, muted: true },
                { key: "status", header: "Status", render: (a) => <><StatusBadge status={a.status} />{a.governing ? " governing" : ""}</> },
                {
                  key: "x",
                  header: "",
                  render: (a) => (
                    <Actions
                      disabled={busy}
                      actions={[
                        ...(a.status === "draft" ? [{ id: "assignment-activate", label: "Make effective", onClick: () => void run(() => api.assignmentCommand(a.id, a.version, "activate"), "Assignment effective") }] : []),
                        ...(a.status !== "retired" ? [{ id: "assignment-retire", label: "Retire", onClick: () => void run(() => api.assignmentCommand(a.id, a.version, "retire"), "Assignment retired") }] : []),
                      ]}
                    />
                  ),
                },
              ]}
            />
            {published.length > 0 && !retired && (
              <QuickForm
                name="new-assignment"
                submitLabel="Create assignment"
                fields={[
                  { name: "scope", label: "Scope", type: "select", initial: "institution:", options: scopes },
                  { name: "calendarVersionId", label: "Version", type: "select", initial: published[published.length - 1]!.id, options: published.map((v) => ({ value: v.id, label: `Version ${v.versionNo}` })) },
                  { name: "effectiveFrom", label: "Effective from", type: "date", required: true, initial: published[published.length - 1]!.startsOn },
                ]}
                onSubmit={(v) => {
                  const [kind = "institution", scopeId = ""] = (v.scope ?? "institution:").split(":");
                  return submit(
                    () => api.createAssignment(cid, { calendarVersionId: v.calendarVersionId!, scopeKind: kind as ScopeKind, scopeId: scopeId || null, effectiveFrom: v.effectiveFrom! }),
                    "Draft assignment created — make it effective",
                  );
                }}
              />
            )}
          </BentoTile>

          <BentoTile span={6} title="Exceptions (temporary deviations)">
            <DataTable
              caption="Calendar exceptions"
              rows={exceptions.data ?? []}
              rowKey={(e) => e.id}
              empty={<EmptyState title="No exceptions">Closures and rescheduled days are recorded here; the published version never changes.</EmptyState>}
              columns={[
                { key: "kind", header: "Kind", render: (e) => <span data-exception={e.kind}>{statusLabel(e.kind)}</span> },
                { key: "dates", header: "Dates", render: (e) => `${fmtDate(e.startsOn)} – ${fmtDate(e.endsOn)}`, muted: true },
                { key: "status", header: "Status", render: (e) => <><StatusBadge status={e.status} />{e.governing ? " governing" : ""}</> },
                {
                  key: "x",
                  header: "",
                  render: (e) => (
                    <Actions disabled={busy} actions={exceptionActions(e).map((a) => ({ id: `exception-${a}`, label: statusLabel(a), onClick: () => void run(() => api.exceptionCommand(e.id, e.version, a), (r) => `Exception ${statusLabel(r.status).toLowerCase()}`) }))} />
                  ),
                },
              ]}
            />
            {published.length > 0 && !retired && (
              <QuickForm
                name="new-exception"
                submitLabel="Request exception"
                fields={[
                  { name: "kind", label: "Kind", type: "select", initial: "closure", options: EXCEPTION_KINDS.map((k) => ({ value: k, label: statusLabel(k) })) },
                  { name: "startsOn", label: "From", type: "date", required: true },
                  { name: "endsOn", label: "To", type: "date", required: true },
                  { name: "reason", label: "Reason", required: true, wide: true },
                ]}
                onSubmit={(v) =>
                  submit(
                    () => api.createException(cid, { calendarVersionId: published[published.length - 1]!.id, kind: v.kind as ExceptionKind, startsOn: v.startsOn!, endsOn: v.endsOn!, reason: v.reason!.trim() }),
                    (r) => `Exception ${statusLabel(r.status).toLowerCase()}`,
                  )
                }
              />
            )}
          </BentoTile>

          <SimulationPreview calendar={calendar} baseline={published[published.length - 1]} />
        </>
      )}

      {calendar && (
        <Modal open={addingYear} onClose={() => setAddingYear(false)} label="Plan academic year">
          <h2>Plan academic year</h2>
          <QuickForm
            name="new-year"
            submitLabel="Plan"
            onCancel={() => setAddingYear(false)}
            fields={[
              { name: "label", label: "Label", required: true, placeholder: "2026-27" },
              { name: "startsOn", label: "Starts", type: "date", required: true },
              { name: "endsOn", label: "Ends", type: "date", required: true },
            ]}
            onSubmit={async (v) => {
              const y = await submit(() => api.createYear(institution.id, { calendarId: calendar.id, label: v.label!.trim(), startsOn: v.startsOn!, endsOn: v.endsOn! }), "Academic year planned");
              if (y) setAddingYear(false);
            }}
          />
        </Modal>
      )}
      <Modal open={closing !== null} onClose={() => setClosing(null)} label="Close academic year">
        {closing && <YearClose year={closing} onDone={() => setClosing(null)} />}
      </Modal>
      {dialog}
    </>
  );
}
