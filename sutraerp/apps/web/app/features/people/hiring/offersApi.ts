/**
 * ERP_EXTENSIONS 5A API layer: offers, onboarding checklists, follow-ups / attention, hiring
 * analytics and the careers page. Same shape as `./api` (3B): one interface, an HTTP
 * implementation (CSRF, Idempotency-Key on create, If-Match `"<version>"` on offer commands,
 * problem+json → `ApiProblem`), plus react-query hooks. Query keys start with `institution` so
 * `useCommand` refetches them after every command. Paths follow docs/extensions-5a-hiring-offers.md
 * (not in schema.d.ts yet, hence no `satisfies keyof paths`).
 */
import { ApiProblem, isProblem, isUnsafeMethod, newIdempotencyKey } from "@sutra/api-client";
import { useQuery } from "@tanstack/react-query";
import { useMemo } from "react";
import { getCsrfToken } from "../../../lib/api";
import { useCommand } from "../../institution/hooks";
import type {
  CareersApplication, CareersListingInput, CareersPage, CareersPageInput, CareersReceipt, Checklist, ChecklistTaskInput, DueTask,
  FollowupInput, FollowupView, HiringAnalytics, HiringAnalyticsQuery, HiringAttention, OfferInput, OfferLetter, OfferSettings,
  OfferStatus, OfferView, PublicCareers,
} from "./offersTypes";

const B = "/api/v1/people";
export const OFFER_PATHS = {
  settings: `${B}/institutions/{id}/offer-settings`,
  institutionOffers: `${B}/institutions/{id}/offers`,
  candidateOffers: `${B}/candidates/{id}/offers`,
  offer: `${B}/offers/{id}`,
  submit: `${B}/offers/{id}/submit`,
  decision: `${B}/offers/{id}/decision`,
  send: `${B}/offers/{id}/send`,
  accept: `${B}/offers/{id}/accept`,
  decline: `${B}/offers/{id}/decline`,
  withdraw: `${B}/offers/{id}/withdraw`,
  letter: `${B}/offers/{id}/letter`,
  checklist: `${B}/onboarding-cases/{id}/checklist`,
  taskComplete: `${B}/onboarding-tasks/{id}/complete`,
  onboardingDue: `${B}/institutions/{id}/onboarding-due`,
  followups: `${B}/candidates/{id}/followups`,
  followupComplete: `${B}/candidate-followups/{id}/complete`,
  attention: `${B}/institutions/{id}/hiring-followups`,
  analytics: `${B}/institutions/{id}/hiring-analytics`,
  careersPage: `${B}/institutions/{id}/careers-page`,
  careersListing: `${B}/requisitions/{id}/careers-listing`,
  publicCareers: `${B}/public/careers/{id}`,
  publicApply: `${B}/public/careers/{id}/requisitions/{requisition_id}/apply`,
} as const;

/** The offer's version travels as `If-Match: "<version>"`. */
type Versioned = Pick<OfferView, "id" | "version">;

export interface OffersApi {
  settings(institutionId: string): Promise<OfferSettings>;
  saveSettings(institutionId: string, s: Pick<OfferSettings, "offerTemplate" | "onboardingTemplates" | "staleAfterDays" | "retentionMonths">): Promise<OfferSettings>;
  institutionOffers(institutionId: string, status?: OfferStatus | null): Promise<OfferView[]>;
  candidateOffers(candidateId: string): Promise<OfferView[]>;
  offer(id: string): Promise<OfferView>;
  draftOffer(candidateId: string, input: OfferInput): Promise<OfferView>;
  updateOffer(offer: Versioned, input: OfferInput): Promise<OfferView>;
  submitOffer(offer: Versioned): Promise<OfferView>;
  decideOffer(offer: Versioned, decision: "approve" | "reject", reason?: string | null): Promise<OfferView>;
  sendOffer(offer: Versioned): Promise<OfferView>;
  acceptOffer(offer: Versioned, acceptedByName?: string | null): Promise<OfferView>;
  declineOffer(offer: Versioned, reason: string): Promise<OfferView>;
  withdrawOffer(offer: Versioned, reason: string): Promise<OfferView>;
  letter(offerId: string): Promise<OfferLetter>;
  checklist(onboardingCaseId: string): Promise<Checklist>;
  addChecklistTask(onboardingCaseId: string, input: ChecklistTaskInput): Promise<Checklist>;
  setTaskDone(taskId: string, done: boolean): Promise<Checklist>;
  onboardingDue(institutionId: string): Promise<DueTask[]>;
  followups(candidateId: string): Promise<FollowupView[]>;
  addFollowup(candidateId: string, input: FollowupInput): Promise<FollowupView>;
  setFollowupDone(followupId: string, done: boolean): Promise<void>;
  attention(institutionId: string): Promise<HiringAttention>;
  analytics(institutionId: string, q?: HiringAnalyticsQuery): Promise<HiringAnalytics>;
  careersPage(institutionId: string): Promise<CareersPage>;
  saveCareersPage(institutionId: string, input: CareersPageInput): Promise<CareersPage>;
  publishRequisition(requisitionId: string, input: CareersListingInput): Promise<CareersPage>;
  publicCareers(slug: string): Promise<PublicCareers>;
  apply(slug: string, requisitionId: string, input: CareersApplication): Promise<CareersReceipt>;
}

