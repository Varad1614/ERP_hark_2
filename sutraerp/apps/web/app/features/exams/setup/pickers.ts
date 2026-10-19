/**
 * Option lists for the Examinations forms (integrator): academic terms (Domain B calendars),
 * grading schemes (foundation API), course offerings (Batch 4) and students (Batch 3), so the
 * exam cell picks from lists instead of pasting ids.
 */
import { useQuery } from "@tanstack/react-query";
import { useOfferingApi } from "../../academic/offering/hooks";
import { useInstitutionApi } from "../../institution/hooks";
import { useRecordApi } from "../../student/record/hooks";
import { ekeys, useExamSetupApi } from "./hooks";

export interface Option {
  value: string;
  label: string;
}

const choose = (label: string, list: Option[]): Option[] => [{ value: "", label }, ...list];

/** Terms of every calendar version of the institution (published first), de-duplicated. */
export function useTermOptions(institutionId: string | null) {
  const api = useInstitutionApi();
  const q = useQuery({
    queryKey: ekeys.scoped(institutionId ?? "-", "term-options"),
    enabled: !!institutionId,
    queryFn: async (): Promise<Option[]> => {
      const calendars = await api.listCalendars(institutionId!);
      const versions = (await Promise.all(calendars.map((c) => api.listVersions(c.id)))).flat();
      const rank = (s: string) => (s === "published" ? 0 : s === "draft" ? 2 : 1);
      versions.sort((a, b) => rank(a.status) - rank(b.status));
      const seen = new Set<string>();
      const out: Option[] = [];
      for (const v of versions)
        for (const t of v.terms ?? []) {
          if (seen.has(t.id)) continue;
          seen.add(t.id);
          out.push({ value: t.id, label: `${t.name} (${t.code})${v.status === "published" ? "" : ` · ${v.status}`}` });
        }
      return out;
    },
  });
  return choose("Choose a term", q.data ?? []);
}

export function useSchemeOptions(institutionId: string | null) {
  const api = useExamSetupApi();
  const q = useQuery({ queryKey: ekeys.scoped(institutionId ?? "-", "scheme-options"), enabled: !!institutionId, queryFn: () => api.gradingSchemes(institutionId!) });
  return choose(
    "Institution default",
    (q.data ?? []).filter((s) => s.status === "active").map((s) => ({ value: s.id, label: `${s.code} — ${s.name}` })),
  );
}

export function useOfferingOptions(institutionId: string | null) {
  const api = useOfferingApi();
  const q = useQuery({ queryKey: ekeys.scoped(institutionId ?? "-", "offering-options"), enabled: !!institutionId, queryFn: () => api.listOfferings(institutionId!) });
  return choose(
    "Choose a course offering",
    (q.data ?? []).filter((o) => !["cancelled", "retired", "archived"].includes(o.status)).map((o) => ({ value: o.id, label: `${o.offeringCode} — ${o.title}` })),
  );
}

export function useStudentOptions(institutionId: string | null) {
  const api = useRecordApi();
  const q = useQuery({ queryKey: ekeys.scoped(institutionId ?? "-", "student-options"), enabled: !!institutionId, queryFn: () => api.directory(institutionId!) });
  return choose(
    "Choose a student",
    (q.data ?? []).map((s) => ({ value: s.id, label: `${s.displayName} (${s.studentNumber})` })),
  );
}
