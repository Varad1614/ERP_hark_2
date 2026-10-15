/**
 * Re-skin v2 (R3) structured editors that replace the comma-separated "key:name:level" and
 * "term:code:credits" text inputs of the Academics setup forms (docs/reskin2-contracts.md §2.4).
 */
import { Plus, X } from "lucide-react";
import { FormSelect, TagInput, TextField } from "@sutra/ui";
import type { ComponentCategory } from "../../institution/types";

/** Level keys with their names and NEP 2020 NHEQF levels. */
export const LEVELS: { key: string; name: string; nheqf: number }[] = [
  { key: "certificate", name: "Certificate", nheqf: 4.5 },
  { key: "diploma", name: "Diploma", nheqf: 5 },
  { key: "ug", name: "Undergraduate", nheqf: 6 },
  { key: "pg", name: "Postgraduate", nheqf: 6.5 },
  { key: "phd", name: "Doctoral", nheqf: 8 },
];

export const levelName = (key: string) => LEVELS.find((l) => l.key === key)?.name ?? key.replace(/^./, (c) => c.toUpperCase());

/** Level nodes for the chosen keys. */
export function levelNodes(keys: string[]) {
  return keys.map((k) => {
    const l = LEVELS.find((x) => x.key === k);
    return l ? { kind: "level" as const, key: k, name: l.name, nheqfLevel: l.nheqf } : { kind: "level" as const, key: k, name: levelName(k) };
  });
}

export function LevelsPicker({ value, onChange, error }: { value: string[]; onChange: (v: string[]) => void; error?: string }) {
  return <TagInput label="Levels" hint="Pick every level the institution awards (NHEQF level in brackets)." name="levels" value={value} onChange={onChange} allowCustom={false} error={error} options={LEVELS.map((l) => ({ value: l.key, label: `${l.name} (${l.nheqf})` }))} wide />;
}

export interface SlotRow {
  term: string;
  code: string;
  credits: string;
  category: string;
}

export const CATEGORY_LABEL: Record<string, string> = {
  major_core: "Major (core)",
  major_elective: "Major (elective)",
  minor: "Minor",
  multidisciplinary: "Multidisciplinary",
  ability_enhancement: "Ability enhancement",
  skill_enhancement: "Skill enhancement",
  value_added: "Value added",
  internship: "Internship",
  research_project: "Research project",
  open_elective: "Open elective",
};

export const emptySlot = (): SlotRow => ({ term: "1", code: "", credits: "", category: "major_core" });

/** Course-slot rows (term · code · credits · category) with add / remove. */
export function SlotsEditor({ rows, onChange, terms, error }: { rows: SlotRow[]; onChange: (rows: SlotRow[]) => void; terms: number; error?: string }) {
  const set = (i: number, patch: Partial<SlotRow>) => onChange(rows.map((r, j) => (j === i ? { ...r, ...patch } : r)));
  return (
    <div className="ac-rows" data-slots-editor="">
      <span className="ac-form-title">Course slots</span>
      {rows.map((r, i) => (
        <div key={i} className="ac-row-edit" style={{ gridTemplateColumns: "100px 1fr 80px 1.6fr 40px" }}>
          <FormSelect label="Term" name={`slotTerm${i}`} value={r.term} onChange={(e) => set(i, { term: e.target.value })} options={Array.from({ length: Math.max(1, terms) }, (_, t) => ({ value: String(t + 1), label: `Term ${t + 1}` }))} hideLabel={i > 0} />
          <TextField label="Course code" name={`slotCode${i}`} value={r.code} placeholder="CS101" onChange={(e) => set(i, { code: e.target.value })} hideLabel={i > 0} />
          <TextField label="Credits" name={`slotCredits${i}`} type="number" inputMode="decimal" value={r.credits} placeholder="4" onChange={(e) => set(i, { credits: e.target.value })} hideLabel={i > 0} />
          <FormSelect label="Category" name={`slotCategory${i}`} value={r.category} onChange={(e) => set(i, { category: e.target.value })} options={Object.entries(CATEGORY_LABEL).map(([value, label]) => ({ value, label }))} hideLabel={i > 0} />
          <button type="button" className="sb-icon-btn" aria-label={`Remove slot ${i + 1}`} disabled={rows.length === 1} onClick={() => onChange(rows.filter((_, j) => j !== i))}>
            <X strokeWidth={1.75} />
          </button>
        </div>
      ))}
      {error && (
        <p className="sb-field-error" role="alert">
          {error}
        </p>
      )}
      <button type="button" className="sb-btn sb-btn--ghost ac-add-row" data-add-slot="" onClick={() => onChange([...rows, emptySlot()])}>
        <Plus strokeWidth={1.75} aria-hidden="true" />
        Add slot
      </button>
    </div>
  );
}

/** Complete rows as curriculum components (code upper-cased, title = code). */
export function slotComponents(rows: SlotRow[]) {
  return rows
    .filter((r) => r.code.trim() && r.credits.trim())
    .map((r) => ({ termNo: Number(r.term), slotCode: r.code.trim().toUpperCase(), title: r.code.trim().toUpperCase(), credits: Number(r.credits), category: r.category as ComponentCategory }));
}