const fill = (t: string, id: string) => t.replace("{id}", encodeURIComponent(id));
const query = (q: Record<string, string | number | null | undefined>) => {
  const s = new URLSearchParams();
  for (const [k, v] of Object.entries(q)) if (v != null && v !== "") s.set(k, String(v));
  const out = s.toString();
  return out ? `?${out}` : "";
};
type Fetch = (input: string, init: RequestInit) => Promise<Response>;

export function createHttpOffersApi(fetchImpl: Fetch = (i, init) => fetch(i, init), baseUrl: string = import.meta.env.VITE_API_BASE ?? ""): OffersApi {
  async function request<T>(method: string, path: string, opts: { body?: unknown; idempotent?: boolean; ifMatch?: number } = {}): Promise<T> {
    const headers: Record<string, string> = { Accept: "application/json" };
    const unsafe = isUnsafeMethod(method);
    if (unsafe) {
      headers["Content-Type"] = "application/json";
      const csrf = getCsrfToken();
      if (csrf) headers["X-CSRF-Token"] = csrf;
    }
    if (opts.idempotent) headers["Idempotency-Key"] = newIdempotencyKey();
    if (opts.ifMatch !== undefined) headers["If-Match"] = `"${opts.ifMatch}"`;
    const res = await fetchImpl(`${baseUrl}${path}`, { method, headers, credentials: "include", body: unsafe ? JSON.stringify(opts.body ?? {}) : undefined });
    const text = await res.text();
    let data: unknown;
    try {
      data = text ? JSON.parse(text) : undefined;
    } catch {
      data = undefined;
    }
    if (!res.ok) {
      if (isProblem(data)) throw new ApiProblem(data);
      throw new ApiProblem({ type: "about:blank", title: `Request failed (${res.status})`, status: res.status, errorCode: `http-${res.status}` });
    }
    return data as T;
  }
  const P = OFFER_PATHS;
  const cmd = (path: string, o: Versioned, body?: unknown) => request<OfferView>("POST", fill(path, o.id), { body, ifMatch: o.version });
  return {
    settings: (id) => request("GET", fill(P.settings, id)),
    saveSettings: (id, s) => request("PUT", fill(P.settings, id), { body: s }),
    institutionOffers: async (id, status) => (await request<{ items: OfferView[] }>("GET", fill(P.institutionOffers, id) + query({ status }))).items,
    candidateOffers: async (id) => (await request<{ items: OfferView[] }>("GET", fill(P.candidateOffers, id))).items,
    offer: (id) => request("GET", fill(P.offer, id)),
    draftOffer: (id, input) => request("POST", fill(P.candidateOffers, id), { body: input, idempotent: true }),
    updateOffer: (o, input) => request("PUT", fill(P.offer, o.id), { body: input, ifMatch: o.version }),
    submitOffer: (o) => cmd(P.submit, o),
    decideOffer: (o, decision, reason) => cmd(P.decision, o, { decision, reason: reason ?? null }),
    sendOffer: (o) => cmd(P.send, o),
    acceptOffer: (o, acceptedByName) => cmd(P.accept, o, { acceptedByName: acceptedByName ?? null }),
    declineOffer: (o, reason) => cmd(P.decline, o, { reason }),
    withdrawOffer: (o, reason) => cmd(P.withdraw, o, { reason }),
    letter: (id) => request("GET", fill(P.letter, id)),
    checklist: (id) => request("GET", fill(P.checklist, id)),
    addChecklistTask: (id, input) => request("POST", fill(P.checklist, id), { body: input }),
    setTaskDone: (id, done) => request("POST", fill(P.taskComplete, id), { body: { done } }),
    onboardingDue: async (id) => (await request<{ items: DueTask[] }>("GET", fill(P.onboardingDue, id))).items,
    followups: async (id) => (await request<{ items: FollowupView[] }>("GET", fill(P.followups, id))).items,
    addFollowup: (id, input) => request("POST", fill(P.followups, id), { body: input }),
    setFollowupDone: async (id, done) => {
      await request<void>("POST", fill(P.followupComplete, id), { body: { done } });
    },
    attention: (id) => request("GET", fill(P.attention, id)),
    analytics: (id, q = {}) => request("GET", fill(P.analytics, id) + query({ requisitionId: q.requisitionId, sinceDays: q.sinceDays })),
    careersPage: (id) => request("GET", fill(P.careersPage, id)),
    saveCareersPage: (id, input) => request("PUT", fill(P.careersPage, id), { body: input }),
    publishRequisition: (id, input) => request("PUT", fill(P.careersListing, id), { body: input }),
    publicCareers: (slug) => request("GET", fill(P.publicCareers, slug)),
    apply: (slug, rid, input) => request("POST", fill(P.publicApply, slug).replace("{requisition_id}", encodeURIComponent(rid)), { body: input }),
  };
}

