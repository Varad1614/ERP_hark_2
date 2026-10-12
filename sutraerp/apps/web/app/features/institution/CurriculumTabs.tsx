/**
 * Admin → Academics tabs backed by slice 2B: academic structure, programmes, curriculum
 * (versions: draft → published → immutable successor → retired) and the school academic model.
 */
import { BookOpen, Plus } from "lucide-react";
import { BentoTile, Button, DataTable, EmptyState, Modal, Select } from "@sutra/ui";
import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import type { PublishCommand } from "./api";
import { Actions, fmtDate, NoInstitution, QuickForm, REASON, useAsk } from "./forms";
import { keys, StatusBadge, useCommand, useInstitutionApi } from "./hooks";
import type { AcademicModel, AcademicStructure, ComponentCategory, CurriculumComponent, GradeLevel, Institution, PublicationStatus, StructureNode } from "./types";

export const MODELS: AcademicModel[] = ["higher_education", "school", "mixed"];
const CATEGORIES: ComponentCategory[] = ["major_core", "major_elective", "minor", "multidisciplinary", "ability_enhancement", "skill_enhancement", "value_added", "internship", "research_project", "open_elective"];

/** `ug:Undergraduate, pg:Postgraduate` → level nodes of an academic structure. */
export function parseLevels(text: string): StructureNode[] {
  return text
    .split(",")
    .map((x) => x.trim())
    .filter(Boolean)
    .map((x) => {
      const [key = "", ...name] = x.split(":");
      const k = key.trim().toLowerCase();
      return { kind: "level" as const, key: k, name: name.join(":").trim() || k };
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
  if (!institution) return <Needs institution={institution} />;
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
            { key: "levels", header: "Levels", render: (s) => s.nodes.filter((n) => n.kind === "level").map((n) => n.key).join(", ") || "—", muted: true },
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
        <QuickForm
          name="new-structure"
          submitLabel="Create draft structure"
          fields={[
            { name: "name", label: "Name", required: true, initial: "Academic structure" },
            { name: "model", label: "Model", type: "select", initial: "higher_education", options: MODELS.map((m) => ({ value: m, label: m.replace(/_/g, " ") })) },
            { name: "levels", label: "Levels (key:name, comma separated)", required: true, wide: true, initial: "ug:Undergraduate, pg:Postgraduate" },
            { name: "effectiveFrom", label: "Effective from", type: "date" },
          ]}
          onSubmit={(v) =>
            submit(
              () => api.createStructure(institution.id, { name: v.name!.trim(), model: v.model as AcademicModel, nepAligned: true, nodes: parseLevels(v.levels!), effectiveFrom: v.effectiveFrom || null }),
              "Draft structure created",
            )
          }
        />
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
  const [adding, setAdding] = useState(false);
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
          <Button size="sm" icon={<Plus strokeWidth={1.75} />} onClick={() => setAdding(true)} data-new-programme>
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
            { key: "code", header: "Code", render: (p) => p.code },
            { key: "name", header: "Name", render: (p) => <span data-programme={p.code}>{p.name}</span> },
            { key: "level", header: "Level", render: (p) => `${p.level} · ${p.durationTerms} terms`, muted: true },
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
      <Modal open={adding} onClose={() => setAdding(false)} label="Register programme">
        <h2>Register programme</h2>
        <QuickForm
          name="new-programme"
          submitLabel="Register"
          onCancel={() => setAdding(false)}
          fields={[
            { name: "code", label: "Code", required: true, placeholder: "BTECH-CSE" },
            { name: "name", label: "Name", required: true, placeholder: "B.Tech Computer Science" },
            { name: "level", label: "Level (from the academic structure)", type: "select", required: true, initial: levels[0]?.key ?? "", options: levels.map((l) => ({ value: l.key, label: `${l.name} (${l.key})` })) },
            { name: "durationTerms", label: "Duration (terms)", type: "number", required: true, initial: "8" },
            { name: "orgUnitId", label: "Owning unit", type: "select", options: [{ value: "", label: "—" }, ...(units.data ?? []).map((u) => ({ value: u.id, label: u.name }))] },
          ]}
          onSubmit={async (v) => {
            await submit(
              () => api.createProgramme(institution.id, { code: v.code!.trim().toUpperCase(), name: v.name!.trim(), level: v.level!, durationTerms: Number(v.durationTerms), orgUnitId: v.orgUnitId || null }),
              "Programme registered",
            );
            setAdding(false);
          }}
        />
      </Modal>
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
        <Select label="Programme" name="programme" value={pid} options={(programmes.data ?? []).map((p) => ({ value: p.id, label: `${p.name} (${p.code})` }))} onChange={(e) => setProgrammeId(e.target.value)} />
        <div className="sb-list">
          {(curricula.data ?? []).map((c) => (
            <article key={c.id} className={`sb-list-row${c.id === cid ? "" : " is-muted"}`} data-curriculum={c.code}>
              <div>
                <h3>{c.title}</h3>
                <small>{c.code}</small>
              </div>
              <Button size="sm" variant="ghost" onClick={() => setCurriculumId(c.id)}>
                Open
              </Button>
            </article>
          ))}
        </div>
        <QuickForm
          name="new-curriculum"
          submitLabel="Add curriculum"
          fields={[
            { name: "code", label: "Code", required: true, placeholder: "CSE-2026" },
            { name: "title", label: "Title", required: true, placeholder: "CSE scheme 2026" },
            { name: "effectiveFrom", label: "Effective from", type: "date", required: true },
            { name: "termStructure", label: "Terms", type: "select", initial: "semester", options: ["semester", "trimester", "annual"].map((t) => ({ value: t, label: t })) },
            { name: "creditFramework", label: "Credit framework", type: "select", initial: "nep2020", options: ["nep2020", "cbcs", "custom"].map((t) => ({ value: t, label: t })) },
            { name: "slots", label: `Course slots (term:code:credits, comma separated; ${programme?.durationTerms ?? "?"} terms)`, required: true, wide: true, placeholder: "1:CS101:20, 2:CS201:20" },
          ]}
          onSubmit={(v) =>
            submit(
              () =>
                api.createCurriculum(pid, {
                  code: v.code!.trim().toUpperCase(),
                  title: v.title!.trim(),
                  initialVersion: {
                    effectiveFrom: v.effectiveFrom!,
                    termStructure: v.termStructure as "semester" | "trimester" | "annual",
                    termCount: programme?.durationTerms ?? 1,
                    creditFramework: v.creditFramework as "nep2020" | "cbcs" | "custom",
                    components: parseSlots(v.slots!),
                  },
                }),
              "Curriculum added (draft version 1)",
            )
          }
        />
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
        <QuickForm
          name="new-school-model"
          submitLabel="Create draft model"
          fields={[
            { name: "name", label: "Name", required: true, initial: "School model" },
            { name: "board", label: "Board", placeholder: "CBSE" },
            { name: "gradeLevels", label: "Grade levels (comma separated)", required: true, placeholder: "1, 2, 3, 4, 5" },
          ]}
          onSubmit={(v) =>
            submit(
              () =>
                api.createSchoolModel(institution.id, {
                  name: v.name!.trim(),
                  board: v.board?.trim() || undefined,
                  gradeLevels: v.gradeLevels!
                    .split(",")
                    .map((g) => g.trim().toLowerCase())
                    .filter(Boolean)
                    .map((key): GradeLevel => ({ key, name: /^\d+$/.test(key) ? `Class ${key}` : key })),
                }),
              "Draft school model created",
            )
          }
        />
      </BentoTile>
      {dialog}
    </>
  );
}
