/**
 * Re-skin v2 (R3): syllabus coverage of the faculty's sections (`GET /academic/sections/{id}/
 * syllabus-coverage`, faculty / attendance managers only) → the "Syllabus · 2 of 5 units" progress
 * of the prototype class cards. Read-only; mock mode has no coverage (the cards then omit the bar).
 */
import { useQueries } from "@tanstack/react-query";
import type { components } from "@sutra/api-client";
import { USES_API } from "../../../lib/api";

type Coverage = components["schemas"]["SyllabusCoverage"];

async function fetchCoverage(sectionId: string): Promise<Coverage[]> {
  if (!USES_API) return [];
  const base = import.meta.env.VITE_API_BASE ?? "";
  const res = await fetch(`${base}/api/v1/academic/sections/${encodeURIComponent(sectionId)}/syllabus-coverage`, { headers: { Accept: "application/json" }, credentials: "include" });
  if (!res.ok) return [];
  return ((await res.json()) as { items: Coverage[] }).items ?? [];
}

export interface SyllabusProgress {
  done: number;
  total: number;
  percent: number;
}

/** Progress of the latest published (else newest) coverage record per section. */
export function useSyllabus(sectionIds: string[]): Map<string, SyllabusProgress> {
  const qs = useQueries({ queries: sectionIds.map((id) => ({ queryKey: ["institution", "r3", id, "syllabus"], queryFn: () => fetchCoverage(id) })) });
  const out = new Map<string, SyllabusProgress>();
  qs.forEach((q, i) => {
    const list = q.data ?? [];
    const rec = list.find((c) => c.status === "published") ?? list[0];
    if (!rec) return;
    const topics = (rec.topics ?? []).filter((t) => t.status !== "cancelled");
    if (topics.length === 0) return;
    out.set(sectionIds[i]!, { done: topics.filter((t) => t.status === "completed").length, total: topics.length, percent: rec.completedPercent ?? Math.round((100 * topics.filter((t) => t.status === "completed").length) / topics.length) });
  });
  return out;
}
