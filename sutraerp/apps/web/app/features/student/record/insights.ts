/**
 * Read-only cross-domain facts shown on the Students screens (R2 re-skin): fee status from the
 * finance student accounts, attendance from the academic section summaries. GET only, so no CSRF
 * / If-Match; mock mode returns nothing (the screens then show "—", never invented numbers).
 */
import { useQuery } from "@tanstack/react-query";
import type { components, paths } from "@sutra/api-client";
import { USES_API } from "../../../lib/api";

type S = components["schemas"];
export type FeeAccount = S["StudentAccountView"];
export type FeeCharge = S["ChargeListItem"];
export type FeePayment = S["PaymentView"];
export type AttendanceLine = S["AttendanceSummaryLine"];

const P = {
  accounts: "/api/v1/finance/institutions/{id}/student-accounts",
  charges: "/api/v1/finance/student-accounts/{id}/charges",
  payments: "/api/v1/finance/student-accounts/{id}/payments",
  offerings: "/api/v1/academic/institutions/{id}/offerings",
  summary: "/api/v1/academic/sections/{id}/attendance-summary",
} as const satisfies Record<string, keyof paths>;

const fillId = (t: string, id: string) => t.replace("{id}", encodeURIComponent(id));

async function getJson<T>(url: string): Promise<T | null> {
  if (!USES_API) return null;
  const base = import.meta.env.VITE_API_BASE ?? "";
  const res = await fetch(`${base}${url}`, { headers: { Accept: "application/json" }, credentials: "include" });
  // A role without finance / academic read access simply sees no figures.
  if (res.status === 401 || res.status === 403 || res.status === 404) return null;
  if (!res.ok) throw new Error(`GET ${url} → ${res.status}`);
  return (await res.json()) as T;
}

/** Fee standing of an account: paid / partial / overdue / due (no charges → null). */
export type FeeStatus = "paid" | "partial" | "overdue" | "due";
export function feeStatus(a: FeeAccount | null | undefined): FeeStatus | null {
  if (!a) return null;
  const charged = Number(a.balance.totalCharged);
  const outstanding = Number(a.balance.outstanding);
  const settled = Number(a.balance.totalSettled);
  if (!(charged > 0)) return null;
  if (outstanding <= 0) return "paid";
  if (Number(a.balance.overdue) > 0) return "overdue";
  if (settled > 0) return "partial";
  return "due";
}
export const FEE_LABEL: Record<FeeStatus, string> = { paid: "Paid", partial: "Partial", overdue: "Overdue", due: "Due" };

/** Finance accounts of an institution, by student id. */
export function useFeeAccounts(institutionId: string | null | undefined, allowed = true) {
  return useQuery({
    queryKey: ["student-r2", "fee-accounts", institutionId ?? "-"],
    queryFn: async () => {
      const r = await getJson<{ items: FeeAccount[] }>(`${fillId(P.accounts, institutionId!)}?limit=200`);
      const map = new Map<string, FeeAccount>();
      for (const a of r?.items ?? []) if (a.studentId) map.set(a.studentId, a);
      return map;
    },
    enabled: !!institutionId && allowed,
    staleTime: 60_000,
  });
}

export function useFeeDetail(accountId: string | null | undefined) {
  const charges = useQuery({
    queryKey: ["student-r2", "fee-charges", accountId ?? "-"],
    queryFn: async () => (await getJson<{ items: FeeCharge[] }>(fillId(P.charges, accountId!)))?.items ?? [],
    enabled: !!accountId,
  });
  const payments = useQuery({
    queryKey: ["student-r2", "fee-payments", accountId ?? "-"],
    queryFn: async () => (await getJson<{ items: FeePayment[] }>(fillId(P.payments, accountId!)))?.items ?? [],
    enabled: !!accountId,
  });
  return { charges, payments };
}

export interface CourseAttendance {
  offeringId: string;
  sectionCode: string;
  courseCode: string;
  title: string;
  credits: number;
  line: AttendanceLine;
  threshold: number;
}

/** Per-student attendance across the institution's live sections (finalised sessions only). */
export function useAttendanceIndex(institutionId: string | null | undefined, allowed = true) {
  return useQuery({
    queryKey: ["student-r2", "attendance", institutionId ?? "-"],
    queryFn: async () => {
      const byStudent = new Map<string, CourseAttendance[]>();
      const offerings = (await getJson<{ items: S["Offering"][] }>(fillId(P.offerings, institutionId!)))?.items ?? [];
      const live = offerings.filter((o) => !["cancelled", "archived", "merged", "draft"].includes(o.status)).slice(0, 25);
      const sections = live.flatMap((o) => o.sections.map((s) => ({ o, s })));
      const summaries = await Promise.all(sections.map(({ s }) => getJson<S["AttendanceSummary"]>(fillId(P.summary, s.id)).catch(() => null)));
      summaries.forEach((sum, i) => {
        if (!sum) return;
        const { o, s } = sections[i]!;
        for (const line of sum.items) {
          if (line.total <= 0) continue;
          const list = byStudent.get(line.studentId) ?? [];
          list.push({ offeringId: o.id, sectionCode: s.sectionCode, courseCode: o.courseCode ?? o.offeringCode, title: o.title, credits: o.credits, line, threshold: sum.thresholdPercent });
          byStudent.set(line.studentId, list);
        }
      });
      return byStudent;
    },
    enabled: !!institutionId && allowed,
    staleTime: 60_000,
  });
}

/** Overall attendance % of a student's courses (null without finalised sessions). */
export function overallAttendance(list: CourseAttendance[] | undefined): { percent: number; below: boolean; threshold: number } | null {
  if (!list || list.length === 0) return null;
  const attended = list.reduce((a, c) => a + c.line.attended, 0);
  const total = list.reduce((a, c) => a + c.line.total, 0);
  if (total <= 0) return null;
  const threshold = Math.max(...list.map((c) => c.threshold));
  const percent = (attended / total) * 100;
  return { percent, below: percent < threshold, threshold };
}
