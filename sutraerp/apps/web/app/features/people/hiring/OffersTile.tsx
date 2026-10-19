/**
 * ERP_EXTENSIONS 5A: offers & onboarding tile. Draft an offer with a live CTC breakup, move it
 * through approval → send → accept / decline / withdraw, preview the letter, tick the onboarding
 * checklist, see what needs attention, and run the careers page.
 */
import { FileSignature } from "lucide-react";
import { Badge, BentoTile, Button, Checkbox, CheckboxList, DataTable, EmptyState, FormGrid, FormSelect, KeyValue, TextField, Toggle } from "@sutra/ui";
import { useState } from "react";
import { allowedActions, annualCtc, formatDate, formatInr, monthlyCtc, offerDates, offerStatusKind, offerStatusLabel, REASON_ACTIONS, suggestedEmployerPf, type OfferAction } from "./offers";
import { useCandidateOffers, useCareersPage, useHiringAttention, useOfferCommands, useOfferLetter, useOnboardingChecklist, type OffersApi, defaultOffersApi } from "./offersApi";
import type { OfferInput, OfferView } from "./offersTypes";

/** Minimal shapes of the 7A records this tile reads (structural, so the workforce types fit). */
export interface OffersCandidate { id: string; givenName: string; familyName?: string | null; onboardingCaseId?: string | null }
export interface OffersRequisition { id: string; number: string; title: string }
export interface OffersOnboardingCase { id: string; number: string; candidateId: string; candidateName: string }

type Tab = "offers" | "onboarding" | "attention" | "careers";
const TABS: { value: Tab; label: string }[] = [
  { value: "offers", label: "Offers" },
  { value: "onboarding", label: "Onboarding" },
  { value: "attention", label: "Attention" },
  { value: "careers", label: "Careers" },
];

/** Button labels of the offer commands (`data-action="offer-<verb>"`). */
export const OFFER_BUTTON_LABELS: Record<OfferAction, string> = {
  submit: "Submit for approval",
  approve: "Approve",
  reject: "Reject",
  send: "Send",
  accept: "Mark accepted",
  decline: "Mark declined",
  withdraw: "Withdraw",
};

const nameOf = (c: OffersCandidate) => [c.givenName, c.familyName].filter(Boolean).join(" ");
const emptyForm = { designation: "", joiningDate: "", expiresOn: "", basic: "", da: "", hra: "", allowances: "", employerPf: "" };

