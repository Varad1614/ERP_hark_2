/**
 * Admin → Academics tabs backed by slice 2B: academic structure, programmes, curriculum
 * (versions: draft → published → immutable successor → retired) and the school academic model.
 */
import { BookOpen, Plus } from "lucide-react";
import { BentoTile, Button, DataTable, EmptyState, FormSelect, ListRow, TagInput } from "@sutra/ui";
import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
// Re-skin v2 (R3): StepDialog quick forms, one-pill row menus and structured editors.
import { emptySlot, LevelsPicker, levelName, levelNodes, slotComponents, SlotsEditor, type SlotRow } from "../academic/admin/editors";
import { Actions, AskForm, fmtDate, NoInstitution, REASON, useAsk } from "../academic/kit";
import type { PublishCommand } from "./api";
import { keys, StatusBadge, useCommand, useInstitutionApi } from "./hooks";
import type { AcademicModel, AcademicStructure, ComponentCategory, CurriculumComponent, GradeLevel, Institution, PublicationStatus, StructureNode } from "./types";

export const MODELS: AcademicModel[] = ["higher_education", "school", "mixed"];
const CATEGORIES: ComponentCategory[] = ["major_core", "major_elective", "minor", "multidisciplinary", "ability_enhancement", "skill_enhancement", "value_added", "internship", "research_project", "open_elective"];

/** Default NHEQF level (NEP 2020) for common level keys when none is typed. */
const NHEQF_DEFAULTS: Record<string, number> = { certificate: 4.5, diploma: 5, ug: 6, pg: 6.5, phd: 8 };

/** `ug:Undergraduate:6, pg:Postgraduate:6.5` → level nodes (key : name [: NHEQF level]). */
export function parseLevels(text: string): StructureNode[] {
  return text
    .split(",")
    .map((x) => x.trim())
    .filter(Boolean)
    .map((x) => {
      const parts = x.split(":").map((p) => p.trim());
      const k = (parts[0] ?? "").toLowerCase();
      let level: number | undefined;
      const last = parts.length > 2 ? Number(parts[parts.length - 1]) : NaN;
      if (parts.length > 2 && Number.isFinite(last)) {
        level = last;
        parts.pop();
      }
      const name = parts.slice(1).join(":").trim() || k;
      level ??= NHEQF_DEFAULTS[k];
      return level === undefined ? { kind: "level" as const, key: k, name } : { kind: "level" as const, key: k, name, nheqfLevel: level };
    });
}

/** `1:CS101:20, 1:MD101:4:multidisciplinary` → course slots (term : code : credits [: category]). */
export function parseSlots(text: string): CurriculumComponent[] {
  return text
    .split(",")
    .map((x) => x.trim())
    .filter(Boolean)
    .map((x) => {
      const [term = "", code = "", credits = "", category = ""] = x.split(":").map((y) => y.trim());
      const cat = (CATEGORIES as string[]).includes(category) ? (category as ComponentCategory) : "major_core";
      return { termNo: Number(term), slotCode: code.toUpperCase(), title: code.toUpperCase(), credits: Number(credits), category: cat };
    });
}

/** Level keys of the published (else the newest) academic structure. */
export function levelKeys(structures: AcademicStructure[]): StructureNode[] {
  const s = structures.find((x) => x.status === "published") ?? [...structures].sort((a, b) => b.versionNo - a.versionNo)[0];
  return (s?.nodes ?? []).filter((n) => n.kind === "level");
}

/** Publication actions per status (published versions are immutable). */
export function publicationActions(status: PublicationStatus, revisable = false): (PublishCommand | "revise")[] {
  if (status === "draft") return ["publish"];
  if (status === "published") return revisable ? ["revise", "retire"] : ["retire"];
  return [];
}

const LABEL: Record<string, string> = { publish: "Publish", retire: "Retire", revise: "Revise" };

function Needs({ institution }: { institution: Institution | null }) {
  return institution ? null : (
    <BentoTile span={12}>
      <NoInstitution />
    </BentoTile>
  );
}

