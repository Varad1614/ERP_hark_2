/**
 * ERP_EXTENSIONS 2A: the admissions funnel (ported from Everything's CRM funnel, forecast and
 * duplicate checks, MIT). Record enquiries from walk-ins, calls, fairs and campaigns, move them
 * through the institution's stages, see where enquirers drop out, how many are expected to
 * join each month against the seats on offer, and which records look like the same person.
 * Enquiries linked to an application follow it to Offered and Admitted on their own.
 */
import { Filter } from "lucide-react";
import { Badge, BentoTile, Button, DataTable, EmptyState, FormGrid, FormSelect, KeyValue, TextField, formatDate } from "@sutra/ui";
import { useQuery } from "@tanstack/react-query";
import { useRef, useState } from "react";
import { USES_API } from "../../../lib/api";
import { focusFirstInvalid, inputAttrs, problemFieldErrors, problemMessage, validateField, validateFields, type Errors, type ValidatedField } from "../../../lib/forms";
import { useCommand } from "../../institution/hooks";
import { createHttpFunnelApi, type FunnelApi } from "./api";
import { barWidths, LOST_REASONS, nextStage, pct, rupees, SOURCES, sourceLabel, stageKind } from "./funnel";
import { createMockFunnelApi } from "./mock";
import type { Enquiry, FunnelStage } from "./types";

/** New-enquiry fields (Phase B shared form layer: rules mirror the server's A2 policy). */
const ENQUIRY_FIELDS: ValidatedField[] = [
  { name: "givenName", label: "First name", required: true },
  { name: "familyName", label: "Last name" },
  { name: "phone", label: "Mobile", rule: "mobile" },
  { name: "email", label: "E-mail", rule: "email" },
  { name: "intake", label: "Intake", rule: "integer", min: 2000, max: 2100 },
  { name: "expectedFee", label: "Expected fee", rule: "amount" },
];
const ENQUIRY_ORDER = ENQUIRY_FIELDS.map((f) => f.name);
const enquiryField = (name: string) => ENQUIRY_FIELDS.find((f) => f.name === name)!;

let fallback: FunnelApi | null = null;
export function defaultFunnelApi(): FunnelApi {
  fallback ??= USES_API ? createHttpFunnelApi() : createMockFunnelApi();
  return fallback;
}

type View = "enquiries" | "funnel" | "forecast" | "duplicates" | "settings";
const VIEWS: { value: View; label: string }[] = [
  { value: "enquiries", label: "Enquiries" },
  { value: "funnel", label: "Funnel" },
  { value: "forecast", label: "Forecast" },
  { value: "duplicates", label: "Duplicates" },
  { value: "settings", label: "Stages & seats" },
];