export function OffersTile({
  institutionId,
  requisitionId = "",
  candidates = [],
  requisitions = [],
  onboardingCases = [],
  api = defaultOffersApi(),
}: {
  institutionId: string;
  requisitionId?: string;
  candidates?: OffersCandidate[];
  requisitions?: OffersRequisition[];
  onboardingCases?: OffersOnboardingCase[];
  api?: OffersApi;
}) {
  const cmd = useOfferCommands(api);
  const { busy, dialog } = cmd;
  const [tab, setTab] = useState<Tab>("offers");
  const [candidateId, setCandidateId] = useState("");
  const [form, setForm] = useState(emptyForm);
  const [pfTouched, setPfTouched] = useState(false);
  const [reason, setReason] = useState("");
  const [acceptedBy, setAcceptedBy] = useState("");
  const [previewId, setPreviewId] = useState("");
  const [casePick, setCasePick] = useState("");
  const [newTask, setNewTask] = useState("");
  const [slug, setSlug] = useState<string | null>(null);
  const [enabled, setEnabled] = useState<boolean | null>(null);

  const candidate = candidates.find((c) => c.id === candidateId) ?? null;
  const offers = useCandidateOffers(tab === "offers" ? candidateId : "", api);
  const letter = useOfferLetter(tab === "offers" ? previewId : "", api);
  const caseId = casePick || candidate?.onboardingCaseId || onboardingCases.find((o) => o.candidateId === candidateId)?.id || "";
  const checklist = useOnboardingChecklist(tab === "onboarding" ? caseId : "", api);
  const attention = useHiringAttention(tab === "attention" ? institutionId : "", api);
  const careers = useCareersPage(tab === "careers" ? institutionId : "", api);

  const set = (k: keyof typeof emptyForm, v: string) => {
    const next = { ...form, [k]: v };
    if (!pfTouched && (k === "basic" || k === "da")) next.employerPf = suggestedEmployerPf(next.basic, next.da);
    setForm(next);
  };
  const ctc = { basic: form.basic, da: form.da, hra: form.hra, allowances: form.allowances, employerPf: form.employerPf };
  const canDraft = candidateId !== "" && form.basic.trim() !== "" && form.joiningDate !== "";
  const draft = () => {
    const input: OfferInput = {
      designation: form.designation.trim() || null,
      joiningDate: form.joiningDate,
      expiresOn: form.expiresOn || null,
      ctc: { basic: form.basic, da: form.da || null, hra: form.hra || null, allowances: form.allowances || null, employerPf: form.employerPf || null },
    };
    void cmd.draftOffer(candidateId, input).then((r) => {
      if (!r) return;
      setForm(emptyForm);
      setPfTouched(false);
    });
  };
  const act = (o: OfferView, a: OfferAction) => {
    const r = reason.trim();
    const p =
      a === "submit" ? cmd.submitOffer(o)
      : a === "approve" ? cmd.approveOffer(o)
      : a === "reject" ? cmd.rejectOffer(o, r)
      : a === "send" ? cmd.sendOffer(o)
      : a === "accept" ? cmd.acceptOffer(o, acceptedBy.trim() || null)
      : a === "decline" ? cmd.declineOffer(o, r)
      : cmd.withdrawOffer(o, r);
    void p.then((done) => done && setReason(""));
  };

  const tabs = (
    <div className="flex gap-1" role="tablist">
      {TABS.map((t) => (
        <Button key={t.value} size="sm" variant={tab === t.value ? "primary" : "ghost"} data-offers-tab={t.value} onClick={() => setTab(t.value)}>
          {t.label}
        </Button>
      ))}
    </div>
  );
  const candidateSelect = (
    <FormSelect label="Candidate" value={candidateId} onChange={(e) => { setCandidateId(e.target.value); setPreviewId(""); setCasePick(""); }} data-field="offers-candidate"
      options={[{ value: "", label: "Choose a candidate" }, ...candidates.map((c) => ({ value: c.id, label: nameOf(c) }))]} />
  );

  const page = careers.data;
  const slugValue = slug ?? page?.slug ?? page?.suggestedSlug ?? "";
  const enabledValue = enabled ?? page?.enabled ?? true;
  const listings = page?.listings ?? [];
  const unlisted = requisitions.filter((r) => !listings.some((l) => l.requisitionId === r.id));

  return (
    <BentoTile span={12} title="Offers & onboarding" data-slice="5A" data-offers action={tabs}>
      {tab === "offers" && (
        <>
          {candidateSelect}
          {candidateId === "" ? (
            <EmptyState icon={FileSignature} title="Pick a candidate" compact>{requisitionId ? "Choose an applicant of this requisition." : "Choose a requisition in Candidates first."}</EmptyState>
          ) : (
            <>
              <DataTable caption="Offers" rows={offers.data ?? []} rowKey={(o) => o.id}
                empty={<EmptyState icon={FileSignature} title="No offers yet" compact>Draft one below.</EmptyState>}
                columns={[
                  { key: "n", header: "Offer", render: (o) => `${o.number} · ${o.designation}` },
                  { key: "d", header: "Dates", render: (o) => offerDates(o), muted: true },
                  { key: "c", header: "CTC", render: (o) => `${formatInr(o.ctcMonthly)} / month · ${formatInr(o.ctcAnnual)} / year` },
                  { key: "s", header: "Status", render: (o) => <span data-offer-status={o.status}><Badge kind={offerStatusKind(o.status)}>{offerStatusLabel(o.status)}</Badge></span> },
                  { key: "a", header: "", render: (o) => (
                    <div className="flex gap-1">
                      {allowedActions(o.status).map((a) => (
                        <Button key={a} size="sm" variant={a === "withdraw" || a === "reject" || a === "decline" ? "ghost" : "primary"} data-action={`offer-${a}`}
                          disabled={busy || (REASON_ACTIONS.includes(a) && reason.trim() === "")} onClick={() => act(o, a)}>
                          {OFFER_BUTTON_LABELS[a]}
                        </Button>
                      ))}
                      <Button size="sm" variant="ghost" data-action="offer-preview" onClick={() => setPreviewId(previewId === o.id ? "" : o.id)}>Preview letter</Button>
                    </div>
                  ) },
                ]} />
              <FormGrid columns={2}>
                <TextField label="Reason (for Reject, Mark declined, Withdraw)" value={reason} onChange={(e) => setReason(e.target.value)} data-field="offer-reason" />
                <TextField label="Accepted by (name as signed)" value={acceptedBy} onChange={(e) => setAcceptedBy(e.target.value)} data-field="offer-accepted-by" />
              </FormGrid>
              {previewId !== "" && (
                <div data-offer-letter>
                  {letter.isLoading ? <p className="text-sm">Loading letter…</p> : letter.data ? (
                    <>
                      <KeyValue label={letter.data.number} value={letter.data.snapshot ? "Letter as sent" : "Draft preview"} />
                      <pre>{letter.data.text}</pre>
                    </>
                  ) : <p className="text-sm">The letter could not be loaded.</p>}
                </div>
              )}
              <div data-offer-form>
                <FormGrid columns={3}>
                  <TextField label="Designation" value={form.designation} onChange={(e) => set("designation", e.target.value)} data-field="offer-designation" />
                  <TextField label="Joining date" type="date" value={form.joiningDate} onChange={(e) => set("joiningDate", e.target.value)} data-field="offer-joining" />
                  <TextField label="Expiry date" type="date" value={form.expiresOn} onChange={(e) => set("expiresOn", e.target.value)} data-field="offer-expiry" />
                  <TextField label="Basic (₹ / month)" inputMode="decimal" value={form.basic} onChange={(e) => set("basic", e.target.value)} data-field="offer-basic" />
                  <TextField label="DA (₹ / month)" inputMode="decimal" value={form.da} onChange={(e) => set("da", e.target.value)} data-field="offer-da" />
                  <TextField label="HRA (₹ / month)" inputMode="decimal" value={form.hra} onChange={(e) => set("hra", e.target.value)} data-field="offer-hra" />
                  <TextField label="Other allowances (₹ / month)" inputMode="decimal" value={form.allowances} onChange={(e) => set("allowances", e.target.value)} data-field="offer-allowances" />
                  <TextField label="Employer PF (₹ / month)" inputMode="decimal" value={form.employerPf} hint={`Statutory: ${formatInr(suggestedEmployerPf(form.basic, form.da))}`}
                    onChange={(e) => { setPfTouched(true); set("employerPf", e.target.value); }} data-field="offer-employer-pf" />
                </FormGrid>
                <div className="flex gap-2 items-center">
                  <KeyValue label="Monthly CTC" value={<span data-offer-ctc-monthly>{formatInr(monthlyCtc(ctc))}</span>} />
                  <KeyValue label="Annual CTC" value={<span data-offer-ctc-annual>{formatInr(annualCtc(ctc))}</span>} />
                  <Button size="sm" data-action="offer-draft" disabled={busy || !canDraft} onClick={draft}>Draft offer</Button>
                </div>
              </div>
            </>
          )}
        </>
      )}

      {tab === "onboarding" && (
        <>
          <div className="flex gap-2 items-end">
            {candidateSelect}
            <FormSelect label="Onboarding case" value={caseId} onChange={(e) => setCasePick(e.target.value)} data-field="offers-case"
              options={[{ value: "", label: "Choose a case" }, ...onboardingCases.map((o) => ({ value: o.id, label: `${o.number} · ${o.candidateName}` }))]} />
          </div>
          {caseId === "" ? (
            <EmptyState icon={FileSignature} title="No onboarding case" compact>Start onboarding from a candidate who accepted an offer.</EmptyState>
          ) : checklist.data ? (
            <div data-onboarding-checklist>
              <KeyValue label={checklist.data.candidateName}
                value={`Joins ${formatDate(checklist.data.joiningDate)} · ${checklist.data.progress.done}/${checklist.data.progress.total} done${checklist.data.overdue ? ` · ${checklist.data.overdue} overdue` : ""}`} />
              {checklist.data.groups.map((g) => (
                <CheckboxList key={g.category} label={g.category}>
                  <strong className="text-sm">{g.category}</strong>
                  {g.tasks.map((t) => (
                    <Checkbox key={t.id} data-task={t.id} data-overdue={t.overdue && !t.done ? "true" : undefined} checked={t.done} disabled={busy}
                      onChange={() => void (t.done ? cmd.untickTask(t.id) : cmd.tickTask(t.id))}
                      label={t.title}
                      description={`${t.owner ? `Owner: ${t.owner.toUpperCase()}` : "No owner"} · ${t.dueDate ? `due ${formatDate(t.dueDate)}` : t.offsetLabel}`}
                      note={t.overdue && !t.done ? <Badge kind="attention">Overdue</Badge> : undefined} />
                  ))}
                </CheckboxList>
              ))}
              <div className="flex gap-2 items-end">
                <TextField label="Add a task" value={newTask} placeholder="Issue ID card" onChange={(e) => setNewTask(e.target.value)} data-field="checklist-task" />
                <Button size="sm" variant="ghost" data-action="checklist-add" disabled={busy || !newTask.trim()}
                  onClick={() => void cmd.addChecklistTask(caseId, { title: newTask.trim() }).then(() => setNewTask(""))}>Add task</Button>
              </div>
            </div>
          ) : <p className="text-sm">{checklist.isLoading ? "Loading checklist…" : "The checklist could not be loaded."}</p>}
        </>
      )}

      {tab === "attention" && (
        <div data-hiring-attention>
          <DataTable caption="Reminders" rows={attention.data?.reminders ?? []} rowKey={(r) => r.key}
            empty={<EmptyState icon={FileSignature} title="Nothing needs attention" compact>Offers about to expire, due follow-ups and onboarding tasks show here.</EmptyState>}
            columns={[
              { key: "k", header: "Kind", render: (r) => <Badge kind={r.kind === "offer" ? "attention" : "info"}>{r.kind}</Badge> },
              { key: "t", header: "What", render: (r) => r.title },
              { key: "b", header: "Detail", render: (r) => r.body, muted: true },
            ]} />
          {(attention.data?.stale.length ?? 0) > 0 && (
            <DataTable caption={`No movement for ${attention.data!.staleAfterDays}+ days`} rows={attention.data!.stale} rowKey={(r) => r.candidateId}
              columns={[
                { key: "n", header: "Candidate", render: (r) => r.name },
                { key: "q", header: "Requisition", render: (r) => r.requisitionNumber },
                { key: "s", header: "Stage", render: (r) => r.stageLabel },
                { key: "d", header: "Days", render: (r) => String(r.days) },
              ]} />
          )}
          {(attention.data?.retentionDue.length ?? 0) > 0 && (
            <DataTable caption={`Past ${attention.data!.retentionMonths}-month retention`} rows={attention.data!.retentionDue} rowKey={(r) => r.candidateId}
              columns={[
                { key: "n", header: "Candidate", render: (r) => r.name },
                { key: "q", header: "Requisition", render: (r) => r.requisitionNumber },
                { key: "a", header: "Archived on", render: (r) => formatDate(r.archivedOn) },
              ]} />
          )}
        </div>
      )}

      {tab === "careers" && (
        <div data-careers>
          <div className="flex gap-2 items-end">
            <TextField label="Careers page slug" value={slugValue} onChange={(e) => setSlug(e.target.value)} data-field="careers-slug" />
            <Toggle label="Page live" checked={enabledValue} onChange={(e) => setEnabled(e.target.checked)} data-field="careers-enabled" />
            <Button size="sm" data-action="careers-save" disabled={busy || slugValue.trim() === ""}
              onClick={() => void cmd.saveCareersPage(institutionId, { enabled: enabledValue, slug: slugValue.trim(), intro: page?.intro ?? null, contactEmail: page?.contactEmail ?? null }).then(() => setSlug(null))}>Save</Button>
          </div>
          <KeyValue label="Public path" value={<span data-careers-path>{page?.publicPath ?? "Not published yet"}</span>} />
          <DataTable caption="Requisitions on the careers page" rows={listings} rowKey={(l) => l.requisitionId}
            empty={<EmptyState icon={FileSignature} title="No requisitions" compact>Raise a requisition to list it here.</EmptyState>}
            columns={[
              { key: "n", header: "Requisition", render: (l) => `${l.number} · ${l.title}` },
              { key: "s", header: "Status", render: (l) => l.status },
              { key: "a", header: "Applications", render: (l) => String(l.applications) },
              { key: "p", header: "Listed", render: (l) => (
                <Toggle label={l.published ? "Published" : "Hidden"} checked={l.published} disabled={busy} data-action="careers-publish" data-requisition={l.requisitionId}
                  onChange={(e) => void cmd.publishRequisition(l.requisitionId, { published: e.target.checked, description: l.description, location: l.location, closesOn: l.closesOn })} />
              ) },
            ]} />
          {unlisted.length > 0 && (
            <div className="flex gap-1">
              {unlisted.map((r) => (
                <Toggle key={r.id} label={`${r.number} · ${r.title}`} checked={false} disabled={busy} data-action="careers-publish" data-requisition={r.id}
                  onChange={(e) => void cmd.publishRequisition(r.id, { published: e.target.checked })} />
              ))}
            </div>
          )}
        </div>
      )}
      {dialog}
    </BentoTile>
  );
}