export function StructureTab({ institution }: { institution: Institution | null }) {
  const api = useInstitutionApi();
  const { run, submit, busy, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const q = useQuery({ queryKey: keys.scoped(institution?.id ?? "-", "structures"), queryFn: () => api.listStructures(institution!.id), enabled: !!institution });
  const [levels, setLevels] = useState<string[]>(["ug", "pg"]);
  if (!institution) return <Needs institution={institution} />;
  const draft = (q.data ?? []).find((s) => s.status === "draft");
  return (
    <>
      <BentoTile span={12} title="Academic structure">
        <DataTable
          caption="Academic structures"
          rows={q.data ?? []}
          rowKey={(s) => s.id}
          empty={<EmptyState icon={BookOpen} title="No academic structure yet">Define how faculties, programmes and levels are arranged, then publish it.</EmptyState>}
          columns={[
            { key: "v", header: "Version", render: (s) => `v${s.versionNo}` },
            { key: "name", header: "Name", render: (s) => <span data-structure={s.versionNo}>{s.name}</span> },
            { key: "levels", header: "Levels", render: (s) => s.nodes.filter((n) => n.kind === "level").map((n) => n.name || levelName(n.key)).join(" · ") || "—", muted: true },
            { key: "status", header: "Status", render: (s) => <StatusBadge status={s.status} /> },
            {
              key: "a",
              header: "",
              render: (s) => (
                <Actions
                  disabled={busy}
                  actions={publicationActions(s.status).map((a) => ({
                    id: `structure-${a}`,
                    label: LABEL[a]!,
                    onClick: () =>
                      a === "retire"
                        ? ask({ title: `Retire structure v${s.versionNo}`, name: "structure-retire", submitLabel: "Retire", fields: [REASON], onSubmit: (v) => submit(() => api.structureCommand(s.id, s.version, "retire", v.reason!.trim()), `Structure v${s.versionNo}: retired`) })
                        : void run(() => api.structureCommand(s.id, s.version, a as PublishCommand), `Structure v${s.versionNo}: ${a} done`),
                  }))}
                />
              ),
            },
          ]}
        />
        <AskForm
          name="new-structure"
          title={draft ? `Update draft v${draft.versionNo}` : "New draft structure"}
          submitLabel={draft ? `Update draft v${draft.versionNo}` : "Create draft structure"}
          fields={[
            { name: "name", label: "Name", required: true, initial: "Academic structure" },
            { name: "model", label: "Model", type: "select", initial: "higher_education", options: MODELS.map((m) => ({ value: m, label: m === "higher_education" ? "Higher education" : m === "school" ? "School" : "Mixed" })) },
            { name: "effectiveFrom", label: "Effective from", type: "date" },
          ]}
          onSubmit={(v) => {
            if (levels.length === 0) return Promise.reject(new Error("Pick at least one level"));
            return submit(
              () => {
                const input = { name: v.name!.trim(), model: v.model as AcademicModel, nepAligned: true, nodes: levelNodes(levels), effectiveFrom: v.effectiveFrom || null };
                return draft ? api.updateStructure(draft.id, draft.version, input) : api.createStructure(institution.id, input);
              },
              draft ? `Draft v${draft.versionNo} updated` : "Draft structure created",
            );
          }}
        >
          <LevelsPicker value={levels} onChange={setLevels} />
        </AskForm>
      </BentoTile>
      {prompt}
      {dialog}
    </>
  );
}

export function ProgrammesTab({ institution }: { institution: Institution | null }) {
  const api = useInstitutionApi();
  const { run, submit, busy, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const inst = institution?.id ?? "-";
  const q = useQuery({ queryKey: keys.scoped(inst, "programmes"), queryFn: () => api.listProgrammes(institution!.id), enabled: !!institution });
  const units = useQuery({ queryKey: keys.scoped(inst, "org-units"), queryFn: () => api.listOrgUnits(institution!.id), enabled: !!institution });
  const structures = useQuery({ queryKey: keys.scoped(inst, "structures"), queryFn: () => api.listStructures(institution!.id), enabled: !!institution });
  const levels = levelKeys(structures.data ?? []);
  if (!institution) return <Needs institution={institution} />;
  return (
    <>
      <BentoTile
        span={12}
        title="Programme catalogue"
        action={
          <Button
            size="sm"
            icon={<Plus strokeWidth={1.75} />}
            data-new-programme
            onClick={() =>
              ask({
                eyebrow: "Academics · Programmes",
                title: "Register programme",
                name: "new-programme",
                submitLabel: "Register",
                fields: [
                  { name: "code", label: "Code", required: true, placeholder: "BTECH-CSE" },
                  { name: "name", label: "Name", required: true, placeholder: "B.Tech Computer Science" },
                  { name: "level", label: "Level", type: "select", required: true, initial: levels[0]?.key ?? "", options: levels.map((l) => ({ value: l.key, label: l.name || levelName(l.key) })) },
                  { name: "durationTerms", label: "Duration (terms)", type: "number", required: true, initial: "8" },
                  { name: "orgUnitId", label: "Owning unit", type: "select", wide: true, options: [{ value: "", label: "Not set" }, ...(units.data ?? []).map((u) => ({ value: u.id, label: u.name }))] },
                ],
                onSubmit: (v) =>
                  submit(
                    () => api.createProgramme(institution.id, { code: v.code!.trim().toUpperCase(), name: v.name!.trim(), level: v.level!, durationTerms: Number(v.durationTerms), orgUnitId: v.orgUnitId || null }),
                    "Programme registered",
                  ),
              })
            }
          >
            Register programme
          </Button>
        }
      >
        <DataTable
          caption="Programmes"
          rows={q.data ?? []}
          rowKey={(p) => p.id}
          empty={<EmptyState icon={BookOpen} title="No programmes yet">Register a programme, then publish it.</EmptyState>}
          columns={[
            { key: "code", header: "Code", render: (p) => <b>{p.code}</b> },
            { key: "name", header: "Name", render: (p) => <span data-programme={p.code}>{p.name}</span> },
            { key: "level", header: "Level", render: (p) => `${levelName(p.level)} · ${p.durationTerms} terms`, muted: true },
            { key: "status", header: "Status", render: (p) => <StatusBadge status={p.status} /> },
            {
              key: "a",
              header: "",
              render: (p) => (
                <Actions
                  disabled={busy}
                  actions={publicationActions(p.status).map((a) => ({
                    id: `programme-${a}`,
                    label: LABEL[a]!,
                    onClick: () =>
                      a === "retire"
                        ? ask({ title: `Retire ${p.name}`, name: "programme-retire", submitLabel: "Retire", fields: [REASON], onSubmit: (v) => submit(() => api.programmeCommand(p.id, p.version, "retire", v.reason!.trim()), `${p.name}: retired`) })
                        : void run(() => api.programmeCommand(p.id, p.version, a as PublishCommand), `${p.name}: ${a} done`),
                  }))}
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

export function CurriculumTab({ institution }: { institution: Institution | null }) {
  const api = useInstitutionApi();
  const { run, submit, busy, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const inst = institution?.id ?? "-";
  const programmes = useQuery({ queryKey: keys.scoped(inst, "programmes"), queryFn: () => api.listProgrammes(institution!.id), enabled: !!institution });
  const [programmeId, setProgrammeId] = useState<string>("");
  const pid = programmeId || programmes.data?.[0]?.id || "";
  const programme = programmes.data?.find((p) => p.id === pid);
  const curricula = useQuery({ queryKey: keys.scoped(inst, "curricula", pid), queryFn: () => api.listCurricula(pid), enabled: !!pid });
  const [curriculumId, setCurriculumId] = useState<string>("");
  const cid = curricula.data?.some((c) => c.id === curriculumId) ? curriculumId : (curricula.data?.[0]?.id ?? "");
  const versions = useQuery({ queryKey: keys.scoped(inst, "curriculum-versions", cid), queryFn: () => api.listCurriculumVersions(cid), enabled: !!cid });
  const [slots, setSlots] = useState<SlotRow[]>([emptySlot()]);
  if (!institution) return <Needs institution={institution} />;
  if ((programmes.data ?? []).length === 0)
    return (
      <BentoTile span={12} title="Curriculum">
        <EmptyState icon={BookOpen} title="No programmes yet">Register a programme in the Programmes tab first.</EmptyState>
      </BentoTile>
    );
  return (
    <>
      <BentoTile span={5} title="Curricula">
        <FormSelect label="Programme" name="programme" value={pid} options={(programmes.data ?? []).map((p) => ({ value: p.id, label: p.name }))} onChange={(e) => setProgrammeId(e.target.value)} />
        <div className="sb-list-rows" style={{ marginTop: 12 }}>
          {(curricula.data ?? []).map((c) => (
            <div key={c.id} data-curriculum={c.code}>
              <ListRow icon={BookOpen} tone="violet" title={c.title} meta={c.code} selected={c.id === cid} onOpen={c.id === cid ? undefined : () => setCurriculumId(c.id)} badge={c.id === cid ? { label: "Open", kind: "info" } : undefined} />
            </div>
          ))}
        </div>
        <AskForm
          title="Add a curriculum"
          columns={2}
          name="new-curriculum"
          submitLabel="Add curriculum"
          fields={[
            { name: "code", label: "Code", required: true, placeholder: "CSE-2026" },
            { name: "title", label: "Title", required: true, placeholder: "CSE scheme 2026" },
            { name: "effectiveFrom", label: "Effective from", type: "date", required: true },
            { name: "termStructure", label: "Terms", type: "select", initial: "semester", options: [{ value: "semester", label: "Semesters" }, { value: "trimester", label: "Trimesters" }, { value: "annual", label: "Annual" }] },
            { name: "creditFramework", label: "Credit framework", type: "select", initial: "nep2020", options: [{ value: "nep2020", label: "NEP 2020" }, { value: "cbcs", label: "CBCS" }, { value: "custom", label: "Custom" }] },
          ]}
          onSubmit={(v) => {
            const components = slotComponents(slots);
            if (components.length === 0) return Promise.reject(new Error("Add at least one course slot (code and credits)"));
            return submit(
              () =>
                api.createCurriculum(pid, {
                  code: v.code!.trim().toUpperCase(),
                  title: v.title!.trim(),
                  initialVersion: {
                    effectiveFrom: v.effectiveFrom!,
                    termStructure: v.termStructure as "semester" | "trimester" | "annual",
                    termCount: programme?.durationTerms ?? 1,
                    creditFramework: v.creditFramework as "nep2020" | "cbcs" | "custom",
                    components,
                  },
                }),
              "Curriculum added (draft version 1)",
            ).then((r) => {
              setSlots([emptySlot()]);
              return r;
            });
          }}
        >
          <SlotsEditor rows={slots} onChange={setSlots} terms={programme?.durationTerms ?? 1} />
        </AskForm>
      </BentoTile>
      <BentoTile span={7} title="Versions">
        {!cid ? (
          <EmptyState title="No curriculum selected">Add a curriculum to manage its versions.</EmptyState>
        ) : (
          <>
            <DataTable
              caption="Curriculum versions"
              rows={versions.data ?? []}
              rowKey={(v) => v.id}
              empty={<EmptyState title="No versions yet">Create the first draft version.</EmptyState>}
              columns={[
                { key: "v", header: "Version", render: (v) => <span data-curriculum-version={v.versionNo}>v{v.versionNo}{v.supersedesVersionId ? " (successor)" : ""}</span> },
                { key: "eff", header: "Effective from", render: (v) => fmtDate(v.effectiveFrom), muted: true },
                { key: "status", header: "Status", render: (v) => <StatusBadge status={v.status} /> },
                {
                  key: "a",
                  header: "",
                  render: (v) => (
                    <Actions
                      disabled={busy}
                      actions={publicationActions(v.status, true).map((a) => ({
                        id: `curriculum-${a}`,
                        label: LABEL[a]!,
                        onClick: () =>
                          a === "revise"
                            ? ask({
                                title: `Revise version ${v.versionNo}`,
                                name: "curriculum-revise",
                                submitLabel: "Create successor draft",
                                fields: [
                                  { name: "changeSummary", label: "What changes", required: true, wide: true },
                                  { name: "effectiveFrom", label: "Effective from", type: "date", required: true },
                                ],
                                onSubmit: (f) => submit(() => api.reviseCurriculum(cid, { changeSummary: f.changeSummary!.trim(), effectiveFrom: f.effectiveFrom! }), `Successor draft of version ${v.versionNo} created`),
                              })
                            : a === "retire"
                              ? ask({ title: `Retire version ${v.versionNo}`, name: "curriculum-retire", submitLabel: "Retire", fields: [REASON], onSubmit: (f) => submit(() => api.curriculumVersionCommand(v.id, v.version, "retire", f.reason!.trim()), `Version ${v.versionNo}: retired`) })
                              : void run(() => api.curriculumVersionCommand(v.id, v.version, "publish"), `Version ${v.versionNo}: published`),
                      }))}
                    />
                  ),
                },
              ]}
            />
            <p>
              <small>Published versions are immutable: Revise creates a linked successor draft.</small>
            </p>
          </>
        )}
      </BentoTile>
      {prompt}
      {dialog}
    </>
  );
}

export function SchoolModelTab({ institution }: { institution: Institution | null }) {
  const api = useInstitutionApi();
  const { run, submit, busy, dialog } = useCommand();
  const q = useQuery({ queryKey: keys.scoped(institution?.id ?? "-", "school-models"), queryFn: () => api.listSchoolModels(institution!.id), enabled: !!institution });
  const [grades, setGrades] = useState<string[]>([]);
  if (!institution) return <Needs institution={institution} />;
  return (
    <>
      <BentoTile span={12} title="School academic model">
        <p>
          <small>Only for schools (K-12). For other institutions the readiness check is not applicable.</small>
        </p>
        <DataTable
          caption="School models"
          rows={q.data ?? []}
          rowKey={(m) => m.id}
          empty={<EmptyState title="No school model">Define grade levels and the board if this institution is a school.</EmptyState>}
          columns={[
            { key: "v", header: "Version", render: (m) => `v${m.versionNo}` },
            { key: "board", header: "Board", render: (m) => m.board ?? "—" },
            { key: "grades", header: "Grade levels", render: (m) => m.gradeLevels.map((g) => g.key).join(", ") || "—", muted: true },
            { key: "status", header: "Status", render: (m) => <StatusBadge status={m.status} /> },
            {
              key: "a",
              header: "",
              render: (m) => <Actions disabled={busy} actions={m.status === "draft" ? [{ id: "school-publish", label: "Publish", onClick: () => void run(() => api.schoolModelCommand(m.id, m.version, "publish"), "School model published") }] : []} />,
            },
          ]}
        />
        <AskForm
          name="new-school-model"
          title="New draft model"
          submitLabel="Create draft model"
          fields={[
            { name: "name", label: "Name", required: true, initial: "School model" },
            { name: "board", label: "Board", placeholder: "CBSE" },
                      ]}
          onSubmit={(v) => {
            if (grades.length === 0) return Promise.reject(new Error("Pick at least one grade level"));
            return submit(
              () =>
                api.createSchoolModel(institution.id, {
                  name: v.name!.trim(),
                  board: v.board?.trim() || undefined,
                  gradeLevels: grades.map((g) => g.trim().toLowerCase()).map((key): GradeLevel => ({ key, name: /^\d+$/.test(key) ? `Class ${key}` : key })),
                }),
              "Draft school model created",
            );
          }}
        >
          <TagInput label="Grade levels" name="gradeLevels" value={grades} onChange={setGrades} options={Array.from({ length: 12 }, (_, n) => ({ value: String(n + 1), label: `Class ${n + 1}` }))} placeholder="Other grade (e.g. nursery) and Enter" wide />
        </AskForm>
      </BentoTile>
      {dialog}
    </>
  );
}
