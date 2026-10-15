/**
 * Re-skin v2 (R3): Admin → Academics overview (prototype `adm-academics.png`,
 * `adm-academics-dlg-a2-addc.png`): programme cards (level · duration, courses offered, seats
 * filled from the real offerings) and the Courses table (code, title, credits, faculty, term,
 * status) with search and the "Add a course" dialog (new course offering, same API call).
 *
 * The API has no programme intake / enrolled-per-programme figure, so the cards show the seats
 * of the programme's offerings instead of the prototype's "Enrolled 236 · Intake 240".
 */
import { BookOpen, Briefcase, Building2, Cog, Cpu, GraduationCap, Laptop, Plus, Radio } from "lucide-react";
import { Badge, BentoTile, CardGrid, DataTable, EmptyState, formatNumber, IconChip, ProgressBar, SearchField, StatusBadge, type Tone } from "@sutra/ui";
import type { LucideIcon } from "lucide-react";
import { useQueries, useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { statusLabel, useCommand, useInstitutionApi } from "../../institution/hooks";
import type { Institution, Programme } from "../../institution/types";
import { okeys, useOfferingApi } from "../offering/hooks";
import { useOfferingCreate } from "../offering";
import type { Offering } from "../offering/types";
import { courseCode, NoInstitution, useAsk, useTerms } from "../kit";
import { levelName } from "./editors";

const ICONS: LucideIcon[] = [Cpu, Radio, Cog, Building2, Briefcase, Laptop, GraduationCap];
const TONES: Tone[] = ["orange", "blue", "teal", "amber", "violet", "rose", "green"];
const LIVE = (o: Offering) => !["archived", "cancelled", "merged", "split"].includes(o.status);

/** Programmes, offerings and the curriculum-version → programme map of an institution. */
export function useAcademicsData(institutionId: string | null | undefined) {
  const inst = useInstitutionApi();
  const offeringApi = useOfferingApi();
  const programmes = useQuery({ queryKey: okeys.scoped(institutionId ?? "-", "programmes"), queryFn: () => inst.listProgrammes(institutionId!), enabled: !!institutionId });
  const offerings = useQuery({ queryKey: okeys.scoped(institutionId ?? "-", "offerings"), queryFn: () => offeringApi.listOfferings(institutionId!), enabled: !!institutionId });
  const curricula = useQueries({ queries: (programmes.data ?? []).map((p) => ({ queryKey: okeys.scoped(p.id, "curricula"), queryFn: () => inst.listCurricula(p.id) })) });
  const flat = curricula.flatMap((q, i) => (q.data ?? []).map((c) => ({ c, programmeId: programmes.data?.[i]?.id ?? "" })));
  const versions = useQueries({ queries: flat.map(({ c }) => ({ queryKey: okeys.scoped(c.id, "curriculum-versions"), queryFn: () => inst.listCurriculumVersions(c.id) })) });
  const programmeOfVersion = new Map<string, string>();
  versions.forEach((q, i) => (q.data ?? []).forEach((v) => programmeOfVersion.set(v.id, flat[i]?.programmeId ?? "")));
  const list = (offerings.data ?? []).filter(LIVE);
  return { programmes: programmes.data ?? [], offerings: list, programmeOfVersion, loading: programmes.isLoading || offerings.isLoading };
}

/** "6 programmes · 12 courses · 778 students seated" — the live summary of the Academics header. */
export function academicsLine(d: ReturnType<typeof useAcademicsData>): string {
  if (d.loading) return "Loading programmes and courses…";
  const seated = d.offerings.reduce((n, o) => n + o.seatsTaken, 0);
  const p = d.programmes.filter((x) => x.status !== "retired").length;
  return `${p} ${p === 1 ? "programme" : "programmes"} · ${d.offerings.length} ${d.offerings.length === 1 ? "course" : "courses"} offered · ${formatNumber(seated)} ${seated === 1 ? "seat" : "seats"} taken`;
}

const facultyOf = (o: Offering) =>
  [
    ...new Set(
      o.sections
        .flatMap((s) => s.faculty)
        .filter((f) => f.status === "active")
        .map((f) => f.facultyName ?? "Faculty"),
    ),
  ].join(", ") || "Not assigned";

const capacityOf = (o: Offering) => o.capacity ?? (o.sections.length > 0 && o.sections.every((s) => s.capacity !== null && s.capacity !== undefined) ? o.sections.reduce((n, s) => n + (s.capacity ?? 0), 0) : null);

/** Programme tile (prototype `adm-academics.png` cards): icon chip, name, level · duration, seats taken vs capacity bar. */
function ProgrammeCard({ p, i, offerings }: { p: Programme; i: number; offerings: Offering[] }) {
  const seats = offerings.reduce((n, o) => n + o.seatsTaken, 0);
  const caps = offerings.map(capacityOf);
  const cap = caps.length > 0 && caps.every((c) => c !== null) ? caps.reduce((n: number, c) => n + (c ?? 0), 0) : null;
  const years = p.durationTerms % 2 === 0 ? `${p.durationTerms / 2} years` : `${p.durationTerms} terms`;
  const pct = cap ? Math.round((seats / cap) * 100) : null;
  const Icon = ICONS[i % ICONS.length]!;
  return (
    <BentoTile span={4} className="ac-prog" data-programme-card={p.code}>
      <div className="ac-prog-top">
        <IconChip icon={Icon} tone={TONES[i % TONES.length]!} />
        {p.status !== "published" && <Badge kind={p.status === "draft" ? "neutral" : "review"}>{statusLabel(p.status)}</Badge>}
      </div>
      <h3>{p.name}</h3>
      <small>
        {levelName(p.level)} · {years} · {offerings.length} {offerings.length === 1 ? "course" : "courses"}
      </small>
      <div className="ac-prog-row">
        <span>
          Seats taken <b>{formatNumber(seats)}</b>
        </span>
        {cap !== null && <span>Capacity {formatNumber(cap)}</span>}
      </div>
      <ProgressBar value={pct ?? 0} label={`${p.name} seats filled`} />
      <small>{offerings.length === 0 ? "No courses offered this term yet" : pct !== null ? `${pct}% filled · ${formatNumber(Math.max(0, (cap ?? 0) - seats))} seats open` : "Open capacity"}</small>
    </BentoTile>
  );
}

export function AcademicsOverview({ institution }: { institution: Institution | null }) {
  const { submit, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const create = useOfferingCreate(institution?.id ?? null, submit);
  const d = useAcademicsData(institution?.id);
  const terms = useTerms(institution?.id);
  const [search, setSearch] = useState("");
  if (!institution)
    return (
      <BentoTile span={12} data-slice="2C">
        <NoInstitution />
      </BentoTile>
    );
  const termName = (id: string) => terms.find((t) => t.value === id)?.name ?? "—";
  const needle = search.trim().toLowerCase();
  const rows = d.offerings
    .filter((o) => !needle || `${o.offeringCode} ${o.courseCode ?? ""} ${o.title} ${facultyOf(o)}`.toLowerCase().includes(needle))
    .sort((a, b) => a.offeringCode.localeCompare(b.offeringCode));
  const live = d.programmes.filter((p) => p.status !== "retired");
  return (
    <>
      {live.length === 0 ? (
        <BentoTile span={12}>
          <EmptyState icon={GraduationCap} title="No programmes yet" compact>
            Register a programme under More → Programmes, then publish it.
          </EmptyState>
        </BentoTile>
      ) : (
        <CardGrid data-programme-cards="">
          {live.map((p, i) => (
            <ProgrammeCard key={p.id} p={p} i={i} offerings={d.offerings.filter((o) => d.programmeOfVersion.get(o.curriculumVersionId) === p.id)} />
          ))}
        </CardGrid>
      )}
      <BentoTile span={12} data-courses="">
        <div className="ac-th">
          <h2>Courses</h2>
          <span className="ac-th-sub">{d.offerings.length} offered</span>
          <div className="ac-th-end">
            <SearchField label="Search code, title or faculty" placeholder="Search code, title or faculty" value={search} onChange={(e) => setSearch(e.target.value)} />
            <button type="button" className="sb-btn" data-add-course="" onClick={() => ask(create())}>
              <Plus strokeWidth={1.75} aria-hidden="true" />
              Add course
            </button>
          </div>
        </div>
        <DataTable
          caption="Courses"
          rows={rows}
          rowKey={(o) => o.id}
          empty={
            <EmptyState icon={BookOpen} title={needle ? "No course matches" : "No courses offered yet"} compact>
              {needle ? "Try another code, title or faculty name." : "Add a course from a published curriculum."}
            </EmptyState>
          }
          columns={[
            { key: "c", header: "Code", render: (o) => <b data-course={o.offeringCode}>{courseCode(o.offeringCode, o.courseCode)}</b> },
            { key: "t", header: "Title", render: (o) => o.title },
            { key: "cr", header: "Credits", render: (o) => formatNumber(o.credits) },
            { key: "f", header: "Faculty", render: (o) => facultyOf(o), muted: true },
            { key: "s", header: "Term", render: (o) => termName(o.academicTermId), muted: true },
            { key: "st", header: "Status", render: (o) => <StatusBadge status={o.status} labels={{ enrollment_open: "Enrolment open", enrollment_closed: "Enrolment closed" }} /> },
          ]}
        />
      </BentoTile>
      {prompt}
      {dialog}
    </>
  );
}