let fallback: OffersApi | null = null;
/** The shared HTTP instance (no mock yet: in mock mode these calls reach the network). */
export function defaultOffersApi(): OffersApi {
  fallback ??= createHttpOffersApi();
  return fallback;
}

// ------------------------------------------------------------------ query keys

/** Prefix `institution`: `useCommand` refreshes every `institution` query after a command. */
export const offerKeys = {
  all: ["institution", "hiring", "offers"] as const,
  settings: (institutionId: string) => ["institution", "hiring", "offers", "settings", institutionId] as const,
  institution: (institutionId: string, status?: string | null) => ["institution", "hiring", "offers", "institution", institutionId, status ?? "all"] as const,
  candidate: (candidateId: string) => ["institution", "hiring", "offers", "candidate", candidateId] as const,
  offer: (id: string) => ["institution", "hiring", "offers", "offer", id] as const,
  letter: (id: string) => ["institution", "hiring", "offers", "letter", id] as const,
  checklist: (caseId: string) => ["institution", "hiring", "onboarding", "checklist", caseId] as const,
  onboardingDue: (institutionId: string) => ["institution", "hiring", "onboarding", "due", institutionId] as const,
  followups: (candidateId: string) => ["institution", "hiring", "followups", candidateId] as const,
  attention: (institutionId: string) => ["institution", "hiring", "attention", institutionId] as const,
  analytics: (institutionId: string, requisitionId?: string | null, sinceDays?: number | null) =>
    ["institution", "hiring", "analytics", institutionId, requisitionId ?? "all", sinceDays ?? "all"] as const,
  careers: (institutionId: string) => ["institution", "hiring", "careers", institutionId] as const,
  publicCareers: (slug: string) => ["institution", "hiring", "careers-public", slug] as const,
};

// ------------------------------------------------------------------ queries

export const useOfferSettings = (institutionId: string, api: OffersApi = defaultOffersApi()) =>
  useQuery({ queryKey: offerKeys.settings(institutionId), queryFn: () => api.settings(institutionId), enabled: institutionId !== "" });

export const useInstitutionOffers = (institutionId: string, status?: OfferStatus | null, api: OffersApi = defaultOffersApi()) =>
  useQuery({ queryKey: offerKeys.institution(institutionId, status), queryFn: () => api.institutionOffers(institutionId, status), enabled: institutionId !== "" });

export const useCandidateOffers = (candidateId: string, api: OffersApi = defaultOffersApi()) =>
  useQuery({ queryKey: offerKeys.candidate(candidateId), queryFn: () => api.candidateOffers(candidateId), enabled: candidateId !== "" });

export const useOffer = (offerId: string, api: OffersApi = defaultOffersApi()) =>
  useQuery({ queryKey: offerKeys.offer(offerId), queryFn: () => api.offer(offerId), enabled: offerId !== "" });

export const useOfferLetter = (offerId: string, api: OffersApi = defaultOffersApi()) =>
  useQuery({ queryKey: offerKeys.letter(offerId), queryFn: () => api.letter(offerId), enabled: offerId !== "" });

export const useOnboardingChecklist = (onboardingCaseId: string, api: OffersApi = defaultOffersApi()) =>
  useQuery({ queryKey: offerKeys.checklist(onboardingCaseId), queryFn: () => api.checklist(onboardingCaseId), enabled: onboardingCaseId !== "" });

export const useOnboardingDue = (institutionId: string, api: OffersApi = defaultOffersApi()) =>
  useQuery({ queryKey: offerKeys.onboardingDue(institutionId), queryFn: () => api.onboardingDue(institutionId), enabled: institutionId !== "" });

