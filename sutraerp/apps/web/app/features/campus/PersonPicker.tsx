/**
 * Slice 9E — `PersonPicker`: choose a student (Domain C) or staff member (Domain E) by number and
 * name instead of typing a raw record id. Reads the existing directory endpoints
 * (`GET /api/v1/student/institutions/{id}/students`, `GET /api/v1/people/institutions/{id}/staff`)
 * and turns them into select options; the option value is the record id the campus APIs expect.
 *
 * Two shapes: `usePersonOptions` + `personField` for the declarative dialog forms
 * (`useAsk` / `QuickForm`), and the standalone `<PersonPicker>` component (@sutra/ui `Select`).
 */
import { Select, type SelectOption } from "@sutra/ui";
import { useQuery } from "@tanstack/react-query";
import { getCsrfToken, USES_API } from "../../lib/api";
import { fill } from "../institution/api";
import type { FieldSpec } from "../institution/forms";

export type PersonKind = "student" | "staff";

export interface PersonOption {
  kind: PersonKind;
  id: string;
  number: string;
  name: string;
}

export const PERSON_PATHS = {
  students: "/api/v1/student/institutions/{id}/students",
  staff: "/api/v1/people/institutions/{id}/staff",
} as const;

type Fetch = (input: string, init: RequestInit) => Promise<Response>;

export interface PersonDirectory {
  people(institutionId: string, kind: PersonKind): Promise<PersonOption[]>;
}

interface StudentRow {
  id: string;
  studentNumber: string;
  displayName: string;
  status?: string;
}
interface StaffRow {
  id: string;
  staffNumber: string;
  displayName: string;
  status?: string;
}

export function createHttpPersonDirectory(fetchImpl: Fetch = (i, init) => fetch(i, init), baseUrl: string = import.meta.env.VITE_API_BASE ?? ""): PersonDirectory {
  async function items<T>(path: string): Promise<T[]> {
    const headers: Record<string, string> = { Accept: "application/json" };
    const csrf = getCsrfToken();
    if (csrf) headers["X-CSRF-Token"] = csrf;
    const res = await fetchImpl(`${baseUrl}${path}`, { method: "GET", headers, credentials: "include" });
    if (!res.ok) return [];
    const body = (await res.json().catch(() => ({ items: [] }))) as { items?: T[] };
    return body.items ?? [];
  }
  return {
    async people(institutionId, kind) {
      if (kind === "student") {
        const rows = await items<StudentRow>(`${fill(PERSON_PATHS.students, { id: institutionId })}?limit=200`);
        return rows.filter((r) => r.status !== "merged").map((r) => ({ kind, id: r.id, number: r.studentNumber, name: r.displayName }));
      }
      const rows = await items<StaffRow>(`${fill(PERSON_PATHS.staff, { id: institutionId })}?limit=200`);
      return rows.filter((r) => r.status !== "merged").map((r) => ({ kind, id: r.id, number: r.staffNumber, name: r.displayName }));
    },
  };
}

/** Demo directory (mock mode): the personas the campus mocks already use. */
export const MOCK_PEOPLE: PersonOption[] = [
  { kind: "student", id: "stu-aarav", number: "S2026-0001", name: "Aarav Desai" },
  { kind: "student", id: "stu-isha", number: "S2026-0002", name: "Isha Kulkarni" },
  { kind: "student", id: "stu-rohan", number: "S2026-0003", name: "Rohan Mehta" },
  { kind: "staff", id: "stf-meera", number: "E-0001", name: "Meera Shah" },
  { kind: "staff", id: "stf-vikram", number: "E-0002", name: "Vikram Rao" },
];

export function createMockPersonDirectory(people: PersonOption[] = MOCK_PEOPLE): PersonDirectory {
  return { people: async (_institutionId, kind) => people.filter((p) => p.kind === kind) };
}

let fallback: PersonDirectory | null = null;
let override: PersonDirectory | null = null;

/** Tests swap the directory (`null` restores the default). */
export function setPersonDirectory(d: PersonDirectory | null) {
  override = d;
}

function directory(): PersonDirectory {
  if (override) return override;
  fallback ??= USES_API ? createHttpPersonDirectory() : createMockPersonDirectory();
  return fallback;
}

/** `S2026-0001 · Aarav Desai` (+ ` (staff)` when both kinds are listed together). */
export function personLabel(p: PersonOption, withKind = false): string {
  return `${p.number} · ${p.name}${withKind ? ` (${p.kind})` : ""}`;
}

/** Select options of people (`value` = record id). */
export function personOptions(people: readonly PersonOption[], withKind = false): SelectOption[] {
  return [...people].sort((a, b) => a.kind.localeCompare(b.kind) || a.number.localeCompare(b.number)).map((p) => ({ value: p.id, label: personLabel(p, withKind) }));
}

/** Students and / or staff of an institution for the pickers (empty while loading). */
export function usePeople(institutionId: string | undefined, kinds: readonly PersonKind[] = ["student", "staff"]): { people: PersonOption[]; loading: boolean } {
  const q = useQuery({
    queryKey: ["institution", "9e-people", institutionId ?? "-", kinds.join(",")],
    queryFn: async () => (await Promise.all(kinds.map((k) => directory().people(institutionId!, k)))).flat(),
    enabled: !!institutionId,
    staleTime: 60_000,
  });
  return { people: q.data ?? [], loading: q.isLoading };
}

/** The kind of the chosen record id (falls back to `fallback`, e.g. the form's kind select). */
export function kindOf(people: readonly PersonOption[], id: string | undefined, fallbackKind?: string): PersonKind {
  const hit = people.find((p) => p.id === id);
  if (hit) return hit.kind;
  return fallbackKind === "staff" ? "staff" : "student";
}

/** A dialog-form field choosing a person by number + name (value = record id). */
export function personField(people: readonly PersonOption[], opts: { name?: string; label?: string; kind?: PersonKind } = {}): FieldSpec {
  const shown = opts.kind ? people.filter((p) => p.kind === opts.kind) : people;
  return {
    name: opts.name ?? "personId",
    label: opts.label ?? (opts.kind === "staff" ? "Staff member" : opts.kind === "student" ? "Student" : "Student or staff member"),
    type: "select",
    required: true,
    wide: true,
    options: personOptions(shown, !opts.kind),
  };
}

/** Standalone picker. */
export function PersonPicker({
  institutionId,
  kind,
  value,
  onChange,
  label,
  name = "personId",
}: {
  institutionId: string;
  kind?: PersonKind;
  value: string;
  onChange: (id: string, person: PersonOption | undefined) => void;
  label?: string;
  name?: string;
}) {
  const { people, loading } = usePeople(institutionId, kind ? [kind] : ["student", "staff"]);
  const field = personField(people, { kind, label });
  const options: SelectOption[] = [{ value: "", label: loading ? "Loading…" : "Choose…" }, ...(field.options ?? [])];
  return (
    <span data-slice="9E" data-person-picker={kind ?? "any"}>
      <Select label={field.label} name={name} value={value} options={options} onChange={(e) => onChange(e.target.value, people.find((p) => p.id === e.target.value))} />
    </span>
  );
}