export function AdmissionFunnelTile({ institutionId, programmes = [], api = defaultFunnelApi() }: { institutionId: string; programmes?: { id: string; name: string }[]; api?: FunnelApi }) {
  const { run, submit, busy, dialog } = useCommand();
  const [enqErrors, setEnqErrors] = useState<Errors>({});
  const [enqError, setEnqError] = useState<string | null>(null);
  const enqRef = useRef<HTMLDivElement | null>(null);
  const [view, setView] = useState<View>("enquiries");
  const [q, setQ] = useState("");
  const [onlyOpen, setOnlyOpen] = useState(true);
  const [window, setWindow] = useState("all");
  const [draft, setDraft] = useState({ givenName: "", familyName: "", phone: "", email: "", source: "walk_in", programmeId: "", intake: "", expectedFee: "" });
  const [dupes, setDupes] = useState<string | null>(null);
  const [losing, setLosing] = useState<{ e: Enquiry; reason: string } | null>(null);
  const [linking, setLinking] = useState<{ e: Enquiry; applicationId: string } | null>(null);
  const [target, setTarget] = useState({ programmeId: "", intake: "", seats: "" });
  const key = (...p: string[]) => ["institution", "admission-funnel", institutionId, ...p];
  const list = useQuery({ queryKey: key("enquiries", q, String(onlyOpen)), queryFn: () => api.enquiries(institutionId, { q, open: onlyOpen }) });
  const funnel = useQuery({ queryKey: key("funnel", window), queryFn: () => api.funnel(institutionId, window), enabled: view === "funnel" });
  const forecast = useQuery({ queryKey: key("forecast"), queryFn: () => api.forecast(institutionId, 6), enabled: view === "forecast" });
  const duplicates = useQuery({ queryKey: key("duplicates"), queryFn: () => api.duplicates(institutionId), enabled: view === "duplicates" });
  const settings = useQuery({ queryKey: key("settings"), queryFn: () => api.settings(institutionId), enabled: view === "settings" });
  const stages: FunnelStage[] = list.data?.stages ?? [];
  const progName = (id?: string | null) => programmes.find((p) => p.id === id)?.name ?? "—";

  type DraftKey = keyof typeof draft;
  const enqBlur = (name: string) =>
    setEnqErrors((e) => {
      const m = validateField(enquiryField(name), draft[name as DraftKey]);
      const n = { ...e };
      if (m) n[name] = m;
      else delete n[name];
      return n;
    });
  const enqText = (name: DraftKey, placeholder?: string, label?: string) => {
    const f = enquiryField(name);
    return (
      <TextField label={label ?? f.label} name={name} {...inputAttrs(f)} value={draft[name]} placeholder={placeholder} error={enqErrors[name]}
        onChange={(e) => setDraft({ ...draft, [name]: e.target.value })} onBlur={() => enqBlur(name)} />
    );
  };
  /** Validates (rules + "a phone or an e-mail"), records, and maps a 422 back onto the fields. */
  const recordEnquiry = async <T extends { enquiry: { enquiryNumber: string } }>(action: () => Promise<T>) => {
    const errs = validateFields(ENQUIRY_FIELDS, draft);
    if (!errs.phone && !errs.email && !draft.phone.trim() && !draft.email.trim()) errs.phone = "Give a mobile number or an e-mail address";
    setEnqErrors(errs);
    setEnqError(null);
    if (Object.keys(errs).length) return void focusFirstInvalid(enqRef.current, ENQUIRY_ORDER, errs);
    try {
      await submit(action, (r) => `Enquiry ${r.enquiry.enquiryNumber} recorded`);
    } catch (err) {
      const { fields, unmatched } = problemFieldErrors(err, ENQUIRY_ORDER);
      setEnqErrors(fields);
      focusFirstInvalid(enqRef.current, ENQUIRY_ORDER, fields);
      if (unmatched.length || !Object.keys(fields).length) setEnqError(unmatched[0] ?? problemMessage(err));
    }
  };

  const tabs = (
    <div className="flex gap-1" role="tablist">
      {VIEWS.map((v) => (
        <Button key={v.value} size="sm" variant={view === v.value ? "primary" : "ghost"} data-funnel-view={v.value} onClick={() => setView(v.value)}>
          {v.label}
        </Button>
      ))}
    </div>
  );

  const advance = (e: Enquiry) => {
    const next = nextStage(stages, e.stage);
    return next ? (
      <Button size="sm" variant="ghost" data-action="advance-enquiry" disabled={busy} onClick={() => void run(() => api.updateEnquiry(e.id, e.version, { stage: next.id }), `Moved to ${next.label}`)}>
        {`→ ${next.label}`}
      </Button>
    ) : null;
  };

  return (
    <BentoTile span={12} title="Admissions funnel" data-slice="2A" data-admission-funnel action={tabs}>
      {view === "enquiries" && (
        <>
          <div ref={enqRef} style={{ display: "contents" }}>
          <FormGrid columns={3}>
            {enqText("givenName")}
            {enqText("familyName")}
            {enqText("phone", "+91 98200 12345")}
            {enqText("email")}
            <FormSelect label="Came from" value={draft.source} onChange={(e) => setDraft({ ...draft, source: e.target.value })} options={SOURCES.map((s) => ({ value: s.value, label: s.label }))} />
            <FormSelect label="Programme" value={draft.programmeId} onChange={(e) => setDraft({ ...draft, programmeId: e.target.value })}
              options={[{ value: "", label: "Not decided" }, ...programmes.map((p) => ({ value: p.id, label: p.name }))]} />
            {enqText("intake", "2027", "Intake (YYYY)")}
            {enqText("expectedFee", "150000", "Expected fee (₹)")}
          </FormGrid>
          {enqError && (
            <p className="sb-field-error" role="alert">
              {enqError}
            </p>
          )}
          </div>
          <Button size="sm" data-action="new-enquiry" disabled={busy || !draft.givenName.trim()}
            onClick={() => void recordEnquiry(async () => {
                const r = await api.createEnquiry(institutionId, {
                  givenName: draft.givenName,
                  familyName: draft.familyName || undefined,
                  phone: draft.phone || undefined,
                  email: draft.email || undefined,
                  source: draft.source,
                  programmeId: draft.programmeId || undefined,
                  intake: draft.intake || undefined,
                  expectedFee: draft.expectedFee || undefined,
                });
                setDupes(r.possibleDuplicates.length ? `${r.enquiry.displayName} may already be ${r.possibleDuplicates.map((d) => `${d.number ?? d.kind} (${d.name}, same ${d.reasons.join(" + ")})`).join(", ")}` : null);
                setDraft({ ...draft, givenName: "", familyName: "", phone: "", email: "" });
                return r;
              })}>
            Record enquiry
          </Button>
          {dupes && <p className="text-sm" data-duplicate-hint role="status"><Badge kind="attention">Possible duplicate</Badge> {dupes}</p>}
          <div className="flex gap-2 items-end">
            <TextField label="Search" value={q} placeholder="Name, number, phone" onChange={(e) => setQ(e.target.value)} />
            <Button size="sm" variant={onlyOpen ? "primary" : "ghost"} data-action="only-open" onClick={() => setOnlyOpen(!onlyOpen)}>
              <Filter size={14} /> {onlyOpen ? "Open only" : "All"}
            </Button>
          </div>
          <DataTable caption="Enquiries" rows={list.data?.items ?? []} rowKey={(e) => e.id}
            empty={<EmptyState icon={Filter} title="No enquiries">Record walk-ins, calls and fair leads here before they apply.</EmptyState>}
            columns={[
              { key: "n", header: "Enquirer", render: (e) => <span data-enquiry={e.enquiryNumber}>{`${e.displayName} · ${e.enquiryNumber}`}</span> },
              { key: "c", header: "Contact", render: (e) => e.phone ?? e.email ?? "—" },
              { key: "p", header: "Programme", render: (e) => `${progName(e.programmeId)}${e.intake ? ` · ${e.intake}` : ""}` },
              { key: "s", header: "Came from", render: (e) => sourceLabel(e.source) },
              { key: "g", header: "Stage", render: (e) => <Badge kind={stageKind(e.stage, e.stalled)}>{`${e.stageLabel}${e.stalled ? ` · ${e.daysInStage}d idle` : ""}`}</Badge> },
              { key: "f", header: "Fee", render: (e) => rupees(e.expectedFee) },
              { key: "x", header: "", render: (e) =>
                e.stage === "admitted" || e.stage === "lost" ? (
                  <span className="text-sm">{e.closeReason ?? (e.closedAt ? formatDate(e.closedAt.slice(0, 10)) : "")}</span>
                ) : (
                  <span className="flex gap-1">
                    {advance(e)}
                    {!e.applicationId && <Button size="sm" variant="ghost" data-action="link-application" onClick={() => setLinking({ e, applicationId: "" })}>Link application</Button>}
                    <Button size="sm" variant="ghost" data-action="lose-enquiry" onClick={() => setLosing({ e, reason: LOST_REASONS[0] })}>Lost</Button>
                  </span>
                ) },
            ]} />
          {losing && (
            <div className="flex gap-2 items-end" data-lose-form>
              <FormSelect label={`Why did ${losing.e.displayName} drop out?`} value={losing.reason} onChange={(ev) => setLosing({ ...losing, reason: ev.target.value })} options={LOST_REASONS.map((r) => ({ value: r, label: r }))} />
              <Button size="sm" data-action="confirm-lost" disabled={busy} onClick={() => void run(async () => { const r = await api.updateEnquiry(losing.e.id, losing.e.version, { stage: "lost", closeReason: losing.reason }); setLosing(null); return r; }, "Marked lost")}>Mark lost</Button>
              <Button size="sm" variant="ghost" onClick={() => setLosing(null)}>Cancel</Button>
            </div>
          )}
          {linking && (
            <div className="flex gap-2 items-end" data-link-form>
              <TextField label={`Application ID for ${linking.e.displayName}`} value={linking.applicationId} onChange={(ev) => setLinking({ ...linking, applicationId: ev.target.value.trim() })} />
              <Button size="sm" data-action="confirm-link" disabled={busy || !linking.applicationId} onClick={() => void run(async () => { const r = await api.linkApplication(linking.e.id, linking.e.version, linking.applicationId); setLinking(null); return r; }, "Application linked")}>Link</Button>
              <Button size="sm" variant="ghost" onClick={() => setLinking(null)}>Cancel</Button>
            </div>
          )}
        </>
      )}
      {view === "funnel" && (
        <>
          <FormSelect label="Enquiries from" value={window} onChange={(e) => setWindow(e.target.value)}
            options={[{ value: "all", label: "All time" }, { value: "fy", label: "This financial year" }, { value: "quarter", label: "This quarter" }, { value: "last_quarter", label: "Last quarter" }, { value: "year", label: "This calendar year" }]} />
          {funnel.data && (
            <>
              <KeyValue label="Enquiries" value={`${funnel.data.report.totals.enquiries} · ${funnel.data.report.totals.admitted} admitted (${pct(funnel.data.report.totals.admittedRate)}) · ${funnel.data.report.totals.lost} lost`} />
              <div className="flex flex-col gap-1" data-funnel-bars>
                {funnel.data.report.rows.map((r, i) => (
                  <div key={r.stage} className="flex items-center gap-2 text-sm" data-funnel-row={r.stage}>
                    <span className="sb-funnel-label">{r.label}{r.gone ? " (removed)" : ""}</span>
                    <span className="sb-funnel-bar" style={{ width: `calc(${barWidths(funnel.data!.report.rows)[i]}% * 0.4)`, minWidth: 2 }} aria-hidden />
                    <span>{`${r.entered} in · ${pct(r.advanceRate)} moved on · ${r.lostHere} dropped · ${r.open} waiting`}</span>
                  </div>
                ))}
              </div>
              {funnel.data.report.untracked > 0 && <p className="text-sm">{`${funnel.data.report.untracked} closed enquiries have no stage history and are left out.`}</p>}
            </>
          )}
        </>
      )}
      {view === "forecast" && forecast.data && (
        <>
          <KeyValue label="Open pipeline" value={`${rupees(forecast.data.pipeline)} in fees · ${rupees(forecast.data.weighted)} expected`} />
          <KeyValue label="Admitted vs lost" value={`${forecast.data.winRate.admitted} / ${forecast.data.winRate.lost} (${pct(forecast.data.winRate.rate)})${forecast.data.avgCycleDays != null ? ` · ${forecast.data.avgCycleDays} days enquiry to admission` : ""}`} />
          <DataTable caption="Expected joiners by month" rows={forecast.data.months} rowKey={(m) => m.key}
            columns={[
              { key: "k", header: "Month", render: (m) => m.key },
              { key: "c", header: "Enquiries", render: (m) => m.count },
              { key: "j", header: "Expected joiners", render: (m) => m.expectedJoiners.toFixed(1) },
              { key: "w", header: "Expected fees", render: (m) => rupees(m.weighted) },
              { key: "a", header: "Admitted", render: (m) => rupees(m.admitted) },
            ]} />
          <DataTable caption="Seats" rows={forecast.data.seats} rowKey={(s) => `${s.programmeId}-${s.intake}`}
            empty={<EmptyState icon={Filter} title="No seat targets">Set seats per programme and intake under Stages & seats.</EmptyState>}
            columns={[
              { key: "p", header: "Programme", render: (s) => `${progName(s.programmeId)} · ${s.intake}` },
              { key: "s", header: "Seats", render: (s) => s.seats },
              { key: "a", header: "Admitted", render: (s) => s.admitted },
              { key: "e", header: "Expected from open", render: (s) => `${s.expected.toFixed(1)} of ${s.open}` },
              { key: "c", header: "Cover", render: (s) => (s.coverage == null ? "Full" : <Badge kind={s.coverage >= 1 ? "done" : "attention"}>{`${s.coverage.toFixed(2)}×`}</Badge>) },
            ]} />
          <DataTable caption="By source" rows={forecast.data.bySource} rowKey={(g) => g.key}
            columns={[
              { key: "k", header: "Came from", render: (g) => sourceLabel(g.key) },
              { key: "a", header: "Admitted", render: (g) => `${g.admitted} of ${g.total}` },
              { key: "r", header: "Rate", render: (g) => pct(g.rate) },
            ]} />
          <DataTable caption="Why enquirers drop out" rows={forecast.data.reasonsLost.rows} rowKey={(r) => r.reason}
            columns={[
              { key: "r", header: "Reason", render: (r) => r.reason },
              { key: "c", header: "Enquiries", render: (r) => r.count },
            ]} />
          <DataTable caption="Days per stage" rows={forecast.data.velocity} rowKey={(v) => v.stage}
            columns={[
              { key: "s", header: "Stage", render: (v) => v.label },
              { key: "d", header: "Average days", render: (v) => (v.days == null ? "—" : v.days) },
            ]} />
          {forecast.data.stalled.length > 0 && (
            <DataTable caption={`Idle for ${forecast.data.staleDays}+ days`} rows={forecast.data.stalled} rowKey={(s) => s.id}
              columns={[
                { key: "n", header: "Enquirer", render: (s) => `${s.name} · ${s.enquiryNumber}` },
                { key: "s", header: "Stage", render: (s) => s.stage },
                { key: "d", header: "Days", render: (s) => s.daysInStage },
              ]} />
          )}
        </>
      )}
      {view === "duplicates" && (
        <DataTable caption="Possible duplicates" rows={duplicates.data?.pairs ?? []} rowKey={(p) => `${p.a.id}-${p.b.id}`}
          empty={<EmptyState icon={Filter} title="No duplicates">No two open enquiries or applications share an e-mail, name or phone.</EmptyState>}
          columns={[
            { key: "a", header: "Keep (older)", render: (p) => `${p.a.name} · ${p.a.number ?? p.a.kind}` },
            { key: "b", header: "Looks the same", render: (p) => `${p.b.name} · ${p.b.number ?? p.b.kind}` },
            { key: "r", header: "Same", render: (p) => p.reasons.join(", ") },
          ]} />
      )}
      {view === "settings" && settings.data && (
        <>
          <DataTable caption="Stages" rows={settings.data.stages} rowKey={(s) => s.id}
            columns={[
              { key: "l", header: "Stage", render: (s) => s.label },
              { key: "p", header: "Chance of joining", render: (s) => pct(s.probability) },
            ]} />
          <KeyValue label="Flag idle after" value={`${settings.data.staleDays} days`} />
          <DataTable caption="Seat targets" rows={settings.data.targets} rowKey={(t) => `${t.programmeId}-${t.intake}`}
            columns={[
              { key: "p", header: "Programme", render: (t) => progName(t.programmeId) },
              { key: "i", header: "Intake", render: (t) => t.intake },
              { key: "s", header: "Seats", render: (t) => t.seats },
            ]} />
          <FormGrid columns={3}>
            <FormSelect label="Programme" value={target.programmeId} onChange={(e) => setTarget({ ...target, programmeId: e.target.value })} options={[{ value: "", label: "Choose…" }, ...programmes.map((p) => ({ value: p.id, label: p.name }))]} />
            <TextField label="Intake (YYYY)" value={target.intake} onChange={(e) => setTarget({ ...target, intake: e.target.value })} />
            <TextField label="Seats" value={target.seats} onChange={(e) => setTarget({ ...target, seats: e.target.value })} />
          </FormGrid>
          <Button size="sm" data-action="save-target" disabled={busy || !target.programmeId || !target.intake || !target.seats}
            onClick={() => {
              const s = settings.data!;
              const custom = s.customised ? s.stages.filter((x) => x.id !== "admitted" && x.id !== "lost") : [];
              const targets = [...s.targets.filter((t) => !(t.programmeId === target.programmeId && t.intake === target.intake)), { programmeId: target.programmeId, intake: target.intake, seats: Number.parseInt(target.seats, 10) || 0 }];
              void run(() => api.saveSettings(institutionId, { stages: custom, staleDays: s.staleDays, appliedStage: s.appliedStage ?? null, offeredStage: s.offeredStage ?? null, targets, version: s.version }), "Seat target saved");
            }}>
            Save seats
          </Button>
        </>
      )}
      {dialog}
    </BentoTile>
  );
}