export const useCandidateFollowups = (candidateId: string, api: OffersApi = defaultOffersApi()) =>
  useQuery({ queryKey: offerKeys.followups(candidateId), queryFn: () => api.followups(candidateId), enabled: candidateId !== "" });

export const useHiringAttention = (institutionId: string, api: OffersApi = defaultOffersApi()) =>
  useQuery({ queryKey: offerKeys.attention(institutionId), queryFn: () => api.attention(institutionId), enabled: institutionId !== "" });

/** Institution-wide analytics (optionally limited to the last `sinceDays`). */
export const useHiringAnalytics = (institutionId: string, sinceDays?: number | null, api: OffersApi = defaultOffersApi()) =>
  useQuery({ queryKey: offerKeys.analytics(institutionId, null, sinceDays), queryFn: () => api.analytics(institutionId, { sinceDays }), enabled: institutionId !== "" });

/** Analytics of one requisition. */
export const useRequisitionAnalytics = (institutionId: string, requisitionId: string, sinceDays?: number | null, api: OffersApi = defaultOffersApi()) =>
  useQuery({
    queryKey: offerKeys.analytics(institutionId, requisitionId, sinceDays),
    queryFn: () => api.analytics(institutionId, { requisitionId, sinceDays }),
    enabled: institutionId !== "" && requisitionId !== "",
  });

export const useCareersPage = (institutionId: string, api: OffersApi = defaultOffersApi()) =>
  useQuery({ queryKey: offerKeys.careers(institutionId), queryFn: () => api.careersPage(institutionId), enabled: institutionId !== "" });

export const usePublicCareers = (slug: string, api: OffersApi = defaultOffersApi()) =>
  useQuery({ queryKey: offerKeys.publicCareers(slug), queryFn: () => api.publicCareers(slug), enabled: slug !== "" });

// ------------------------------------------------------------------ commands

/**
 * Every 5A mutation through `useCommand` (toast, problem+json messages, step-up retry, then a
 * refetch of all `institution` queries). Render `dialog` once in the tile.
 */
export function useOfferCommands(api: OffersApi = defaultOffersApi()) {
  const { run, busy, dialog } = useCommand();
  const commands = useMemo(
    () => ({
      saveSettings: (institutionId: string, s: Parameters<OffersApi["saveSettings"]>[1]) => run(() => api.saveSettings(institutionId, s), "Offer settings saved"),
      draftOffer: (candidateId: string, input: OfferInput) => run(() => api.draftOffer(candidateId, input), (o) => `Offer ${o.number} drafted`),
      updateOffer: (offer: Versioned, input: OfferInput) => run(() => api.updateOffer(offer, input), "Offer updated"),
      submitOffer: (offer: Versioned) => run(() => api.submitOffer(offer), (o) => (o.status === "approved" ? "Offer submitted and approved" : "Offer sent for approval")),
      approveOffer: (offer: Versioned) => run(() => api.decideOffer(offer, "approve"), "Offer approved"),
      rejectOffer: (offer: Versioned, reason: string) => run(() => api.decideOffer(offer, "reject", reason), "Offer returned to draft"),
      sendOffer: (offer: Versioned) => run(() => api.sendOffer(offer), "Offer letter sent"),
      acceptOffer: (offer: Versioned, acceptedByName?: string | null) => run(() => api.acceptOffer(offer, acceptedByName), "Acceptance recorded"),
      declineOffer: (offer: Versioned, reason: string) => run(() => api.declineOffer(offer, reason), "Decline recorded"),
      withdrawOffer: (offer: Versioned, reason: string) => run(() => api.withdrawOffer(offer, reason), "Offer withdrawn"),
      addChecklistTask: (onboardingCaseId: string, input: ChecklistTaskInput) => run(() => api.addChecklistTask(onboardingCaseId, input), "Task added"),
      tickTask: (taskId: string) => run(() => api.setTaskDone(taskId, true), "Task done"),
      untickTask: (taskId: string) => run(() => api.setTaskDone(taskId, false), "Task reopened"),
      addFollowup: (candidateId: string, input: FollowupInput) => run(() => api.addFollowup(candidateId, input), "Follow-up added"),
      completeFollowup: (followupId: string, done = true) => run(() => api.setFollowupDone(followupId, done), done ? "Follow-up done" : "Follow-up reopened"),
      saveCareersPage: (institutionId: string, input: CareersPageInput) => run(() => api.saveCareersPage(institutionId, input), "Careers page saved"),
      publishRequisition: (requisitionId: string, input: CareersListingInput) =>
        run(() => api.publishRequisition(requisitionId, input), input.published ? "Listed on the careers page" : "Removed from the careers page"),
    }),
    [api, run],
  );
  return { ...commands, busy, dialog };
}
